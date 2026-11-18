// 디자인 토큰의 대비를 셈한다(WCAG 2.x). `index.css`의 oklch 값을 sRGB로 바꿔 상대 휘도를 구한다.
// 브라우저 없이 도는 순수 함수다(jsdom은 색을 계산하지 않는다).

/** 0~1 sRGB(감마 적용) 값과 불투명도 */
export interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

const clamp = (v: number) => Math.min(1, Math.max(0, v));

function encode(linear: number): number {
  const v = clamp(linear);
  return v <= 0.0031308 ? 12.92 * v : 1.055 * v ** (1 / 2.4) - 0.055;
}

function decode(srgb: number): number {
  return srgb <= 0.04045 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
}

/** `oklch(L C H [/ A])`를 sRGB로. 색역 밖은 채널마다 자른다. L은 0~1 또는 %, A는 0~1 또는 %. */
export function parseOklch(text: string): Rgba {
  const m = /^oklch\(\s*([\d.]+%?)\s+([\d.]+)\s+([\d.]+)\s*(?:\/\s*([\d.]+%?))?\s*\)$/.exec(
    text.trim(),
  );
  if (!m) {
    throw new Error(`not an oklch color: ${text}`);
  }
  const num = (s: string) => (s.endsWith("%") ? parseFloat(s) / 100 : parseFloat(s));
  const l = num(m[1]!);
  const c = parseFloat(m[2]!);
  const h = (parseFloat(m[3]!) * Math.PI) / 180;
  const a = m[4] === undefined ? 1 : num(m[4]);
  const labA = c * Math.cos(h);
  const labB = c * Math.sin(h);
  // OKLab → LMS → 선형 sRGB(Björn Ottosson)
  const l_ = (l + 0.3963377774 * labA + 0.2158037573 * labB) ** 3;
  const m_ = (l - 0.1055613458 * labA - 0.0638541728 * labB) ** 3;
  const s_ = (l - 0.0894841775 * labA - 1.291485548 * labB) ** 3;
  return {
    r: encode(4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_),
    g: encode(-1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_),
    b: encode(-0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_),
    a,
  };
}

/** 반투명 색을 불투명한 바탕 위에 섞는다(브라우저처럼 감마 공간에서). */
export function over(top: Rgba, bottom: Rgba): Rgba {
  const mix = (t: number, b: number) => t * top.a + b * (1 - top.a);
  return { r: mix(top.r, bottom.r), g: mix(top.g, bottom.g), b: mix(top.b, bottom.b), a: 1 };
}

export function luminance({ r, g, b }: Rgba): number {
  return 0.2126 * decode(r) + 0.7152 * decode(g) + 0.0722 * decode(b);
}

/** 대비 비율(1~21). 앞 색이 반투명이면 바탕 위에 섞어서 잰다. */
export function contrast(fg: Rgba, bg: Rgba): number {
  const a = luminance(fg.a < 1 ? over(fg, bg) : fg);
  const b = luminance(bg);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

/** CSS 글에서 `selector { … }` 한 덩어리의 사용자 정의 속성(`--name: value`)을 읽는다. */
export function readBlock(css: string, selector: string): Record<string, string> {
  const start = css.search(new RegExp(`(^|\\n)${selector.replace(/[.[\]]/g, "\\$&")}\\s*\\{`));
  if (start < 0) {
    throw new Error(`no block ${selector}`);
  }
  const open = css.indexOf("{", start);
  const close = css.indexOf("}", open);
  const vars: Record<string, string> = {};
  for (const m of css.slice(open + 1, close).matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    vars[m[1]!] = m[2]!.trim();
  }
  return vars;
}

/** 토큰 이름 → 색. `var(--x)`는 같은 표에서 따라간다. */
export function resolveColor(vars: Record<string, string>, name: string): Rgba {
  let value = vars[name];
  for (let i = 0; value?.startsWith("var("); i++) {
    if (i > 8) {
      throw new Error(`var loop at ${name}`);
    }
    value = vars[/^var\((--[\w-]+)\)$/.exec(value)?.[1] ?? ""];
  }
  if (value === undefined) {
    throw new Error(`no token ${name}`);
  }
  return parseOklch(value);
}
