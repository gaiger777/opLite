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
 * 하드웨어 바꾸기(PUT·`actions/hotplug`)의 오류에서 번역 문구보다 서버의 설명을 앞세우는 code. 서버가 어느 장치를
 * 왜 바꿀 수 없는지 말한다.
 */
export const HOTPLUG_ERROR_CODES = ["vm_running", "vm_busy", "no_capacity", "hotplug_unavailable"];

/** {@link errorMessage}와 같되 `codes`의 API 오류는 서버의 설명을 그대로 쓴다. */
export function serverErrorMessage(t: TFunction, error: unknown, codes: readonly string[]): string {
  if (error instanceof ApiError && codes.includes(error.code)) {
    return error.message;
  }
  return errorMessage(t, error);
}

/**
 * 서버 설명이 늘 같은 글(이름·값을 넣지 않음)이라 덧붙여도 번역보다 더 알려 주지 않는 code(서버 코드에서 모음, 계획 77).
 * 로그인·2단계 인증 오류처럼 번역이 이미 다 말하는 것이다. 시험(`i18n.test.ts`)이 서버 코드(`crates/`)에서 이 code의 설명이
 * 모두 글자 그대로(`format!`·변수가 아님)인지 본다 — 서버가 이름을 넣기 시작하면 여기서 빼야 한다.
 */
export const FIXED_MESSAGE = new Set([
  "unauthorized",
  "internal",
  "invalid_credentials",
  "cross_origin",
  "cluster_no_quorum",
  "not_leader",
  "last_admin",
  "cannot_delete_self",
  "untrusted_certificate",
  "subscription_required",
  "telemetry_endpoint_required",
  "no_role",
  "external_user",
  "remote_is_local",
  "recommendation_not_found",
  "invalid_second_factor",
  "second_factor_required",
  "enrollment_required",
  "two_factor_required",
  "totp_enabled",
  "cannot_reset_self",
  "webauthn_unavailable",
  "passkey_exists",
  "no_second_factor",
  "already_enrolled",
  "sdn_last_central",
]);

/**
 * code와 상관없이 덧붙이지 않는 늘 같은 설명. `forbidden`은 구체 설명(“cannot grant privileges you do not hold: …”)을
 * 주기도 해서 code로 빼지 않고, 그 기본 글(`ApiError::forbidden`)만 뺀다.
 */
const FIXED_TEXT = new Set(["permission denied"]);

/**
 * 서버의 오류 code와 설명을 문장으로: `errors.<code>` 번역, 번역이 없으면 설명 그대로. 예약 오류도 쓴다.
 * `own`이 있으면 `<own>.<code>` 전용 문구를 먼저 찾는다. 번역이 서버 설명을 담지 않으면 설명을 "자세히"로 덧붙인다
 * (계획 77 — 무엇이 이미 있는지·어느 칸이 틀렸는지 같은 구체 설명을 버리지 않는다).
 */
export function codeMessage(
  t: TFunction,
  code: string,
  message: string,
  own?: string,
  /** false면 설명을 덧붙이지 않는다(부르는 쪽이 `details`로 더 자세히 보일 때) */
  withDetail = true,
): string {
  for (const key of [own && `${own}.${code}`, `errors.${code}`]) {
    if (key && i18n.exists(key)) {
      const text = t(key, { message });
      const detail = message.trim();
      return !withDetail ||
        detail === "" ||
        FIXED_MESSAGE.has(code) ||
        FIXED_TEXT.has(detail) ||
        text.includes(detail)
        ? text
        : t("errors.withDetail", { text, detail });
    }
  }
  // 번역이 없으면 서버의 설명을 그대로 쓴다(번역 틀로 해석하지 않는다).
  return message;
}
