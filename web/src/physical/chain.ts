// 노드 하나의 네트워크 사슬도(계획 75 §4.1 자세히 판 4칸): 물리 포트 → 본딩 → VLAN → 브리지 → underlay → 네트워크. 서버의
// `NodePhysical`(포트·브리지·underlay·영향)과 레일(네트워크)로 칸과 선을 만들고, 장애 포트(`impact`)에서 앞으로 닿는 칸·선에
// 심각도를 붙인다. VM 경로(NIC → 네트워크 → 브리지 → 포트)도 같은 그래프를 거꾸로 걷는다. 그리기와 떨어진 순수 함수다.
import type { NetRail, NodePhysical, PhysicalPort } from "@/physical/api";

export type LinkHealth = "up" | "degraded" | "down";
/** 칸의 상태: 레일이 이 노드에 없으면 `absent`, 모르면(노드 오프라인·underlay 주인을 못 찾음) `unknown` */
export type ChainHealth = LinkHealth | "absent" | "unknown";
export type Fault = "down" | "degraded";
export type ChainKind = "ethernet" | "bond" | "vlan" | "bridge" | "underlay" | "network";

/** 칸의 열(빈 열은 그리는 쪽이 접는다) */
export const CHAIN_COLUMNS: readonly ChainKind[] = [
  "ethernet",
  "bond",
  "vlan",
  "bridge",
  "underlay",
  "network",
];

export interface ChainItem {
  /** `i:<인터페이스>`, `u:underlay`, `n:<네트워크 id>` */
  id: string;
  kind: ChainKind;
  label: string;
  /** 이 노드에서 본 상태 */
  health: ChainHealth;
  /** 장애 포트에서 닿으면 그 심각도(둘이면 `down`) */
  fault?: Fault;
  /** 포트(이더넷·그 밖의 물리 장치·본딩·VLAN)면 서버 값 */
  port?: PhysicalPort;
  /** 네트워크면 이 노드에서 NIC가 붙은 VM 수, 브리지면 `vm_nics` */
  vms?: number;
  /** VLAN id·physnet 이름 같은 짧은 덧말 */
  note?: string;
}

export interface ChainEdge {
  from: string;
  to: string;
  fault?: Fault;
}

export interface Chain {
  items: ChainItem[];
  edges: ChainEdge[];
  /** 보이지 않는 쓰지 않는 이더넷 포트 수 */
  unused: number;
}

const UNDERLAY = "u:underlay";
const OVERLAY_KINDS: readonly NetRail["kind"][] = ["vxlan", "sdn-overlay"];

const iface = (name: string) => `i:${name}`;

function worse(a: Fault | undefined, b: Fault): Fault {
  return a === "down" || b === "down" ? "down" : "degraded";
}

/**
 * 노드의 사슬. `rails`는 클러스터의 네트워크(이 노드에 쓸 수단이 없는 것은 빠진다), `vmsByNetwork`는 이 노드에서 그 네트워크에
 * NIC가 있는 VM 수(네트워크 id → 수, 없으면 칸에 수를 쓰지 않는다).
 */
export function buildChain(
  node: NodePhysical,
  rails: readonly NetRail[],
  vmsByNetwork?: ReadonlyMap<string, number>,
): Chain {
  const items: ChainItem[] = [];
  const edges: ChainEdge[] = [];
  const has = new Set<string>();
  const add = (item: ChainItem) => {
    if (!has.has(item.id)) {
      has.add(item.id);
      items.push(item);
    }
  };
  let unused = 0;
  for (const p of node.ports) {
    // 서버의 `other`(dummy·team 등)는 물리 포트처럼 이더넷 열에 둔다(지도 포트 줄과 같게). 어디에도 묶이지 않은 가상
    // 장치(`ovs-system` 등 — 역할 `other`)는 그리지 않는다.
    if (p.kind === "bridge" || (p.kind === "other" && p.role === "other")) {
      continue;
    }
    if (p.role === "unused") {
      unused += 1;
      continue;
    }
    add({
      id: iface(p.name),
      kind: p.kind === "other" ? "ethernet" : p.kind,
      label: p.name,
      health: p.health,
      port: p,
      note: p.kind === "vlan" && p.vlan_id != null ? `VLAN ${p.vlan_id}` : undefined,
    });
  }
  for (const b of node.bridges) {
    add({ id: iface(b.name), kind: "bridge", label: b.name, health: b.health, vms: b.vm_nics });
  }
  const onNode = (rail: NetRail) => rail.nodes.find((n) => n.node === node.node)?.state ?? "absent";
  if (node.underlay) {
    const overlays = rails.filter((r) => OVERLAY_KINDS.includes(r.kind));
    const holder = node.underlay.port ? iface(node.underlay.port) : undefined;
    // 주인을 찾았으면 그 상태, 이름만 남았는데 목록에 없으면(사라짐) 끊김, 이름도 없으면 모름(정상으로 보이지 않게)
    add({
      id: UNDERLAY,
      kind: "underlay",
      label: node.underlay.address,
      health: holder ? (has.has(holder) ? healthOf(items, holder) : "down") : "unknown",
      note: node.underlay.port,
    });
    for (const r of overlays) {
      if (onNode(r) !== "absent") {
        edges.push({ from: UNDERLAY, to: `n:${r.id}` });
      }
    }
    if (holder && has.has(holder)) {
      edges.push({ from: holder, to: UNDERLAY });
    }
  }
  for (const p of node.ports) {
    const from = iface(p.name);
    if (!has.has(from)) {
      continue;
    }
    if (p.master && has.has(iface(p.master))) {
      edges.push({ from, to: iface(p.master) });
    }
    if (p.kind === "vlan" && p.parent && has.has(iface(p.parent))) {
      edges.push({ from: iface(p.parent), to: from });
    }
  }
  for (const b of node.bridges) {
    for (const id of b.networks) {
      edges.push({ from: iface(b.name), to: `n:${id}` });
    }
  }
  // 네트워크 칸: 선이 닿는 것만(레일 순 — 이름 순)
  const wanted = new Set(edges.filter((e) => e.to.startsWith("n:")).map((e) => e.to));
  for (const r of rails) {
    const id = `n:${r.id}`;
    if (!wanted.has(id)) {
      continue;
    }
    const notes = [
      r.vlan != null ? `VLAN ${r.vlan}` : undefined,
      r.kind === "sdn-provider" ? `physnet ${r.carrier}` : undefined,
    ].filter((x) => x !== undefined);
    add({
      id,
      kind: "network",
      label: r.name,
      health: onNode(r),
      vms: vmsByNetwork?.get(r.id),
      note: notes.length > 0 ? notes.join(" · ") : undefined,
    });
  }
  // 모르는 네트워크로 가는 선(레일에 없음)은 버린다.
  const kept = edges.filter((e) => has.has(e.from) && has.has(e.to));
  markFaults(node, items, kept);
  return { items, edges: kept, unused };
}

function healthOf(items: readonly ChainItem[], id: string): ChainHealth {
  const h = items.find((i) => i.id === id)?.health;
  return h === "absent" || h === undefined ? "up" : h;
}

/** 장애마다 그 포트(없으면 본딩)에서 앞으로 걸어 닿는 칸·선에 심각도를 붙인다. */
function markFaults(node: NodePhysical, items: ChainItem[], edges: ChainEdge[]) {
  const byId = new Map(items.map((i) => [i.id, i]));
  const out = new Map<string, ChainEdge[]>();
  for (const e of edges) {
    out.set(e.from, [...(out.get(e.from) ?? []), e]);
  }
  for (const impact of node.impact ?? []) {
    const start = iface(impact.port);
    const seen = new Set<string>();
    const stack = [start];
    while (stack.length > 0) {
      const id = stack.pop()!;
      if (seen.has(id)) {
        continue;
      }
      seen.add(id);
      const item = byId.get(id);
      if (item) {
        item.fault = worse(item.fault, impact.severity);
      }
      for (const e of out.get(id) ?? []) {
        e.fault = worse(e.fault, impact.severity);
        stack.push(e.to);
      }
    }
  }
}

/** 칸 `id` 아래(뒤로 걸어)의 물리 포트들(이더넷 열 — 서버 `other` 포함) — VM 경로의 "→ 포트". 이름 순. */
export function lowerPorts(chain: Chain, id: string): ChainItem[] {
  const into = new Map<string, string[]>();
  for (const e of chain.edges) {
    into.set(e.to, [...(into.get(e.to) ?? []), e.from]);
  }
  const byId = new Map(chain.items.map((i) => [i.id, i]));
  const seen = new Set<string>();
  const found: ChainItem[] = [];
  const stack = [id];
  while (stack.length > 0) {
    const cur = stack.pop()!;
    if (seen.has(cur)) {
      continue;
    }
    seen.add(cur);
    const item = byId.get(cur);
    if (item?.kind === "ethernet") {
      found.push(item);
    }
    stack.push(...(into.get(cur) ?? []));
  }
  return found.sort((a, b) => a.label.localeCompare(b.label));
}

/** 네트워크 칸 바로 앞의 운반자(브리지 또는 underlay) */
export function carrierOf(chain: Chain, networkId: string): ChainItem | undefined {
  const from = chain.edges.find((e) => e.to === `n:${networkId}`)?.from;
  return chain.items.find((i) => i.id === from);
}

/** 장애 하나: 포트·본딩 장애, 또는 이 노드에 없는 네트워크에 붙은 VM NIC(`network`) */
export interface FaultEntry {
  id: string;
  node: string;
  /** 포트 장애면 보이는 이름: 포트, 본딩 구성 포트의 장애는 본딩 하나로 접는다(리뷰 2 — 경보와 같은 수) */
  port?: string;
  /** 고를 포트(지도 포트 줄에 있는 것 — 본딩이면 끊긴 첫 구성 포트, 브리지면 없음 → 노드를 고름) */
  focus?: string;
  /** 본딩으로 접었으면 장애가 난 구성 포트(서버 순서) */
  members: { port: string; missing: boolean }[];
  /** 노드에서 사라진 포트(기억 포트) */
  missing: boolean;
  /** 주소를 가진 포트의 쓰임·주소(VM 수 대신 보인다 — 스토리지·클러스터 링크망) */
  uses: string[];
  addresses: string[];
  /** 노드에 없는 네트워크면 그 id */
  network?: string;
  severity: Fault;
  vms: number;
  /** 오프라인 노드의 마지막으로 본 값(배지 수에 넣지 않는다) */
  stale: boolean;
  /** 유지보수 노드(경보는 조용하다 — 배지·목록에 표시) */
  maintenance: boolean;
}

/**
 * 클러스터의 장애(노드 이름 순, 노드 안은 서버 순서 — 포트 장애 다음 없는 네트워크) — "물리 장애 n" 배지와 요약. 오프라인
 * 노드의 것은 `stale`(마지막으로 본 값)이다. 같은 본딩의 구성 포트 장애는 하나로 접는다. `maintenance`는 유지보수 노드 id.
 */
export function faultList(
  nodes: readonly NodePhysical[],
  maintenance: ReadonlySet<string> = new Set(),
): FaultEntry[] {
  return nodes.flatMap((n) => {
    const base = { id: n.id, node: n.node, stale: !n.online, maintenance: maintenance.has(n.id) };
    const ports = new Map(n.ports.map((p) => [p.name, p]));
    const out: FaultEntry[] = [];
    for (const i of n.impact ?? []) {
      const p = ports.get(i.port);
      const member = { port: i.port, missing: i.missing === true };
      const folded = i.bond ? out.find((e) => e.port === i.bond) : undefined;
      if (folded) {
        folded.members.push(member);
        folded.severity = worse(folded.severity, i.severity);
        folded.missing ||= member.missing;
        continue;
      }
      out.push({
        ...base,
        port: i.bond ?? i.port,
        focus: p?.kind === "bridge" ? undefined : i.port,
        members: i.bond ? [member] : [],
        missing: member.missing,
        uses: i.uses ?? [],
        addresses: p?.addresses ?? [],
        severity: i.severity,
        vms: i.vms,
      });
    }
    for (const u of n.unreachable ?? []) {
      out.push({
        ...base,
        network: u.network,
        members: [],
        missing: false,
        uses: [],
        addresses: [],
        severity: "down",
        vms: u.vms,
      });
    }
    return out;
  });
}
