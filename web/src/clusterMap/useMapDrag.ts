// 지도 끌어 옮기기의 쪽 몫(계획 71 설계 §10): 끌 수 있는 타일, 노드마다 놓을 자리 판정(`dropCheck` — 든 VM의 CPU·NUMA
// 맞음은 들 때 묻고 옮기기 대화상자와 캐시를 함께 씀), 놓으면 대상 노드를 고른 지금의 옮기기 대화상자. 바로 옮기지 않는다.
// 판정 함수는 서랍 조각과 함께 오고(`DragLogic`), 오기 전에는 끌기가 없다(그림은 기다리지 않음).
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, PlacementStatus, Vm } from "@/api/types";
import { objectTarget } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import type { MapDrag } from "@/clusterMap/MapScene";
import type { DragLogic } from "@/clusterMap/MapSheet";
import { useVmCpu } from "@/cpu/cpu";
import type { Item, Verdict } from "@/dnd/types";
import { formatMib } from "@/lib/format";
import { numaInUse, useVmNuma } from "@/numa/numa";
import { useVmActionHost } from "@/vms/actionHost";

export function useMapDrag(
  nodes: readonly Node[] | undefined,
  vms: readonly Vm[] | undefined,
  placement: PlacementStatus | null | undefined,
  logic: DragLogic | null,
) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const tasks = useTaskIndex();
  const host = useVmActionHost();
  const rules = useList("placement-rule");
  const groups = useList("ha-group");
  const volumes = useList("volume");
  const pools = useList("storage-pool");
  const [held, setHeld] = useState<string | null>(null);
  const heldVm = held ? vms?.find((v) => v.id === held) : undefined;
  const cpu = useVmCpu(held ?? "", heldVm !== undefined);
  const usesNuma =
    heldVm !== undefined && (numaInUse(heldVm.spec) || heldVm.status.hardware?.numa != null);
  const numa = useVmNuma(held ?? "", usesNuma);
  const freeMib = useMemo(
    () =>
      placement
        ? new Map(placement.nodes.map((n) => [n.node, n.mem_total_mib - n.mem_allocated_mib]))
        : undefined,
    [placement],
  );

  const vmOf = (id: string) => vms?.find((v) => v.id === id);
  const nodeOf = (key: string) =>
    key.startsWith("node:") ? nodes?.find((n) => n.id === key.slice(5)) : undefined;
  const check = ({ dropCheck, localDisks }: DragLogic, vm: Vm, node: Node) =>
    dropCheck(vm, node, {
      vms: vms ?? [],
      rules: rules.data,
      groups: groups.data,
      cpu: vm.id === held ? cpu.data?.nodes : undefined,
      numa: vm.id === held && usesNuma ? numa.data?.nodes : undefined,
      freeMib,
      copyBytes:
        volumes.data && pools.data
          ? localDisks(vm, volumes.data, pools.data).reduce(
              (sum, d) => sum + d.volume.spec.size_bytes,
              0,
            )
          : undefined,
      mayForce: perms.can("write", "placement-rule"),
    });

  const accepts = (item: Item, slot: string): Verdict => {
    const vm = vmOf(item.id);
    const node = nodeOf(slot);
    return logic && vm && node ? logic.toVerdict(check(logic, vm, node)) : { ok: false };
  };

  const onDrop = (item: Item, slot: string) => {
    const vm = vmOf(item.id);
    const node = nodeOf(slot);
    if (vm && node) {
      const owner = document.querySelector(`g[data-key="${slot}"]`) as HTMLElement | null;
      host.open({ vm, which: "migrate", node: node.id, returnFocus: { current: owner } });
    }
  };

  const drag: MapDrag | null = logic && {
    item(tileKey) {
      const vm = tileKey.startsWith("vm:") ? vmOf(tileKey.slice(3)) : undefined;
      if (!vm) {
        return null;
      }
      const block = logic.dragBlock(
        vm,
        tasks.get(vm.id) ?? [],
        perms.has("vm.migrate", objectTarget("vm", vm)),
      );
      return block ? t(block.key, block.values) : { id: vm.id, label: vm.name };
    },
    free(nodeKey) {
      const node = nodeOf(nodeKey);
      const free = node ? freeMib?.get(node.id) : undefined;
      return free === undefined
        ? null
        : t("clusterMap.drag.free", { free: formatMib(Math.max(0, free)) });
    },
    onHold: setHeld,
  };

  return { accepts, onDrop, drag };
}
