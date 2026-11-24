// 화면 읽기 이름(계획 70 §10.3): 장면의 노드·타일·화살표마다 한 번 셈한다. 가리키기 카드와 `aria-live` 줄이 같은 글을
// 쓴다. 모델의 VM·노드를 한 번씩만 훑는다(원소마다 목록을 훑지 않음).
import type { MapLink, MapScene, NodeItem, SlabItem } from "@/clusterMap/layout";
import type { ClusterModel, MapNode, MapVm } from "@/clusterMap/model";

type Translate = (key: string, values?: Record<string, unknown>) => string;

const L = "clusterMap.label";

function slabText(t: Translate, slab: SlabItem | undefined): string | null {
  if (!slab) {
    return null;
  }
  switch (slab.kind) {
    case "group":
      return t(`${L}.slab`, { name: slab.name });
    case "nogroup":
      return t(`${L}.noGroup`);
    default:
      return null;
  }
}

function nodeText(t: Translate, item: NodeItem, n: MapNode, slab: SlabItem | undefined): string {
  const parts: string[] = [t(n.witness ? `${L}.witness` : `${L}.node`, { name: n.name })];
  const s = slabText(t, slab);
  if (s) {
    parts.push(s);
  }
  parts.push(t(n.online ? `${L}.online` : `${L}.offline`));
  if (n.frozen) {
    parts.push(t(`${L}.frozen`));
  }
  if (n.maintenance) {
    parts.push(
      n.outgoing > 0
        ? t(`${L}.draining`, { moving: n.outgoing, total: n.pinnedRunning })
        : t(`${L}.maintenance`),
    );
  }
  if (n.leader) {
    parts.push(t(`${L}.leader`));
  }
  if (n.witness) {
    return parts.join(", ");
  }
  parts.push(
    n.load
      ? t(`${L}.load`, { cpu: Math.round(n.load.cpu), mem: Math.round(n.load.mem) })
      : t(`${L}.loadUnknown`),
  );
  if (n.allocatedPct !== null && n.allocatedPct > 100) {
    parts.push(t(`${L}.overcommit`, { pct: n.allocatedPct }));
  }
  parts.push(vmsText(t, item));
  if (n.hiddenVms > 0) {
    parts.push(t(`${L}.hiddenCount`, { count: n.hiddenVms }));
  }
  if (n.alerts > 0) {
    parts.push(t(`${L}.alerts`, { count: n.alerts }));
  }
  return parts.join(", ");
}

/** "VM 14개(켜짐 12, 실패 1)" */
function vmsText(t: Translate, item: NodeItem): string {
  const c = item.counts;
  const total = c.running + c.stopped + c.failed + c.busy;
  if (total === 0) {
    return t(`${L}.noVms`);
  }
  const detail = (["running", "busy", "failed", "stopped"] as const)
    .filter((k) => c[k] > 0)
    .map((k) => t(`${L}.count.${k}`, { count: c[k] }))
    .join(", ");
  return t(`${L}.vms`, { count: total, detail });
}

function vmText(
  t: Translate,
  vm: MapVm,
  nodeName: (id: string) => string,
  stale: boolean,
  progress: number | null,
): string {
  const parts = [t(`${L}.vm`, { name: vm.name, phase: t(`vms.phase.${vm.phase}`) })];
  parts.push(vm.node ? t(`${L}.onNode`, { node: nodeName(vm.node) }) : t(`${L}.notPlaced`));
  if (vm.ha) {
    parts.push(t(`${L}.ha`));
  }
  if (vm.violations > 0) {
    parts.push(t(`${L}.violations`, { count: vm.violations }));
  }
  if (vm.migration) {
    const to = nodeName(vm.migration.to);
    parts.push(
      progress !== null
        ? t(`${L}.movingPct`, { node: to, progress })
        : t(`${L}.moving`, { node: to }),
    );
  }
  if (stale) {
    parts.push(t(`${L}.stale`));
  }
  return parts.join(", ");
}

/** 화살표의 글: "web → n3 · 62%", 추천은 "추천: web n1 → n3" */
export function linkText(t: Translate, link: MapLink, nodeName: (key: string) => string): string {
  const from = nodeName(link.ends[0]);
  const to = nodeName(link.ends[1]);
  if (link.kind === "recommendation") {
    return t(`${L}.recommendation`, { vm: link.vmName, from, to });
  }
  if (link.progress !== null) {
    return t(`${L}.migrationPct`, { vm: link.vmName, to, progress: link.progress });
  }
  return link.stage
    ? t(`${L}.migrationStage`, { vm: link.vmName, to, stage: link.stage })
    : t(`${L}.migration`, { vm: link.vmName, to });
}

/** 열쇠(노드 `node:`·`unplaced`, 타일 `vm:`·`more:`·`hidden:`·`ghost:`, 화살표) → 화면 읽기 이름 */
export function mapLabels(t: Translate, model: ClusterModel, scene: MapScene): Map<string, string> {
  const nodes = new Map(model.nodes.map((n) => [n.id, n]));
  const vms = new Map(model.vms.map((v) => [v.id, v]));
  const slabs = new Map(scene.slabs.map((s) => [s.key, s]));
  const progress = new Map(
    model.moves.filter((m) => m.kind === "migration").map((m) => [m.vm, m.progress]),
  );
  const nameOf = (id: string) => nodes.get(id)?.name ?? id;
  const ownerName = (item: NodeItem) =>
    item.kind === "unplaced" ? t(`${L}.unplacedName`) : item.name;
  const out = new Map<string, string>();

  for (const item of scene.nodes) {
    const n = nodes.get(item.id);
    if (item.kind === "unplaced") {
      out.set(item.key, t(`${L}.unplaced`, { count: slabs.get(item.slab)?.count ?? 0 }));
    } else if (n) {
      out.set(item.key, nodeText(t, item, n, slabs.get(item.slab)));
    }
    for (const cell of item.grid?.cells ?? []) {
      switch (cell.kind) {
        case "vm": {
          const vm = vms.get(cell.vm!);
          if (vm) {
            out.set(cell.key, vmText(t, vm, nameOf, cell.stale, progress.get(vm.id) ?? null));
          }
          break;
        }
        case "ghost": {
          const vm = vms.get(cell.vm!);
          if (vm) {
            out.set(
              cell.key,
              t(`${L}.ghost`, { name: vm.name, node: vm.node ? nameOf(vm.node) : "" }),
            );
          }
          break;
        }
        case "more":
          out.set(cell.key, t(`${L}.more`, { count: cell.count, node: ownerName(item) }));
          break;
        case "hidden":
          out.set(cell.key, t(`${L}.hidden`, { count: cell.count, node: ownerName(item) }));
          break;
      }
    }
  }
  const nodeName = (key: string) => scene.byKey.get(key)?.name ?? key;
  for (const link of scene.links) {
    out.set(link.key, linkText(t, link, nodeName));
  }
  return out;
}
