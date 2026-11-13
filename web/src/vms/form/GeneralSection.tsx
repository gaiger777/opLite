import { useTranslation } from "react-i18next";

import type { Node } from "@/api/types";
import { Field } from "@/components/Field";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import { defaultMachine, haActive, haUnavailable, type VmFormValues } from "@/vms/form";
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
  autoMachine,
  onMachineChosen,
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
  /** 노드를 바꾸면 그 노드에 맞게 머신 기본값을 다시 정한다. */
  autoMachine: boolean;
  /** 사용자가 머신을 직접 골랐다. */
  onMachineChosen: () => void;
}) {
  const { t } = useTranslation();
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
        <Field id="vm-cores" label={t("vmForm.cores")} error={errors.cores}>
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
        <Field id="vm-cpu-model" label={t("vmForm.cpuModel")}>
          <NativeSelect
            id="vm-cpu-model"
            className="w-full"
            value={values.model}
            onChange={(e) => onChange({ model: e.target.value as VmFormValues["model"] })}
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
              onChange({ machine: e.target.value as VmFormValues["machine"] });
            }}
          >
            <NativeSelectOption value="q35">q35 (x86_64)</NativeSelectOption>
            <NativeSelectOption value="virt">virt (aarch64)</NativeSelectOption>
          </NativeSelect>
        </Field>
        <Field id="vm-node" label={t("common.node")}>
          <NativeSelect
            id="vm-node"
            className="w-full"
            disabled={locked}
            value={values.node}
            onChange={(e) => {
              const node = e.target.value;
              onChange(
                autoMachine ? { node, machine: defaultMachine(nodes ?? [], node) } : { node },
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
