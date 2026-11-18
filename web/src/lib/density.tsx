import { createContext, useContext, useEffect, useState } from "react";

import { readPref, writePref } from "@/lib/prefs";

/** 표 행·입력 칸의 높이와 간격(`index.css`의 `--row-h`·`--control-h`·`--gap`) */
export type Density = "comfortable" | "compact";

/** 좁은 화면(휴대폰)은 손가락으로 누르므로 늘 편안함이다. */
const WIDE = "(min-width: 768px)";

export function readDensity(): Density {
  return readPref("density") === "compact" ? "compact" : "comfortable";
}

/** `<html data-density>`로 밀도를 바꾼다. 첫 화면이 흔들리지 않게 그리기 전에도 부른다. */
export function applyDensity(density: Density) {
  const compact = density === "compact" && window.matchMedia(WIDE).matches;
  document.documentElement.dataset.density = compact ? "compact" : "comfortable";
}

const DensityContext = createContext<{
  density: Density;
  setDensity(density: Density): void;
} | null>(null);

export function DensityProvider({ children }: { children: React.ReactNode }) {
  const [density, setDensityState] = useState<Density>(readDensity);

  useEffect(() => {
    applyDensity(density);
    if (density !== "compact") {
      return;
    }
    // 창 폭이 768px을 넘나들면 다시 정한다.
    const media = window.matchMedia(WIDE);
    const follow = () => applyDensity(density);
    media.addEventListener("change", follow);
    return () => media.removeEventListener("change", follow);
  }, [density]);

  const setDensity = (next: Density) => {
    writePref("density", next);
    setDensityState(next);
  };
  return <DensityContext value={{ density, setDensity }}>{children}</DensityContext>;
}

export function useDensity() {
  const value = useContext(DensityContext);
  if (!value) {
    throw new Error("useDensity outside DensityProvider");
  }
  return value;
}
