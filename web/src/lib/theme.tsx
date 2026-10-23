import { createContext, useContext, useEffect, useState } from "react";

import { readPref, writePref } from "@/lib/prefs";

export type Theme = "system" | "light" | "dark";

const DARK = "(prefers-color-scheme: dark)";

export function readTheme(): Theme {
  const saved = readPref("theme");
  return saved === "light" || saved === "dark" ? saved : "system";
}

/** `<html>`의 `dark` 클래스로 테마를 바꾼다. 첫 화면이 깜빡이지 않게 그리기 전에도 부른다. */
export function applyTheme(theme: Theme) {
  const dark = theme === "dark" || (theme === "system" && window.matchMedia(DARK).matches);
  document.documentElement.classList.toggle("dark", dark);
}

const ThemeContext = createContext<{ theme: Theme; setTheme(theme: Theme): void } | null>(null);

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [theme, setThemeState] = useState<Theme>(readTheme);

  useEffect(() => {
    applyTheme(theme);
    if (theme !== "system") {
      return;
    }
    // 시스템 설정을 따르면 운영체제에서 바꿀 때 함께 바뀐다.
    const media = window.matchMedia(DARK);
    const follow = () => applyTheme("system");
    media.addEventListener("change", follow);
    return () => media.removeEventListener("change", follow);
  }, [theme]);

  const setTheme = (next: Theme) => {
    writePref("theme", next);
    setThemeState(next);
  };
  return <ThemeContext value={{ theme, setTheme }}>{children}</ThemeContext>;
}

export function useTheme() {
  const value = useContext(ThemeContext);
  if (!value) {
    throw new Error("useTheme outside ThemeProvider");
  }
  return value;
}
