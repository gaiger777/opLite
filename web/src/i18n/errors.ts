import type { TFunction } from "i18next";

import { ApiError } from "@/api/client";

/** 오류를 사용자에게 보일 문장으로 바꾼다. API 오류는 `errors.<code>` 번역을 쓰고, 번역이 없으면 서버의 설명을 그대로 쓴다. */
export function errorMessage(t: TFunction, error: unknown): string {
  if (error instanceof ApiError) {
    const key = `errors.${error.code}`;
    return t(key, { message: error.message, defaultValue: error.message });
  }
  if (error instanceof TypeError) {
    return t("errors.network");
  }
  return error instanceof Error ? error.message : String(error);
}
