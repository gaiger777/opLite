// 고급 설정 서랍(계획 77-5 — 진단 §4.3 "고급은 오른쪽 서랍 하나"): CPU 모델·플래그·최대 vCPU/메모리·펌웨어·머신·NUMA·
// 전용 CPU·hugepages·재시작 정책·노드 고정·HA 세부(GeneralSection의 고급), 보안 부팅·vTPM·게스트 에이전트, 장치 세부
// (디스크·NIC — QoS·MAC·부팅 순번·CD), 첫 부팅 세부(cloud-init), 부팅 순서, 예약 스냅샷, 복제. 칸은 VM 고치기 폼의 섹션 그대로(칸 설명·ⓘ도). 기본값은 손대지 않아도 된다.
// 오류는 이 서랍의 칸을 떠난 뒤나 만들기를 누른 뒤에만 보인다. 템플릿 바탕은 배포가 이 칸들을 싣지 못한다고 알린다.
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import { choosableIsos } from "@/isos/isos";
import { useBuilder } from "@/vmBuilder/context";
import { freeVolumes } from "@/vmBuilder/model/parts";
import { sdnOf } from "@/vmBuilder/model/requests";
import { renumber } from "@/vmBuilder/model/state";
import {
  type DiskRow,
  emptyDisk,
  haActive,
  type NicRow,
  tcMissing,
  validate,
  type VmFormValues,
} from "@/vms/form";
import { BootOrderSection } from "@/vms/form/BootOrderSection";
import { CdromsSection } from "@/vms/form/CdromsSection";
import { DisksSection } from "@/vms/form/DisksSection";
import { NicsSection } from "@/vms/form/NicsSection";
import { CloudInitSection } from "@/vms/form/CloudInitSection";
import { GuestAgentSection, SecuritySection } from "@/vms/form/FirmwareSection";
import { GeneralSection } from "@/vms/form/GeneralSection";
import { ReplicationSection } from "@/vms/form/ReplicationSection";
import { ScheduleSection } from "@/vms/form/ScheduleSection";
import { firstLocalDisk } from "@/vms/replication";
import { formSnapshotSupport } from "@/vms/snapshots";

export function Advanced({ onClose, attempted }: { onClose(): void; attempted: boolean }) {
  const { t } = useTranslation();
  const b = useBuilder();
  const v = b.s.values;
  const ctx = b.ctx;
  const [left, setLeft] = useState(false);
  const sdn = sdnOf(b.s, ctx);
  const found = validate(
    v,
    true,
    ctx.volumes.map((x) => x.name),
    sdn,
  );
  const errors = attempted || left ? found : {};
  const change = (patch: Partial<VmFormValues>) => b.edit(patch);
  const nodes = ctx.nodes ?? undefined;
  const noReplica =
    haActive(v) && v.replication.length === 0
      ? firstLocalDisk(
          v.disks.map((d) => ({ volume: d.volume, ...(d.kind === "new" && { pool: d.pool }) })),
          ctx.volumes,
          ctx.pools,
        )
      : null;
  const template = b.s.base.kind === "template";
  // 디스크 행을 바꾸면 새 디스크가 VM 이름을 따라가는지 다시 본다(그림의 놓기와 같은 규칙).
  const setDisks = (disks: DiskRow[]) =>
    b.set(
      renumber({
        ...b.s,
        values: { ...v, disks },
        autoNames: disks.map(
          (d, i) => d.kind === "new" && d.volume === emptyDisk(v.name, i).volume,
        ),
      }),
    );
  // NIC를 빼면 cloud-init의 그 NIC 주소도 뺀다(주소는 NIC 순번을 따른다 — 고치기 폼과 같음).
  const setNics = (nics: NicRow[]) => {
    const removed = v.nics.findIndex((n, i) => nics[i] !== n);
    const networks =
      nics.length < v.nics.length && removed !== -1
        ? v.cloudInit.networks.filter((_, i) => i !== removed)
        : v.cloudInit.networks;
    change({ nics, cloudInit: { ...v.cloudInit, networks } });
  };
  return (
    <Sheet open onOpenChange={(o) => !o && onClose()}>
      <SheetContent
        side="right"
        className="w-full gap-0 overflow-y-auto p-0 sm:max-w-2xl"
        onBlurCapture={(e) => {
          if ((e.target as Element).matches?.("input, select, textarea")) {
            setLeft(true);
          }
        }}
      >
        <div className="flex flex-col gap-1 border-b p-4">
          <SheetTitle>{t("vmBuilder.advanced.title")}</SheetTitle>
          <SheetDescription>{t("vmBuilder.advanced.text")}</SheetDescription>
        </div>
        {template ? (
          <p className="p-4 text-sm text-muted-foreground">{t("vmBuilder.advanced.template")}</p>
        ) : (
          <fieldset disabled={b.busy} className="flex min-w-0 flex-col gap-4 p-4">
            <section aria-labelledby="vb-adv-cpu" className="flex flex-col gap-3">
              <h3 id="vb-adv-cpu" className="text-sm font-medium">
                {t("vmBuilder.advanced.cpu")}
              </h3>
              <GeneralSection
                advanced
                values={v}
                errors={errors}
                onChange={change}
                nodes={nodes}
                groups={ctx.haGroups.map((g) => g.name).sort()}
                creating
                locked={false}
                haLocked={false}
                live={false}
                autoMachine={false}
                onMachineChosen={() => undefined}
                noReplica={noReplica}
              />
            </section>
            <SecuritySection
              values={v}
              errors={errors}
              onChange={change}
              nodes={nodes}
              stopped
              firmwareNode={null}
              live={false}
            />
            <GuestAgentSection values={v} onChange={change} live={false} />
            <DisksSection
              values={v}
              errors={errors}
              onChange={change}
              onRowsChange={setDisks}
              volumes={freeVolumes(ctx, { ...b.s, values: { ...v, disks: [] } })}
              pools={ctx.pools}
              creating
              mode="free"
              saved={[]}
              boot={0}
            />
            <NicsSection
              values={v}
              errors={errors}
              onChange={change}
              onRowsChange={setNics}
              networks={ctx.networks}
              securityGroups={ctx.securityGroups}
              mode="free"
              saved={[]}
              boot={0}
              noTc={tcMissing(nodes, v.node)}
            />
            <CdromsSection
              values={v}
              errors={errors}
              onChange={change}
              onRowsChange={(cdroms) => change({ cdroms })}
              isos={choosableIsos(ctx.volumes, ctx.pools, haActive(v) || v.replication.length > 0)}
              mode="free"
            />
            <CloudInitSection
              values={v}
              errors={errors}
              onChange={change}
              running={false}
              sdn={sdn}
            />
            <BootOrderSection values={v} errors={errors} onChange={change} locked={false} />
            <ScheduleSection
              values={v}
              errors={errors}
              onChange={change}
              support={formSnapshotSupport(v, ctx.volumes, ctx.pools)}
            />
            <ReplicationSection
              values={v}
              errors={errors}
              onChange={change}
              onRowsChange={(replication) => change({ replication })}
              nodes={nodes}
              pools={ctx.pools}
              ownNode={v.node}
            />
          </fieldset>
        )}
      </SheetContent>
    </Sheet>
  );
}
