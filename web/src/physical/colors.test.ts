import { readFileSync } from "node:fs";

import { BAR_TOKEN, HEALTH_TOKEN, THREAD_TOKEN } from "@/physical/colors";
import { contrast, readBlock, resolveColor } from "@/test/contrast";

// 홈 그림 색의 대비(계획 75): 상태 점·스레드 칸·막대는 그림 원소라 `--card` 위에서 3:1 이상(두 테마).
const css = readFileSync(`${import.meta.dirname}/../index.css`, "utf8");
const root = readBlock(css, ":root");
const THEMES = { light: root, dark: { ...root, ...readBlock(css, ".dark") } };

const TOKENS = [
  ...Object.values(HEALTH_TOKEN),
  ...Object.values(THREAD_TOKEN),
  ...Object.values(BAR_TOKEN),
];

describe.each(Object.entries(THEMES))("%s theme", (_theme, vars) => {
  it.each([...new Set(TOKENS)])("%s on --card is at least 3:1", (token) => {
    expect(
      contrast(resolveColor(vars, token), resolveColor(vars, "--card")),
    ).toBeGreaterThanOrEqual(3);
  });
});
