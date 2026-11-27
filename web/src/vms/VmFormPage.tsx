import { useEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams } from "react-router";

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { HOTPLUG_ERROR_CODES, serverErrorMessage } from "@/i18n/errors";
import { choosableIsos } from "@/isos/isos";
import { sdnNetworkNamed } from "@/sdn/ips";
import { isStopped } from "@/vms/actions";
import { fromVm, haActive, type NicRow, tcMissing, validate, type VmFormValues } from "@/vms/form";
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
import { FormSteps, ReviewSection, StepSection } from "@/vms/form/FormSteps";
import { errorCounts, FORM_STEPS, stepId } from "@/vms/form/steps";
import { isConflict, trackHotplug, updateVm } from "@/vms/form/submit";
import { firstLocalDisk } from "@/vms/replication";
import { formSnapshotSupport } from "@/vms/snapshots";
import { useVmByName } from "@/vms/VmDetailPage";

// VM 만들기는 계획 77-5부터 그림 한 화면(`vmBuilder/BuilderPage`)이다. 이 폼은 고치기만 한다.
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

/** VM 고치기 */
function VmForm({ vm, initial }: { vm: Vm; initial: VmFormValues }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const nodes = useList("node");
  const pools = useList("storage-pool");
  const networks = useList("network");
  const securityGroups = useList("security-group");
  const volumes = useList("volume");
  const groups = useList("ha-group");
  const [values, setValues] = useState(initial);
  // NIC마다 SDN 네트워크인지(계획 65 — 주소 칸과 cloud-init 주소). 목록을 읽기 전에는 모른다(빈 목록 → 단순으로 본다).
  const sdnNics = values.nics.map((n) => sdnNetworkNamed(networks.data, n.network));
  const noTc = tcMissing(nodes.data, values.node || (vm.status.node ?? ""));
  const [revision, setRevision] = useState(vm.revision);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<{ message: string; conflict: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  // 화면을 떠나면 저장 뒤 VM 화면으로 가지 않는다.
  const abort = useRef<AbortController | null>(null);
  useEffect(() => () => abort.current?.abort(), []);
  // 멈추지 않은 VM(서버의 `is_stopped`가 아닌)의 노드는 서버가 바꾸지 않는다.
  const locked = !isStopped(vm);
  // 실행 중인 VM의 디스크·NIC: 핫플러그 배치로 켰으면 끝에 붙이기·실행 중 붙인 것 떼기, 아니면 잠금
  const mode = deviceMode(vm);
  const layout = vm.status.hardware?.layout;

  const change = (patch: Partial<VmFormValues>) => setValues((v) => ({ ...v, ...patch }));
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
      nics,
      cloudInit: { ...values.cloudInit, networks },
    });
  };

  // 이 VM에 붙어 있거나 아무 데도 붙지 않은 준비된 디스크 볼륨(ISO는 CD-ROM에만 넣는다)
  const freeVolumes = (volumes.data ?? []).filter(
    (v) =>
      v.spec.content !== "iso" &&
      v.status.phase === "ready" &&
      (!v.status.attached_to || v.status.attached_to === vm.id),
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
    setValues(fromVm(vm));
    setRevision(vm.revision);
    setFailure(null);
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const found = validate(
      values,
      false,
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
    try {
      const task = await updateVm(vm, revision, values, sdnNics);
      // 실행 중인 VM의 하드웨어를 바꾸는 Task를 알림으로 지켜본다.
      if (task) {
        void trackHotplug(task, t("tasks.op.vm-hotplug", { target: vm.name }));
      }
      if (!signal.aborted) {
        void navigate(`/vms/${encodeURIComponent(vm.name)}`);
      }
    } catch (err) {
      if (signal.aborted) {
        return;
      }
      // 하드웨어를 바꾸지 못한 까닭(어느 장치를 왜)은 서버의 설명이 말한다.
      setFailure({
        message: serverErrorMessage(t, err, HOTPLUG_ERROR_CODES),
        conflict: isConflict(err),
      });
    } finally {
      if (!signal.aborted) {
        setBusy(false);
      }
    }
  }

  const counts = errorCounts(errors);
  return (
    <form
      onSubmit={submit}
      className="grid max-w-5xl grid-cols-1 gap-x-8 gap-y-4 lg:grid-cols-[12rem_minmax(0,1fr)]"
      noValidate
    >
      <div className="flex flex-wrap items-center gap-3 lg:col-span-2">
        <h1 className="text-xl font-semibold">{t("vmForm.editTitle", { name: vm.name })}</h1>
      </div>
      {/* 긴 폼은 넓은 화면에서 쓰기 좋다(설계 §5.9 당직 보기). */}
      <p className="text-sm text-muted-foreground md:hidden lg:col-span-2">
        {t("common.desktopOnly")}
      </p>
      <FormSteps steps={FORM_STEPS} errors={counts} />
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

        {/* 보내는 동안에는 고치지 못하게 한다. */}
        <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
          <StepSection step="general">
            <GeneralSection
              values={values}
              errors={errors}
              onChange={change}
              nodes={nodes.data}
              groups={groups.data?.map((g) => g.name).sort()}
              creating={false}
              locked={locked}
              haLocked={locked && !haEnabled(vm.spec)}
              live={mode === "live" || mode === "restart"}
              autoMachine={false}
              onMachineChosen={() => undefined}
              noReplica={noReplica}
              vmId={vm.id}
              own={ownUse(vm, (id) => nodes.data?.find((n) => n.id === id)?.name)}
            />
          </StepSection>
          <StepSection step="disks">
            <DisksSection
              values={values}
              errors={errors}
              onChange={change}
              onRowsChange={(disks) => changeRows({ disks })}
              volumes={freeVolumes}
              pools={pools.data}
              creating={false}
              mode={mode}
              saved={(vm.spec.disks ?? []).map((d) => d.volume)}
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
              saved={vm.spec.nics ?? []}
              boot={layout?.boot_nics ?? 0}
              template={vm.spec.template === true}
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
              stopped={isStopped(vm)}
              firmwareNode={vm.status.firmware_node ?? null}
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
            <ScheduleSection values={values} errors={errors} onChange={change} support={support} />
          </StepSection>
          <StepSection step="replication">
            <ReplicationSection
              values={values}
              errors={errors}
              onChange={change}
              onRowsChange={(replication) => changeRows({ replication })}
              nodes={nodes.data}
              pools={pools.data}
              ownNode={values.node || (vm.status.node ?? "")}
            />
          </StepSection>

          <StepSection step="review">
            <ReviewSection values={values} nodes={nodes.data} creating={false} />
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
        <div className="flex gap-2">
          <Button type="submit" disabled={busy}>
            {t("common.save")}
          </Button>
          <Button variant="ghost" asChild>
            <Link to={`/vms/${encodeURIComponent(vm.name)}`}>{t("common.cancel")}</Link>
          </Button>
        </div>
      </div>
    </form>
  );
}
