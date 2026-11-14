import type { TFunction } from "i18next";

import type {
  PlacementMode,
  PlacementMove,
  PlacementPatch,
  PlacementPolicy,
  PlacementSettings,
} from "@/api/types";

/** 서버의 `DEFAULT_PLACEMENT_THRESHOLD`·`MAX_PLACEMENT_THRESHOLD`(정수 백분율) */
export const DEFAULT_THRESHOLD = 15;
export const MAX_THRESHOLD = 50;
export const MODES: PlacementMode[] = ["off", "recommend", "auto"];
export const POLICIES: PlacementPolicy[] = ["balance", "pack"];

/** 설정 카드의 폼 값 */
export interface PlacementForm {
  mode: PlacementMode;
  policy: PlacementPolicy;
  threshold: string;
}

/** 저장된 설정의 폼 값. 서버는 기본값을 쓰지 않으므로(설정이 없거나 키가 없음) 없으면 기본이다. */
export function formFromSettings(s: Partial<PlacementSettings> | undefined): PlacementForm {
  return {
    mode: s?.mode ?? "recommend",
    policy: s?.policy ?? "balance",
    threshold: String(s?.threshold ?? DEFAULT_THRESHOLD),
  };
}

/** 문턱 입력 오류(번역 키). 서버처럼 1~50의 정수만 받는다. */
export function thresholdError(text: string): string | null {
  const n = Number(text);
  return /^\d+$/.test(text) && n >= 1 && n <= MAX_THRESHOLD ? null : "placement.errors.threshold";
}

/** 바꾼 값만 담은 설정 바꾸기(서버가 준 것만 합친다). 바뀐 것이 없으면 빈 객체 */
export function placementPatch(form: PlacementForm, current: PlacementForm): PlacementPatch {
  return {
    ...(form.mode !== current.mode && { mode: form.mode }),
    ...(form.policy !== current.policy && { policy: form.policy }),
    ...(Number(form.threshold) !== Number(current.threshold) && {
      threshold: Number(form.threshold),
    }),
  };
}

/** 0.1% 단위 값을 `21.5%`로. 없으면 `—` */
export function pct(permille: number | undefined): string {
  return permille == null ? "—" : `${(permille / 10).toFixed(1)}%`;
}

/** 사용률 막대의 너비(%): 0~100으로 자른다(사용률은 100%를 넘을 수 있다). */
export function barWidth(permille: number): number {
  return Math.min(100, Math.max(0, permille / 10));
}

/** 서버가 정한 문장으로만 쉬는 이유의 번역 키. 노드 이름 등이 들어가는 이유는 서버 문구를 그대로 보인다. */
const FIXED_PAUSES: Record<string, string> = {
  "mode is off": "placement.pausedReasons.off",
  "policy is pack": "placement.pausedReasons.pack",
  "fewer than two nodes have a recent load summary": "placement.pausedReasons.fewNodes",
};

/** 쉬는 이유의 문장: 고정 이유는 번역, 나머지는 서버 문구 */
export function pausedText(t: TFunction, paused: string): string {
  const key = FIXED_PAUSES[paused];
  return key ? t(key) : paused;
}

/** 추천 하나의 이유 문장: 균형이면 줄어드는 불균형, 규칙이면 바로잡는 hard 규칙 */
export function moveReasonText(t: TFunction, move: PlacementMove): string {
  return move.reason === "rule"
    ? t("placement.reasonRule", { rules: (move.rules ?? []).join(", ") || "—" })
    : t("placement.reasonBalance", { gain: pct(move.gain_pct) });
}
