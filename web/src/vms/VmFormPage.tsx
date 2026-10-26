import { useQueryClient } from "@tanstack/react-query";
import { Plus, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams } from "react-router";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { errorMessage } from "@/i18n/errors";
import { trackTask, waitForTask } from "@/tasks/tasks";
import { isStopped } from "@/vms/actions";
import {
  defaultMachine,
  type DiskRow,
  emptyDisk,
  emptyForm,
  fromVm,
  gibToBytes,
  MAX_NICS,
  type NicRow,
  toSpec,
  validate,
  type VmFormValues,
} from "@/vms/form";
import { useVmByName } from "@/vms/VmDetailPage";

export function VmCreatePage() {
  const nodes = useList("node");
  if (!nodes.data) {
    return <ListPending error={nodes.error} />;
  }
  return <VmForm initial={emptyForm(defaultMachine(nodes.data))} />;
}

export function VmEditPage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const vms = useList("vm");
  const vm = useVmByName(name);
  if (vm === undefined) {
    return <ListPending error={vms.error} />;
  }
  if (vm === null) {
    return <h1 className="text-2xl font-semibold">{t("vms.notFound", { name })}</h1>;
  }
  return <VmForm key={vm.id} vm={vm} initial={fromVm(vm)} />;
}

/** VM 만들기(`vm` 없음)와 고치기 */
function VmForm({ vm, initial }: { vm?: Vm; initial: VmFormValues }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const pools = useList("storage-pool");
  const networks = useList("network");
  const volumes = useList("volume");
  const [values, setValues] = useState(initial);
  const [revision, setRevision] = useState(vm?.revision);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<{ message: string; conflict: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  // 화면을 떠나면 기다리던 볼륨 Task를 그만 기다리고 VM을 만들지 않는다.
  const abort = useRef<AbortController | null>(null);
  useEffect(() => () => abort.current?.abort(), []);
  const creating = vm === undefined;
  // 멈추지 않은 VM(서버의 `is_stopped`가 아닌)의 노드와 디스크는 서버가 바꾸지 않는다.
  const locked = !creating && !isStopped(vm);

  const set = <K extends keyof VmFormValues>(key: K, value: VmFormValues[K]) =>
    setValues((v) => ({ ...v, [key]: value }));
  const setDisk = (i: number, patch: Partial<DiskRow>) =>
    set(
      "disks",
      values.disks.map((d, j) => (j === i ? { ...d, ...patch } : d)),
    );
  const setNic = (i: number, patch: Partial<NicRow>) =>
    set(
      "nics",
      values.nics.map((n, j) => (j === i ? { ...n, ...patch } : n)),
    );

  // 이 VM에 붙어 있거나 아무 데도 붙지 않은 준비된 볼륨
  const freeVolumes = (volumes.data ?? []).filter(
    (v) => v.status.phase === "ready" && (!v.status.attached_to || v.status.attached_to === vm?.id),
  );

  // 행을 넣거나 빼면 행 번호로 단 오류가 다른 행을 가리키므로 지운다.
  const setRows = <K extends "disks" | "nics">(key: K, rows: VmFormValues[K]) => {
    set(key, rows);
    setErrors({});
  };

  /** 다른 사람이 바꾼 VM을 다시 읽는다. `vm`은 watch로 늘 최신이다(지워지면 이 폼은 사라진다). */
  function reload() {
    if (vm) {
      setValues(fromVm(vm));
      setRevision(vm.revision);
    }
    setFailure(null);
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const found = validate(
      values,
      creating,
      (volumes.data ?? []).map((v) => v.name),
    );
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    setBusy(true);
    setFailure(null);
    const controller = new AbortController();
    abort.current = controller;
    const { signal } = controller;
    // 이번에 만든 볼륨. VM 만들기가 실패하면 남았다고 알린다.
    const created: string[] = [];
    try {
      if (creating) {
        // 새 빈 볼륨을 먼저 만들고, 그 Task가 끝나면 VM을 만든다.
        for (const disk of values.disks.filter((d) => d.kind === "new")) {
          const task = await call(
            api.POST("/volumes", {
              body: {
                name: disk.volume,
                spec: {
                  pool: disk.pool,
                  size_bytes: gibToBytes(disk.sizeGib)!,
                  format: "qcow2",
                },
              },
            }),
          );
          trackTask(task, t("tasks.op.volume-create", { target: disk.volume }));
          await waitForTask(queryClient, task.id, signal);
          created.push(disk.volume);
          // 다시 보낼 때 같은 볼륨을 또 만들지 않게 있는 볼륨으로 바꾼다.
          setValues((v) => ({
            ...v,
            disks: v.disks.map((d) =>
              d.kind === "new" && d.volume === disk.volume ? { ...d, kind: "existing" } : d,
            ),
          }));
        }
        if (signal.aborted) {
          return;
        }
        await call(
          api.POST("/vms", {
            body: { name: values.name, spec: toSpec(values, values.start ? "running" : "stopped") },
          }),
        );
        if (!signal.aborted) {
          void navigate(`/vms/${encodeURIComponent(values.name)}`);
        }
      } else {
        await call(
          api.PUT("/vms/{key}", {
            params: {
              path: { key: vm.id },
              header: { "If-Match": revision },
            },
            body: { labels: vm.labels, spec: toSpec(values, vm.spec.power ?? "stopped") },
          }),
        );
        if (!signal.aborted) {
          void navigate(`/vms/${encodeURIComponent(vm.name)}`);
        }
      }
    } catch (err) {
      if (signal.aborted) {
        return;
      }
      const conflict = err instanceof ApiError && err.code === "revision_mismatch";
      const message = errorMessage(t, err);
      setFailure({
        message:
          created.length > 0
            ? t("vmForm.volumesCreated", { names: created.join(", "), error: message })
            : message,
        conflict,
      });
    } finally {
      if (!signal.aborted) {
        setBusy(false);
      }
    }
  }

  return (
    <form onSubmit={submit} className="flex max-w-3xl flex-col gap-4" noValidate>
      <h1 className="text-2xl font-semibold">
        {creating ? t("vmForm.createTitle") : t("vmForm.editTitle", { name: vm.name })}
      </h1>
      {locked && <p className="text-sm text-muted-foreground">{t("vmForm.runningNote")}</p>}

      {/* 보내는 동안에는 고치지 못하게 한다. */}
      <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
        <Card>
          <CardHeader>
            <CardTitle>{t("vmForm.general")}</CardTitle>
          </CardHeader>
          <CardContent className="grid grid-cols-1 gap-4 sm:grid-cols-2">
            {creating && (
              <Field id="vm-name" label={t("vms.name")} error={errors.name}>
                {(a11y) => (
                  <Input
                    id="vm-name"
                    {...a11y}
                    value={values.name}
                    onChange={(e) => set("name", e.target.value)}
                  />
                )}
              </Field>
            )}
            <Field id="vm-cores" label={t("vmForm.cores")} error={errors.cores}>
              {(a11y) => (
                <Input
                  id="vm-cores"
                  {...a11y}
                  type="number"
                  min={1}
                  max={65535}
                  value={values.cores}
                  onChange={(e) => set("cores", e.target.value)}
                />
              )}
            </Field>
            <Field id="vm-memory" label={t("vmForm.memory")} error={errors.memory}>
              {(a11y) => (
                <Input
                  id="vm-memory"
                  {...a11y}
                  type="number"
                  min={64}
                  step={256}
                  value={values.memory}
                  onChange={(e) => set("memory", e.target.value)}
                />
              )}
            </Field>
            <Field id="vm-cpu-model" label={t("vmForm.cpuModel")}>
              <NativeSelect
                id="vm-cpu-model"
                className="w-full"
                value={values.model}
                onChange={(e) => set("model", e.target.value as VmFormValues["model"])}
              >
                <NativeSelectOption value="host">host</NativeSelectOption>
                <NativeSelectOption value="max">max</NativeSelectOption>
              </NativeSelect>
            </Field>
            <Field id="vm-firmware" label={t("vms.firmware")}>
              <NativeSelect
                id="vm-firmware"
                className="w-full"
                value={values.firmware}
                onChange={(e) => set("firmware", e.target.value as VmFormValues["firmware"])}
              >
                <NativeSelectOption value="uefi">UEFI</NativeSelectOption>
                <NativeSelectOption value="bios">BIOS</NativeSelectOption>
              </NativeSelect>
            </Field>
            <Field id="vm-machine" label={t("vms.machine")}>
              <NativeSelect
                id="vm-machine"
                className="w-full"
                value={values.machine}
                onChange={(e) => set("machine", e.target.value as VmFormValues["machine"])}
              >
                <NativeSelectOption value="q35">q35 (x86_64)</NativeSelectOption>
                <NativeSelectOption value="virt">virt (aarch64)</NativeSelectOption>
              </NativeSelect>
            </Field>
            <Field id="vm-node" label={t("vms.node")}>
              <NativeSelect
                id="vm-node"
                className="w-full"
                disabled={locked}
                value={values.node}
                onChange={(e) => set("node", e.target.value)}
              >
                <NativeSelectOption value="">{t("vmForm.nodeAuto")}</NativeSelectOption>
                {nodes.data?.map((n) => (
                  <NativeSelectOption key={n.id} value={n.id}>
                    {n.name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            </Field>
            <Field id="vm-restart" label={t("vms.restart")}>
              <NativeSelect
                id="vm-restart"
                className="w-full"
                value={values.restart}
                onChange={(e) => set("restart", e.target.value as VmFormValues["restart"])}
              >
                <NativeSelectOption value="never">
                  {t("vms.restartPolicy.never")}
                </NativeSelectOption>
                <NativeSelectOption value="on-failure">
                  {t("vms.restartPolicy.on-failure")}
                </NativeSelectOption>
              </NativeSelect>
            </Field>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>{t("vms.disks")}</CardTitle>
          </CardHeader>
          <CardContent className="flex flex-col gap-3">
            {values.disks.map((disk, i) => (
              <fieldset
                key={i}
                disabled={locked}
                aria-label={t("vmForm.diskN", { n: i + 1 })}
                className="grid grid-cols-2 items-end gap-2 rounded-md border p-3 sm:grid-cols-5"
              >
                {disk.kind === "existing" ? (
                  <Field
                    id={`disk-${i}-volume`}
                    label={t("vms.volume")}
                    error={errors[`disks.${i}.volume`]}
                  >
                    {(a11y) => (
                      <NativeSelect
                        id={`disk-${i}-volume`}
                        {...a11y}
                        className="w-full"
                        value={disk.volume}
                        onChange={(e) => setDisk(i, { volume: e.target.value })}
                      >
                        <NativeSelectOption value="">—</NativeSelectOption>
                        {freeVolumes
                          .filter(
                            (v) =>
                              v.name === disk.volume ||
                              !values.disks.some((d) => d.volume === v.name),
                          )
                          .map((v) => (
                            <NativeSelectOption key={v.id} value={v.name}>
                              {v.name}
                            </NativeSelectOption>
                          ))}
                      </NativeSelect>
                    )}
                  </Field>
                ) : (
                  <>
                    <Field
                      id={`disk-${i}-volume`}
                      label={t("vmForm.newVolume")}
                      error={errors[`disks.${i}.volume`]}
                    >
                      {(a11y) => (
                        <Input
                          id={`disk-${i}-volume`}
                          {...a11y}
                          value={disk.volume}
                          onChange={(e) => setDisk(i, { volume: e.target.value })}
                        />
                      )}
                    </Field>
                    <Field
                      id={`disk-${i}-pool`}
                      label={t("vmForm.pool")}
                      error={errors[`disks.${i}.pool`]}
                    >
                      {(a11y) => (
                        <NativeSelect
                          id={`disk-${i}-pool`}
                          {...a11y}
                          className="w-full"
                          value={disk.pool}
                          onChange={(e) => setDisk(i, { pool: e.target.value })}
                        >
                          <NativeSelectOption value="">—</NativeSelectOption>
                          {pools.data?.map((p) => (
                            <NativeSelectOption key={p.id} value={p.name}>
                              {p.name}
                            </NativeSelectOption>
                          ))}
                        </NativeSelect>
                      )}
                    </Field>
                    <Field
                      id={`disk-${i}-size`}
                      label={t("vmForm.sizeGib")}
                      error={errors[`disks.${i}.size`]}
                    >
                      {(a11y) => (
                        <Input
                          id={`disk-${i}-size`}
                          {...a11y}
                          type="number"
                          min={1}
                          value={disk.sizeGib}
                          onChange={(e) => setDisk(i, { sizeGib: e.target.value })}
                        />
                      )}
                    </Field>
                  </>
                )}
                <Field id={`disk-${i}-bus`} label={t("vms.bus")}>
                  <NativeSelect
                    id={`disk-${i}-bus`}
                    className="w-full"
                    value={disk.bus}
                    onChange={(e) => setDisk(i, { bus: e.target.value as DiskRow["bus"] })}
                  >
                    <NativeSelectOption value="virtio-blk">virtio-blk</NativeSelectOption>
                    <NativeSelectOption value="sata">sata</NativeSelectOption>
                  </NativeSelect>
                </Field>
                <div className="flex items-end gap-2">
                  <Field
                    id={`disk-${i}-boot`}
                    label={t("vms.bootOrder")}
                    error={errors[`disks.${i}.bootOrder`]}
                  >
                    {(a11y) => (
                      <Input
                        id={`disk-${i}-boot`}
                        {...a11y}
                        type="number"
                        min={1}
                        max={255}
                        className="w-20"
                        value={disk.bootOrder}
                        onChange={(e) => setDisk(i, { bootOrder: e.target.value })}
                      />
                    )}
                  </Field>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    aria-label={t("vmForm.removeDisk", { n: i + 1 })}
                    onClick={() =>
                      setRows(
                        "disks",
                        values.disks.filter((_, j) => j !== i),
                      )
                    }
                  >
                    <X />
                  </Button>
                </div>
              </fieldset>
            ))}
            {!locked && (
              <div className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    setRows("disks", [
                      ...values.disks,
                      { ...emptyDisk("", values.disks.length), kind: "existing" },
                    ])
                  }
                >
                  <Plus />
                  {t("vmForm.addExisting")}
                </Button>
                {creating && (
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() =>
                      setRows("disks", [
                        ...values.disks,
                        emptyDisk(values.name, values.disks.length),
                      ])
                    }
                  >
                    <Plus />
                    {t("vmForm.addNew")}
                  </Button>
                )}
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>{t("vms.nics")}</CardTitle>
          </CardHeader>
          <CardContent className="flex flex-col gap-3">
            {values.nics.map((nic, i) => (
              <fieldset
                key={i}
                aria-label={t("vmForm.nicN", { n: i + 1 })}
                className="grid grid-cols-2 items-end gap-2 rounded-md border p-3 sm:grid-cols-4"
              >
                <Field id={`nic-${i}-network`} label={t("vms.network")}>
                  <NativeSelect
                    id={`nic-${i}-network`}
                    className="w-full"
                    value={nic.network}
                    onChange={(e) => setNic(i, { network: e.target.value })}
                  >
                    <NativeSelectOption value="user">{t("vmForm.userNetwork")}</NativeSelectOption>
                    {networks.data?.map((n) => (
                      <NativeSelectOption key={n.id} value={n.name}>
                        {n.name}
                      </NativeSelectOption>
                    ))}
                  </NativeSelect>
                </Field>
                <Field id={`nic-${i}-model`} label={t("vms.model")}>
                  <NativeSelect
                    id={`nic-${i}-model`}
                    className="w-full"
                    value={nic.model}
                    onChange={(e) => setNic(i, { model: e.target.value as NicRow["model"] })}
                  >
                    <NativeSelectOption value="virtio">virtio</NativeSelectOption>
                    <NativeSelectOption value="e1000e">e1000e</NativeSelectOption>
                  </NativeSelect>
                </Field>
                <Field id={`nic-${i}-mac`} label={t("vms.mac")} error={errors[`nics.${i}.mac`]}>
                  {(a11y) => (
                    <Input
                      id={`nic-${i}-mac`}
                      {...a11y}
                      placeholder={t("vmForm.macAuto")}
                      value={nic.mac}
                      onChange={(e) => setNic(i, { mac: e.target.value })}
                    />
                  )}
                </Field>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  aria-label={t("vmForm.removeNic", { n: i + 1 })}
                  onClick={() =>
                    setRows(
                      "nics",
                      values.nics.filter((_, j) => j !== i),
                    )
                  }
                >
                  <X />
                </Button>
              </fieldset>
            ))}
            {values.nics.length < MAX_NICS && (
              <div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    setRows("nics", [...values.nics, { network: "user", model: "virtio", mac: "" }])
                  }
                >
                  <Plus />
                  {t("vmForm.addNic")}
                </Button>
              </div>
            )}
          </CardContent>
        </Card>

        {creating && (
          <div className="flex items-center gap-2">
            <Checkbox
              id="vm-start"
              checked={values.start}
              onCheckedChange={(v) => set("start", v === true)}
            />
            <Label htmlFor="vm-start">{t("vmForm.startAfter")}</Label>
          </div>
        )}
      </fieldset>
      {failure && (
        <div role="alert" className="flex items-center gap-3 text-sm text-destructive">
          <span>{failure.message}</span>
          {failure.conflict && (
            <Button type="button" variant="outline" size="sm" onClick={reload}>
              {t("vmForm.reload")}
            </Button>
          )}
        </div>
      )}
      <div className="flex gap-2">
        <Button type="submit" disabled={busy}>
          {creating ? t("vms.create") : t("vmForm.save")}
        </Button>
        <Button variant="ghost" asChild>
          <Link to={creating ? "/vms" : `/vms/${encodeURIComponent(vm.name)}`}>
            {t("confirm.cancel")}
          </Link>
        </Button>
      </div>
    </form>
  );
}
