// 적중 검사(계획 70 §4): 합친 경로는 원소 하나라 어느 타일인지 DOM이 모른다. 배치가 순수 셈이므로 화면 점을 세계로 되돌려
// 노드·층·칸을 셈으로 찾는다. 그리는 차례의 거꾸로(가까운 것부터) 상자의 그림자(화면 윤곽)에 점이 드는지 본다.
import { type Box, type Point, screenToWorld } from "@/components/iso/iso";
import type { MapScene, NodeItem, TileCell } from "@/clusterMap/layout";
import type { Lod } from "@/clusterMap/lod";

export type Hit =
  | { kind: "node"; key: string }
  /** 타일(`vm:`·`more:`·`hidden:`·`ghost:` 열쇠)과 그 노드 열쇠 */
  | { kind: "tile"; key: string; node: string }
  /** 포트 줄의 포트(계획 75)와 그 노드 열쇠 */
  | { kind: "port"; key: string; node: string }
  /** 판 앞쪽 레일 */
  | { kind: "rail"; key: string };

/** 적중 셈의 네트워크 층: 포트 줄을 그리는가(네트워크 레이어 + 요약 아님), 레일을 그리는가 */
export interface HitNet {
  ports: boolean;
  rails: boolean;
}

/**
 * 화면 점 `p`가 상자의 보이는 윤곽 안인가. 화면의 한 점은 세계의 선 (A + z, B + z, z)이므로 그 선이 상자를 지나는 z 구간이
 * 있는지 본다(정확한 셈 — 면 셋 어디든).
 */
export function hitsBox(p: Point, b: Box): boolean {
  const base = screenToWorld(p, 0);
  const lo = Math.max(b.z, b.x - base.x, b.y - base.y);
  const hi = Math.min(b.z + b.h, b.x + b.w - base.x, b.y + b.d - base.y);
  return lo <= hi;
}

/** 노드 위 타일 중 점을 맞힌 것: 위 층부터, 층 안에서는 앞(열 + 줄이 큰 것)부터 */
function hitTile(cells: readonly TileCell[], p: Point): TileCell | null {
  let best: TileCell | null = null;
  for (const cell of cells) {
    if (!hitsBox(p, cell.box)) {
      continue;
    }
    if (
      best === null ||
      cell.layer > best.layer ||
      (cell.layer === best.layer && cell.col + cell.row > best.col + best.row)
    ) {
      best = cell;
    }
  }
  return best;
}

function hitNode(node: NodeItem, lod: Lod, p: Point, ports: boolean): Hit | null {
  if (lod !== "summary" && node.grid) {
    const cell = hitTile(node.grid.cells, p);
    if (cell) {
      return { kind: "tile", key: cell.key, node: node.key };
    }
  }
  // 포트는 블록 앞 바닥이라 자기 블록과 화면에서 겹치지 않는다 — 앞 줄 블록이 먼저 맞으면 그것이 이긴다.
  if (ports && lod !== "summary") {
    const port = node.net?.ports.find((x) => hitsBox(p, x.box));
    if (port) {
      return { kind: "port", key: port.key, node: node.key };
    }
  }
  if (lod === "summary" && node.bars.some((bar) => hitsBox(p, bar.box))) {
    return { kind: "node", key: node.key };
  }
  return hitsBox(p, node.box) ? { kind: "node", key: node.key } : null;
}

/**
 * 화면(SVG 사용자 좌표) 점 → 맞힌 노드·타일·포트·레일, 빈 곳(판 바닥 포함)이면 `null`. 레일은 바닥 위라 노드를 모두 본
 * 뒤에 본다.
 */
export function hitTest(
  scene: MapScene,
  lod: Lod,
  p: Point,
  net: HitNet = { ports: false, rails: false },
): Hit | null {
  for (let i = scene.nodes.length - 1; i >= 0; i--) {
    const hit = hitNode(scene.nodes[i]!, lod, p, net.ports);
    if (hit) {
      return hit;
    }
  }
  if (net.rails) {
    for (const rail of scene.rails.values()) {
      if (hitsBox(p, rail.box)) {
        return { kind: "rail", key: rail.key };
      }
    }
  }
  return null;
}
