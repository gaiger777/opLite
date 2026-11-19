import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import type { Node } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import { archOf, useCpuModels } from "@/cpu/cpu";
import { CpuFlagsInput } from "@/cpu/CpuFlagsInput";
import { CpuModelPicker } from "@/cpu/CpuModelPicker";
import { folderPath, folderTree } from "@/folders/folders";
import { coresFor, type OwnUse } from "@/numa/numa";
import { NumaSection } from "@/numa/NumaSection";
import {
  defaultMachine,
  haActive,
  haUnavailable,
  MAX_HOTPLUG_MEMORY_MIB,
  MAX_VCPUS,
  MEMORY_HOTPLUG_ALIGN_MIB,
  toSpec,
  type VmFormValues,
} from "@/vms/form";
import {
  MAX_GUEST_WATCH_SECS,
  MAX_HA_COUNT,
  MAX_HA_PRIORITY,
  MIN_GUEST_WATCH_SECS,
} from "@/vms/ha";

const HA_STATES: VmFormValues["haState"][] = ["started", "stopped", "ignored"];

/** 이름·CPU·메모리·펌웨어·머신·노드·재시작 정책·HA */
export function GeneralSection({
  values,
  errors,
  onChange,
  nodes,
  groups,
  creating,
  locked,
  haLocked,
  live,
  autoMachine,
  onMachineChosen,
  noReplica,
  vmId,
  own,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  nodes: Node[] | undefined;
  /** HA 그룹 이름 */
  groups: string[] | undefined;
  creating: boolean;
  /** 멈추지 않은 VM의 노드는 바꾸지 못한다. */
  locked: boolean;
  /** 실행 중이고 HA가 활성이 아닌(없거나 `ignored`) VM은 HA를 활성으로 바꾸지 못한다(서버의 409). */
  haLocked: boolean;
  /** 실행 중이고 서버가 하드웨어 변경을 핫플러그 Task로 적용한다(상한 안의 cores·메모리는 바로). */
  live: boolean;
  /** 노드를 바꾸면 그 노드에 맞게 머신 기본값을 다시 정한다. */
  autoMachine: boolean;
  /** 사용자가 머신을 직접 골랐다. */
  onMachineChosen: () => void;
  /** 활성 HA인데 복제가 없는 노드 로컬 풀의 디스크. 그 노드가 죽으면 다른 노드에서 켤 수 없다. */
  noReplica?: { volume: string; pool: string } | null;
  /** 고치는 VM의 id(만들 때는 없음). 전용 CPU·hugepages의 권한 `vm.dedicated`를 그 VM에서 본다(만들 때는 폴더). */
  vmId?: string;
  /** 고치는 VM이 지금 잡은 전용 CPU·hugepages("들어갈 노드" 어림) */
  own?: OwnUse;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const canDedicate = perms.has("vm.dedicated", {
    type: "object",
    kind: "vm",
    id: vmId,
    folder: values.folder || null,
  });
  // 클러스터 기준이 있으면(auto·pinned) 새 VM의 기본 모델은 `cluster`다(설계 §13). 사용자가 고르면 그대로 둔다.
  // 서버는 기준이 있을 때만 `current`를 준다(끔·업그레이드 중에는 없다 — 리뷰 1 웹 #1).
  const chose = useRef(false);
  // 효과가 고른 `cluster`(사용자가 고르지 않음). 아키텍처가 바뀌면 되돌리고 새 아키텍처에서 다시 고르게 한다(리뷰 2 웹 #5).
  const autoCluster = useRef(false);
  const arch = archOf(values.machine);
  const models = useCpuModels(arch, creating);
  const clusterNow = models.data?.models.find((m) => m.kind === "cluster")?.current;
  // 머신 유형이 아키텍처를 바꾸면 그 아키텍처에 없는 모델·플래그를 처음으로 돌리고 그렇다고 말한다(리뷰 1 웹 #8).
  const [archReset, setArchReset] = useState(false);
  const withMachine = (next: Partial<VmFormValues>): Partial<VmFormValues> => {
    const to = next.machine === undefined ? arch : archOf(next.machine);
    const portable = ["host", "max", "cluster"].includes(values.model);
    if (to === undefined || to === arch) {
      return next;
    }
    // 자동 머신(`""`)은 아키텍처를 모른다: 다른 아키텍처일 수 있으니 옮길 수 없는 모델·플래그만 되돌린다.
    if (arch === undefined && portable && values.cpuFlags.length === 0) {
      return next;
    }
    // 효과가 고른 `cluster`는 그 아키텍처의 기준이었다 — `host`로 돌리고 새 아키텍처에 기준이 있으면 다시 고른다.
    if (autoCluster.current && values.model === "cluster") {
      autoCluster.current = false;
      chose.current = false;
      return values.cpuFlags.length === 0
        ? { ...next, model: "host" }
        : { ...next, model: "host", cpuFlags: [] };
    }
    if (portable && values.cpuFlags.length === 0) {
      return next;
    }
    setArchReset(true);
    return { ...next, model: portable ? values.model : "host", cpuFlags: [] };
  };
  useEffect(() => {
    if (creating && !chose.current && clusterNow && values.model === "host") {
      chose.current = true;
      autoCluster.current = true;
      onChange({ model: "cluster" });
    }
  }, [creating, clusterNow, values.model, onChange]);
  const noHa = haLocked ? { key: "vmForm.haRunning" } : haUnavailable(nodes, values.node);
  // 그룹 목록에 없는 그룹(지운 그룹)도 고른 값으로 보인다(서버가 저장할 때 거절한다).
  const groupNames = [...(groups ?? [])];
  if (values.haGroup !== "" && !groupNames.includes(values.haGroup)) {
    groupNames.unshift(values.haGroup);
  }
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vmForm.general")}</CardTitle>
      </CardHeader>
      <CardContent className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        {live && (
          <p className="text-sm text-muted-foreground sm:col-span-2">{t("vmForm.liveHint")}</p>
        )}
        {creating && (
          <Field id="vm-name" label={t("common.name")} error={errors.name}>
            {(a11y) => (
              <Input
                id="vm-name"
                {...a11y}
                value={values.name}
                onChange={(e) => onChange({ name: e.target.value })}
              />
            )}
          </Field>
        )}
        <Field
          id="vm-cores"
          label={values.sockets > 1 ? t("vmForm.coresPerSocket") : t("vmForm.cores")}
          error={errors.cores}
          hint={
            values.sockets > 1 && /^\d+$/.test(values.cores)
              ? t("vmForm.totalVcpus", {
                  sockets: values.sockets,
                  count: values.sockets * Number(values.cores),
                })
              : undefined
          }
        >
          {(a11y) => (
            <Input
              id="vm-cores"
              {...a11y}
              type="number"
              min={1}
              max={65535}
              value={values.cores}
              onChange={(e) => onChange({ cores: e.target.value })}
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
              onChange={(e) => onChange({ memory: e.target.value })}
            />
          )}
        </Field>
        <Field
          id="vm-max-vcpus"
          label={t("vmForm.maxVcpus")}
          error={errors.maxVcpus}
          hint={t("vmForm.maxVcpusHint")}
        >
          {(a11y) => (
            <Input
              id="vm-max-vcpus"
              {...a11y}
              type="number"
              min={1}
              max={MAX_VCPUS}
              value={values.maxVcpus}
              onChange={(e) => onChange({ maxVcpus: e.target.value })}
            />
          )}
        </Field>
        <Field
          id="vm-max-memory"
          label={t("vmForm.maxMemory")}
          error={errors.maxMemory}
          hint={t("vmForm.maxMemoryHint")}
        >
          {(a11y) => (
            <Input
              id="vm-max-memory"
              {...a11y}
              type="number"
              min={MEMORY_HOTPLUG_ALIGN_MIB}
              max={MAX_HOTPLUG_MEMORY_MIB}
              step={MEMORY_HOTPLUG_ALIGN_MIB}
              value={values.maxMemory}
              onChange={(e) => onChange({ maxMemory: e.target.value })}
            />
          )}
        </Field>
        <CpuModelPicker
          id="vm-cpu-model"
          label={t("vmForm.cpuModel")}
          value={values.model}
          arch={archOf(values.machine)}
          onChange={(model) => {
            chose.current = true;
            autoCluster.current = false;
            setArchReset(false);
            onChange({ model });
          }}
        />
        {archReset && (
          <p role="status" className="text-xs text-muted-foreground sm:col-span-2">
            {t("cpu.model.archReset")}
          </p>
        )}
        <Field id="vm-firmware" label={t("vms.firmware")}>
          <NativeSelect
            id="vm-firmware"
            className="w-full"
            value={values.firmware}
            onChange={(e) => onChange({ firmware: e.target.value as VmFormValues["firmware"] })}
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
            onChange={(e) => {
              onMachineChosen();
              onChange(withMachine({ machine: e.target.value as VmFormValues["machine"] }));
            }}
          >
            {/* 노드를 모르면(폴더 사용자) 서버가 클러스터 아키텍처로 고른다. */}
            {(nodes === undefined || values.machine === "") && (
              <NativeSelectOption value="">{t("vmForm.machineAuto")}</NativeSelectOption>
            )}
            <NativeSelectOption value="q35">q35 (x86_64)</NativeSelectOption>
            <NativeSelectOption value="virt">virt (aarch64)</NativeSelectOption>
          </NativeSelect>
        </Field>
        <CpuFlagsInput
          id="vm-cpu-flags"
          label={t("cpu.flags.label")}
          hint={live ? t("cpu.flags.liveHint") : t("cpu.flags.hint")}
          flags={values.cpuFlags}
          arches={[archOf(values.machine)]}
          signed
          onChange={(cpuFlags) => onChange({ cpuFlags })}
        />
        <NumaSection
          sockets={values.sockets}
          cores={values.cores}
          maxVcpus={values.maxVcpus}
          value={values.numa}
          onChange={(numa) => onChange({ numa })}
          onSockets={(sockets) => {
            // vCPU 수(소켓 × 코어)를 그대로 둔다(리뷰 1 웹 — 소켓을 늘리면 vCPU가 곱해졌다).
            const cores = coresFor(values.sockets, values.cores, sockets);
            onChange(cores === null ? { sockets } : { sockets, cores: String(cores) });
          }}
          own={own}
          errors={errors}
          canDedicate={canDedicate}
          spec={toSpec(values, "stopped")}
          running={!creating && locked}
        />
        {creating && (
          <FolderField value={values.folder} onChange={(folder) => onChange({ folder })} />
        )}
        {/* 노드 목록을 읽을 수 없으면(폴더 사용자) 자동 배치다. */}
        {nodes !== undefined && (
          <Field id="vm-node" label={t("common.node")}>
            <NativeSelect
              id="vm-node"
              className="w-full"
              disabled={locked}
              value={values.node}
              onChange={(e) => {
                const node = e.target.value;
                onChange(
                  autoMachine
                    ? withMachine({ node, machine: defaultMachine(nodes ?? [], node) })
                    : { node },
                );
              }}
            >
              <NativeSelectOption value="">{t("vmForm.nodeAuto")}</NativeSelectOption>
              {/* 위트니스는 VM을 돌리지 않는다. */}
              {nodes
                ?.filter((n) => !n.spec.witness)
                .map((n) => (
                  <NativeSelectOption key={n.id} value={n.id}>
                    {n.name}
                  </NativeSelectOption>
                ))}
            </NativeSelect>
          </Field>
        )}
        <Field id="vm-restart" label={t("vms.restart")}>
          <NativeSelect
            id="vm-restart"
            className="w-full"
            value={values.restart}
            onChange={(e) => onChange({ restart: e.target.value as VmFormValues["restart"] })}
          >
            <NativeSelectOption value="never">{t("vms.restartPolicy.never")}</NativeSelectOption>
            <NativeSelectOption value="on-failure">
              {t("vms.restartPolicy.on-failure")}
            </NativeSelectOption>
          </NativeSelect>
        </Field>
        <div className="flex flex-col gap-1.5 sm:col-span-2">
          <div className="flex items-center gap-2">
            {/* 실행 중에도 바꿀 수 있다(장치 변화 없음). */}
            <Switch
              id="vm-pinned"
              aria-describedby="vm-pinned-hint"
              checked={values.pinned}
              onCheckedChange={(pinned) => onChange({ pinned })}
            />
            <Label htmlFor="vm-pinned">{t("vmForm.pinned")}</Label>
          </div>
          <p id="vm-pinned-hint" className="text-xs text-muted-foreground">
            {t("vmForm.pinnedHint")}
          </p>
        </div>
        <div className="flex flex-col gap-1.5 sm:col-span-2">
          <div className="flex items-center gap-2">
            {/* 켤 수 없어도 이미 켜진 HA는 끌 수 있다(실행 중이어도). */}
            <Switch
              id="vm-ha"
              aria-describedby="vm-ha-hint"
              disabled={noHa !== null && !values.ha}
              checked={values.ha}
              onCheckedChange={(ha) => onChange({ ha })}
            />
            <Label htmlFor="vm-ha">{t("vmForm.ha")}</Label>
          </div>
          <p id="vm-ha-hint" className="text-xs text-muted-foreground">
            {t("vmForm.haHint")}
          </p>
          {noHa && (
            <p
              className={
                haActive(values) ? "text-sm text-destructive" : "text-sm text-muted-foreground"
              }
            >
              {t(noHa.key, noHa.values)}
            </p>
          )}
          {noReplica && (
            <p className="text-sm text-warning-fg">{t("vmForm.haNoReplica", noReplica)}</p>
          )}
        </div>
        {values.ha && (
          <>
            <Field
              id="vm-ha-state"
              label={t("vmForm.haState")}
              hint={t(`vmForm.haStateHint.${values.haState}`)}
            >
              {(a11y) => (
                <NativeSelect
                  id="vm-ha-state"
                  className="w-full"
                  value={values.haState}
                  onChange={(e) => onChange({ haState: e.target.value as VmFormValues["haState"] })}
                  {...a11y}
                >
                  {HA_STATES.map((state) => (
                    // 실행 중인 VM은 HA를 활성으로 바꾸지 못한다.
                    <NativeSelectOption
                      key={state}
                      value={state}
                      disabled={haLocked && state !== "ignored"}
                    >
                      {t(`vms.haState.${state}`)}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              )}
            </Field>
            <Field id="vm-ha-group" label={t("vmForm.haGroup")} hint={t("vmForm.haGroupHint")}>
              {(a11y) => (
                <NativeSelect
                  id="vm-ha-group"
                  className="w-full"
                  value={values.haGroup}
                  onChange={(e) => onChange({ haGroup: e.target.value })}
                  {...a11y}
                >
                  <NativeSelectOption value="">{t("vmForm.haNoGroup")}</NativeSelectOption>
                  {groupNames.map((g) => (
                    <NativeSelectOption key={g} value={g}>
                      {g}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              )}
            </Field>
            <Field
              id="vm-max-restart"
              label={t("vmForm.maxRestart")}
              error={errors.maxRestart}
              hint={t("vmForm.maxRestartHint")}
            >
              {(a11y) => (
                <Input
                  id="vm-max-restart"
                  {...a11y}
                  type="number"
                  min={0}
                  max={MAX_HA_COUNT}
                  value={values.maxRestart}
                  onChange={(e) => onChange({ maxRestart: e.target.value })}
                />
              )}
            </Field>
            <Field
              id="vm-max-relocate"
              label={t("vmForm.maxRelocate")}
              error={errors.maxRelocate}
              hint={t("vmForm.maxRelocateHint")}
            >
              {(a11y) => (
                <Input
                  id="vm-max-relocate"
                  {...a11y}
                  type="number"
                  min={0}
                  max={MAX_HA_COUNT}
                  value={values.maxRelocate}
                  onChange={(e) => onChange({ maxRelocate: e.target.value })}
                />
              )}
            </Field>
            <Field
              id="vm-ha-priority"
              label={t("vmForm.haPriority")}
              error={errors.haPriority}
              hint={t("vmForm.haPriorityHint")}
            >
              {(a11y) => (
                <Input
                  id="vm-ha-priority"
                  {...a11y}
                  type="number"
                  min={0}
                  max={MAX_HA_PRIORITY}
                  value={values.haPriority}
                  onChange={(e) => onChange({ haPriority: e.target.value })}
                />
              )}
            </Field>
            <div className="flex flex-col gap-1.5 sm:col-span-2">
              <div className="flex items-center gap-2">
                <Switch
                  id="vm-guest-watch"
                  aria-describedby="vm-guest-watch-hint"
                  checked={values.guestWatch}
                  onCheckedChange={(guestWatch) => onChange({ guestWatch })}
                />
                <Label htmlFor="vm-guest-watch">{t("vmForm.guestWatch")}</Label>
              </div>
              <p id="vm-guest-watch-hint" className="text-xs text-muted-foreground">
                {t("vmForm.guestWatchHint")}
              </p>
            </div>
            {values.guestWatch && (
              <Field
                id="vm-guest-watch-secs"
                label={t("vmForm.guestWatchSecs")}
                error={errors.guestWatchSecs}
                hint={t("vmForm.guestWatchSecsHint")}
              >
                {(a11y) => (
                  <Input
                    id="vm-guest-watch-secs"
                    {...a11y}
                    type="number"
                    min={MIN_GUEST_WATCH_SECS}
                    max={MAX_GUEST_WATCH_SECS}
                    value={values.guestWatchSecs}
                    onChange={(e) => onChange({ guestWatchSecs: e.target.value })}
                  />
                )}
              </Field>
            )}
          </>
        )}
      </CardContent>
    </Card>
  );
}

/**
 * 새 VM의 폴더(계획 62): 루트(루트에 `vm.create`가 있을 때)와 `vm.create`가 있는 VM 폴더. 폴더가 없으면 보이지
 * 않는다.
 */
function FolderField({ value, onChange }: { value: string; onChange(folder: string): void }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const folders = useList("folder");
  const tree = folderTree(folders.data, "vm");
  if (tree.folders.length === 0) {
    return null;
  }
  const allowed = tree.folders
    .filter(
      (f) => f.id === value || perms.has("vm.create", { type: "object", kind: "vm", folder: f.id }),
    )
    .map((f) => ({ id: f.id, path: folderPath(tree, f.id) }))
    .sort((a, b) => a.path.localeCompare(b.path));
  return (
    <Field id="vm-folder" label={t("folders.folder")}>
      <NativeSelect
        id="vm-folder"
        className="w-full"
        value={value}
        onChange={(e) => onChange(e.target.value)}
      >
        {(perms.has("vm.create") || value === "") && (
          <NativeSelectOption value="">{t("folders.root")}</NativeSelectOption>
        )}
        {allowed.map((f) => (
          <NativeSelectOption key={f.id} value={f.id}>
            {f.path}
          </NativeSelectOption>
        ))}
      </NativeSelect>
    </Field>
  );
}
