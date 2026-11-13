import type { TFunction } from "i18next";

import { ApiError, NetworkError } from "@/api/client";
import i18n from "@/i18n";
import { formatMib } from "@/lib/format";

/** 409 `ha_capacity`의 `details`(MiB) */
interface HaCapacity {
  capacity_mib: number;
  reserve_mib: number;
  used_mib: number;
  need_mib: number;
}

function isHaCapacity(details: unknown): details is HaCapacity {
  return (
    typeof details === "object" &&
    details !== null &&
    ["capacity_mib", "reserve_mib", "used_mib", "need_mib"].every(
      (k) => typeof (details as Record<string, unknown>)[k] === "number",
    )
  );
}

/** 오류를 사용자에게 보일 문장으로 바꾼다. API 오류는 `errors.<code>` 번역을 쓰고, 번역이 없으면 서버의 설명을 그대로 쓴다. */
export function errorMessage(t: TFunction, error: unknown): string {
  if (error instanceof ApiError) {
    // HA 용량 예약은 서버의 셈(details)으로 남은 자리를 알린다.
    if (error.code === "ha_capacity" && isHaCapacity(error.details)) {
      const d = error.details;
      const detail = t("vms.haCapacityDetail", {
        need: formatMib(d.need_mib),
        free: formatMib(Math.max(0, d.capacity_mib - d.reserve_mib - d.used_mib)),
        reserve: formatMib(d.reserve_mib),
        capacity: formatMib(d.capacity_mib),
      });
      return `${t("errors.ha_capacity")} ${detail}`;
    }
    return codeMessage(t, error.code, error.message);
  }
  if (error instanceof NetworkError) {
    return t("errors.network");
  }
  return error instanceof Error ? error.message : String(error);
}

/**
 * 서버의 오류 code와 설명을 문장으로: `errors.<code>` 번역, 번역이 없으면 설명 그대로. 예약 오류도 쓴다.
 * `own`이 있으면 `<own>.<code>` 전용 문구를 먼저 찾는다.
 */
export function codeMessage(t: TFunction, code: string, message: string, own?: string): string {
  for (const key of [own && `${own}.${code}`, `errors.${code}`]) {
    if (key && i18n.exists(key)) {
      return t(key, { message });
    }
  }
  // 번역이 없으면 서버의 설명을 그대로 쓴다(번역 틀로 해석하지 않는다).
  return message;
}
