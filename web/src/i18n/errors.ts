import type { TFunction } from "i18next";

import { ApiError, NetworkError } from "@/api/client";
import i18n from "@/i18n";

/** 오류를 사용자에게 보일 문장으로 바꾼다. API 오류는 `errors.<code>` 번역을 쓰고, 번역이 없으면 서버의 설명을 그대로 쓴다. */
export function errorMessage(t: TFunction, error: unknown): string {
  if (error instanceof ApiError) {
    const key = `errors.${error.code}`;
    // 번역이 없으면 서버의 설명을 그대로 쓴다(번역 틀로 해석하지 않는다).
    return i18n.exists(key) ? t(key, { message: error.message }) : error.message;
  }
  if (error instanceof NetworkError) {
    return t("errors.network");
  }
  return error instanceof Error ? error.message : String(error);
}
