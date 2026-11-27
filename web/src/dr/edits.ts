// 계획 탭의 저장하지 않은 초안(리뷰 1 웹 7 — 탭을 떠났다 와도 남음). 로그아웃·세션 만료 때 `AuthProvider`가 비운다(리뷰 2 웹 8 —
// 다음 사용자가 앞 사람의 초안을 보지 않게). 늦게 읽는 DR 조각 밖에 두어 첫 화면 묶음이 커지지 않는다.
import type { RecoveryPlanSpec } from "@/dr/api";
import type { Draft } from "@/dr/model";

/** 고치는 중인 초안: 편집을 시작한 때의 계획(`base`)과 그 판 */
export interface Edit {
  draft: Draft;
  base: RecoveryPlanSpec;
  revision: number;
}

/** 계획 id → 초안 */
export const planEdits = new Map<string, Edit>();

export function forgetPlanEdits() {
  planEdits.clear();
}
