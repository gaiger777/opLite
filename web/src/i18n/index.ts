import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import { readPref, writePref } from "@/lib/prefs";

export const LANGUAGES = ["ko", "en"] as const;
export type Language = (typeof LANGUAGES)[number];

/**
 * 첫 화면 묶음의 문구는 언어마다 따로 늦게 받는다(계획 77-4 리뷰 — 고른 언어 하나만 처음에 받고, 다른 언어는 바꿀 때
 * 받는다). 시험은 `test/setup.ts`가 두 언어를 미리 더한다.
 */
const LOADERS: Record<Language, () => Promise<{ default: object }>> = {
  ko: () => import("@/i18n/ko.json"),
  en: () => import("@/i18n/en.json"),
};
const loaded = new Map<Language, Promise<void>>();

/** 언어 `language`의 첫 화면 문구를 더한다(한 번만 받는다). */
export function loadLanguage(language: Language): Promise<void> {
  let pending = loaded.get(language);
  if (!pending) {
    pending = LOADERS[language]().then((m) => {
      // 늦게 받는 모듈 묶음(지도·DR…)이 먼저 더했을 수 있으므로 깊게 합친다.
      i18n.addResourceBundle(language, "translation", m.default, true, false);
    });
    pending.catch(() => loaded.delete(language));
    loaded.set(language, pending);
  }
  return pending;
}

/** 고른 언어, 없으면 브라우저 언어(한국어가 아니면 영어) */
function initialLanguage(): Language {
  const saved = readPref("language");
  if (saved === "ko" || saved === "en") {
    return saved;
  }
  return navigator.language.toLowerCase().startsWith("ko") ? "ko" : "en";
}

// 화면 언어를 `<html lang>`에 맞춘다. 초기화도 이 이벤트를 낸다.
i18n.on("languageChanged", (language) => {
  document.documentElement.lang = language;
});

const first = initialLanguage();

void i18n.use(initReactI18next).init({
  resources: {},
  lng: first,
  fallbackLng: "en",
  // React가 이스케이프한다.
  interpolation: { escapeValue: false },
});

/** 첫 그리기 전에 기다린다: 고른 언어의 문구가 들어오면 풀린다. */
export const i18nReady: Promise<void> = loadLanguage(first);

export function setLanguage(language: Language) {
  writePref("language", language);
  void loadLanguage(language).then(() => i18n.changeLanguage(language));
}

export default i18n;
