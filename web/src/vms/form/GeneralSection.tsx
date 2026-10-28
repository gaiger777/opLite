import { useTranslation } from "react-i18next";

import type { Node } from "@/api/types";
import { Field } from "@/components/Field";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { defaultMachine, type VmFormValues } from "@/vms/form";

/** 이름·CPU·메모리·펌웨어·머신·노드·재시작 정책 */
export function GeneralSection({
  values,
  errors,
  onChange,
  nodes,
  creating,
  locked,
  autoMachine,
  onMachineChosen,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  nodes: Node[] | undefined;
  creating: boolean;
  /** 멈추지 않은 VM의 노드는 바꾸지 못한다. */
  locked: boolean;
  /** 노드를 바꾸면 그 노드에 맞게 머신 기본값을 다시 정한다. */
  autoMachine: boolean;
  /** 사용자가 머신을 직접 골랐다. */
  onMachineChosen: () => void;
}) {
  const { t } = useTranslation();
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
            {nodes?.map((n) => (
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
      </CardContent>
    </Card>
  );
}
