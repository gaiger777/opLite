// 화면 읽기 이름(계획 70 §10.3): 장면의 노드·타일·화살표마다 한 번 셈한다. 가리키기 카드와 `aria-live` 줄이 같은 글을
// 쓴다. 모델의 VM·노드를 한 번씩만 훑는다(원소마다 목록을 훑지 않음).
import type {
  MapLink,
  MapScene,
  NodeItem,
  PortItem,
  RailItem,
  SlabItem,
} from "@/clusterMap/layout";
import type { ClusterModel, MapNode, MapVm } from "@/clusterMap/model";
import { migrationStageText } from "@/tasks/tasks";

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
  if (n.net && n.net.faults > 0) {
    parts.push(t(`${L}.faults`, { count: n.net.faults }));
  }
  return parts.join(", ");
}

/** 레일 이름: 브리지·physnet 이름, VXLAN·오버레이는 "underlay" */
export function railName(t: Translate, rail: { carrier: string; kind: string }): string {
  return rail.carrier === "underlay" && (rail.kind === "vxlan" || rail.kind === "sdn-overlay")
    ? t("clusterMap.net.underlay")
    : rail.carrier;
}

/** 속도(Mb/s) → "10 Gb/s"·"100 Mb/s" */
export function speedText(mbps: number | undefined | null): string | null {
  if (!mbps || mbps <= 0) {
    return null;
  }
  return mbps >= 1000 ? `${Math.round(mbps / 100) / 10} Gb/s` : `${mbps} Mb/s`;
}

/** "포트 eno1, 노드 n1, 끊김, 본딩 bond0, 10 Gb/s, 영향: 네트워크 2개, VM 12개" */
function portText(t: Translate, item: PortItem, n: MapNode): string {
  const raw = n.net?.raw.ports.find((p) => p.name === item.name);
  const parts = [t(`${L}.port`, { name: item.name, node: n.name }), t(`${L}.link.${item.tone}`)];
  if (item.bond) {
    parts.push(t(`${L}.bond`, { name: item.bond }));
  }
  const speed = speedText(raw?.speed_mbps);
  if (speed) {
    parts.push(speed);
  }
  if (raw?.lldp?.chassis_name || raw?.lldp?.port_id) {
    parts.push(
      t(`${L}.lldp`, {
        switch: raw.lldp.chassis_name ?? raw.lldp.chassis_id ?? "?",
        port: raw.lldp.port_id ?? "?",
      }),
    );
  }
  const impact =
    n.net?.raw.impact?.find((i) => i.port === item.name) ??
    (item.bond ? n.net?.raw.impact?.find((i) => i.port === item.bond) : undefined);
  if (impact) {
    parts.push(
      t(`${L}.impact.${impact.severity}`, { networks: impact.networks.length, vms: impact.vms }),
    );
  }
  return parts.join(", ");
}

/** "레일 vmbr0, 네트워크 3개, 노드 4개, 끊김 1" */
function railText(t: Translate, rail: RailItem): string {
  const parts = [
    t(`${L}.rail`, { name: railName(t, rail) }),
    t(`${L}.networks`, { count: rail.networks }),
    t(`${L}.railNodes`, { count: rail.nodes }),
  ];
  if (rail.down > 0) {
    parts.push(t(`${L}.railDown`, { count: rail.down }));
  }
  if (rail.degraded > 0) {
    parts.push(t(`${L}.railDegraded`, { count: rail.degraded }));
  }
  if (rail.unknown > 0) {
    parts.push(t(`${L}.railUnknown`, { count: rail.unknown }));
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

/** 화살표의 글: "web → n3 · 62%"(디스크 복사)·"web → n3 · 메모리 옮기는 중", 추천은 "추천: web n1 → n3" */
export function linkText(t: Translate, link: MapLink, nodeName: (key: string) => string): string {
  const from = nodeName(link.ends[0]);
  const to = nodeName(link.ends[1]);
  if (link.kind === "recommendation") {
    return t(`${L}.recommendation`, { vm: link.vmName, from, to });
  }
  if (link.progress !== null) {
    return t(`${L}.migrationPct`, { vm: link.vmName, to, progress: link.progress });
  }
  const stage = link.stage ? migrationStageText(link.stage, t) : null;
  return stage
    ? t(`${L}.migrationStage`, { vm: link.vmName, to, stage })
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
        case "fold":
          out.set(
            cell.key,
            t(`${L}.${cell.open ? "unfold" : "fold"}`, {
              count: cell.count,
              node: ownerName(item),
            }),
          );
          break;
      }
    }
  }
  const nodeName = (key: string) => scene.byKey.get(key)?.name ?? key;
  for (const link of scene.links) {
    out.set(link.key, linkText(t, link, nodeName));
  }
  for (const port of scene.ports.values()) {
    const n = nodes.get(scene.byKey.get(port.node)?.id ?? "");
    if (n) {
      out.set(port.key, portText(t, port, n));
    }
  }
  for (const rail of scene.rails.values()) {
    out.set(rail.key, railText(t, rail));
  }
  return out;
}
