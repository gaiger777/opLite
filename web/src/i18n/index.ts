import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import en from "@/i18n/en.json";
import ko from "@/i18n/ko.json";
import { readPref, writePref } from "@/lib/prefs";

export const LANGUAGES = ["ko", "en"] as const;
export type Language = (typeof LANGUAGES)[number];

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

void i18n.use(initReactI18next).init({
  resources: { ko: { translation: ko }, en: { translation: en } },
  lng: initialLanguage(),
  fallbackLng: "en",
  // React가 이스케이프한다.
  interpolation: { escapeValue: false },
});

export function setLanguage(language: Language) {
  void i18n.changeLanguage(language);
  writePref("language", language);
}

export default i18n;
