import type { components } from "@/api/schema";

export type Severity = components["schemas"]["Severity"];

/** 심각도가 쓰는 상태색 묶음(`--danger`·`--warning`·`--info`와 그 `-soft`·`-fg`) */
export type Tone = "danger" | "warning" | "info";

/** 높은 것부터 */
export const SEVERITIES: readonly Severity[] = ["critical", "warning", "info"];

export function severityTone(severity: Severity): Tone {
  switch (severity) {
    case "critical":
      return "danger";
    case "warning":
      return "warning";
    case "info":
      return "info";
  }
}

/** 칩(배경·글자)의 클래스. 색은 토큰으로만 쓴다. */
export const TONE_CHIP: Record<Tone, string> = {
  danger: "bg-danger-soft text-danger-fg",
  warning: "bg-warning-soft text-warning-fg",
  info: "bg-info-soft text-info-fg",
};
