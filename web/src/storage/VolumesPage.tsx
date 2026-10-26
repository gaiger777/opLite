import { Plus, Trash2, Upload } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import type { Volume } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
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

export function VolumesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const volumes = useList("volume");
  const vms = useList("vm");
  const nodes = useList("node");
  const [creating, setCreating] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [deleting, setDeleting] = useState<Volume | null>(null);
  const write = can(me.role, "write", "volume");
  // 서버는 VM spec의 디스크로 사용 중인지 판단한다(`attached_to`는 마지막으로 실행한 VM이다).
  const userOf = (volume: Volume) =>
    vms.data?.find((vm) => vm.spec.disks?.some((d) => d.volume === volume.name));

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
              {t("volumes.create")}
            </Button>
          </div>
        )}
      </div>
      <Table aria-labelledby="volumes-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("volumes.name")}</TableHead>
            <TableHead>{t("volumes.pool")}</TableHead>
            <TableHead>{t("volumes.size")}</TableHead>
            <TableHead>{t("volumes.allocated")}</TableHead>
            <TableHead>{t("volumes.format")}</TableHead>
            <TableHead>{t("volumes.state")}</TableHead>
            <TableHead>{t("volumes.node")}</TableHead>
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
            const vm = userOf(volume);
            const ready = volume.status.phase === "ready";
            const node = nodes.data?.find((n) => n.id === volume.status.node);
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
                    <>
                      <Link
                        to={`/vms/${encodeURIComponent(vm.name)}`}
                        className="underline-offset-4 hover:underline"
                      >
                        {vm.name}
                      </Link>
                      {write && (
                        <div className="text-xs text-muted-foreground">{t("volumes.inUse")}</div>
                      )}
                    </>
                  ) : (
                    "—"
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("volumes.deleteNamed", { name: volume.name })}
                      // 서버는 VM이 쓰거나 아직 만드는 볼륨을 지우지 않는다. VM 목록을 다 읽어야 안다.
                      disabled={!vms.isSuccess || vm !== undefined || !ready}
                      onClick={() => setDeleting(volume)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreateVolumeDialog onClose={() => setCreating(false)} />}
      {uploading && <UploadDialog onClose={() => setUploading(false)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("volumes.deleteTitle", { name: deleting?.name })}
        description={t("volumes.deleteDescription")}
        confirmLabel={t("volumes.delete")}
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
      submitLabel={t("volumes.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="volume-name" label={t("volumes.name")} error={errors.name}>
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
