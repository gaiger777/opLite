// 클러스터 지도 시험의 자료(계획 70) — 순수 시험과 성능 시험이 함께 쓴다.
import type { Node, NodeLoadView, PlacementStatus, StoragePool, Vm } from "@/api/types";
import { NODE, vm as baseVm } from "@/test/fixtures";

export const nodeId = (n: number) => `019a0f3c-0000-7000-8a00-${String(n).padStart(12, "0")}`;
export const vmId = (n: number) => `019a0f3c-0000-7000-8b00-${String(n).padStart(12, "0")}`;

/** 노드 하나(이름 `name`, id `nodeId(n)`) */
export function mapNode(
  n: number,
  name: string,
  extra: {
    labels?: Record<string, string>;
    spec?: Partial<Node["spec"]>;
    status?: Partial<Node["status"]>;
  } = {},
): Node {
  return {
    ...NODE,
    id: nodeId(n),
    name,
    labels: extra.labels ?? {},
    spec: { ...NODE.spec, ...extra.spec },
    status: { ...NODE.status, ...extra.status },
  };
}

/** VM 하나: 노드 `node`(id)에 고정, `phase` 상태 */
export function mapVm(
  n: number,
  name: string,
  node: string | null,
  phase: Vm["status"]["phase"] = "running",
  extra: { spec?: Partial<Vm["spec"]>; status?: Partial<Vm["status"]> } = {},
): Vm {
  const base = baseVm(name, phase);
  return {
    ...base,
    id: vmId(n),
    spec: { ...base.spec, node, ...extra.spec },
    status: {
      ...base.status,
      node: phase === "running" || phase === "migrating" ? node : null,
      ...extra.status,
    },
  };
}

export function load(node: Node, extra: Partial<NodeLoadView> = {}): NodeLoadView {
  return {
    node: node.id,
    name: node.name,
    cpu_use_pct: 300,
    mem_use_pct: 400,
    mem_allocated_mib: 4096,
    mem_total_mib: 16384,
    vms: 0,
    age_secs: 2,
    ...extra,
  };
}

export function placement(
  nodes: NodeLoadView[],
  extra: Partial<PlacementStatus> = {},
): PlacementStatus {
  return {
    leader: nodes[0]?.node ?? "",
    settings: {} as PlacementStatus["settings"],
    nodes,
    recommendations: [],
    ...extra,
  };
}

/** 스토리지 풀 하나(`shared`면 공유 풀) */
export function mapPool(n: number, name: string, shared = false): StoragePool {
  return {
    id: `019a0f3c-0000-7000-8c00-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: { backend: { kind: "dir", path: `/srv/${name}` }, shared },
    status: { available_bytes: null, total_bytes: null },
  };
}

/** 노드 하나의 레이어 자료: 로컬 풀 `local`(사용률 `usedPct`)·공유 풀 `nfs`, 업링크 `eth0`, 펜싱(watchdog) */
export function layerStatus(usedPct = 40): Partial<Node["status"]> {
  const total = 1000 * 2 ** 30;
  return {
    version: "1.0.0",
    pools: {
      local: {
        available_bytes: (total * (100 - usedPct)) / 100,
        total_bytes: total,
        ready: true,
        error: null,
        pool_id: mapPool(1, "local").id,
      },
      nfs: { available_bytes: total, total_bytes: total, ready: true, error: null, pool_id: "" },
    },
    interfaces: [
      { name: "eth0", kind: "ethernet", state: "up", carrier: true, master: "br0" },
      { name: "br0", kind: "bridge", state: "up", addresses: ["10.0.0.5/24"] },
    ],
    ha: { fencing: "watchdog", recover_after_secs: 60 },
  };
}

/** 결정적 의사 난수(mulberry32) — 성질 시험의 무작위 클러스터 */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const PHASES: Vm["status"]["phase"][] = [
  "running",
  "running",
  "running",
  "stopped",
  "failed",
  "starting",
];

/**
 * 큰 클러스터: 노드 `nodes`개를 zone `zones`개에 나누고(랙 둘씩), 노드마다 VM `perNode`개(대부분 켜짐, 몇은 꺼짐·실패·
 * 시작 중), 부하 요약 모두.
 */
export function bigCluster(nodes = 100, perNode = 50, zones = 10) {
  const ns: Node[] = [];
  const vms: Vm[] = [];
  const loads: NodeLoadView[] = [];
  for (let i = 0; i < nodes; i++) {
    const zone = i % zones;
    const node = mapNode(i, `n${i}`, {
      labels: { zone: `z${zone}`, rack: `z${zone}-r${Math.floor(i / zones) % 2}` },
      status: layerStatus((i * 29) % 100),
    });
    ns.push(node);
    let running = 0;
    for (let j = 0; j < perNode; j++) {
      const k = i * perNode + j;
      const phase = PHASES[(k * 7) % PHASES.length]!;
      running += phase === "running" ? 1 : 0;
      vms.push(mapVm(k, `vm${k}`, node.id, phase));
    }
    loads.push(
      load(node, { cpu_use_pct: (i * 37) % 1000, mem_use_pct: (i * 53) % 1000, vms: running }),
    );
  }
  return {
    nodes: ns,
    vms,
    placement: placement(loads),
    pools: [mapPool(1, "local"), mapPool(2, "nfs", true)],
  };
}

/** 무작위 클러스터(성질 시험): 노드 1~30, 노드마다 VM 0~70, 레이블 zone·rack 일부, 증인·오프라인·배치 안 됨 섞임 */
export function randomCluster(seed: number) {
  const r = rng(seed);
  const pick = <T>(xs: readonly T[]) => xs[Math.floor(r() * xs.length)]!;
  const count = 1 + Math.floor(r() * 30);
  const nodes: Node[] = [];
  const vms: Vm[] = [];
  let v = 0;
  for (let i = 0; i < count; i++) {
    const labels: Record<string, string> = {};
    if (r() < 0.7) {
      labels.zone = pick(["a", "b", "c", "d10", "d2"]);
    }
    if (r() < 0.5) {
      labels.rack = pick(["r1", "r2", "r3"]);
    }
    const witness = r() < 0.08;
    const node = mapNode(i, `node-${Math.floor(r() * 1000)}-${i}`, {
      labels,
      spec: { witness },
      status: { online: r() > 0.1 },
    });
    nodes.push(node);
    const n = witness ? 0 : Math.floor(r() * 71);
    for (let j = 0; j < n; j++) {
      vms.push(mapVm(v, `vm-${Math.floor(r() * 10000)}-${v}`, node.id, pick(PHASES)));
      v++;
    }
  }
  for (let j = 0; j < Math.floor(r() * 12); j++) {
    vms.push(mapVm(v, `loose-${v}`, null, "stopped"));
    v++;
  }
  return { nodes, vms };
}
