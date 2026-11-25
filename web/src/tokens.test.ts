import { readFileSync } from "node:fs";

import { contrast, parseOklch, readBlock, resolveColor } from "@/test/contrast";

// 설계 §10: 디자인 토큰의 대비(WCAG 2.x AA). jsdom은 색을 계산하지 않으므로 axe의 color-contrast 규칙
// 대신 이 시험이 맡는다.

describe("contrast math", () => {
  it("matches known WCAG ratios", () => {
    const white = parseOklch("oklch(1 0 0)");
    const black = parseOklch("oklch(0 0 0)");
    expect(contrast(black, white)).toBeCloseTo(21, 1);
    expect(contrast(white, white)).toBeCloseTo(1, 5);
    // #767676(oklch 0.5658 0 0)은 흰 바탕에서 4.54:1이다.
    expect(contrast(parseOklch("oklch(0.5658 0 0)"), white)).toBeCloseTo(4.54, 1);
  });

  it("blends translucent colors over the background", () => {
    const white = parseOklch("oklch(1 0 0)");
    expect(contrast(parseOklch("oklch(0 0 0 / 0%)"), white)).toBeCloseTo(1, 5);
    expect(contrast(parseOklch("oklch(0 0 0 / 100%)"), white)).toBeCloseTo(21, 1);
  });

  it("rejects other color syntaxes", () => {
    expect(() => parseOklch("#fff")).toThrow();
  });
});

// vitest는 CSS 가져오기를 비우므로(`?raw`도) 파일을 직접 읽는다.
const css = readFileSync(`${import.meta.dirname}/index.css`, "utf8");
const root = readBlock(css, ":root");
const THEMES = {
  light: root,
  dark: { ...root, ...readBlock(css, ".dark") },
};

/** 글자/바탕: 4.5:1 */
const TEXT: [string, string][] = [
  ["--foreground", "--background"],
  ["--card-foreground", "--card"],
  ["--popover-foreground", "--popover"],
  ["--primary-foreground", "--primary"],
  ["--secondary-foreground", "--secondary"],
  ["--muted-foreground", "--background"],
  ["--muted-foreground", "--muted"],
  ["--muted-foreground", "--card"],
  ["--accent-foreground", "--accent"],
  ["--destructive", "--background"],
  ["--brand", "--background"],
  ["--brand-foreground", "--brand"],
  ["--foreground", "--surface-1"],
  ["--foreground", "--surface-2"],
  ["--foreground", "--surface-3"],
  ["--muted-foreground", "--surface-1"],
  ["--sidebar-foreground", "--sidebar"],
  ["--sidebar-accent-foreground", "--sidebar-accent"],
  ["--sidebar-primary-foreground", "--sidebar-primary"],
  ...["success", "warning", "danger", "info"].flatMap((tone): [string, string][] => [
    [`--${tone}-fg`, `--${tone}-soft`],
    [`--${tone}-fg`, "--background"],
  ]),
  // VM 빌더 칸의 문제 글(계획 71 §11)
  ["--warning-fg", "--card"],
  ["--danger-fg", "--card"],
  // 설정 기록 서랍·계획 표의 글(계획 73 — 서랍과 대화상자 바탕은 popover)
  ["--muted-foreground", "--popover"],
  ["--warning-fg", "--popover"],
  ["--danger-fg", "--popover"],
];

/** 아이콘·포커스 링·차트 선·상태 점/바탕: 3:1 */
const GRAPHIC: [string, string][] = [
  ["--ring", "--background"],
  ["--ring", "--card"],
  ["--brand", "--card"],
  // VM 빌더의 놓을 칸 테두리·문제 칸 테두리(계획 71 §11)
  ["--warning", "--card"],
  ["--danger", "--card"],
  ...["--chart-1", "--chart-2", "--chart-3", "--chart-4", "--chart-5"].flatMap(
    (chart): [string, string][] => [
      [chart, "--background"],
      [chart, "--card"],
    ],
  ),
  ...["success", "warning", "danger", "info"].flatMap((tone): [string, string][] => [
    [`--${tone}`, "--background"],
    [`--${tone}`, `--${tone}-soft`],
  ]),
];

describe.each(Object.entries(THEMES))("%s theme tokens", (_theme, vars) => {
  it.each(TEXT)("%s on %s is at least 4.5:1", (fg, bg) => {
    expect(contrast(resolveColor(vars, fg), resolveColor(vars, bg))).toBeGreaterThanOrEqual(4.5);
  });

  it.each(GRAPHIC)("%s on %s is at least 3:1", (fg, bg) => {
    expect(contrast(resolveColor(vars, fg), resolveColor(vars, bg))).toBeGreaterThanOrEqual(3);
  });

  it("keeps --danger the same as --destructive", () => {
    expect(vars["--danger"]).toBe("var(--destructive)");
  });

  it("uses the brand color for the focus ring", () => {
    expect(vars["--ring"]).toBe("var(--brand)");
  });
});

describe("density and motion tokens", () => {
  it("defines comfortable and compact sizes", () => {
    expect(root["--row-h"]).toBe("40px");
    expect(root["--control-h"]).toBe("36px");
    expect(root["--gap"]).toBe("16px");
    const compact = readBlock(css, 'html[data-density="compact"]');
    expect(compact["--row-h"]).toBe("32px");
    expect(compact["--control-h"]).toBe("32px");
    expect(compact["--gap"]).toBe("12px");
  });

  it("turns motion off when the user asks for reduced motion", () => {
    expect(root["--duration-fast"]).toBe("120ms");
    expect(root["--duration-normal"]).toBe("200ms");
    const reduced = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));
    expect(reduced).toMatch(/--duration-fast:\s*0ms/);
    expect(reduced).toMatch(/--duration-normal:\s*0ms/);
    expect(reduced).toMatch(/animation-duration:\s*0\.01ms\s*!important/);
    expect(reduced).toMatch(/transition-duration:\s*0\.01ms\s*!important/);
  });
});
