// 클러스터 지도 배치(계획 70 §6): 순수 함수 `layoutCluster(model, opts) → MapScene`. 노드 레이블(zone·rack·고른 키)
// 값마다 바닥 판, 판 안은 격자(랙이 있으면 랙마다 한 줄), 노드 블록 위에 VM 타일을 이름 순으로 쌓는다. 같은 입력이면 같은
// 그림이고 VM 상태가 바뀌어도 자리는 그대로다(색만 바뀜).
//
// 그리기 차례(§6.3): 판(바닥) 모두 → 노드 "띠"(band) 차례 → 화살표. 띠는 한 판 안에서 `col + row`가 같은 노드다 — 이런
// 노드끼리는 화면에서 겹치지 않으므로(가로로 노드 하나 반 넘게 떨어짐) 띠 하나의 타일을 경로 하나로 합칠 수 있다(`paths.ts`).
import { type Box, boxBounds, iso, type Point, topCenter } from "@/components/iso/iso";
import {
  type ClusterModel,
  heatTone,
  type Heat,
  type MapNode,
  type MapVm,
  naturalCompare,
} from "@/clusterMap/model";

/** 노드 블록 바닥 한 변과 높이(§6.2) */
export const NODE = 120;
export const NODE_H = 44;
/** 노드 사이 간격 */
export const GAP = 64;
export const PITCH = NODE + GAP;
/**
 * VM 타일 한 변·간격·높이. 한 층 안 이웃 타일은 높이가 간격보다 클 때만 화면에서 겹친다(앞 타일의 윗면·옆면이 뒤 타일의
 * 옆면을 가림) — 그러면 층·색·면마다 합친 경로로는 그리는 차례를 맞출 수 없다. 높이 ≤ 간격이면 한 층 안 타일은 서로 겹치지
 * 않아(경계만 닿음) 층 차례만 지키면 된다(§18 스테이지 1 — 설계의 22·4·8에서 20·6·6으로).
 */
export const TILE = 20;
export const TILE_GAP = 6;
export const TILE_H = 6;
export const TILE_STEP = TILE + TILE_GAP;
/** 노드 윗면의 타일 격자: 4 × 4 = 16칸, 많아야 4층 */
export const PER_ROW = 4;
export const PER_LAYER = PER_ROW * PER_ROW;
export const MAX_LAYERS = 4;
export const MAX_CELLS = PER_LAYER * MAX_LAYERS;
const TILE_MARGIN = (NODE - (PER_ROW * TILE + (PER_ROW - 1) * TILE_GAP)) / 2;
/** 판: 노드 격자 둘레 여백·두께, 판 사이 */
export const SLAB_PAD = 40;
export const SLAB_H = 6;
const SLAB_GAP = 96;
/** 앞 줄의 쌓인 층마다 줄 사이를 늘리는 양(앞 줄의 타일이 뒤 줄 블록의 옆면을 덜 가리게) */
export const ROW_EXTRA = 10;
/** 증인 노드 기둥(판 뒤 가장자리) */
const WITNESS = 28;
const WITNESS_H = 30;
const WITNESS_STEP = 48;
/** 증인 기둥 띠 번호를 노드 띠(0부터) 앞에 두는 양 */
const WITNESS_BANDS = 1_000_000;
/** "배치 안 됨" 판: 한 층, 열 8, 많아야 64칸 */
const TRAY_COLS = 8;
const TRAY_PAD = 16;
/** 요약 단계의 상태 막대(노드 윗면 대각선 위 네 자리 — 서로 화면에서 겹치지 않는다) */
const BAR = 16;
const BAR_STEP = 26;
const BAR_MAX = 64;
/** 노드 위 꼬리표가 솟는 높이(범위 셈) */
const TAG_RISE = 30;
/** 한 판 줄의 노드 수 한도(묶지 않을 때 `min(8, ⌈√n⌉)`) */
const MAX_COLS = 8;
/** 판을 가로로 늘어놓는 한도(노드 24개 폭) */
const ROW_LIMIT = 24 * PITCH;

export type GroupBy =
  { kind: "zone" } | { kind: "rack" } | { kind: "label"; key: string } | { kind: "none" };

export interface LayoutOptions {
  group?: GroupBy;
}

export type TileTone =
  "running" | "stopped" | "busy" | "failed" | "migrating" | "more" | "hidden" | "ghost";

export type BarTone = "running" | "busy" | "failed" | "stopped";

export type NodeTag =
  "offline" | "maintenance" | "draining" | "frozen" | "leader" | "alerts" | "overcommit";

export interface TileCell {
  /** `vm:<id>` · `more:<노드 열쇠>` · `hidden:<노드 열쇠>` · `ghost:<VM id>`(옮겨 오는 VM의 미리 보기) */
  key: string;
  kind: "vm" | "more" | "hidden" | "ghost";
  vm: string | null;
  /** 칸 번호(이름 순 자리) — 층 = ⌊index / 칸 수⌋ */
  index: number;
  layer: number;
  col: number;
  row: number;
  box: Box;
  tone: TileTone;
  ha: boolean;
  /** 배치 규칙 어김 */
  warn: boolean;
  /** 노드가 오프라인 — 마지막으로 알려진 자리(빗금) */
  stale: boolean;
  /** "+N"의 N */
  count: number;
}

export interface TileGrid {
  /** 타일 바닥(첫 층 아래) 앞 왼쪽 모서리의 세계 좌표 */
  origin: { x: number; y: number; z: number };
  cols: number;
  perLayer: number;
  layers: number;
  /** 칸 번호 순 */
  cells: TileCell[];
}

export interface Bar {
  tone: BarTone;
  count: number;
  box: Box;
}

export interface Counts {
  running: number;
  stopped: number;
  failed: number;
  busy: number;
}

export interface NodeItem {
  /** `node:<id>`, "배치 안 됨" 판은 `unplaced` */
  key: string;
  kind: "node" | "witness" | "unplaced";
  /** 노드 id(배치 안 됨은 빈 글) */
  id: string;
  name: string;
  /** 판 열쇠 */
  slab: string;
  /** 판 안 줄(증인은 −1)·열 */
  row: number;
  col: number;
  /** 그리기 띠(작은 것부터) */
  band: number;
  box: Box;
  heat: Heat;
  tags: NodeTag[];
  counts: Counts;
  grid: TileGrid | null;
  /** 요약 단계의 상태 막대(수가 있는 것만) */
  bars: Bar[];
  /** 블록 윗면에서 쌓인 타일 높이 */
  stack: number;
}

export interface SlabItem {
  key: string;
  kind: "group" | "nogroup" | "all" | "unplaced";
  /** 값(묶음 이름) — "영역 없음"·클러스터 이름 없음·배치 안 됨은 `null`(그리는 쪽이 번역) */
  name: string | null;
  box: Box;
  /** 판 위 노드 수(증인 포함, 배치 안 됨은 VM 수) */
  count: number;
}

export interface MapLink {
  key: string;
  kind: "migration" | "recommendation";
  vm: string;
  vmName: string;
  /** 두 끝의 노드 열쇠 */
  ends: [string, string];
  from: Point;
  to: Point;
  lift: number;
  progress: number | null;
  stage: string | null;
  task: string | null;
  recommendation: string | null;
}

export interface MapScene {
  slabs: SlabItem[];
  /** 그리기 차례(띠 순, 띠 안 열 순) */
  nodes: NodeItem[];
  links: MapLink[];
  bounds: { minX: number; minY: number; maxX: number; maxY: number };
  bands: number;
  /** 열쇠 → 노드 원소 */
  byKey: Map<string, NodeItem>;
  /** 타일 열쇠 → 그 노드와 칸 */
  tiles: Map<string, { node: NodeItem; cell: TileCell }>;
}

/** VM 상태 → 타일 색 */
export function tileTone(vm: MapVm): TileTone {
  if (vm.migration) {
    return "migrating";
  }
  switch (vm.phase) {
    case "running":
      return "running";
    case "stopped":
      return "stopped";
    case "failed":
      return "failed";
    case "migrating":
      return "migrating";
    default:
      return "busy";
  }
}

const BAR_TONES: readonly BarTone[] = ["running", "busy", "failed", "stopped"];

function barTone(tone: TileTone): BarTone {
  return tone === "migrating"
    ? "busy"
    : tone === "running" || tone === "failed" || tone === "stopped"
      ? tone
      : "busy";
}

interface Bucket {
  key: string;
  kind: SlabItem["kind"];
  name: string | null;
  members: MapNode[];
}

/** 묶기 열쇠의 레이블 키(묶지 않으면 `null`) */
function groupLabel(by: GroupBy): string | null {
  switch (by.kind) {
    case "zone":
      return "zone";
    case "rack":
      return "rack";
    case "label":
      return by.key;
    case "none":
      return null;
  }
}

/** 노드를 판으로 나눈다: 값마다 하나(자연 차례), 값 없는 노드는 맨 끝 "영역 없음". 어느 노드에도 없으면 판 하나. */
function buckets(nodes: readonly MapNode[], by: GroupBy, clusterName: string | null): Bucket[] {
  const label = groupLabel(by);
  if (label === null || !nodes.some((n) => n.labels[label] !== undefined)) {
    return nodes.length > 0
      ? [{ key: "slab:~all", kind: "all", name: clusterName, members: [...nodes] }]
      : [];
  }
  const groups = new Map<string, MapNode[]>();
  const none: MapNode[] = [];
  for (const n of nodes) {
    const v = n.labels[label];
    if (v === undefined) {
      none.push(n);
    } else {
      groups.set(v, [...(groups.get(v) ?? []), n]);
    }
  }
  const out: Bucket[] = [...groups.keys()]
    .sort(naturalCompare)
    .map((v) => ({ key: `slab:=${v}`, kind: "group", name: v, members: groups.get(v)! }));
  if (none.length > 0) {
    out.push({ key: "slab:~none", kind: "nogroup", name: null, members: none });
  }
  return out;
}

/** 판 안의 줄(노드 이름 순). zone으로 묶으면 랙마다 한 줄(랙 이름 순, 랙 없는 노드는 끝 줄들). */
function gridRows(members: readonly MapNode[], by: GroupBy): { rows: MapNode[][]; cols: number } {
  const hosts = members.filter((n) => !n.witness);
  if (hosts.length === 0) {
    return { rows: [], cols: 1 };
  }
  if (by.kind === "zone" && hosts.some((n) => n.labels.rack !== undefined)) {
    const racks = new Map<string, MapNode[]>();
    const loose: MapNode[] = [];
    for (const n of hosts) {
      const r = n.labels.rack;
      if (r === undefined) {
        loose.push(n);
      } else {
        racks.set(r, [...(racks.get(r) ?? []), n]);
      }
    }
    const rows = [...racks.keys()].sort(naturalCompare).map((r) => racks.get(r)!);
    const cols = Math.max(...rows.map((r) => r.length));
    for (let i = 0; i < loose.length; i += cols) {
      rows.push(loose.slice(i, i + cols));
    }
    return { rows, cols };
  }
  const cols = Math.min(MAX_COLS, Math.ceil(Math.sqrt(hosts.length)));
  const rows: MapNode[][] = [];
  for (let i = 0; i < hosts.length; i += cols) {
    rows.push(hosts.slice(i, i + cols));
  }
  return { rows, cols };
}

/** 칸 `index`(층마다 `cols × cols`)의 상자 */
function cellBox(
  grid: TileGrid["origin"],
  cols: number,
  perLayer: number,
  index: number,
): Box & {
  layer: number;
  col: number;
  row: number;
} {
  const layer = Math.floor(index / perLayer);
  const k = index % perLayer;
  const col = k % cols;
  const row = Math.floor(k / cols);
  return {
    x: grid.x + col * TILE_STEP,
    y: grid.y + row * TILE_STEP,
    z: grid.z + layer * TILE_H,
    w: TILE,
    d: TILE,
    h: TILE_H,
    layer,
    col,
    row,
  };
}

interface CellPlan {
  kind: TileCell["kind"];
  vm: MapVm | null;
  count: number;
}

/**
 * 칸 계획: 이름 순 VM, 넘치면 마지막 칸 앞까지 VM과 "+N", 읽을 수 없는 VM이 있으면 "+N 볼 수 없음", 남는 칸에 옮겨 오는
 * VM의 유령 타일. 앞의 것의 자리는 뒤의 것이 바뀌어도 그대로다.
 */
function planCells(
  vms: readonly MapVm[],
  hidden: number,
  incoming: readonly MapVm[],
  max: number,
): CellPlan[] {
  const extra = hidden > 0 ? 1 : 0;
  const fits = vms.length + extra <= max;
  const shown = fits ? vms.length : max - 1 - extra;
  const plan: CellPlan[] = vms.slice(0, shown).map((vm) => ({ kind: "vm", vm, count: 1 }));
  if (!fits) {
    plan.push({ kind: "more", vm: null, count: vms.length - shown });
  }
  if (hidden > 0) {
    plan.push({ kind: "hidden", vm: null, count: hidden });
  }
  for (const vm of incoming) {
    if (plan.length >= max) {
      break;
    }
    plan.push({ kind: "ghost", vm, count: 1 });
  }
  return plan;
}

function makeGrid(
  owner: string,
  origin: TileGrid["origin"],
  cols: number,
  perLayer: number,
  plan: readonly CellPlan[],
  stale: boolean,
): TileGrid {
  const cells = plan.map((p, index): TileCell => {
    const { layer, col, row, ...box } = cellBox(origin, cols, perLayer, index);
    const vm = p.vm;
    return {
      key:
        p.kind === "vm"
          ? `vm:${vm!.id}`
          : p.kind === "ghost"
            ? `ghost:${vm!.id}`
            : `${p.kind}:${owner}`,
      kind: p.kind,
      vm: vm?.id ?? null,
      index,
      layer,
      col,
      row,
      box,
      tone: p.kind === "vm" ? tileTone(vm!) : p.kind,
      ha: p.kind === "vm" && vm!.ha,
      warn: p.kind === "vm" && vm!.violations > 0,
      stale,
      count: p.count,
    };
  });
  return { origin, cols, perLayer, layers: Math.ceil(cells.length / perLayer), cells };
}

function counts(vms: readonly MapVm[]): Counts {
  const c: Counts = { running: 0, stopped: 0, failed: 0, busy: 0 };
  for (const vm of vms) {
    c[barTone(tileTone(vm))] += 1;
  }
  return c;
}

/** 요약 단계 막대: 윗면의 뒤 왼쪽 → 앞 오른쪽 대각선 위 네 자리(상태마다 자리 고정) */
function bars(box: Box, c: Counts): Bar[] {
  const out: Bar[] = [];
  BAR_TONES.forEach((tone, i) => {
    const count = c[tone];
    if (count === 0) {
      return;
    }
    out.push({
      tone,
      count,
      box: {
        x: box.x + 14 + i * BAR_STEP,
        y: box.y + box.d - 14 - BAR - i * BAR_STEP,
        z: box.z + box.h,
        w: BAR,
        d: BAR,
        h: 4 + Math.min(count, BAR_MAX),
      },
    });
  });
  return out;
}

function nodeTags(n: MapNode): NodeTag[] {
  const tags: NodeTag[] = [];
  if (!n.online) {
    tags.push("offline");
  }
  if (n.frozen) {
    tags.push("frozen");
  }
  if (n.maintenance) {
    tags.push("maintenance");
  }
  if (n.maintenance && n.outgoing > 0) {
    tags.push("draining");
  }
  if (n.leader) {
    tags.push("leader");
  }
  if (n.alerts > 0) {
    tags.push("alerts");
  }
  if (n.allocatedPct !== null && n.allocatedPct > 100) {
    tags.push("overcommit");
  }
  return tags;
}

/** 모델 → 장면 */
export function layoutCluster(model: ClusterModel, opts: LayoutOptions = {}): MapScene {
  const by = opts.group ?? { kind: "zone" };
  const onNode = new Map<string, MapVm[]>();
  const incoming = new Map<string, MapVm[]>();
  const unplaced: MapVm[] = [];
  for (const vm of model.vms) {
    if (vm.node === null) {
      unplaced.push(vm);
      continue;
    }
    onNode.set(vm.node, [...(onNode.get(vm.node) ?? []), vm]);
    if (vm.migration) {
      incoming.set(vm.migration.to, [...(incoming.get(vm.migration.to) ?? []), vm]);
    }
  }

  // 노드마다 칸 계획(층 수가 줄 간격을 정한다)
  const plans = new Map<string, CellPlan[]>();
  for (const n of model.nodes) {
    if (!n.witness) {
      plans.set(
        n.id,
        planCells(onNode.get(n.id) ?? [], n.hiddenVms, incoming.get(n.id) ?? [], MAX_CELLS),
      );
    }
  }
  const layersOf = (n: MapNode) => Math.ceil((plans.get(n.id)?.length ?? 0) / PER_LAYER);

  // 판 크기와 노드 자리(판 안 좌표)
  interface Placed {
    node: MapNode;
    row: number;
    col: number;
    x: number;
    y: number;
  }
  const laid = buckets(model.nodes, by, model.name).map((b) => {
    const { rows, cols } = gridRows(b.members, by);
    const witnesses = b.members.filter((n) => n.witness);
    const placed: Placed[] = [];
    let y = SLAB_PAD;
    rows.forEach((row, r) => {
      if (r > 0) {
        y += PITCH + ROW_EXTRA * Math.max(0, ...row.map(layersOf));
      }
      row.forEach((node, c) => placed.push({ node, row: r, col: c, x: SLAB_PAD + c * PITCH, y }));
    });
    witnesses.forEach((node, i) =>
      placed.push({ node, row: -1, col: i, x: SLAB_PAD + i * WITNESS_STEP, y: 6 }),
    );
    const w = Math.max(
      SLAB_PAD * 2 + cols * NODE + (cols - 1) * GAP,
      SLAB_PAD * 2 + witnesses.length * WITNESS_STEP - (WITNESS_STEP - WITNESS),
    );
    const d = rows.length > 0 ? y + NODE + SLAB_PAD : SLAB_PAD + WITNESS;
    return { bucket: b, placed, w, d };
  });

  // 판 늘어놓기: 가로로, 한도(판 셋 또는 노드 24개 폭 중 작은 것, 가장 넓은 판보다는 크게)를 넘으면 다음 줄
  const widest = Math.max(0, ...laid.map((l) => l.w));
  const limit = Math.max(widest, Math.min(3 * widest + 2 * SLAB_GAP, ROW_LIMIT));
  const origins: { x: number; y: number }[] = [];
  let cx = 0;
  let cy = 0;
  let rowDepth = 0;
  for (const l of laid) {
    if (cx > 0 && cx + l.w > limit) {
      cx = 0;
      cy += rowDepth + SLAB_GAP;
      rowDepth = 0;
    }
    origins.push({ x: cx, y: cy });
    cx += l.w + SLAB_GAP;
    rowDepth = Math.max(rowDepth, l.d);
  }
  const frontY = laid.length > 0 ? cy + rowDepth + SLAB_GAP : 0;

  const slabs: SlabItem[] = [];
  const nodes: NodeItem[] = [];
  let band = 0;
  laid.forEach((l, i) => {
    const o = origins[i]!;
    slabs.push({
      key: l.bucket.key,
      kind: l.bucket.kind,
      name: l.bucket.name,
      box: { x: o.x, y: o.y, z: 0, w: l.w, d: l.d, h: SLAB_H },
      count: l.bucket.members.length,
    });
    // 띠: 증인 기둥(x로 늘어서 화면에서 이웃끼리 겹치므로 하나씩, 왼쪽부터) → col + row 차례
    const diag = (p: Placed) => (p.row < 0 ? p.col - WITNESS_BANDS : p.row + p.col);
    const order = [...l.placed].sort((a, b) => diag(a) - diag(b) || a.col - b.col);
    let last: number | null = null;
    for (const p of order) {
      if (last !== null && diag(p) !== last) {
        band += 1;
      }
      last = diag(p);
      const n = p.node;
      const box: Box = p.node.witness
        ? { x: o.x + p.x, y: o.y + p.y, z: SLAB_H, w: WITNESS, d: WITNESS, h: WITNESS_H }
        : { x: o.x + p.x, y: o.y + p.y, z: SLAB_H, w: NODE, d: NODE, h: NODE_H };
      const key = `node:${n.id}`;
      const grid = n.witness
        ? null
        : makeGrid(
            key,
            { x: box.x + TILE_MARGIN, y: box.y + TILE_MARGIN, z: box.z + box.h },
            PER_ROW,
            PER_LAYER,
            plans.get(n.id)!,
            !n.online,
          );
      const c = counts(onNode.get(n.id) ?? []);
      nodes.push({
        key,
        kind: n.witness ? "witness" : "node",
        id: n.id,
        name: n.name,
        slab: l.bucket.key,
        row: p.row,
        col: p.col,
        band,
        box,
        heat: n.witness ? "unknown" : heatTone(n.load),
        tags: nodeTags(n),
        counts: c,
        grid,
        bars: n.witness ? [] : bars(box, c),
        stack: (grid?.layers ?? 0) * TILE_H,
      });
    }
    if (order.length > 0) {
      band += 1;
    }
  });

  if (unplaced.length > 0) {
    const plan = planCells(unplaced, 0, [], MAX_CELLS);
    const cols = Math.min(TRAY_COLS, plan.length);
    const rows = Math.ceil(plan.length / cols);
    const box: Box = {
      x: 0,
      y: frontY,
      z: 0,
      w: TRAY_PAD * 2 + cols * TILE_STEP - TILE_GAP,
      d: TRAY_PAD * 2 + rows * TILE_STEP - TILE_GAP,
      h: SLAB_H,
    };
    slabs.push({
      key: "slab:~unplaced",
      kind: "unplaced",
      name: null,
      box,
      count: unplaced.length,
    });
    // 한 층(칸 수 = 층 하나의 칸 수), 열 `cols`
    const grid = makeGrid(
      "unplaced",
      { x: box.x + TRAY_PAD, y: box.y + TRAY_PAD, z: SLAB_H },
      cols,
      plan.length,
      plan,
      false,
    );
    nodes.push({
      key: "unplaced",
      kind: "unplaced",
      id: "",
      name: "",
      slab: "slab:~unplaced",
      row: 0,
      col: 0,
      band,
      box,
      heat: "unknown",
      tags: [],
      counts: counts(unplaced),
      grid,
      bars: [],
      stack: TILE_H,
    });
    band += 1;
  }

  const byKey = new Map(nodes.map((n) => [n.key, n]));
  const tiles = new Map<string, { node: NodeItem; cell: TileCell }>();
  for (const n of nodes) {
    for (const cell of n.grid?.cells ?? []) {
      tiles.set(cell.key, { node: n, cell });
    }
  }

  const links = migrationLinks(model, byKey);
  return {
    slabs,
    nodes,
    links,
    bounds: sceneBounds(slabs, nodes, links),
    bands: band,
    byKey,
    tiles,
  };
}

/** 노드 위(쌓인 타일 꼭대기)의 가운데 — 화살표의 끝 */
export function nodeTop(n: NodeItem): Point {
  return topCenter({ ...n.box, h: n.box.h + n.stack });
}

/**
 * 마이그레이션·추천 화살표(§8): 원본 노드 위 가운데 → 대상 노드 위 가운데의 휜 선, 높이는 두 점 거리의 0.3배. 모델이 이미
 * 같은 VM에 하나만 두었다(진행 중이 이김).
 */
export function migrationLinks(
  model: ClusterModel,
  byKey: ReadonlyMap<string, NodeItem>,
): MapLink[] {
  const out: MapLink[] = [];
  for (const m of model.moves) {
    const a = byKey.get(`node:${m.from}`);
    const b = byKey.get(`node:${m.to}`);
    if (!a || !b || a === b) {
      continue;
    }
    const from = nodeTop(a);
    const to = nodeTop(b);
    out.push({
      key: m.key,
      kind: m.kind,
      vm: m.vm,
      vmName: m.vmName,
      ends: [a.key, b.key],
      from,
      to,
      lift: Math.round(Math.hypot(to.x - from.x, to.y - from.y) * 0.3),
      progress: m.progress,
      stage: m.stage,
      task: m.task,
      recommendation: m.recommendation,
    });
  }
  return out;
}

function sceneBounds(
  slabs: readonly SlabItem[],
  nodes: readonly NodeItem[],
  links: readonly MapLink[],
): MapScene["bounds"] {
  const b = { minX: Infinity, minY: Infinity, maxX: -Infinity, maxY: -Infinity };
  const add = (r: { minX: number; minY: number; maxX: number; maxY: number }) => {
    b.minX = Math.min(b.minX, r.minX);
    b.minY = Math.min(b.minY, r.minY);
    b.maxX = Math.max(b.maxX, r.maxX);
    b.maxY = Math.max(b.maxY, r.maxY);
  };
  for (const s of slabs) {
    add(boxBounds(s.box));
  }
  for (const n of nodes) {
    const tall = Math.max(n.stack, ...n.bars.map((bar) => bar.box.h));
    add(boxBounds({ ...n.box, h: n.box.h + tall + TAG_RISE }));
  }
  for (const l of links) {
    // 2차 곡선의 꼭대기는 조절점의 반 높이
    const top = Math.min(l.from.y, l.to.y, (l.from.y + l.to.y) / 2 - l.lift / 2);
    add({
      minX: Math.min(l.from.x, l.to.x),
      minY: top,
      maxX: Math.max(l.from.x, l.to.x),
      maxY: top,
    });
  }
  if (!Number.isFinite(b.minX)) {
    const o = iso(0, 0, 0);
    return { minX: o.x, minY: o.y, maxX: o.x, maxY: o.y };
  }
  return b;
}
