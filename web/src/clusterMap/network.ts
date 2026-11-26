// 지도의 네트워크 층(계획 75 §2·§4.2): 서버의 물리 관계(`GET /topology/physical`)를 지도가 그릴 꼴로 바꾸는 순수 함수.
// 노드마다 포트 줄(업링크·underlay의 물리 포트 — 본딩 구성 포트는 붙여서), 본딩 묶음, 장애 수, 그리고 레일(네트워크를
// 실어 나르는 것 — 브리지·physnet·underlay — 마다 하나, 그 위 네트워크들)과 레일마다 노드의 상태.
import type { Impact, NetRail, NodePhysical, PhysicalPort, PhysicalTopology } from "@/physical/api";

/** 이름 차례(`model.ts`의 `naturalCompare`와 같은 셈 — 모델이 이 파일을 읽으므로 되읽지 않는다) */
const collator = new Intl.Collator("en", { numeric: true, sensitivity: "variant" });
const naturalCompare = (a: string, b: string) =>
  collator.compare(a, b) || (a < b ? -1 : a > b ? 1 : 0);

export type LinkTone = "up" | "degraded" | "down";
/** 레일 위 노드 상태(`absent`는 그 노드에 그 네트워크를 쓸 수단이 없음 — 선을 긋지 않는다) */
export type RailTone = LinkTone | "absent";

/** 포트 줄의 포트 하나 */
export interface MapPort {
  name: string;
  health: LinkTone;
  role: "uplink" | "underlay";
  /** 이 포트를 묶은 본딩(줄에서 이웃으로 붙고 묶음 테두리를 같이 쓴다) */
  bond: string | null;
  /** 이 포트의 장애(영향) */
  fault: Impact | null;
}

/** 본딩 묶음(구성 포트를 두르는 테두리) */
export interface MapBond {
  name: string;
  health: LinkTone;
  mode: string;
  /** 본딩 자체의 장애(구성 포트에 링크 끊김이 없는 저하·끊김) */
  fault: Impact | null;
}

export interface NodeNet {
  /** 줄 차례(노드가 알린 차례, 본딩 구성 포트는 첫 구성 포트 자리에 모아서) — 배치가 앞 `PORT_MAX`개만 그린다 */
  ports: MapPort[];
  bonds: MapBond[];
  /** 장애 수와 가장 나쁜 무게(블록의 경고 꼬리표) */
  faults: number;
  severity: "down" | "degraded" | null;
  /** 옛 판 노드(일부 정보 없음) */
  legacy: boolean;
  /** 레일 열쇠(`carrier`) → 그 레일로 가는 줄의 포트 이름(아래로 따라간 물리 포트) */
  carriers: Record<string, string[]>;
  /** 카드·서랍이 읽는 서버 자료 그대로 */
  raw: NodePhysical;
}

/** 레일 하나: 같은 것(`carrier`)이 실어 나르는 네트워크들 */
export interface MapRail {
  carrier: string;
  kind: NetRail["kind"];
  networks: { id: string; name: string; vlan: number | null }[];
  /** 노드 이름 → 상태(네트워크들 중 가장 나쁜 것, 모두 없으면 `absent`) */
  states: Map<string, RailTone>;
}

const WORSE: Record<RailTone, number> = { absent: 0, up: 1, degraded: 2, down: 3 };

function worse(a: RailTone, b: RailTone): RailTone {
  return WORSE[b] > WORSE[a] ? b : a;
}

/** 이 인터페이스 아래의 물리 포트(본딩 → 구성 포트, VLAN → 아래 장치, 브리지 → 업링크) */
export function leafPorts(ports: readonly PhysicalPort[], name: string, depth = 0): string[] {
  const p = ports.find((x) => x.name === name);
  if (!p || depth > 6) {
    return [];
  }
  switch (p.kind) {
    case "bond": {
      const members = new Set([
        ...(p.bond?.members.map((m) => m.name) ?? []),
        ...ports.filter((x) => x.master === name).map((x) => x.name),
      ]);
      return [...members].flatMap((m) => leafPorts(ports, m, depth + 1));
    }
    case "vlan":
      return p.parent ? leafPorts(ports, p.parent, depth + 1) : [];
    case "bridge":
      return ports
        .filter((x) => x.master === name)
        .flatMap((x) => leafPorts(ports, x.name, depth + 1));
    default:
      return [name];
  }
}

const isRowPort = (p: PhysicalPort) =>
  (p.role === "uplink" || p.role === "underlay") && (p.kind === "ethernet" || p.kind === "other");

/** 레일의 열쇠가 이 노드에서 닿는 인터페이스: 브리지 이름, physnet의 브리지, `underlay`면 underlay 포트 */
function carrierTop(np: NodePhysical, rail: Pick<NetRail, "kind" | "carrier">): string | null {
  if (rail.kind === "sdn-provider") {
    return np.physnets?.find((p) => p.name === rail.carrier)?.bridge ?? null;
  }
  if (rail.carrier === "underlay" && (rail.kind === "vxlan" || rail.kind === "sdn-overlay")) {
    return np.underlay?.port ?? null;
  }
  return rail.carrier;
}

/** 노드 하나의 네트워크 층 자료 */
export function nodeNet(np: NodePhysical, rails: readonly MapRail[]): NodeNet {
  const impact = np.impact ?? [];
  const faultOf = (name: string) => impact.find((i) => i.port === name) ?? null;
  const tone = (h: PhysicalPort["health"]): LinkTone => h;
  const row = np.ports.filter(isRowPort);
  const ports: MapPort[] = [];
  const seen = new Set<string>();
  for (const p of row) {
    if (seen.has(p.name)) {
      continue;
    }
    const bond = p.master && np.ports.some((x) => x.name === p.master && x.kind === "bond");
    const group = bond ? row.filter((x) => x.master === p.master) : [p];
    for (const g of group) {
      seen.add(g.name);
      ports.push({
        name: g.name,
        health: tone(g.health),
        role: g.role === "underlay" ? "underlay" : "uplink",
        bond: bond ? p.master! : null,
        fault: faultOf(g.name),
      });
    }
  }
  const bonds: MapBond[] = np.ports
    .filter((p) => p.kind === "bond" && ports.some((x) => x.bond === p.name))
    .map((p) => ({
      name: p.name,
      health: tone(p.health),
      mode: p.bond?.mode ?? "",
      fault: faultOf(p.name),
    }));
  const carriers: Record<string, string[]> = {};
  for (const rail of rails) {
    const top = carrierTop(np, rail);
    if (top && carriers[rail.carrier] === undefined) {
      const names = new Set(ports.map((x) => x.name));
      carriers[rail.carrier] = leafPorts(np.ports, top).filter((n) => names.has(n));
    }
  }
  return {
    ports,
    bonds,
    faults: impact.length,
    severity:
      impact.length === 0 ? null : impact.some((i) => i.severity === "down") ? "down" : "degraded",
    legacy: np.legacy === true,
    carriers,
    raw: np,
  };
}

/** 레일: 네트워크를 `carrier`로 묶는다(자연 차례). 노드 상태는 네트워크들 중 가장 나쁜 것. */
export function railsOf(networks: readonly NetRail[]): MapRail[] {
  const by = new Map<string, MapRail>();
  for (const n of networks) {
    let rail = by.get(n.carrier);
    if (!rail) {
      rail = { carrier: n.carrier, kind: n.kind, networks: [], states: new Map() };
      by.set(n.carrier, rail);
    }
    rail.networks.push({ id: n.id, name: n.name, vlan: n.vlan ?? null });
    for (const s of n.nodes) {
      rail.states.set(s.node, worse(rail.states.get(s.node) ?? "absent", s.state));
    }
  }
  const out = [...by.values()].sort((a, b) => naturalCompare(a.carrier, b.carrier));
  for (const r of out) {
    r.networks.sort((a, b) => naturalCompare(a.name, b.name));
  }
  return out;
}

/** 서버 자료 → 노드 id마다 네트워크 층과 레일 */
export function physicalLayer(physical: PhysicalTopology): {
  nodes: Map<string, NodeNet>;
  rails: MapRail[];
} {
  const rails = railsOf(physical.networks);
  return { nodes: new Map(physical.nodes.map((n) => [n.id, nodeNet(n, rails)])), rails };
}
