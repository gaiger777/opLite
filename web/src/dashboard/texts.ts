// 막대의 글(계획 75): 표·자세히 판·요약이 같은 말을 쓴다. 문구는 늦게 읽는 `physical.*`(physical/i18n)다.
import type { TFunction } from "i18next";

import { formatBytes, formatCapacity } from "@/lib/format";
import type { Measure } from "@/physical/capacity";

const times = (x: number | undefined) => (x === undefined ? undefined : `×${x.toFixed(1)}`);

/** "공유 vCPU 14 / 스레드 7 · ×2.0 · 사용 40 %" */
export function cpuText(t: TFunction, m: Measure, overcommit?: number, usePct?: number): string {
  return [
    t("physical.bar.cpu", { allocated: m.allocated ?? 0, total: m.total }),
    times(overcommit),
    usePct === undefined ? undefined : t("physical.bar.use", { pct: Math.round(usePct) }),
  ]
    .filter((x) => x !== undefined)
    .join(" · ");
}

/** "할당 32 GiB / 64 GiB · 사용 20 GiB · ×0.5" (MiB) */
export function memoryText(t: TFunction, m: Measure, overcommit?: number): string {
  return [
    t("physical.bar.allocated", {
      allocated: formatCapacity(m.allocated ?? 0),
      total: formatCapacity(m.total),
    }),
    m.used === undefined ? undefined : t("physical.bar.used", { used: formatCapacity(m.used) }),
    times(overcommit),
  ]
    .filter((x) => x !== undefined)
    .join(" · ");
}

/** "프로비전 150 GiB / 100 GiB · 사용 95 GiB" (바이트) */
export function storageText(t: TFunction, m: Measure): string {
  if (m.total <= 0 && !m.allocated) {
    return t("physical.bar.noStorage");
  }
  return [
    t("physical.bar.provisioned", {
      allocated: formatBytes(m.allocated ?? 0),
      total: formatBytes(m.total),
    }),
    m.used === undefined ? undefined : t("physical.bar.used", { used: formatBytes(m.used) }),
  ]
    .filter((x) => x !== undefined)
    .join(" · ");
}
