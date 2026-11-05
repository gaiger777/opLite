import type { QueryClient } from "@tanstack/react-query";
import type { TFunction } from "i18next";

import { api, ApiError, call } from "@/api/client";
import { SETTINGS_KEY } from "@/api/live";
import type { Settings, SettingsPatch } from "@/api/types";
import { codeMessage, errorMessage } from "@/i18n/errors";

/** 설정·구독·텔레메트리 오류 문구. `settings.errors.<code>`(설정 쓰기가 겹친 busy 등)를 먼저 찾는다. */
export function settingsError(t: TFunction, err: unknown): string {
  return err instanceof ApiError
    ? codeMessage(t, err.code, err.message, "settings.errors")
    : errorMessage(t, err);
}

/** 대화상자가 보여 줄 수 있게 실패를 설정 오류 문구로 바꿔 던진다. */
export async function withSettingsError(t: TFunction, action: () => Promise<void>) {
  try {
    await action();
  } catch (err) {
    throw new Error(settingsError(t, err), { cause: err });
  }
}

/** 서버가 돌려준 설정을 캐시에 넣는다. 응답을 기다리는 사이 watch가 더 새 설정을 넣었으면 그것을 남긴다. */
export function storeSettings(queryClient: QueryClient, updated: Settings) {
  queryClient.setQueryData<Settings>(SETTINGS_KEY, (old) =>
    old && old.revision > updated.revision ? old : updated,
  );
}

/** 읽은 revision으로(`If-Match`) 설정을 바꾼다. */
export async function patchSettings(
  queryClient: QueryClient,
  revision: number,
  body: SettingsPatch,
) {
  const updated = await call(
    api.PATCH("/settings", { params: { header: { "If-Match": revision } }, body }),
  );
  storeSettings(queryClient, updated);
}
