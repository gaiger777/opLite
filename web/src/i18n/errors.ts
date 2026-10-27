import type { TFunction } from "i18next";

import { ApiError, NetworkError } from "@/api/client";
import i18n from "@/i18n";

/** 오류를 사용자에게 보일 문장으로 바꾼다. API 오류는 `errors.<code>` 번역을 쓰고, 번역이 없으면 서버의 설명을 그대로 쓴다. */
export function errorMessage(t: TFunction, error: unknown): string {
  if (error instanceof ApiError) {
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
