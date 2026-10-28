import { Maximize2, Plus, Trash2, Upload } from "lucide-react";
import { type RefObject, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import type { Vm, Volume } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes } from "@/lib/format";
import { UploadDialog } from "@/storage/UploadDialog";
import { volumeNameError } from "@/storage/volumes";
import { trackTask } from "@/tasks/tasks";
import { gibToBytes } from "@/vms/form";

const MIB = 1024 ** 2;

export function VolumesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const volumes = useList("volume");
  const vms = useList("vm");
  const nodes = useList("node");
  const [creating, setCreating] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [deleting, setDeleting] = useState<Volume | null>(null);
  // 볼륨 id. 대화상자는 살아 있는 목록에서 찾아 지금 크기를 최신으로 보인다.
  const [resizing, setResizing] = useState<string | null>(null);
  const resizingVolume = volumes.data?.find((v) => v.id === resizing);
  // 트리거 없이 연 대화상자는 닫힐 때 포커스를 돌려줄 곳을 모르므로 연 버튼을 기억한다.
  const opener = useRef<HTMLElement | null>(null);
  const write = can(me.role, "write", "volume");
  // 서버는 VM spec의 디스크로 사용 중인지 판단한다(`attached_to`는 마지막으로 실행한 VM이다).
  const userByVolume = useMemo(() => {
    const users = new Map<string, Vm>();
    for (const vm of vms.data ?? []) {
      for (const disk of vm.spec.disks ?? []) {
        // 목록에서 먼저 나온 VM을 보인다.
        if (!users.has(disk.volume)) {
          users.set(disk.volume, vm);
        }
      }
    }
    return users;
  }, [vms.data]);
  const nodeById = useMemo(() => new Map(nodes.data?.map((n) => [n.id, n])), [nodes.data]);
  const reasonBase = useId();

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="volumes-title" className="text-2xl font-semibold">
          {t("nav.volumes")}
        </h1>
        {write && (
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => setUploading(true)}>
              <Upload />
              {t("uploads.open")}
            </Button>
            <Button onClick={() => setCreating(true)}>
              <Plus />
              {t("common.create")}
            </Button>
          </div>
        )}
      </div>
      <Table aria-labelledby="volumes-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("volumes.pool")}</TableHead>
            <TableHead>{t("volumes.size")}</TableHead>
            <TableHead>{t("volumes.allocated")}</TableHead>
            <TableHead>{t("volumes.format")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("common.node")}</TableHead>
            <TableHead>{t("volumes.vm")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(volumes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 9 : 8} query={volumes} empty={t("volumes.empty")} />
          )}
          {(volumes.data ?? []).map((volume) => {
            const vm = userByVolume.get(volume.name);
            const ready = volume.status.phase === "ready";
            const node = volume.status.node ? nodeById.get(volume.status.node) : undefined;
            // 만드는 중인 볼륨은 서버가 크기를 바꾸지 않는다(volume_not_ready).
            const resizeReason = ready ? null : "volumes.reason.notReady";
            // 서버는 VM이 쓰거나 아직 만드는 볼륨을 지우지 않는다. VM 목록을 다 읽어야 안다.
            const deleteReason = !ready
              ? "volumes.reason.notReady"
              : !vms.isSuccess
                ? "volumes.reason.vmsUnknown"
                : vm
                  ? "volumes.reason.inUse"
                  : null;
            const reasons = `${reasonBase}-${volume.id}`;
            return (
              <TableRow key={volume.id}>
                <TableCell className="font-medium">{volume.name}</TableCell>
                <TableCell>{volume.spec.pool}</TableCell>
                {/* 가져오는 볼륨은 끝나기 전까지 크기를 모른다. */}
                <TableCell>
                  {volume.spec.size_bytes > 0 ? formatBytes(volume.spec.size_bytes) : "—"}
                </TableCell>
                <TableCell>
                  {volume.status.allocated_bytes !== null
                    ? formatBytes(volume.status.allocated_bytes)
                    : "—"}
                </TableCell>
                <TableCell>{volume.spec.format}</TableCell>
                <TableCell>
                  <Badge variant={ready ? "default" : "secondary"}>
                    {t(`volumes.phase.${volume.status.phase}`)}
                  </Badge>
                </TableCell>
                <TableCell>{node?.name ?? volume.status.node?.slice(-8) ?? "—"}</TableCell>
                <TableCell>
                  {vm ? (
                    <Link
                      to={`/vms/${encodeURIComponent(vm.name)}`}
                      className="underline-offset-4 hover:underline"
                    >
                      {vm.name}
                    </Link>
                  ) : (
                    "—"
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <ReasonButton
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("volumes.resizeNamed", { name: volume.name })}
                      reason={resizeReason}
                      reasons={reasons}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setResizing(volume.id);
                      }}
                    >
                      <Maximize2 />
                    </ReasonButton>
                    <ReasonButton
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: volume.name })}
                      reason={deleteReason}
                      reasons={reasons}
                      onClick={() => setDeleting(volume)}
                    >
                      <Trash2 />
                    </ReasonButton>
                    <ReasonList base={reasons} reasons={[resizeReason, deleteReason]} />
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreateVolumeDialog onClose={() => setCreating(false)} />}
      {uploading && <UploadDialog onClose={() => setUploading(false)} />}
      {resizingVolume && (
        <ResizeVolumeDialog
          volume={resizingVolume}
          returnFocus={opener}
          onClose={() => setResizing(null)}
        />
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("volumes.deleteTitle", { name: deleting?.name })}
        description={t("volumes.deleteDescription")}
        confirmLabel={t("common.delete")}
        typeToConfirm={deleting?.name}
        onConfirm={async () => {
          const volume = deleting!;
          const task = await call(
            api.DELETE("/volumes/{key}", { params: { path: { key: volume.id } } }),
          );
          trackTask(task, t("tasks.op.volume-delete", { target: volume.name }));
        }}
      />
    </div>
  );
}

function CreateVolumeDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const [name, setName] = useState("");
  const [pool, setPool] = useState("");
  const [sizeGib, setSizeGib] = useState("16");
  const [format, setFormat] = useState<components["schemas"]["ImageFormat"]>("qcow2");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    const nameError = volumeNameError(name, volumes.data?.map((v) => v.name) ?? []);
    if (nameError) {
      found.name = nameError;
    }
    if (!pool) {
      found.pool = "vmForm.errors.pool";
    }
    const size = gibToBytes(sizeGib);
    if (size === undefined) {
      found.size = "vmForm.errors.size";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/volumes", {
        body: { name, spec: { pool, size_bytes: size!, format } },
      }),
    );
    trackTask(task, t("tasks.op.volume-create", { target: name }));
  }

  return (
    <FormDialog
      title={t("volumes.createTitle")}
      description={t("volumes.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="volume-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="volume-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      {!pools.isSuccess && <ListPending error={pools.error} />}
      <Field id="volume-pool" label={t("volumes.pool")} error={errors.pool}>
        {(a11y) => (
          <NativeSelect
            id="volume-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">—</NativeSelectOption>
            {(pools.data ?? []).map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <div className="grid grid-cols-2 gap-4">
        <Field id="volume-size" label={t("vmForm.sizeGib")} error={errors.size}>
          {(a11y) => (
            <Input
              id="volume-size"
              inputMode="decimal"
              value={sizeGib}
              onChange={(e) => setSizeGib(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
        <Field id="volume-format" label={t("volumes.format")}>
          <NativeSelect
            id="volume-format"
            className="w-full"
            value={format}
            onChange={(e) => setFormat(e.target.value as typeof format)}
          >
            <NativeSelectOption value="qcow2">qcow2</NativeSelectOption>
            <NativeSelectOption value="raw">raw</NativeSelectOption>
          </NativeSelect>
        </Field>
      </div>
    </FormDialog>
  );
}

/** 볼륨 크기 늘리기. 서버는 지금보다 크고 512 바이트 배수인 크기만 받는다. */
function ResizeVolumeDialog({
  volume,
  returnFocus,
  onClose,
}: {
  volume: Volume;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [sizeGib, setSizeGib] = useState("");
  const [error, setError] = useState<string>();

  async function submit() {
    // qcow2는 512 배수가 아닌 크기로 늘리지 못하므로 1 MiB 단위로 올린 값을 비교하고 보낸다.
    const bytes = gibToBytes(sizeGib);
    const size = bytes === undefined ? undefined : Math.ceil(bytes / MIB) * MIB;
    const found =
      size === undefined
        ? "vmForm.errors.size"
        : size <= volume.spec.size_bytes
          ? "volumes.errors.grow"
          : undefined;
    setError(found);
    if (found) {
      return false;
    }
    const task = await call(
      api.POST("/volumes/{key}/resize", {
        params: { path: { key: volume.id } },
        body: { size_bytes: size! },
      }),
    );
    trackTask(task, t("tasks.op.volume-resize", { target: volume.name }));
  }

  return (
    <FormDialog
      title={t("volumes.resizeTitle", { name: volume.name })}
      description={t("volumes.resizeDescription")}
      submitLabel={t("volumes.resize")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <p className="text-sm">
        {t("volumes.currentSize", { size: formatBytes(volume.spec.size_bytes) })}
      </p>
      <Field id="volume-new-size" label={t("volumes.newSize")} error={error}>
        {(a11y) => (
          <Input
            id="volume-new-size"
            inputMode="decimal"
            value={sizeGib}
            onChange={(e) => setSizeGib(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}
