// 합친 경로(계획 70 §4·§9.2): 타일을 하나씩 원소로 그리지 않고 "띠 · 층 · 색 · 면"마다 `<path>` 하나로 그린다. 띠는
// 화면에서 서로 겹치지 않는 노드 묶음(`layout.ts`)이라 띠 안 노드의 타일을 합칠 수 있다. 차례: 띠마다 블록(따로 그리는
// 원소) → 층 아래부터(위 층이 아래 층을 가린다), 층 안에서는 옆면 → 오른쪽 면 그늘 → 윗면 → HA 테두리·실패 ×. 한 층 안
// 타일끼리는 화면에서 겹치지 않으므로(`layout.ts`의 타일 높이 ≤ 간격) 옆면 둘을 색마다 한 경로(왼쪽 면 밝기)로 칠하고, 오른쪽
// 면은 층마다 색과 상관없는 검은 반투명 경로 하나로 어둡게 한다(`shade()`의 오른쪽 면과 같은 72%). 위 층 타일이 덮는 윗면은
// 그리지 않는다.
import { boxesPath, boxFace, type Face, facesPath, type Point, round } from "@/components/iso/iso";
import type { Lod } from "@/clusterMap/lod";
import type { BarTone, MapScene, NodeItem, NodeTag, TileCell, TileTone } from "@/clusterMap/layout";

export interface PathPiece {
  /** 그림 안에서 하나뿐인 열쇠(React key) */
  key: string;
  /**
   * `tile`·`bar`는 채움(면 밝기 `face` — 타일의 `left`는 옆면 둘), `shade`는 타일 오른쪽 면을 어둡게 하는 검은 반투명
   * (`SHADE_OPACITY`), `ha`는 윗면 테두리, `cross`는 실패 타일 윗면의 × 선
   */
  kind: "tile" | "bar" | "shade" | "ha" | "cross";
  tone: TileTone | BarTone | null;
  face: Face | null;
  layer: number;
  /** 오프라인 노드의 타일(빗금 — 마지막으로 알려진 자리) */
  stale: boolean;
  d: string;
  /** 경로 조각(다각형·선) 수 */
  parts: number;
}

export interface BandPaths {
  band: number;
  /** 이 띠에서 먼저 그리는 노드 원소(블록·기둥·배치 안 됨 판은 판 목록에서) */
  nodes: NodeItem[];
  pieces: PathPiece[];
}

/** 오른쪽 면 그늘의 불투명도(`shade(color, "right")`의 검정 28%와 같은 밝기) */
export const SHADE_OPACITY = 0.28;

/** 위 층의 같은 자리에 타일이 있어 윗면이 가려지는가 */
function coveredTop(cells: readonly TileCell[], cell: TileCell, perLayer: number): boolean {
  return (
    cell.index + perLayer < cells.length && cells[cell.index + perLayer]!.layer === cell.layer + 1
  );
}

/** 윗면 × 표시: 두 대각선을 가운데로 25% 줄인 선 둘 */
function crossPath(top: readonly Point[]): string {
  const [a, b, c, d] = top as [Point, Point, Point, Point];
  const shrink = (p: Point, q: Point) => {
    const k = 0.25;
    return [
      `${round(p.x + (q.x - p.x) * k)},${round(p.y + (q.y - p.y) * k)}`,
      `${round(q.x + (p.x - q.x) * k)},${round(q.y + (p.y - q.y) * k)}`,
    ];
  };
  const [p1, p2] = shrink(a, c);
  const [p3, p4] = shrink(b, d);
  return `M${p1}L${p2}M${p3}L${p4}`;
}

/** 무리 짓기(삽입 차례를 지키는 Map) */
function groupBy<T>(items: readonly T[], key: (t: T) => string): Map<string, T[]> {
  const out = new Map<string, T[]>();
  for (const it of items) {
    const k = key(it);
    const list = out.get(k);
    if (list) {
      list.push(it);
    } else {
      out.set(k, [it]);
    }
  }
  return out;
}

const toneKey = (c: TileCell) => `${c.tone}${c.stale ? "~" : ""}`;

function tilePieces(band: number, nodes: readonly NodeItem[]): PathPiece[] {
  const pieces: PathPiece[] = [];
  const layered: { cell: TileCell; covered: boolean }[][] = [];
  for (const n of nodes) {
    const grid = n.grid;
    if (!grid) {
      continue;
    }
    for (const cell of grid.cells) {
      (layered[cell.layer] ??= []).push({
        cell,
        covered: coveredTop(grid.cells, cell, grid.perLayer),
      });
    }
  }
  layered.forEach((entries, layer) => {
    for (const [k, list] of groupBy(entries, (e) => toneKey(e.cell))) {
      const first = list[0]!.cell;
      pieces.push({
        key: `b${band}:l${layer}:${k}:sides`,
        kind: "tile",
        tone: first.tone,
        face: "left",
        layer,
        stale: first.stale,
        d: facesPath(
          list.flatMap((e) => [boxFace(e.cell.box, "left"), boxFace(e.cell.box, "right")]),
        ),
        parts: list.length * 2,
      });
    }
    pieces.push({
      key: `b${band}:l${layer}:shade`,
      kind: "shade",
      tone: null,
      face: "right",
      layer,
      stale: false,
      d: boxesPath(
        entries.map((e) => e.cell.box),
        "right",
      ),
      parts: entries.length,
    });
    const visible = entries.filter((e) => !e.covered);
    for (const [k, list] of groupBy(visible, (e) => toneKey(e.cell))) {
      const first = list[0]!.cell;
      pieces.push({
        key: `b${band}:l${layer}:${k}:top`,
        kind: "tile",
        tone: first.tone,
        face: "top",
        layer,
        stale: first.stale,
        d: boxesPath(
          list.map((e) => e.cell.box),
          "top",
        ),
        parts: list.length,
      });
    }
    const ha = visible.filter((e) => e.cell.ha);
    if (ha.length > 0) {
      pieces.push({
        key: `b${band}:l${layer}:ha`,
        kind: "ha",
        tone: null,
        face: "top",
        layer,
        stale: false,
        d: boxesPath(
          ha.map((e) => e.cell.box),
          "top",
        ),
        parts: ha.length,
      });
    }
    const failed = visible.filter((e) => e.cell.tone === "failed");
    if (failed.length > 0) {
      pieces.push({
        key: `b${band}:l${layer}:cross`,
        kind: "cross",
        tone: null,
        face: null,
        layer,
        stale: false,
        d: failed.map((e) => crossPath(boxFace(e.cell.box, "top"))).join(" "),
        parts: failed.length,
      });
    }
  });
  return pieces;
}

function barPieces(band: number, nodes: readonly NodeItem[]): PathPiece[] {
  const bars = nodes.flatMap((n) => n.bars);
  const pieces: PathPiece[] = [];
  for (const [tone, list] of groupBy(bars, (b) => b.tone)) {
    for (const face of ["left", "right", "top"] as const) {
      pieces.push({
        key: `b${band}:bar:${tone}:${face}`,
        kind: "bar",
        tone: tone as BarTone,
        face,
        layer: 0,
        stale: false,
        d: boxesPath(
          list.map((b) => b.box),
          face,
        ),
        parts: list.length,
      });
    }
  }
  return pieces;
}

/**
 * 장면 → 띠마다 그릴 것. 요약 단계는 노드마다 상태 막대(띠 · 상태 · 면마다 경로 하나), 타일·자세히 단계는 VM 타일. 단계가
 * 바뀔 때만 다시 셈한다(가리키기·끌기·확대는 그대로 쓴다).
 */
export function scenePaths(scene: MapScene, lod: Lod): BandPaths[] {
  const out: BandPaths[] = [];
  let current: NodeItem[] = [];
  const flush = () => {
    if (current.length === 0) {
      return;
    }
    const band = current[0]!.band;
    out.push({
      band,
      nodes: current,
      pieces: lod === "summary" ? barPieces(band, current) : tilePieces(band, current),
    });
    current = [];
  };
  for (const n of scene.nodes) {
    if (current.length > 0 && current[0]!.band !== n.band) {
      flush();
    }
    current.push(n);
  }
  flush();
  return out;
}

/** 켜고 끄는 레이어(§7.3) */
export type MapLayer = "load" | "storage" | "network" | "ha" | "recommendations";
export const MAP_LAYERS: readonly MapLayer[] = [
  "load",
  "storage",
  "network",
  "ha",
  "recommendations",
];

/** 노드 꼬리표(알약)의 열쇠: 상태 꼬리표와 레이어가 더하는 게이트웨이·펜스 확인 실패 */
export type MarkTag = NodeTag | "gateway" | "fence";

/**
 * 노드 하나에 그릴 덧표시(상태 꼬리표·레이어). 그리는 쪽(`MapScene`)과 원소 셈(`elementCount`)이 같은 것을 쓴다. 레이어마다
 * 노드당 많아야 서넛이다: 스토리지 = 받침(로컬 풀 조각들, 가장 나쁜 색) + 준비 안 됨 빗금 + 공유 풀 경고 눈금, 네트워크 =
 * 업링크 띠 + "GW" 꼬리표, HA = 펜싱 없음 점선 둘레 + 펜스 확인 실패 꼬리표.
 */
export interface NodeMarks {
  tags: MarkTag[];
  /** 유지보수 줄무늬 띠(윗면 앞 가장자리) */
  stripe: boolean;
  /** 과다 할당 띠(왼쪽 면 위 가장자리) */
  overcommit: boolean;
  /** 스토리지: 받침 / 준비 안 된 조각의 빗금 / 공유 풀 경고 눈금 */
  pedestal: boolean;
  poolBad: boolean;
  sharedTick: boolean;
  /** 네트워크 띠(정보가 없으면 그리지 않음) */
  uplink: boolean;
  /** HA: 펜싱 없음 점선 둘레 */
  ring: boolean;
}

export function nodeMarks(node: NodeItem, layers: ReadonlySet<MapLayer>): NodeMarks {
  const l = node.layers;
  const storage = layers.has("storage") && node.kind === "node" && l !== null;
  const network = layers.has("network") && l !== null;
  const ha = layers.has("ha") && node.kind === "node" && l !== null;
  const tags: MarkTag[] = [...node.tags];
  if (network && l.gateway) {
    tags.push("gateway");
  }
  if (ha && l.fenceError !== null) {
    tags.push("fence");
  }
  return {
    tags,
    stripe: node.tags.includes("maintenance"),
    overcommit: node.tags.includes("overcommit"),
    pedestal: storage && l.pools.length > 0,
    poolBad: storage && l.pools.some((p) => p.tone === "bad"),
    sharedTick: storage && l.sharedDown.length > 0,
    uplink: network && node.kind === "node" && l.uplink !== null,
    ring: ha && l.fencing === false,
  };
}

/** 노드 원소 하나가 만드는 SVG 원소 수(`MapScene`의 `NodeShape`와 같은 셈) */
export function nodeElements(node: NodeItem, lod: Lod, marks: NodeMarks): number {
  if (node.kind === "unplaced") {
    return 1;
  }
  // <g> + 이름 글 + 꼬리표마다 알약·글
  let n = 2 + marks.tags.length * 2;
  if (node.kind === "witness") {
    // 네모 기둥: 보이는 옆면 둘 + 윗면
    return n + 3;
  }
  // 요약은 블록 면 셋, 타일·자세히는 윗면 + 채움 면 둘(다각형 둘씩)
  n += lod === "summary" ? 3 : 5;
  if (lod === "detail") {
    n += 1; // 부하 글
  }
  for (const on of [
    marks.stripe,
    marks.overcommit,
    marks.pedestal,
    marks.poolBad,
    marks.sharedTick,
    marks.uplink,
    marks.ring,
  ]) {
    n += on ? 1 : 0;
  }
  return n;
}

/**
 * 그림 틀의 원소: `<svg>`, `<style>` 둘(밝히기·움직임), `<defs>`, 무늬 셋(부하 모름 빗금·준비 안 됨 빨간 빗금·유지보수
 * 줄무늬 — `<pattern>` + 바탕 + 선), 화살표 끝(`<marker>` + 경로), 층 `<g>` 셋(바닥·띠·화살표), 초점 테두리 경로 하나.
 */
export const FRAME_ELEMENTS = 1 + 2 + 1 + 3 * 3 + 2 + 3 + 1;

/**
 * 칸에 글을 적는가: 윗면이 보이는 칸만(위 층이 덮으면 아니다) — 자세히는 모든 칸(이름 앞 8자·"+N"), 타일 단계는 "+N" 칸만.
 */
export function cellText(node: NodeItem, cell: TileCell, lod: Lod): boolean {
  const grid = node.grid;
  if (lod === "summary" || !grid || coveredTop(grid.cells, cell, grid.perLayer)) {
    return false;
  }
  return lod === "detail" || cell.kind === "more" || cell.kind === "hidden";
}

/**
 * 그리면 생길 SVG 원소 수(그리기 없이 장면으로 셈 — §9.2 예산). 화면(`MapScene`)이 이 셈 그대로 그리고, DOM 시험이 그린
 * 원소 수와 맞춰 본다: 틀 `FRAME_ELEMENTS`, 판마다 면 셋 + 이름 글(스토리지 레이어에 공유 풀이 있으면 띠 + 글), 노드마다
 * `nodeElements`, 경로 조각마다 하나, 글이 있는 칸마다 하나(자세히 단계의 규칙 어김 삼각형 하나 더), 화살표마다 선 + 글.
 */
export function elementCount(
  scene: MapScene,
  paths: readonly BandPaths[],
  lod: Lod,
  layers: readonly MapLayer[] = ["load"],
): number {
  const on = new Set(layers);
  let n = FRAME_ELEMENTS + scene.slabs.length * 4;
  if (on.has("storage") && scene.shared.length > 0) {
    n += scene.slabs.filter((s) => s.kind !== "unplaced").length * 2;
  }
  for (const node of scene.nodes) {
    n += nodeElements(node, lod, nodeMarks(node, on));
    for (const cell of node.grid?.cells ?? []) {
      if (cellText(node, cell, lod)) {
        n += 1 + (lod === "detail" && cell.warn ? 1 : 0);
      }
    }
  }
  for (const band of paths) {
    n += band.pieces.length;
  }
  n += scene.links.filter((l) => l.kind === "migration" || on.has("recommendations")).length * 2;
  return n;
}
