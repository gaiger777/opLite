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

/**
 * 칸의 글 → 상한(빈 칸은 상한 없음). 틀리면 undefined. 개수(`scale` 1: vCPU·VM 수)는 0 이상의 정수만 받고(반올림하면
 * 0.4가 0이 되어 모두 막는다), GiB 칸은 소수를 받아 MiB·바이트로 반올림한다.
 */
export function parseLimit(text: string, scale: number): number | null | undefined {
  const v = text.trim();
  if (v === "") {
    return null;
  }
  const n = Number(v);
  if (!Number.isFinite(n) || n < 0 || (scale === 1 && !Number.isInteger(n))) {
    return undefined;
  }
  return Math.round(n * scale);
}

/** 네트워크 폴더의 할당량 차원(서버 `FolderKind::quota_dimensions`) */
const NETWORK_DIMS = ["networks", "routers", "floating_ips", "load_balancers"] as const;

/**
 * 네트워크 폴더의 할당량(계획 66·67): 네트워크·라우터·유동 IP(부하 분산기의 바깥 VIP 포함)·부하 분산기 수. 빈 칸은 상한이
 * 없고, 모두 비우면 할당량을 지운다. 옛 노드가 남은 클러스터는 서버가 409 `cluster_upgrading`이다.
 */
export function NetworkQuotaDialog({ folder, onClose }: { folder: Folder; onClose(): void }) {
  const { t } = useTranslation();
  const q = folder.spec.quota ?? {};
  const [values, setValues] = useState(
    () =>
      Object.fromEntries(NETWORK_DIMS.map((d) => [d, q[d]?.toString() ?? ""])) as Record<
        (typeof NETWORK_DIMS)[number],
        string
      >,
  );
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const parsed = Object.fromEntries(
      NETWORK_DIMS.map((d) => [d, parseLimit(values[d], 1)]),
    ) as Record<(typeof NETWORK_DIMS)[number], number | null | undefined>;
    const found: Record<string, string> = {};
    for (const key of NETWORK_DIMS) {
      if (parsed[key] === undefined) {
        found[key] = "folders.badCount";
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

  return (
    <FormDialog
      title={t("folders.quotaTitle", { name: folder.spec.name })}
      description={t("folders.networkQuotaDescription")}
      submitLabel={t("common.save")}
      onClose={onClose}
      onSubmit={submit}
    >
      {NETWORK_DIMS.map((id) => (
        <Field
          key={id}
          id={`quota-${id}`}
          label={t(`folders.dim.${id}`)}
          error={errors[id]}
          hint={t("folders.noLimitHint")}
        >
          {(a11y) => (
            <Input
              id={`quota-${id}`}
              inputMode="numeric"
              autoComplete="off"
              value={values[id]}
              onChange={(e) => setValues({ ...values, [id]: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      ))}
    </FormDialog>
  );
}

/**
 * 할당량: vCPU·메모리(GiB)·디스크(GiB)·VM 수·전용 vCPU(계획 64). 빈 칸은 상한이 없다. 모두 비우면 할당량을 지운다.
 */
export function QuotaDialog({ folder, onClose }: { folder: Folder; onClose(): void }) {
  const { t } = useTranslation();
  const q = folder.spec.quota ?? {};
  const [values, setValues] = useState({
    vcpus: q.vcpus?.toString() ?? "",
    memory: q.memory_mib !== undefined ? String(q.memory_mib / 1024) : "",
    disk: q.disk_bytes !== undefined ? String(q.disk_bytes / GIB) : "",
    vms: q.vms?.toString() ?? "",
    dedicated: q.dedicated_vcpus?.toString() ?? "",
  });
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const parsed = {
      vcpus: parseLimit(values.vcpus, 1),
      memory_mib: parseLimit(values.memory, 1024),
      disk_bytes: parseLimit(values.disk, GIB),
      vms: parseLimit(values.vms, 1),
      dedicated_vcpus: parseLimit(values.dedicated, 1),
    };
    const found: Record<string, string> = {};
    for (const [key, field] of [
      ["vcpus", "vcpus"],
      ["memory_mib", "memory"],
      ["disk_bytes", "disk"],
      ["vms", "vms"],
      ["dedicated_vcpus", "dedicated"],
    ] as const) {
      if (parsed[key] === undefined) {
        found[field] =
          field === "memory" || field === "disk" ? "folders.badLimit" : "folders.badCount";
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
          inputMode={id === "memory" || id === "disk" ? "decimal" : "numeric"}
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
      {field("dedicated", t("folders.dim.dedicated_vcpus"))}
    </FormDialog>
  );
}
