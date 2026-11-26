import { readFileSync } from "node:fs";

import {
  HEAT_COLOR,
  LINK_COLOR,
  POOL_COLOR,
  PORT_COLOR,
  RAIL_COLOR,
  TILE_COLOR,
  TILE_STROKE,
  UPLINK_COLOR,
} from "@/clusterMap/colors";
import { shade } from "@/components/iso/iso";
import { NET_COLOR, NET_RAIL } from "@/clusterMap/netPaths";
import { contrast, parseOklch, readBlock, type Rgba } from "@/test/contrast";

// 지도 색의 대비(계획 70 §7.4): 상태 색은 그림 원소라 `--card`·`--background` 위에서 3:1 이상(두 테마). 색은 토큰과
// `color-mix(in oklab, …)`라 같은 셈(OKLab에서 섞기)으로 sRGB를 구해 잰다(jsdom은 색을 셈하지 않는다).
const css = readFileSync(`${import.meta.dirname}/../index.css`, "utf8");
const root = readBlock(css, ":root");
const THEMES = { light: root, dark: { ...root, ...readBlock(css, ".dark") } };

type Lab = { l: number; a: number; b: number };

function oklab(vars: Record<string, string>, token: string): Lab {
  let value = vars[token];
  for (let i = 0; value?.startsWith("var("); i++) {
    value = vars[/^var\((--[\w-]+)\)$/.exec(value)?.[1] ?? ""];
    if (i > 8) {
      throw new Error(`var loop at ${token}`);
    }
  }
  const m = /^oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*\)$/.exec(value ?? "");
  if (!m) {
    throw new Error(`not an oklch token: ${token} = ${value}`);
  }
  const h = (parseFloat(m[3]!) * Math.PI) / 180;
  return {
    l: parseFloat(m[1]!),
    a: parseFloat(m[2]!) * Math.cos(h),
    b: parseFloat(m[2]!) * Math.sin(h),
  };
}

/** `var(--x)`·`color-mix(in oklab, A P%, B)` → OKLab */
function lab(vars: Record<string, string>, color: string): Lab {
  const v = /^var\((--[\w-]+)\)$/.exec(color);
  if (v) {
    return oklab(vars, v[1]!);
  }
  const mix =
    /^color-mix\(in oklab, (var\(--[\w-]+\)) (\d+)%, (var\(--[\w-]+\)|white|black)\)$/.exec(color);
  if (!mix) {
    throw new Error(`unknown color ${color}`);
  }
  const p = parseFloat(mix[2]!) / 100;
  const x = lab(vars, mix[1]!);
  const y =
    mix[3] === "white"
      ? { l: 1, a: 0, b: 0 }
      : mix[3] === "black"
        ? { l: 0, a: 0, b: 0 }
        : lab(vars, mix[3]!);
  return { l: x.l * p + y.l * (1 - p), a: x.a * p + y.a * (1 - p), b: x.b * p + y.b * (1 - p) };
}

function rgb(vars: Record<string, string>, color: string): Rgba {
  const { l, a, b } = lab(vars, color);
  const c = Math.hypot(a, b);
  const h = ((Math.atan2(b, a) * 180) / Math.PI + 360) % 360;
  return parseOklch(`oklch(${l} ${c} ${h})`);
}

/** 그림 원소 색(3:1): 타일 상태, 뜨거운 블록 윗면, 받침·업링크 띠, 테두리만 그리는 타일·화살표 */
const GRAPHIC: [string, string][] = [
  ["running tile", TILE_COLOR.running],
  ["stopped tile", TILE_COLOR.stopped],
  ["busy tile", TILE_COLOR.busy],
  ["failed tile", TILE_COLOR.failed],
  ["+N tile", TILE_COLOR.more],
  ["moving tile outline", TILE_STROKE.migrating!],
  ["arriving tile outline", TILE_STROKE.ghost!],
  ["warm block", HEAT_COLOR.warm],
  ["hot block", HEAT_COLOR.hot],
  ["local pool", POOL_COLOR.cool],
  ["full pool", POOL_COLOR.hot],
  ["uplinks up", UPLINK_COLOR.up],
  ["uplinks degraded", UPLINK_COLOR.degraded],
  ["no uplink", UPLINK_COLOR.down],
  ["migration arrow", LINK_COLOR.migration],
  ["recommendation arrow", LINK_COLOR.recommendation],
  // 네트워크 층(계획 75): 포트·본딩 테두리·레일 선, 레일 띠(윗면 밝기 그대로)
  ["port up", PORT_COLOR.up],
  ["port degraded", PORT_COLOR.degraded],
  ["port down", PORT_COLOR.down],
  ["network rail", shade(RAIL_COLOR, "top")],
];

describe.each(Object.entries(THEMES))("%s theme map colors", (_theme, vars) => {
  it.each(GRAPHIC)("%s is at least 3:1 on the card and the background", (_name, color) => {
    for (const bg of ["var(--card)", "var(--background)"]) {
      expect(contrast(rgb(vars, color), rgb(vars, bg))).toBeGreaterThanOrEqual(3);
    }
  });

  it("tells the tile states apart from each other, not only from the card", () => {
    // 켜짐·실패·꺼짐은 서로도 구별된다(색 + 모양 — 실패는 × 무늬)
    expect(contrast(rgb(vars, TILE_COLOR.running), rgb(vars, TILE_COLOR.stopped))).toBeGreaterThan(
      1.1,
    );
    expect(contrast(rgb(vars, TILE_COLOR.more), rgb(vars, TILE_COLOR.stopped))).toBeGreaterThan(
      1.1,
    );
  });
});

it("draws the network layer with the same colours as the legend", () => {
  // 늦게 받는 네트워크 층 조각은 지도 조각의 `colors.ts`를 읽지 않고 같은 값을 따로 둔다(`netPaths.ts` 머리 설명)
  expect(NET_COLOR).toEqual(PORT_COLOR);
  expect(NET_RAIL).toBe(RAIL_COLOR);
});
