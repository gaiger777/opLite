// 가장자리 자동 스크롤(계획 71 설계 §4): 스크롤 상자·창 가장자리 48px 안이면 가까울수록 빠르게(프레임마다 16px까지).
import type { Point } from "@/dnd/types";

export const EDGE = 48;
export const MAX_STEP = 16;

/** 한 축의 속도: 앞 가장자리 쪽은 음수, 뒤 가장자리 쪽은 양수, 가운데는 0 */
export function edgeSpeed(at: number, start: number, end: number): number {
  const near = (d: number) =>
    d < EDGE ? Math.ceil((MAX_STEP * (EDGE - Math.max(d, 0))) / EDGE) : 0;
  if (at < start || at > end) {
    return 0;
  }
  return near(end - at) - near(at - start);
}

/** 상자(`left`·`top`·`right`·`bottom`) 안의 점에서 이번 프레임에 스크롤할 양 */
export function scrollStep(
  r: Pick<DOMRect, "left" | "top" | "right" | "bottom">,
  p: Point,
): { dx: number; dy: number } {
  return { dx: edgeSpeed(p.x, r.left, r.right), dy: edgeSpeed(p.y, r.top, r.bottom) };
}
