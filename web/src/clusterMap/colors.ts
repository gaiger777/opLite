// 클러스터 지도의 색(계획 70 §7.4): 모두 디자인 토큰(`var(--…)`)과 `color-mix`라 밝음·어둠 테마가 같은 코드다. 면 밝기는
// 그리는 쪽이 `shade()`로 셈한다. 대비 시험(`colors.test.ts`)이 이 표를 두 테마의 `--card` 위에서 잰다.
import type { BarTone, TileTone } from "@/clusterMap/layout";
import type { Heat, PoolTone, Severity, Uplink } from "@/clusterMap/model";

/** VM 타일·요약 막대(§7.2). 옮기는 중·유령은 테두리로 그린다(`TILE_STROKE`). */
export const TILE_COLOR: Record<TileTone | BarTone, string> = {
  running: "var(--success)",
  stopped: "var(--muted-foreground)",
  busy: "var(--info)",
  failed: "var(--danger)",
  migrating: "color-mix(in oklab, var(--info) 22%, var(--card))",
  more: "color-mix(in oklab, var(--muted-foreground) 75%, var(--card))",
  hidden: "color-mix(in oklab, var(--muted-foreground) 75%, var(--card))",
  ghost: "none",
};

/** 테두리만 그리는 타일(옮기는 중의 원본 자리, 대상의 유령) */
export const TILE_STROKE: Partial<Record<TileTone, string>> = {
  migrating: "var(--info)",
  ghost: "var(--info)",
};

/** 블록 윗면의 부하 열 색(§7.1). `unknown`은 빗금 무늬(그리는 쪽의 `pattern`). */
export const HEAT_COLOR: Record<Exclude<Heat, "unknown">, string> = {
  cool: "color-mix(in oklab, var(--brand) 22%, var(--card))",
  warm: "var(--warning)",
  hot: "var(--danger)",
};

/** 부하 레이어를 끄거나 부하를 모를 때의 블록 색 */
export const NEUTRAL = "color-mix(in oklab, var(--muted-foreground) 30%, var(--card))";
/** 오프라인 블록(빛 바램) */
export const FADED = "color-mix(in oklab, var(--muted-foreground) 14%, var(--card))";
/** 채움 면(옆면)의 채운 부분: 같은 문턱의 색, 조용할 때는 브랜드 색 */
export const GAUGE_COLOR: Record<Exclude<Heat, "unknown">, string> = {
  cool: "var(--brand)",
  warm: "var(--warning)",
  hot: "var(--danger)",
};

/** 바닥 판 */
export const SLAB_COLOR = "color-mix(in oklab, var(--muted-foreground) 10%, var(--card))";
export const TRAY_COLOR = "color-mix(in oklab, var(--warning) 14%, var(--card))";

/** 스토리지 받침(§7.3). `bad`는 빨간 빗금. */
export const POOL_COLOR: Record<PoolTone, string> = {
  cool: "var(--chart-2)",
  warm: "var(--warning)",
  hot: "var(--danger)",
  bad: "var(--danger)",
};

/** 네트워크 띠 */
export const UPLINK_COLOR: Record<Uplink | "unknown", string> = {
  up: "var(--success)",
  degraded: "var(--warning)",
  down: "var(--danger)",
  /** 레일 선의 "모름"(계획 75 리뷰 1) */
  unknown: "var(--muted-foreground)",
};

/** 꼬리표(알약) 뜻 → 상태 토큰(`--{tone}-soft` 바탕·`--{tone}-fg` 글 — 이미 4.5:1을 시험함) */
export type TagTone = "danger" | "warning" | "info" | "success" | "neutral";

export const SEVERITY_TONE: Record<Severity, TagTone> = {
  info: "info",
  warning: "warning",
  critical: "danger",
};

export function tagFill(tone: TagTone): string {
  return tone === "neutral" ? "var(--muted)" : `var(--${tone}-soft)`;
}

export function tagText(tone: TagTone): string {
  return tone === "neutral" ? "var(--muted-foreground)" : `var(--${tone}-fg)`;
}

/** 마이그레이션·추천 화살표 */
export const LINK_COLOR = {
  migration: "var(--info)",
  recommendation: "var(--muted-foreground)",
} as const;

/** 네트워크 층(계획 75): 포트·본딩 테두리·레일 선의 상태 색(업링크 띠와 같은 뜻). 저하는 빗금·점선, 끊김은 ×가 함께다. */
export const PORT_COLOR = UPLINK_COLOR;

/** 레일 띠 */
export const RAIL_COLOR = "var(--chart-1)";
