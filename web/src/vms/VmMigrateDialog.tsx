import { type RefObject, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, Vm } from "@/api/types";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { trackTask } from "@/tasks/tasks";
import { availability } from "@/vms/actions";

type Schemas = components["schemas"];

/** 머신 타입의 아키텍처(서버의 `MachineType::arch`) */
const ARCH_OF: Record<Schemas["MachineType"], Schemas["Arch"]> = {
  q35: "x86_64",
  virt: "aarch64",
};

/**
 * 옮겨 갈 수 있어 보이는 노드: 온라인이고 유지보수 중이 아니며 VM 머신의 아키텍처인, 지금 노드가 아닌 노드.
 * 풀·브리지·메모리는 서버가 본다(409로 알린다).
 */
export function migrationTargets(vm: Vm, nodes: readonly Node[]): Node[] {
  const here = vm.spec.node ?? vm.status.node;
  const arch = ARCH_OF[vm.spec.machine];
  return nodes.filter(
    (n) => n.id !== here && n.status.online && !n.spec.maintenance && n.status.arch === arch,
  );
}

/** 실행 중인 VM의 라이브 마이그레이션 대화상자. 열 때만 그린다. */
export function VmMigrateDialog({
  vm,
  onClose,
  returnFocus,
}: {
  vm: Vm;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const targets = migrationTargets(vm, nodes.data ?? []);
  const [chosen, setChosen] = useState<string | null>(null);
  // 고르지 않았거나 고른 노드가 후보에서 빠졌으면 첫 후보
  const target = targets.find((n) => n.id === chosen) ?? targets[0];
  // 대화상자를 연 사이 VM이 멈추거나 옮기기 시작했을 수 있다.
  const able = availability(vm, "migrate", vmTasks);

  async function submit() {
    if (!target) {
      return false;
    }
    const task = await call(
      api.POST("/vms/{key}/migrate", {
        params: { path: { key: vm.id } },
        body: { node: target.id },
      }),
    );
    trackTask(task, t("migrate.op", { vm: vm.name, node: target.name }));
  }

  return (
    <FormDialog
      title={t("migrate.title", { name: vm.name })}
      description={t("migrate.description")}
      submitLabel={t("migrate.submit")}
      submitDisabled={!able.ok || !target}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {!able.ok && <p className="text-sm text-muted-foreground">{t(able.reason)}</p>}
      {!nodes.data ? (
        <ListPending error={nodes.error} />
      ) : (
        targets.length === 0 && (
          <p className="text-sm text-muted-foreground">{t("migrate.noTarget")}</p>
        )
      )}
      <Field id="migrate-node" label={t("migrate.node")}>
        {(a11y) => (
          <NativeSelect
            id="migrate-node"
            className="w-full"
            value={target?.id ?? ""}
            onChange={(e) => setChosen(e.target.value)}
            {...a11y}
          >
            {targets.map((n) => (
              <NativeSelectOption key={n.id} value={n.id}>
                {n.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
    </FormDialog>
  );
}
