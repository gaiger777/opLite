import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { Folder, FolderKind, FolderQuota } from "@/api/types";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Input } from "@/components/ui/input";

/** 폴더 이름 규칙(서버 `validate_folder_name`): 1~64자, `/` 없음, 앞뒤 공백 없음 */
export function validFolderName(name: string): boolean {
  return name.length > 0 && [...name].length <= 64 && !name.includes("/") && name.trim() === name;
}

/** 새 폴더(부모가 없으면 그 트리의 루트 바로 아래). 만든 폴더를 돌려준다. */
export function CreateFolderDialog({
  kind,
  parent,
  parentPath,
  onClose,
  onCreated,
}: {
  kind: FolderKind;
  parent: string | null;
  parentPath: string;
  onClose(): void;
  onCreated?(folder: Folder): void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [error, setError] = useState<string | undefined>();

  async function submit() {
    if (!validFolderName(name)) {
      setError("folders.badName");
      return false;
    }
    const folder = await call(
      api.POST("/folders", { body: { name, kind, ...(parent && { parent }) } }),
    );
    toast.success(t("folders.created", { path: `${parentPath.replace(/\/$/, "")}/${name}` }));
    onCreated?.(folder);
  }

  return (
    <FormDialog
      title={t("folders.createTitle")}
      description={t("folders.createIn", { path: parentPath })}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="folder-name" label={t("common.name")} error={error}>
        {(a11y) => (
          <Input
            id="folder-name"
            autoComplete="off"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}

export function RenameFolderDialog({ folder, onClose }: { folder: Folder; onClose(): void }) {
  const { t } = useTranslation();
  const [name, setName] = useState(folder.spec.name);
  const [error, setError] = useState<string | undefined>();

  async function submit() {
    if (!validFolderName(name)) {
      setError("folders.badName");
      return false;
    }
    await call(
      api.PATCH("/folders/{id}", {
        params: { path: { id: folder.id }, header: { "If-Match": folder.revision } },
        body: { name },
      }),
    );
    toast.success(t("folders.renamed", { name }));
  }

  return (
    <FormDialog
      title={t("folders.renameTitle", { name: folder.spec.name })}
      submitLabel={t("common.save")}
      submitDisabled={name === folder.spec.name}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="folder-rename" label={t("common.name")} error={error}>
        {(a11y) => (
          <Input
            id="folder-rename"
            autoComplete="off"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}

/** 빈 폴더만 지운다(서버 409 `folder_not_empty`). */
export function DeleteFolderDialog({
  folder,
  path,
  open,
  onOpenChange,
  onDeleted,
}: {
  folder: Folder | null;
  path: string;
  open: boolean;
  onOpenChange(open: boolean): void;
  onDeleted?(): void;
}) {
  const { t } = useTranslation();
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title={t("folders.deleteTitle", { path })}
      description={t("folders.deleteDescription")}
      confirmLabel={t("common.delete")}
      onConfirm={async () => {
        await call(api.DELETE("/folders/{id}", { params: { path: { id: folder!.id } } }));
        toast.success(t("folders.deleted", { path }));
        onDeleted?.();
      }}
    />
  );
}

const GIB = 1024 ** 3;

/** 칸의 글 → 상한(빈 칸은 상한 없음). 틀리면 undefined */
function parseLimit(text: string, scale: number): number | null | undefined {
  const v = text.trim();
  if (v === "") {
    return null;
  }
  const n = Number(v);
  return Number.isFinite(n) && n >= 0 ? Math.round(n * scale) : undefined;
}

/** 할당량: vCPU·메모리(GiB)·디스크(GiB)·VM 수. 빈 칸은 상한이 없다. 모두 비우면 할당량을 지운다. */
export function QuotaDialog({ folder, onClose }: { folder: Folder; onClose(): void }) {
  const { t } = useTranslation();
  const q = folder.spec.quota ?? {};
  const [values, setValues] = useState({
    vcpus: q.vcpus?.toString() ?? "",
    memory: q.memory_mib !== undefined ? String(q.memory_mib / 1024) : "",
    disk: q.disk_bytes !== undefined ? String(q.disk_bytes / GIB) : "",
    vms: q.vms?.toString() ?? "",
  });
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const parsed = {
      vcpus: parseLimit(values.vcpus, 1),
      memory_mib: parseLimit(values.memory, 1024),
      disk_bytes: parseLimit(values.disk, GIB),
      vms: parseLimit(values.vms, 1),
    };
    const found: Record<string, string> = {};
    for (const [key, field] of [
      ["vcpus", "vcpus"],
      ["memory_mib", "memory"],
      ["disk_bytes", "disk"],
      ["vms", "vms"],
    ] as const) {
      if (parsed[key] === undefined) {
        found[field] = "folders.badLimit";
      }
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const quota: FolderQuota = Object.fromEntries(
      Object.entries(parsed).filter(([, v]) => v !== null),
    );
    await call(
      api.PATCH("/folders/{id}", {
        params: { path: { id: folder.id }, header: { "If-Match": folder.revision } },
        body: { quota: Object.keys(quota).length > 0 ? quota : null },
      }),
    );
    toast.success(t("folders.quotaSaved", { name: folder.spec.name }));
  }

  const field = (id: keyof typeof values, label: string) => (
    <Field id={`quota-${id}`} label={label} error={errors[id]} hint={t("folders.noLimitHint")}>
      {(a11y) => (
        <Input
          id={`quota-${id}`}
          inputMode="decimal"
          autoComplete="off"
          value={values[id]}
          onChange={(e) => setValues({ ...values, [id]: e.target.value })}
          {...a11y}
        />
      )}
    </Field>
  );

  return (
    <FormDialog
      title={t("folders.quotaTitle", { name: folder.spec.name })}
      description={t("folders.quotaDescription")}
      submitLabel={t("common.save")}
      onClose={onClose}
      onSubmit={submit}
    >
      {field("vcpus", t("folders.dim.vcpus"))}
      {field("memory", t("folders.memoryGib"))}
      {field("disk", t("folders.diskGib"))}
      {field("vms", t("folders.dim.vms"))}
    </FormDialog>
  );
}
