// 아이소메트릭(2.5D) 그림의 순수 셈(계획 67 §12.1): 투영, 상자의 보이는 면, 깊이 차례, 면 밝기. 진짜 3D(WebGL)가
// 아니라 SVG 한 장에 "3D처럼 보이는 2D"를 그린다. 계획 70의 클러스터 보기가 같은 조각을 쓴다.

/** cos 30° — 아이소메트릭 축의 가로 비율 */
export const COS30 = Math.cos(Math.PI / 6);
/** sin 30° */
export const SIN30 = 0.5;

/** 화면(SVG) 좌표 */
export interface Point {
  x: number;
  y: number;
}

/** 세계 좌표의 상자: (x, y, z)에서 x로 `w`, y로 `d`, 위로 `h` */
export interface Box {
  x: number;
  y: number;
  z: number;
  w: number;
  d: number;
  h: number;
}

/**
 * 세계 좌표 → 화면 좌표: `iso(x, y, z) = ((x − y)·cos 30°, (x + y)·sin 30° − z)`. x가 늘면 오른쪽 아래, y가 늘면 왼쪽
 * 아래, z가 늘면 위로 간다(보는 사람은 +x·+y·+z 쪽에 있다).
 */
export function iso(x: number, y: number, z = 0): Point {
  return { x: (x - y) * COS30, y: (x + y) * SIN30 - z };
}

/** SVG `points` 글 */
export function pointsAttr(points: readonly Point[]): string {
  return points.map((p) => `${round(p.x)},${round(p.y)}`).join(" ");
}

/** 소수 둘째 자리(그림 글을 짧게) */
export function round(n: number): number {
  return Math.round(n * 100) / 100;
}

/** 상자의 보이는 세 면(위, +y 쪽 "왼쪽", +x 쪽 "오른쪽")의 화면 꼭짓점 */
export function boxFaces(b: Box): { top: Point[]; left: Point[]; right: Point[] } {
  const { x, y, z, w, d, h } = b;
  const top = z + h;
  return {
    top: [iso(x, y, top), iso(x + w, y, top), iso(x + w, y + d, top), iso(x, y + d, top)],
    left: [iso(x, y + d, top), iso(x + w, y + d, top), iso(x + w, y + d, z), iso(x, y + d, z)],
    right: [iso(x + w, y, top), iso(x + w, y + d, top), iso(x + w, y + d, z), iso(x + w, y, z)],
  };
}

/** 상자 윗면의 가운데(화면) — 글과 이음선의 끝 */
export function topCenter(b: Box): Point {
  return iso(b.x + b.w / 2, b.y + b.d / 2, b.z + b.h);
}

/** 상자 바닥의 가운데(화면) */
export function bottomCenter(b: Box): Point {
  return iso(b.x + b.w / 2, b.y + b.d / 2, b.z);
}

/** 화면에서 상자가 차지하는 사각형 */
export function boxBounds(b: Box): { minX: number; minY: number; maxX: number; maxY: number } {
  const f = boxFaces(b);
  const all = [...f.top, ...f.left, ...f.right];
  return {
    minX: Math.min(...all.map((p) => p.x)),
    minY: Math.min(...all.map((p) => p.y)),
    maxX: Math.max(...all.map((p) => p.x)),
    maxY: Math.max(...all.map((p) => p.y)),
  };
}

/**
 * 깊이 열쇠(먼 것부터 그린다): 바닥 가운데의 x + y, 같으면 z. SVG에는 z-index가 없으므로 원소 순서가 곧 깊이다.
 */
export function depthOf(b: Box): number {
  return b.x + b.w / 2 + (b.y + b.d / 2) + b.z * 1e-3;
}

/** 깊이 순으로 정렬한 새 배열(같으면 원래 차례 — 안정 정렬) */
export function byDepth<T extends { box: Box; layer?: number }>(items: readonly T[]): T[] {
  return [...items].sort(
    (a, b) => (a.layer ?? 0) - (b.layer ?? 0) || depthOf(a.box) - depthOf(b.box),
  );
}

/** 면마다 밝기: 위는 밝게, 왼쪽은 그대로, 오른쪽은 어둡게(디자인 토큰 색에서 셈 — 밝음·어둠 테마 모두) */
export type Face = "top" | "left" | "right";

export function shade(color: string, face: Face): string {
  switch (face) {
    case "top":
      return `color-mix(in oklab, ${color} 78%, white)`;
    case "left":
      return color;
    case "right":
      return `color-mix(in oklab, ${color} 72%, black)`;
  }
}

/**
 * 바닥 원 위의 정다각형 기둥(육각 기둥 따위)의 윗면 꼭짓점(세계 좌표 둘레, 화면으로)과 보이는 옆면. 옆면은 바깥 법선이
 * 보는 쪽(+x·+y)을 향하는 것만 먼 것부터.
 */
export function prismFaces(
  cx: number,
  cy: number,
  z: number,
  r: number,
  h: number,
  sides = 6,
): { top: Point[]; sides: Point[][] } {
  const corners = Array.from({ length: sides }, (_, i) => {
    const a = (Math.PI * 2 * i) / sides + Math.PI / sides;
    return { x: cx + r * Math.cos(a), y: cy + r * Math.sin(a) };
  });
  const top = corners.map((c) => iso(c.x, c.y, z + h));
  const faces: { depth: number; points: Point[] }[] = [];
  corners.forEach((a, i) => {
    const b = corners[(i + 1) % sides]!;
    // 바깥 법선 = 가운데에서 두 꼭짓점의 가운데로
    const nx = (a.x + b.x) / 2 - cx;
    const ny = (a.y + b.y) / 2 - cy;
    if (nx + ny <= 0) {
      return;
    }
    faces.push({
      depth: a.x + b.x + a.y + b.y,
      points: [iso(a.x, a.y, z + h), iso(b.x, b.y, z + h), iso(b.x, b.y, z), iso(a.x, a.y, z)],
    });
  });
  faces.sort((p, q) => p.depth - q.depth);
  return { top, sides: faces.map((f) => f.points) };
}

/**
 * 두 화면 점을 잇는 SVG 경로. `lift`가 있으면 가운데를 그만큼 위로 올린 2차 곡선(휜 선)이다.
 */
export function linkPath(from: Point, to: Point, lift = 0): string {
  if (lift === 0) {
    return `M${round(from.x)},${round(from.y)} L${round(to.x)},${round(to.y)}`;
  }
  const mx = (from.x + to.x) / 2;
  const my = (from.y + to.y) / 2 - lift;
  return `M${round(from.x)},${round(from.y)} Q${round(mx)},${round(my)} ${round(to.x)},${round(to.y)}`;
}

/** 상자의 보이는 면 하나(`boxFaces`의 한 칸 — 셋을 다 셈하지 않는다) */
export function boxFace(b: Box, face: Face): Point[] {
  const { x, y, z, w, d, h } = b;
  const top = z + h;
  switch (face) {
    case "top":
      return [iso(x, y, top), iso(x + w, y, top), iso(x + w, y + d, top), iso(x, y + d, top)];
    case "left":
      return [iso(x, y + d, top), iso(x + w, y + d, top), iso(x + w, y + d, z), iso(x, y + d, z)];
    case "right":
      return [iso(x + w, y, top), iso(x + w, y + d, top), iso(x + w, y + d, z), iso(x + w, y, z)];
  }
}

/**
 * 다각형 여럿을 SVG `<path d>` 하나로(계획 70 §4): `M…L…Z`를 늘어놓는다. 채움 규칙 `nonzero`라 겹치지 않는 면이면 다각형을
 * 따로 그린 것과 같고, 겹치면 뒤에 놓은 조각이 위다(한 색이라 보이는 것은 같다). 원소 하나라 노드마다 "색 × 면" 수만큼만
 * 원소가 생긴다.
 */
export function facesPath(polys: readonly (readonly Point[])[]): string {
  const parts: string[] = [];
  for (const p of polys) {
    if (p.length < 3) {
      continue;
    }
    parts.push(`M${p.map((q) => `${round(q.x)},${round(q.y)}`).join("L")}Z`);
  }
  return parts.join(" ");
}

/** 상자 여럿의 같은 면을 경로 하나로 */
export function boxesPath(boxes: readonly Box[], face: Face): string {
  return facesPath(boxes.map((b) => boxFace(b, face)));
}

/**
 * 화면 좌표 → 높이 `z`의 세계 좌표(`iso`의 역). 높이를 알아야 한 점으로 정해진다(화면의 한 점은 세계의 (1, 1, 1) 방향 선
 * 하나다 — `z`가 1 늘면 x·y도 1씩 는다).
 */
export function screenToWorld(p: Point, z = 0): { x: number; y: number } {
  const diff = p.x / COS30;
  const sum = (p.y + z) / SIN30;
  return { x: (sum + diff) / 2, y: (sum - diff) / 2 };
}

/**
 * 채움 면(`IsoGauge`): 상자의 옆면 하나를 아래 `fraction`(0~1)만큼의 "채움"과 그 위의 "나머지" 두 다각형으로 나눈다.
 * 노드 블록의 왼쪽 면이 메모리, 오른쪽 면이 CPU 사용률이다(계획 70 §7.1).
 */
export function gaugeFaces(
  b: Box,
  face: "left" | "right",
  fraction: number,
): { fill: Point[]; rest: Point[] } {
  const f = Math.min(1, Math.max(0, Number.isFinite(fraction) ? fraction : 0));
  const filled = b.h * f;
  return {
    fill: boxFace({ ...b, h: filled }, face),
    rest: boxFace({ ...b, z: b.z + filled, h: b.h - filled }, face),
  };
}

/** 윗면 × 표시: 사각형 윗면(꼭짓점 넷)의 두 대각선을 가운데로 25% 줄인 선 둘(실패 타일·끊긴 포트) */
export function crossPath(top: readonly Point[]): string {
  const [a, b, c, d] = top as [Point, Point, Point, Point];
  const shrink = (p: Point, q: Point) => {
    const k = 0.25;
    return [
      `${round(p.x + (q.x - p.x) * k)},${round(p.y + (q.y - p.y) * k)}`,
      `${round(q.x + (p.x - q.x) * k)},${round(q.y + (p.y - q.y) * k)}`,
    ];
  };
  const [p1, p2] = shrink(a, c);
  const [p3, p4] = shrink(b, d);
  return `M${p1}L${p2}M${p3}L${p4}`;
}

/** 화면의 글 상자(왼쪽 위 `x`·`y`, 너비·높이) */
export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** 글 너비(재지 않고 셈한다 — 라틴 글자는 글자 크기의 0.6배, 한글처럼 넓은 글자는 1배, 바탕색 테두리 몫 4px) */
export function textWidth(text: string, size: number): number {
  let w = 0;
  for (const ch of text) {
    w += ch > "ɏ" ? size : size * 0.6;
  }
  return w + 4;
}

function overlaps(a: Rect, b: Rect): boolean {
  return a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
}

/**
 * 다른 글 상자(`avoid`)와 겹치지 않을 때까지 한 줄(`step`)씩 아래로 내린 만큼(0이면 그대로). `tries`번 내려도 겹치면
 * 처음 자리(0) — 바닥 글(판·레일 이름)이 노드 이름·꼬리표와 포개지지 않게.
 */
export function clearShift(r: Rect, avoid: readonly Rect[], step = 12, tries = 6): number {
  for (let i = 0; i <= tries; i++) {
    const moved = { ...r, y: r.y + i * step };
    if (!avoid.some((a) => overlaps(moved, a))) {
      return i * step;
    }
  }
  return 0;
}
