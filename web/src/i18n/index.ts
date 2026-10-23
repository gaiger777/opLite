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

void i18n.use(initReactI18next).init({
  resources: { ko: { translation: ko }, en: { translation: en } },
  lng: initialLanguage(),
  fallbackLng: "en",
  // React가 이스케이프한다.
  interpolation: { escapeValue: false },
});
document.documentElement.lang = i18n.language;

export function setLanguage(language: Language) {
  void i18n.changeLanguage(language);
  document.documentElement.lang = language;
  writePref("language", language);
}

export default i18n;
