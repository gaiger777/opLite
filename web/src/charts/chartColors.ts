import { useSyncExternalStore } from "react";

/** 차트가 쓰는 색. 모두 `index.css`의 CSS 토큰에서 읽는다(밝음·어두움 테마를 따른다). */
export interface ChartColors {
  /** `--chart-1..5`: 시리즈 색 */
  series: string[];
  /** 축 글자(`--muted-foreground`) */
  text: string;
  /** 격자·축 선(`--border`) */
  grid: string;
  warning: string;
  danger: string;
}

const SERIES = ["--chart-1", "--chart-2", "--chart-3", "--chart-4", "--chart-5"];

/** 지금 `<html>`에 적용된 토큰 값을 읽는다. */
export function readChartColors(root: HTMLElement = document.documentElement): ChartColors {
  const style = getComputedStyle(root);
  const read = (name: string) => style.getPropertyValue(name).trim();
  return {
    series: SERIES.map(read),
    text: read("--muted-foreground"),
    grid: read("--border"),
    warning: read("--warning"),
    danger: read("--danger"),
  };
}

/**
 * 색을 투명도와 함께: `oklch(L C H)` → `oklch(L C H / a)`. 캔버스는 `color-mix`를 모르므로 글로 바꾼다.
 * 이미 투명도가 있거나 다른 형식이면 그대로 둔다.
 */
export function withAlpha(color: string, alpha: number): string {
  const m = /^(oklch|oklab|rgb|hsl)\(([^/)]*)\)$/.exec(color.trim());
  return m ? `${m[1]}(${m[2]!.trim()} / ${alpha})` : color;
}

// 테마는 `<html>`의 `dark` 클래스로 바뀐다(`lib/theme.tsx`). 클래스가 바뀌면 다시 읽는다.
function subscribe(onChange: () => void) {
  const observer = new MutationObserver(onChange);
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
  return () => observer.disconnect();
}

const theme = () => document.documentElement.className;

/** 테마 토큰의 차트 색. 테마(밝음·어두움)가 바뀌면 다시 읽는다. */
export function useChartColors(): ChartColors {
  // 클래스 글을 상태로 두고, 바뀔 때만 색을 다시 읽는다(같은 테마면 같은 객체).
  const key = useSyncExternalStore(subscribe, theme);
  return cached(key);
}

let last: { key: string; colors: ChartColors } | null = null;

function cached(key: string): ChartColors {
  if (last?.key !== key) {
    last = { key, colors: readChartColors() };
  }
  return last.colors;
}
