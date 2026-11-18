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
