// 자세히 보기 세 단계(계획 70 §9.2): 화면에서 타일 한 칸의 크기(px)와 VM 수로 "요약 · 타일 · 자세히"를 고른다. 문턱
// 둘레 15% 이력(hysteresis)이 있어 확대·축소 중 문턱에서 깜박이지 않는다.
import { boxBounds } from "@/components/iso/iso";
import type { ViewBox } from "@/components/iso/viewport";
import { TILE, type MapScene } from "@/clusterMap/layout";

export type Lod = "summary" | "tiles" | "detail";

/** 타일 한 칸이 이보다 작으면(px) 요약 */
export const SUMMARY_PX = 6;
/** 이 이상(px)이고 화면 안 타일이 `DETAIL_TILES` 밑이면 자세히 */
export const DETAIL_PX = 18;
/** 보이는 VM이 이보다 많으면 늘 요약 */
export const SUMMARY_VMS = 3000;
export const DETAIL_TILES = 300;
/** 이력: 들어가려면 문턱보다 15% 더, 나오려면 15% 덜 */
export const HYSTERESIS = 0.15;

export interface LodInput {
  /** 화면에서 타일 한 변의 크기(px) — `tilePx` */
  tilePx: number;
  /** 그림의 VM 타일 수 */
  vms: number;
  /** 화면 안(viewBox 안) 타일 수 — `tilesInView` */
  inView: number;
}

/** 단계 고르기. `prev`(지금 단계)가 있으면 문턱 둘레 이력을 쓴다. */
export function lodFor(input: LodInput, prev?: Lod): Lod {
  if (input.vms > SUMMARY_VMS) {
    return "summary";
  }
  const enter = prev === undefined ? 1 : 1 + HYSTERESIS;
  const leave = prev === undefined ? 1 : 1 - HYSTERESIS;
  const summary =
    prev === "summary" ? input.tilePx < SUMMARY_PX * enter : input.tilePx < SUMMARY_PX * leave;
  if (summary) {
    return "summary";
  }
  const detail =
    prev === "detail"
      ? input.tilePx >= DETAIL_PX * leave && input.inView <= DETAIL_TILES * enter
      : input.tilePx >= DETAIL_PX * enter && input.inView < DETAIL_TILES * leave;
  return detail ? "detail" : "tiles";
}

/** viewBox가 `size`(px) 상자에 맞춰 그려질 때(`preserveAspectRatio` 기본 meet) 타일 한 변의 화면 크기 */
export function tilePx(view: ViewBox, size: { width: number; height: number }): number {
  const scale = Math.min(size.width / view.w, size.height / view.h);
  return Number.isFinite(scale) ? scale * TILE : 0;
}

/** viewBox와 겹치는 노드 위 타일 수(노드 단위로 — 노드가 조금이라도 보이면 그 타일 모두) */
export function tilesInView(scene: MapScene, view: ViewBox): number {
  let n = 0;
  for (const node of scene.nodes) {
    const cells = node.grid?.cells.length ?? 0;
    if (cells === 0) {
      continue;
    }
    const b = boxBounds({ ...node.box, h: node.box.h + node.stack });
    if (
      b.maxX >= view.x &&
      b.minX <= view.x + view.w &&
      b.maxY >= view.y &&
      b.minY <= view.y + view.h
    ) {
      n += cells;
    }
  }
  return n;
}
