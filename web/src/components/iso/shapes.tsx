// 아이소메트릭 그림 조각(계획 67 §12.1, 계획 70과 나눔): SVG 원소만 쓰고 색은 디자인 토큰(`var(--…)`)에서 면마다 밝기를
// 셈한다. 다루기(누르기·초점)는 쓰는 쪽이 `<g>`로 감싼다.
import {
  boxFaces,
  type Box,
  iso,
  linkPath,
  type Point,
  pointsAttr,
  prismFaces,
  round,
  shade,
} from "@/components/iso/iso";

/** 그림 하나에 한 번: 부드러운 그림자 필터와 윗면 광택 그라디언트. `id`는 같은 쪽의 다른 그림과 겹치지 않게 접두사다. */
export function IsoDefs({ id }: { id: string }) {
  return (
    <defs>
      <filter id={`${id}-shadow`} x="-50%" y="-50%" width="200%" height="200%">
        <feGaussianBlur stdDeviation="6" />
      </filter>
      <linearGradient id={`${id}-sheen`} x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stopColor="white" stopOpacity="0.35" />
        <stop offset="0.6" stopColor="white" stopOpacity="0" />
      </linearGradient>
    </defs>
  );
}

/** 바닥(z=0)에 깔리는 흐린 그림자: 상자 바닥을 조금 밀어 흐리게 */
export function IsoShadow({ box, defs, offset = 10 }: { box: Box; defs: string; offset?: number }) {
  const f = boxFaces({ ...box, x: box.x + offset / 2, y: box.y + offset, z: 0, h: 0 });
  return (
    <polygon
      points={pointsAttr(f.top)}
      fill="black"
      opacity={0.18}
      filter={`url(#${defs}-shadow)`}
      aria-hidden
    />
  );
}

/**
 * 상자 하나(세 면). `sheen`이면 윗면에 광택을 얹는다(큰 것만 — 원소 수). `strong`이면 테두리를 진하게(초점·밝힘).
 */
export function IsoBlock({
  box,
  color,
  defs,
  sheen = false,
  strong = false,
}: {
  box: Box;
  color: string;
  defs?: string;
  sheen?: boolean;
  strong?: boolean;
}) {
  const f = boxFaces(box);
  const stroke = strong ? "var(--foreground)" : `color-mix(in oklab, ${color} 60%, black)`;
  const width = strong ? 1.6 : 0.6;
  return (
    <>
      <polygon
        points={pointsAttr(f.left)}
        style={{ fill: shade(color, "left") }}
        stroke={stroke}
        strokeWidth={width}
        strokeLinejoin="round"
      />
      <polygon
        points={pointsAttr(f.right)}
        style={{ fill: shade(color, "right") }}
        stroke={stroke}
        strokeWidth={width}
        strokeLinejoin="round"
      />
      <polygon
        points={pointsAttr(f.top)}
        style={{ fill: shade(color, "top") }}
        stroke={stroke}
        strokeWidth={width}
        strokeLinejoin="round"
      />
      {sheen && defs && <polygon points={pointsAttr(f.top)} fill={`url(#${defs}-sheen)`} />}
    </>
  );
}

/** 얇은 판(네트워크): 낮은 상자 + 광택 + 앞 가장자리의 띠(폴더 색 따위) */
export function IsoSlab({
  box,
  color,
  defs,
  band,
  strong = false,
}: {
  box: Box;
  color: string;
  defs: string;
  /** 앞 왼쪽 가장자리를 따라 그리는 띠 색 */
  band?: string;
  strong?: boolean;
}) {
  const f = boxFaces(box);
  return (
    <>
      <IsoBlock box={box} color={color} defs={defs} sheen strong={strong} />
      {band && (
        <polygon
          points={pointsAttr([
            f.left[0]!,
            f.left[1]!,
            mid(f.left[1]!, f.left[2]!, 0.5),
            mid(f.left[0]!, f.left[3]!, 0.5),
          ])}
          style={{ fill: band }}
          opacity={0.85}
        />
      )}
    </>
  );
}

function mid(a: Point, b: Point, t: number): Point {
  return { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t };
}

/** 정다각형 기둥(부하 분산기의 육각 기둥) */
export function IsoPrism({
  cx,
  cy,
  z,
  r,
  h,
  color,
  sides = 6,
  strong = false,
}: {
  cx: number;
  cy: number;
  z: number;
  r: number;
  h: number;
  color: string;
  sides?: number;
  strong?: boolean;
}) {
  const f = prismFaces(cx, cy, z, r, h, sides);
  const stroke = strong ? "var(--foreground)" : `color-mix(in oklab, ${color} 60%, black)`;
  const width = strong ? 1.6 : 0.6;
  return (
    <>
      {f.sides.map((side, i) => (
        <polygon
          key={i}
          points={pointsAttr(side)}
          // 오른쪽을 향한 면일수록 어둡게
          style={{ fill: shade(color, i % 2 === 0 ? "left" : "right") }}
          stroke={stroke}
          strokeWidth={width}
          strokeLinejoin="round"
        />
      ))}
      <polygon
        points={pointsAttr(f.top)}
        style={{ fill: shade(color, "top") }}
        stroke={stroke}
        strokeWidth={width}
        strokeLinejoin="round"
      />
    </>
  );
}

/** 꼬리표(유동 IP·바깥 VIP): 세운 깃발 — 기둥 하나와 위의 판 */
export function IsoTag({
  x,
  y,
  z,
  color,
  strong = false,
}: {
  x: number;
  y: number;
  z: number;
  color: string;
  strong?: boolean;
}) {
  const foot = iso(x, y, z);
  const head = iso(x, y, z + 26);
  const flag: Point[] = [
    head,
    { x: head.x + 20, y: head.y + 4 },
    { x: head.x + 16, y: head.y + 10 },
    { x: head.x + 20, y: head.y + 16 },
    { x: head.x, y: head.y + 12 },
  ];
  return (
    <>
      <line
        x1={round(foot.x)}
        y1={round(foot.y)}
        x2={round(head.x)}
        y2={round(head.y)}
        stroke="var(--muted-foreground)"
        strokeWidth={1.4}
      />
      <polygon
        points={pointsAttr(flag)}
        style={{ fill: shade(color, "top") }}
        stroke={strong ? "var(--foreground)" : `color-mix(in oklab, ${color} 60%, black)`}
        strokeWidth={strong ? 1.6 : 0.6}
        strokeLinejoin="round"
      />
    </>
  );
}

/** 두 화면 점을 잇는 선(점선·휜 선·화살표) */
export function IsoLink({
  from,
  to,
  color = "var(--muted-foreground)",
  dashed = false,
  lift = 0,
  width = 1.4,
  arrow,
}: {
  from: Point;
  to: Point;
  color?: string;
  dashed?: boolean;
  lift?: number;
  width?: number;
  /** 화살표 끝 표시(`IsoDefs`가 아니라 쓰는 쪽의 marker id) */
  arrow?: string;
}) {
  return (
    <path
      d={linkPath(from, to, lift)}
      fill="none"
      style={{ stroke: color }}
      strokeWidth={width}
      strokeLinecap="round"
      strokeDasharray={dashed ? "5 4" : undefined}
      markerEnd={arrow ? `url(#${arrow})` : undefined}
    />
  );
}
