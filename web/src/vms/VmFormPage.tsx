import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { useTranslation } from "react-i18next";
import { Link, Navigate, useLocation, useNavigate, useParams, useSearchParams } from "react-router";

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { usePermissions, usePermissionsReady } from "@/auth/usePermissions";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { folderTree } from "@/folders/folders";
import { errorMessage, HOTPLUG_ERROR_CODES, serverErrorMessage } from "@/i18n/errors";
import { choosableIsos } from "@/isos/isos";
import { sdnNetworkNamed } from "@/sdn/ips";
import { poolFormats } from "@/storage/pools";
import { isStopped } from "@/vms/actions";
import {
  defaultMachine,
  emptyForm,
  fromVm,
  haActive,
  type NicRow,
  tcMissing,
  validate,
  type VmFormValues,
} from "@/vms/form";
import { BootOrderSection } from "@/vms/form/BootOrderSection";
import { CdromsSection } from "@/vms/form/CdromsSection";
import { CloudInitSection } from "@/vms/form/CloudInitSection";
import { DisksSection } from "@/vms/form/DisksSection";
import { GuestAgentSection, SecuritySection } from "@/vms/form/FirmwareSection";
import { GeneralSection } from "@/vms/form/GeneralSection";
import { haEnabled } from "@/vms/ha";
import { ownUse } from "@/numa/numa";
import { deviceMode } from "@/vms/hotplug";
import { NicsSection } from "@/vms/form/NicsSection";
import { ReplicationSection } from "@/vms/form/ReplicationSection";
import { ScheduleSection } from "@/vms/form/ScheduleSection";
import {
  FormSteps,
  PresetPicker,
  ReviewSection,
  StepSection,
  TemplatePicker,
} from "@/vms/form/FormSteps";
import { applyPreset, errorCounts, FORM_STEPS, type Preset, stepId } from "@/vms/form/steps";
import { createVm, createVolume, isConflict, trackHotplug, updateVm } from "@/vms/form/submit";
import { firstLocalDisk } from "@/vms/replication";
import { formSnapshotSupport } from "@/vms/snapshots";
import { useVmByName } from "@/vms/VmDetailPage";
import { clearDraft, defaultFolder, putDraft, readDraft, readMode } from "@/vmBuilder/draft";
import { ViewSwitch } from "@/vmBuilder/ViewSwitch";

export function VmCreatePage() {
  const perms = usePermissions();
  const ready = usePermissionsReady();
  const nodes = useList("node");
  const folders = useList("folder");
  const [params] = useSearchParams();
  const { user } = usePrincipal();
  const { search } = useLocation();
  // 노드 목록을 읽을 수 없는 폴더 사용자는 머신을 서버에 맡긴다(계획 62).
  const readsNodes = perms.reads("node");
  // 빌더(계획 71)에서 넘어온 값, 아니면 마지막 보기가 빌더면 그리로(쿼리를 지킴)
  const draft = readDraft();
  if (!draft && readMode(user) === "builder") {
    return <Navigate to={`/vms/-new/builder${search}`} replace />;
  }
  // 기본 폴더는 내 권한을 받은 뒤에 정한다(전역 역할만 본 값으로는 폴더 사용자가 루트를 받는다, 폼이 첫 값을 굳힌다).
  if (!ready || (readsNodes && !nodes.data) || !folders.data) {
    return <ListPending error={nodes.error ?? folders.error} />;
  }
  const folder = defaultFolder(perms, folderTree(folders.data, "vm"), params.get("folder"));
  return (
    <VmForm
      initial={
        draft?.values ?? emptyForm(readsNodes ? defaultMachine(nodes.data ?? []) : "", folder)
      }
      initialPreset={draft?.preset}
    />
  );
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
    return <h1 className="text-xl font-semibold">{t("vms.notFound", { name })}</h1>;
  }
  return <VmForm key={vm.id} vm={vm} initial={fromVm(vm)} />;
}

/**
 * 섹션 안의 첫 틀린 입력. `aria-invalid`를 단 입력이 먼저이고, 없으면 `Field`의 오류 문장(`{id}-error`)이
 * 가리키는 입력이다.
 */
function firstInvalid(section: HTMLElement): HTMLElement | null {
  const invalid = section.querySelector<HTMLElement>("[aria-invalid=true]");
  if (invalid) {
    return invalid;
  }
  const error = section.querySelector<HTMLElement>("[id$='-error']");
  return error ? document.getElementById(error.id.slice(0, -"-error".length)) : null;
}

/** VM 만들기(`vm` 없음)와 고치기 */
function VmForm({
  vm,
  initial,
  initialPreset = "blank",
}: {
  vm?: Vm;
  initial: VmFormValues;
  initialPreset?: Preset;
}) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const pools = useList("storage-pool");
  const networks = useList("network");
  const securityGroups = useList("security-group");
  const volumes = useList("volume");
  const groups = useList("ha-group");
  const [values, setValues] = useState(initial);
  // NIC마다 SDN 네트워크인지(계획 65 — 주소 칸과 cloud-init 주소). 목록을 읽기 전에는 모른다(빈 목록 → 단순으로 본다).
  const sdnNics = values.nics.map((n) => sdnNetworkNamed(networks.data, n.network));
  const noTc = tcMissing(nodes.data, values.node || (vm?.status.node ?? ""));
  const [revision, setRevision] = useState(vm?.revision);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<{ message: string; conflict: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  // 사용자가 머신을 직접 고르기 전에는 노드를 바꾸면 그 노드에 맞게 기본값을 다시 정한다(만들 때만).
  const [machineChosen, setMachineChosen] = useState(false);
  // 고른 프리셋(만들 때만). "Windows 11"이면 그 뒤에 더하는 디스크는 SATA, NIC는 e1000e로 시작한다.
  const [preset, setPreset] = useState<Preset>(initialPreset);
  // 빌더가 넘긴 초안은 한 번만 쓴다(계획 71 §8.6).
  useEffect(() => clearDraft(), []);
  // 폼 값에 마지막으로 얹은 프리셋(템플릿은 값을 바꾸지 않으므로 빠진다). Windows에서 떠날 때 장치를 돌린다.
  const [applied, setApplied] = useState<Preset>("blank");
  const windows = preset === "windows";
  // 화면을 떠나면 기다리던 볼륨 Task를 그만 기다리고 VM을 만들지 않는다.
  const abort = useRef<AbortController | null>(null);
  useEffect(() => () => abort.current?.abort(), []);
  const creating = vm === undefined;
  // 멈추지 않은 VM(서버의 `is_stopped`가 아닌)의 노드는 서버가 바꾸지 않는다.
  const locked = !creating && !isStopped(vm);
  // 실행 중인 VM의 디스크·NIC: 핫플러그 배치로 켰으면 끝에 붙이기·실행 중 붙인 것 떼기, 아니면 잠금
  const mode = deviceMode(vm);
  const layout = vm?.status.hardware?.layout;

  const change = (patch: Partial<VmFormValues>) => setValues((v) => ({ ...v, ...patch }));
  /** 미리 채우기를 썼으면 새로 더한 행(뒤에 붙은 행)을 Windows 장치로 바꾼다. */
  const presetRows = <T,>(rows: T[], old: T[], patch: Partial<T>) =>
    windows ? rows.map((r, i) => (i >= old.length ? { ...r, ...patch } : r)) : rows;
  // 행을 넣거나 빼면 행 번호로 단 오류가 다른 행을 가리키므로 지운다.
  const changeRows = (
    patch:
      | Pick<VmFormValues, "disks">
      | Pick<VmFormValues, "cdroms">
      | Pick<VmFormValues, "nics" | "cloudInit">
      | Pick<VmFormValues, "replication">,
  ) => {
    change(patch);
    setErrors({});
  };
  // NIC를 빼면 cloud-init의 그 NIC 주소도 뺀다(주소는 NIC 순번을 따른다).
  const changeNics = (nics: NicRow[]) => {
    const removed = values.nics.findIndex((n, i) => nics[i] !== n);
    const networks =
      nics.length < values.nics.length && removed !== -1
        ? values.cloudInit.networks.filter((_, i) => i !== removed)
        : values.cloudInit.networks;
    changeRows({
      nics: presetRows(nics, values.nics, { model: "e1000e" }),
      cloudInit: { ...values.cloudInit, networks },
    });
  };

  // 이 VM에 붙어 있거나 아무 데도 붙지 않은 준비된 디스크 볼륨(ISO는 CD-ROM에만 넣는다)
  const freeVolumes = (volumes.data ?? []).filter(
    (v) =>
      v.spec.content !== "iso" &&
      v.status.phase === "ready" &&
      (!v.status.attached_to || v.status.attached_to === vm?.id),
  );
  // CD-ROM에 넣을 ISO. HA·복제 VM은 공유 풀의 ISO만(서버의 400)
  const isos = choosableIsos(
    volumes.data ?? [],
    pools.data,
    haActive(values) || values.replication.length > 0,
  );

  // 디스크의 스냅샷 가능 여부(예약 스냅샷). 볼륨·풀 목록을 다 읽어야 안다.
  const support = formSnapshotSupport(
    values,
    volumes.isSuccess ? volumes.data : undefined,
    pools.isSuccess ? pools.data : undefined,
  );

  // 활성 HA인데 노드 로컬 풀의 디스크가 있고 복제가 없으면 그 디스크(그 노드가 죽으면 다른 노드에서 켤 수 없다).
  // 볼륨·풀 목록을 다 읽어야 안다.
  const noReplica =
    haActive(values) && values.replication.length === 0 && volumes.isSuccess && pools.isSuccess
      ? firstLocalDisk(
          values.disks.map((d) => ({
            volume: d.volume,
            ...(d.kind === "new" && { pool: d.pool }),
          })),
          volumes.data,
          pools.data,
        )
      : null;

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
      sdnNics,
    );
    // 오류를 바로 그려야 그 입력에 포커스를 줄 수 있다.
    flushSync(() => setErrors(found));
    if (Object.keys(found).length > 0) {
      // 오류가 있는 첫 단계로 옮기고 그 안의 첫 틀린 입력에 포커스를 준다(긴 폼·좁은 화면·화면 읽기 프로그램).
      const first = FORM_STEPS.find((step) => errorCounts(found)[step]);
      const section = document.getElementById(stepId(first ?? "general"));
      section?.scrollIntoView?.({ block: "start" });
      if (section) {
        firstInvalid(section)?.focus({ preventScroll: true });
      }
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
        await createVm(values, sdnNics);
        if (!signal.aborted) {
          void navigate(`/vms/${encodeURIComponent(values.name)}`);
        }
      } else {
        const task = await updateVm(vm, revision, values, sdnNics);
        // 실행 중인 VM의 하드웨어를 바꾸는 Task를 알림으로 지켜본다.
        if (task) {
          void trackHotplug(task, t("tasks.op.vm-hotplug", { target: vm.name }));
        }
        if (!signal.aborted) {
          void navigate(`/vms/${encodeURIComponent(vm.name)}`);
        }
      }
    } catch (err) {
      if (signal.aborted) {
        return;
      }
      // 하드웨어를 바꾸지 못한 까닭(어느 장치를 왜)은 서버의 설명이 말한다.
      const message = creating
        ? errorMessage(t, err)
        : serverErrorMessage(t, err, HOTPLUG_ERROR_CODES);
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

  const counts = errorCounts(errors);
  const fromTemplate = creating && preset === "template";
  return (
    <form
      onSubmit={submit}
      className="grid max-w-5xl grid-cols-1 gap-x-8 gap-y-4 lg:grid-cols-[12rem_minmax(0,1fr)]"
      noValidate
    >
      <div className="flex flex-wrap items-center gap-3 lg:col-span-2">
        <h1 className="text-xl font-semibold">
          {creating ? t("vmForm.createTitle") : t("vmForm.editTitle", { name: vm.name })}
        </h1>
        {/* 보기 전환 "폼 | 빌더"(계획 71): 지금 값을 빌더로 넘긴다. */}
        {creating && <ViewSwitch mode="form" onLeave={() => putDraft({ values })} />}
      </div>
      {/* 긴 폼은 넓은 화면에서 쓰기 좋다(설계 §5.9 당직 보기). */}
      <p className="text-sm text-muted-foreground md:hidden lg:col-span-2">
        {t("common.desktopOnly")}
      </p>
      {!fromTemplate ? (
        <FormSteps steps={FORM_STEPS} errors={counts} />
      ) : (
        <div aria-hidden className="hidden lg:block" />
      )}
      <div className="flex min-w-0 flex-col gap-4">
        {mode === "locked" && (
          <p className="text-sm text-muted-foreground">{t("vmForm.runningNote")}</p>
        )}
        {(mode === "live" || mode === "restart") && (
          <p className="text-sm text-muted-foreground">{t("vmForm.runningNoteLive")}</p>
        )}
        {mode === "restart" && (
          <p className="text-sm text-muted-foreground">{t("vmForm.restartForHotplug")}</p>
        )}

        {creating && (
          <div className="flex flex-col gap-1.5">
            <PresetPicker
              value={preset}
              disabled={busy}
              onChange={(next) => {
                setPreset(next);
                // 템플릿은 폼 값을 바꾸지 않는다(돌아와도 입력이 남는다).
                if (next !== "template" && next !== applied) {
                  setValues((v) => applyPreset(next, applied, v));
                  setApplied(next);
                  setErrors({});
                  // 프리셋이 머신을 정했으면 노드를 바꿔도 다시 고르지 않는다.
                  if (next === "windows") {
                    setMachineChosen(true);
                  }
                }
              }}
            />
            {windows && (
              <p role="status" className="text-sm text-muted-foreground">
                {t("vmForm.windows11Hint")}
              </p>
            )}
          </div>
        )}

        {fromTemplate ? (
          <TemplatePicker />
        ) : (
          <>
            {/* 보내는 동안에는 고치지 못하게 한다. */}
            <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
              <StepSection step="general">
                <GeneralSection
                  values={values}
                  errors={errors}
                  onChange={change}
                  nodes={nodes.data}
                  groups={groups.data?.map((g) => g.name).sort()}
                  creating={creating}
                  locked={locked}
                  haLocked={locked && !(vm && haEnabled(vm.spec))}
                  live={mode === "live" || mode === "restart"}
                  autoMachine={creating && !machineChosen}
                  onMachineChosen={() => setMachineChosen(true)}
                  noReplica={noReplica}
                  vmId={vm?.id}
                  own={
                    vm ? ownUse(vm, (id) => nodes.data?.find((n) => n.id === id)?.name) : undefined
                  }
                />
              </StepSection>
              <StepSection step="disks">
                <DisksSection
                  values={values}
                  errors={errors}
                  onChange={change}
                  onRowsChange={(disks) =>
                    changeRows({ disks: presetRows(disks, values.disks, { bus: "sata" }) })
                  }
                  volumes={freeVolumes}
                  pools={pools.data}
                  creating={creating}
                  mode={mode}
                  saved={(vm?.spec.disks ?? []).map((d) => d.volume)}
                  boot={layout?.boot_disks ?? 0}
                />
              </StepSection>
              <StepSection step="nics">
                <NicsSection
                  values={values}
                  errors={errors}
                  onChange={change}
                  onRowsChange={changeNics}
                  networks={networks.data}
                  securityGroups={securityGroups.data}
                  mode={mode}
                  saved={vm?.spec.nics ?? []}
                  boot={layout?.boot_nics ?? 0}
                  template={vm?.spec.template === true}
                  noTc={noTc}
                />
              </StepSection>
              <StepSection step="cdroms">
                <CdromsSection
                  values={values}
                  errors={errors}
                  onChange={change}
                  onRowsChange={(cdroms) => changeRows({ cdroms })}
                  isos={isos}
                  mode={mode}
                />
              </StepSection>
              <StepSection step="boot">
                <BootOrderSection
                  values={values}
                  errors={errors}
                  onChange={change}
                  locked={mode !== "free"}
                />
              </StepSection>
              <StepSection step="security">
                <SecuritySection
                  values={values}
                  errors={errors}
                  onChange={change}
                  nodes={nodes.data}
                  stopped={creating || isStopped(vm)}
                  firmwareNode={vm?.status.firmware_node ?? null}
                  live={mode !== "free"}
                />
              </StepSection>
              <StepSection step="agent">
                <GuestAgentSection values={values} onChange={change} live={mode !== "free"} />
              </StepSection>
              <StepSection step="cloudInit">
                <CloudInitSection
                  values={values}
                  errors={errors}
                  onChange={change}
                  running={mode !== "free"}
                  sdn={sdnNics}
                />
              </StepSection>
              <StepSection step="schedule">
                <ScheduleSection
                  values={values}
                  errors={errors}
                  onChange={change}
                  support={support}
                />
              </StepSection>
              <StepSection step="replication">
                <ReplicationSection
                  values={values}
                  errors={errors}
                  onChange={change}
                  onRowsChange={(replication) => changeRows({ replication })}
                  nodes={nodes.data}
                  pools={pools.data}
                  ownNode={values.node || (vm?.status.node ?? "")}
                />
              </StepSection>

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
              <StepSection step="review">
                <ReviewSection values={values} nodes={nodes.data} creating={creating} />
              </StepSection>
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
          </>
        )}
        <div className="flex gap-2">
          {!fromTemplate && (
            <Button type="submit" disabled={busy}>
              {creating ? t("common.create") : t("common.save")}
            </Button>
          )}
          <Button variant="ghost" asChild>
            <Link to={creating ? "/vms" : `/vms/${encodeURIComponent(vm.name)}`}>
              {t("common.cancel")}
            </Link>
          </Button>
        </div>
      </div>
    </form>
  );
}
