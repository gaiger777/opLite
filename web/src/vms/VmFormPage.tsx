import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams } from "react-router";

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { errorMessage } from "@/i18n/errors";
import { poolFormats } from "@/storage/pools";
import { isStopped } from "@/vms/actions";
import { defaultMachine, emptyForm, fromVm, validate, type VmFormValues } from "@/vms/form";
import { DisksSection } from "@/vms/form/DisksSection";
import { GeneralSection } from "@/vms/form/GeneralSection";
import { NicsSection } from "@/vms/form/NicsSection";
import { ScheduleSection } from "@/vms/form/ScheduleSection";
import { createVm, createVolume, isConflict, updateVm } from "@/vms/form/submit";
import { formSnapshotSupport } from "@/vms/snapshots";
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
  // 사용자가 머신을 직접 고르기 전에는 노드를 바꾸면 그 노드에 맞게 기본값을 다시 정한다(만들 때만).
  const [machineChosen, setMachineChosen] = useState(false);
  // 화면을 떠나면 기다리던 볼륨 Task를 그만 기다리고 VM을 만들지 않는다.
  const abort = useRef<AbortController | null>(null);
  useEffect(() => () => abort.current?.abort(), []);
  const creating = vm === undefined;
  // 멈추지 않은 VM(서버의 `is_stopped`가 아닌)의 노드와 디스크는 서버가 바꾸지 않는다.
  const locked = !creating && !isStopped(vm);

  const change = (patch: Partial<VmFormValues>) => setValues((v) => ({ ...v, ...patch }));
  // 행을 넣거나 빼면 행 번호로 단 오류가 다른 행을 가리키므로 지운다.
  const changeRows = (patch: Pick<VmFormValues, "disks"> | Pick<VmFormValues, "nics">) => {
    change(patch);
    setErrors({});
  };

  // 이 VM에 붙어 있거나 아무 데도 붙지 않은 준비된 볼륨
  const freeVolumes = (volumes.data ?? []).filter(
    (v) => v.status.phase === "ready" && (!v.status.attached_to || v.status.attached_to === vm?.id),
  );

  // 디스크의 스냅샷 가능 여부(예약 스냅샷). 볼륨·풀 목록을 다 읽어야 안다.
  const support = formSnapshotSupport(
    values,
    volumes.isSuccess ? volumes.data : undefined,
    pools.isSuccess ? pools.data : undefined,
  );

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
          await createVolume(
            queryClient,
            disk,
            // 새 볼륨은 풀 종류의 기본 형식이다(LVM-thin 풀은 raw).
            poolFormats(pools.data?.find((p) => p.name === disk.pool))[0],
            t("tasks.op.volume-create", { target: disk.volume }),
            signal,
          );
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
        await createVm(values);
        if (!signal.aborted) {
          void navigate(`/vms/${encodeURIComponent(values.name)}`);
        }
      } else {
        await updateVm(vm, revision, values);
        if (!signal.aborted) {
          void navigate(`/vms/${encodeURIComponent(vm.name)}`);
        }
      }
    } catch (err) {
      if (signal.aborted) {
        return;
      }
      const message = errorMessage(t, err);
      setFailure({
        message:
          created.length > 0
            ? t("vmForm.volumesCreated", { names: created.join(", "), error: message })
            : message,
        conflict: isConflict(err),
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
        <GeneralSection
          values={values}
          errors={errors}
          onChange={change}
          nodes={nodes.data}
          creating={creating}
          locked={locked}
          autoMachine={creating && !machineChosen}
          onMachineChosen={() => setMachineChosen(true)}
        />
        <DisksSection
          values={values}
          errors={errors}
          onChange={change}
          onRowsChange={(disks) => changeRows({ disks })}
          volumes={freeVolumes}
          pools={pools.data}
          creating={creating}
          locked={locked}
        />
        <NicsSection
          values={values}
          errors={errors}
          onChange={change}
          onRowsChange={(nics) => changeRows({ nics })}
          networks={networks.data}
        />
        <ScheduleSection values={values} errors={errors} onChange={change} support={support} />

        {creating && (
          <div className="flex items-center gap-2">
            <Checkbox
              id="vm-start"
              checked={values.start}
              onCheckedChange={(v) => change({ start: v === true })}
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
          {creating ? t("common.create") : t("common.save")}
        </Button>
        <Button variant="ghost" asChild>
          <Link to={creating ? "/vms" : `/vms/${encodeURIComponent(vm.name)}`}>
            {t("common.cancel")}
          </Link>
        </Button>
      </div>
    </form>
  );
}
