// 합친 경로(계획 70 §4·§9.2): 타일을 하나씩 원소로 그리지 않고 "띠 · 층 · 색 · 면"마다 `<path>` 하나로 그린다. 띠는
// 화면에서 서로 겹치지 않는 노드 묶음(`layout.ts`)이라 띠 안 노드의 타일을 합칠 수 있다. 차례: 띠마다 블록(따로 그리는
// 원소) → 층 아래부터(위 층이 아래 층을 가린다), 층 안에서는 옆면 → 오른쪽 면 그늘 → 윗면 → HA 테두리·실패 ×. 한 층 안
// 타일끼리는 화면에서 겹치지 않으므로(`layout.ts`의 타일 높이 ≤ 간격) 옆면 둘을 색마다 한 경로(왼쪽 면 밝기)로 칠하고, 오른쪽
// 면은 층마다 색과 상관없는 검은 반투명 경로 하나로 어둡게 한다(`shade()`의 오른쪽 면과 같은 72%). 위 층 타일이 덮는 윗면은
// 그리지 않는다.
import { boxesPath, boxFace, type Face, facesPath, type Point, round } from "@/components/iso/iso";
import type { Lod } from "@/clusterMap/lod";
import type { BarTone, MapScene, NodeItem, TileCell, TileTone } from "@/clusterMap/layout";

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

/** 레이어 하나가 노드마다 더하는 원소(§7.3 — 많아야 서넛): 스토리지 받침 3, 네트워크 띠 1, HA 둘레 1 */
export const LAYER_COST = { storage: 3, network: 1, ha: 1 } as const;
export type MapLayer = keyof typeof LAYER_COST | "load" | "recommendations";

/**
 * 그리면 생길 SVG 원소 수(그리기 없이 장면으로 셈 — §9.2 예산). 화면(스테이지 2)이 같은 셈을 따른다: 판 = 면 셋 + 이름 글,
 * 노드 = `<g>` + 요약이면 블록 면 셋, 아니면 윗면 + 옆면 둘을 `IsoGauge`(다각형 둘씩)로 + 꼬리표마다 둘 + (자세히면 윗면 글), 증인 = `<g>` + 기둥
 * 면 셋, 배치 안 됨 = `<g>`, 경로 조각 = 하나, 화살표 = 선 + 글, 자세히 단계의 타일마다 이름 글(+ 규칙 어김 삼각형),
 * 켠 레이어마다 노드당 `LAYER_COST`, 그림 틀(`<svg>`·`<defs>`·`<style>`·무늬) 12.
 */
export function elementCount(
  scene: MapScene,
  paths: readonly BandPaths[],
  lod: Lod,
  layers: readonly MapLayer[] = ["load"],
): number {
  let n = 12 + scene.slabs.length * 4;
  const perLayer = layers.reduce(
    (sum, l) => sum + (l in LAYER_COST ? LAYER_COST[l as keyof typeof LAYER_COST] : 0),
    0,
  );
  for (const node of scene.nodes) {
    switch (node.kind) {
      case "unplaced":
        n += 1;
        break;
      case "witness":
        n += 4 + node.tags.length * 2;
        break;
      case "node":
        n += (lod === "summary" ? 4 : 6) + node.tags.length * 2 + (lod === "detail" ? 1 : 0);
        n += perLayer;
        break;
    }
    if (lod === "detail") {
      for (const cell of node.grid?.cells ?? []) {
        n += 1 + (cell.warn ? 1 : 0);
      }
    }
  }
  for (const band of paths) {
    n += band.pieces.length;
  }
  n +=
    scene.links.filter((l) => l.kind === "migration" || layers.includes("recommendations")).length *
    2;
  return n;
}
