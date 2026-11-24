import { Maximize2, Minus, Plus } from "lucide-react";
import {
  Fragment,
  memo,
  type ReactNode,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import {
  type Box,
  boxFace,
  facesPath,
  fitBox,
  gaugeFaces,
  iso,
  linkPath,
  type Point,
  pointsAttr,
  prismFaces,
  round,
  shade,
  topCenter,
  useIsoViewport,
  type ViewBox,
  viewBoxAttr,
} from "@/components/iso";
import { Button } from "@/components/ui/button";
import {
  FADED,
  GAUGE_COLOR,
  HEAT_COLOR,
  LINK_COLOR,
  NEUTRAL,
  POOL_COLOR,
  SEVERITY_TONE,
  SLAB_COLOR,
  TILE_COLOR,
  TILE_STROKE,
  tagFill,
  tagText,
  type TagTone,
  TRAY_COLOR,
  UPLINK_COLOR,
} from "@/clusterMap/colors";
import { hitTest } from "@/clusterMap/hit";
import { type Focus, isNavKey, neighbor, readingOrder } from "@/clusterMap/keys";
import {
  type MapLink,
  type MapScene as Scene,
  nodeTop,
  type NodeItem,
  type SlabItem,
  type TileCell,
} from "@/clusterMap/layout";
import { type Lod, lodFor, tilePx, tilesInView } from "@/clusterMap/lod";
import type { MapNode } from "@/clusterMap/model";
import {
  cellText,
  type MapLayer,
  type MarkTag,
  nodeMarks,
  type PathPiece,
  scenePaths,
  SHADE_OPACITY,
} from "@/clusterMap/paths";
import { scrollStep } from "@/dnd/autoscroll";
import { useDndMaybe } from "@/dnd/DndProvider";
import type { Item, Point as DndPoint, Verdict } from "@/dnd/types";

/** 그림 상자의 크기를 모를 때(첫 그리기·jsdom) 쓰는 크기 */
const DEFAULT_SIZE = { width: 960, height: 560 };
/** 끌기로 본 움직임(px) — 이보다 많이 움직였으면 누르기가 아니다 */
const CLICK_SLOP = 4;
/** 동작 줄이기에서 끌기 가장자리 보기 옮기기의 뛰기 사이(ms) — 미끄러지지 않고 한 번에 뛴다 */
const JUMP_MS = 600;

/** 그림이 연 것(열쇠: `node:`·`unplaced`·`vm:`·`more:`·`hidden:`·`ghost:`·화살표) */
export interface MapOpen {
  key: string;
  /** 서랍을 닫으면 초점을 돌려줄 그림의 원소(노드 `<g>`) */
  owner: Element | null;
}

/** 동작 줄이기(`prefers-reduced-motion: reduce`) */
export function useReducedMotion(): boolean {
  const query = "(prefers-reduced-motion: reduce)";
  const [reduced, setReduced] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const media = window.matchMedia(query);
    const change = () => setReduced(media.matches);
    media.addEventListener?.("change", change);
    return () => media.removeEventListener?.("change", change);
  }, []);
  return reduced;
}

function useSize(el: HTMLElement | null): { width: number; height: number } {
  const [size, setSize] = useState(DEFAULT_SIZE);
  useLayoutEffect(() => {
    if (!el) {
      return;
    }
    const read = () => {
      const r = el.getBoundingClientRect();
      if (r.width > 0 && r.height > 0) {
        setSize((s) => (s.width === r.width && s.height === r.height ? s : r));
      }
    };
    read();
    const observer = new ResizeObserver(read);
    observer.observe(el);
    return () => observer.disconnect();
  }, [el]);
  return size;
}

/** 상자의 화면 윤곽(육각형) — 초점·가리킨 원소의 테두리 */
function hullPath(b: Box): string {
  const t = b.z + b.h;
  return facesPath([
    [
      iso(b.x, b.y, t),
      iso(b.x + b.w, b.y, t),
      iso(b.x + b.w, b.y, b.z),
      iso(b.x + b.w, b.y + b.d, b.z),
      iso(b.x, b.y + b.d, b.z),
      iso(b.x, b.y + b.d, t),
    ],
  ]);
}

/** viewBox 좌표 → 그림 상자 안 px(`preserveAspectRatio` 기본 meet) */
function toPixel(view: ViewBox, size: { width: number; height: number }, p: Point): Point {
  const scale = Math.min(size.width / view.w, size.height / view.h);
  const ox = (size.width - view.w * scale) / 2;
  const oy = (size.height - view.h * scale) / 2;
  return { x: ox + (p.x - view.x) * scale, y: oy + (p.y - view.y) * scale };
}

/** 화면 점 → SVG 사용자 좌표(viewBox — 정확한 셈은 브라우저의 행렬, 없으면 `null`) */
function toScene(svg: SVGSVGElement, clientX: number, clientY: number): Point | null {
  const ctm = svg.getScreenCTM?.();
  if (!ctm || typeof svg.createSVGPoint !== "function") {
    return null;
  }
  const pt = svg.createSVGPoint();
  pt.x = clientX;
  pt.y = clientY;
  const p = pt.matrixTransform(ctm.inverse());
  return { x: p.x, y: p.y };
}

/** 열쇠가 가리키는 것의 화면 점(카드 자리·보기 옮기기) */
function anchorOf(scene: Scene, key: string): Point | null {
  const node = scene.byKey.get(key);
  if (node) {
    return nodeTop(node);
  }
  const tile = scene.tiles.get(key);
  if (tile) {
    return topCenter(tile.cell.box);
  }
  const link = scene.links.find((l) => l.key === key);
  return link
    ? { x: (link.from.x + link.to.x) / 2, y: (link.from.y + link.to.y) / 2 - link.lift / 2 }
    : null;
}

/** 열쇠의 주인 노드 열쇠 */
function ownerOf(scene: Scene, key: string): string | null {
  if (scene.byKey.has(key)) {
    return key;
  }
  const tile = scene.tiles.get(key);
  if (tile) {
    return tile.node.key;
  }
  const link = scene.links.find((l) => l.key === key);
  return link ? link.ends[0] : null;
}

/** 가리킨 것과 이어진 것만 진하게(원소를 다시 그리지 않고 `<style>` 한 줄 — 토폴로지 `glowStyle`과 같은 꼴) */
function glowStyle(svg: string, scene: Scene, lit: string | null, problems: boolean): string {
  let css = problems ? `#${svg} g[data-key]:not([data-problem]){opacity:.3}` : "";
  if (!lit) {
    return css;
  }
  const owner = ownerOf(scene, lit);
  const keys = new Set<string>(owner ? [owner] : []);
  const links = scene.links.filter(
    (l) => l.key === lit || (owner !== null && l.ends.includes(owner)),
  );
  for (const l of links) {
    keys.add(l.ends[0]);
    keys.add(l.ends[1]);
  }
  css += `#${svg} g[data-key]{opacity:.35}#${svg} [data-link]{opacity:.25}`;
  css += [...keys].map((k) => `#${svg} g[data-key="${k}"]`).join(",") + "{opacity:1}";
  if (links.length > 0) {
    css += links.map((l) => `#${svg} [data-link="${l.key}"]`).join(",") + "{opacity:1}";
  }
  return css;
}

/** 끌기 틀의 판정 → 밝히기 수준 */
export function levelOf(v: Verdict): "ok" | "warn" | "blocked" {
  return !v.ok ? "blocked" : v.warn ? "warn" : "ok";
}

/**
 * 끌어 옮기기의 밝히기(계획 71 §10.1): 노드마다 놓을 수 있음(브랜드 테두리)·경고(노랑 테두리)·막힘(빗금 흐림), 포인터 아래
 * 노드는 더 굵게. 원래 노드는 꼬리표("여기서")만. 블록을 다시 그리지 않고 `<style>` 한 장이다.
 */
function dragStyle(
  svg: string,
  defs: string,
  levels: readonly [string, "ok" | "warn" | "blocked"][],
  over: string | null,
): string {
  const of = (level: string) => levels.filter(([, l]) => l === level).map(([k]) => k);
  const rules: [string[], string, string][] = [
    [of("ok"), " .cm-top", "stroke:var(--brand);stroke-width:3"],
    [of("warn"), " .cm-top", "stroke:var(--warning);stroke-width:3"],
    [of("blocked"), "", "opacity:.45"],
    [of("blocked"), " .cm-top", `fill:url(#${defs}-unknown)!important`],
    [over ? [over] : [], " .cm-top", "stroke-width:5"],
  ];
  return rules
    .filter(([keys]) => keys.length > 0)
    .map(([keys, tail, body]) => `${keys.map((k) => `#${svg} g[data-key="${k}"]${tail}`)}{${body}}`)
    .join("");
}

/** 움직임(§8): 흐르는 점선·맥박. 동작 줄이기면 키프레임이 없다 — 점선은 멈추고 진행률 글은 남는다. */
function motionStyle(svg: string, reduced: boolean): string {
  const base =
    `#${svg} .cm-flow{stroke-dasharray:7 5}` +
    `#${svg} g[data-key]{outline:none}` +
    `#${svg} g[data-key]:focus-visible .cm-top{stroke:var(--foreground);stroke-width:2.4}`;
  if (reduced) {
    return base;
  }
  return (
    base +
    `@keyframes cm-flow{to{stroke-dashoffset:-24}}@keyframes cm-pulse{50%{opacity:.6}}` +
    `#${svg} .cm-flow{animation:cm-flow 1.2s linear infinite}` +
    `#${svg} .cm-pulse{animation:cm-pulse 2s ease-in-out infinite}` +
    `@media (prefers-reduced-motion: reduce){#${svg} .cm-flow,#${svg} .cm-pulse{animation:none}}`
  );
}

/** 글 하나(바탕색 테두리로 선·판 위에서도 읽힘) */
function Text({
  at,
  text,
  size = 11,
  strong = false,
  muted = false,
  anchor = "middle",
  link,
}: {
  at: Point;
  text: string;
  size?: number;
  strong?: boolean;
  muted?: boolean;
  anchor?: "start" | "middle" | "end";
  link?: string;
}) {
  return (
    <text
      x={round(at.x)}
      y={round(at.y)}
      textAnchor={anchor}
      fontSize={size}
      fontWeight={strong ? 600 : undefined}
      style={{ fill: muted ? "var(--muted-foreground)" : "var(--foreground)" }}
      stroke="var(--background)"
      strokeWidth={3}
      paintOrder="stroke"
      strokeLinejoin="round"
      data-link={link}
    >
      {text}
    </text>
  );
}

const TAG_TONE: Record<Exclude<MarkTag, "alerts">, TagTone> = {
  offline: "danger",
  frozen: "info",
  maintenance: "warning",
  draining: "warning",
  leader: "neutral",
  overcommit: "danger",
  gateway: "info",
  fence: "danger",
};

/** 판 하나: 면 셋 + 이름 글(스토리지 레이어에 공유 풀이 있으면 앞 가장자리 띠 + 글) */
const Slab = memo(function Slab({
  slab,
  name,
  shared,
}: {
  slab: SlabItem;
  name: string;
  shared: string | null;
}) {
  const b = slab.box;
  const color = slab.kind === "unplaced" ? TRAY_COLOR : SLAB_COLOR;
  const front = iso(b.x, b.y + b.d, 0);
  return (
    <>
      {(["left", "right", "top"] as const).map((face) => (
        <polygon
          key={face}
          points={pointsAttr(boxFace(b, face))}
          style={{ fill: shade(color, face) }}
          stroke="color-mix(in oklab, var(--muted-foreground) 35%, transparent)"
          strokeWidth={0.6}
        />
      ))}
      <Text at={{ x: front.x + 6, y: front.y + 18 }} text={name} strong anchor="start" />
      {shared !== null && (
        <>
          <polygon
            points={pointsAttr(
              boxFace(
                { x: b.x + 12, y: b.y + b.d - 26, z: b.z, w: b.w - 24, d: 12, h: b.h },
                "top",
              ),
            )}
            style={{ fill: "color-mix(in oklab, var(--chart-2) 45%, var(--card))" }}
          />
          <Text
            at={{
              x: iso(b.x + 16, b.y + b.d - 14, b.z + b.h).x + 4,
              y: iso(b.x + 16, b.y + b.d - 14, b.z + b.h).y - 4,
            }}
            text={shared}
            size={10}
            muted
            anchor="start"
          />
        </>
      )}
    </>
  );
});

/**
 * 알약 너비(10px 굵은 글): 라틴 글자는 약 6px, 한글처럼 넓은 글자(라틴 확장 B `ɏ` 뒤)는 약 10px — 재지 않고 셈한다
 * (원소 수·그리기 한 번 그대로).
 */
export function pillWidth(text: string): number {
  let w = 0;
  for (const ch of text) {
    w += ch > "\u024f" ? 10 : 6;
  }
  return w + 12;
}

/** 노드 아래의 알약 꼬리표(알약 + 글) */
function Tags({ at, tags }: { at: Point; tags: { key: string; text: string; tone: TagTone }[] }) {
  const widths = tags.map((tag) => pillWidth(tag.text));
  const total = widths.reduce((a, b) => a + b, 0) + Math.max(0, tags.length - 1) * 4;
  const lefts = widths.map(
    (_, i) => at.x - total / 2 + widths.slice(0, i).reduce((a, b) => a + b + 4, 0),
  );
  return (
    <>
      {tags.map((tag, i) => {
        const w = widths[i]!;
        const left = lefts[i]!;
        return (
          <Fragment key={tag.key}>
            <rect
              x={round(left)}
              y={round(at.y)}
              width={w}
              height={15}
              rx={7.5}
              style={{ fill: tagFill(tag.tone) }}
            />
            <text
              x={round(left + w / 2)}
              y={round(at.y + 11)}
              textAnchor="middle"
              fontSize={10}
              fontWeight={600}
              style={{ fill: tagText(tag.tone) }}
            >
              {tag.text}
            </text>
          </Fragment>
        );
      })}
    </>
  );
}

/**
 * 노드 블록의 그림(`memo` — 가리키기·초점·끌기에 다시 그리지 않는다). 원소 수는 `nodeElements`와 같다: 이름 글, 요약이면 면
 * 셋, 아니면 윗면 + 채움 면 둘(`IsoGauge` 꼴의 다각형 둘씩), 자세히면 부하 글, 꼬리표마다 알약·글, 줄무늬·과다 할당 띠,
 * 레이어(받침·빗금·눈금·업링크 띠·펜싱 없음 둘레).
 */
const NodeShape = memo(function NodeShape({
  item,
  info,
  lod,
  layersKey,
  defs,
}: {
  item: NodeItem;
  info: MapNode | undefined;
  lod: Lod;
  /** 켠 레이어(쉼표로 이은 글 — 바뀔 때만 다시 그린다) */
  layersKey: string;
  defs: string;
}) {
  const { t } = useTranslation();
  const layers = useMemo(() => new Set(layersKey.split(",") as MapLayer[]), [layersKey]);
  if (item.kind === "unplaced") {
    return null;
  }
  const marks = nodeMarks(item, layers);
  const b = item.box;
  const top = b.z + b.h;
  const offline = item.tags.includes("offline");
  const loadOn = layers.has("load");
  const heat = loadOn && !offline ? item.heat : null;
  const base = offline ? FADED : NEUTRAL;
  const topFill =
    heat === null
      ? shade(base, "top")
      : heat === "unknown"
        ? `url(#${defs}-unknown)`
        : shade(HEAT_COLOR[heat], "top");
  const stroke = offline
    ? "var(--danger)"
    : "color-mix(in oklab, var(--muted-foreground) 70%, black)";
  const strokeWidth = offline ? 2 : 0.8;
  const front = iso(b.x + b.w, b.y + b.d, b.z);
  const tagItems = marks.tags.map((tag) => ({
    key: tag,
    text: tagLabel(t, tag, info),
    tone: tag === "alerts" ? SEVERITY_TONE[info?.severity ?? "warning"] : TAG_TONE[tag],
  }));
  // 이름·부하 글·꼬리표는 블록의 왼쪽(앞) 면 위에 — 블록 아래 바닥에 두면 앞 줄 블록의 이름과 헷갈린다. 증인은 기둥 아래.
  const face = item.kind === "witness" ? front : iso(b.x + b.w / 2, b.y + b.d, b.z + b.h);
  const textY = face.y + (item.kind === "witness" ? 16 : 13);
  const tagsY = textY + (lod === "detail" ? 16 : 4);

  if (item.kind === "witness") {
    const f = prismFaces(b.x + b.w / 2, b.y + b.d / 2, b.z, (b.w / 2) * Math.SQRT2, b.h, 4);
    const color = offline ? FADED : NEUTRAL;
    return (
      <>
        {f.sides.map((side, i) => (
          <polygon
            key={i}
            points={pointsAttr(side)}
            style={{ fill: shade(color, i === 0 ? "left" : "right") }}
            stroke={stroke}
            strokeWidth={strokeWidth}
          />
        ))}
        <polygon
          className="cm-top"
          points={pointsAttr(f.top)}
          style={{ fill: shade(color, "top") }}
          stroke={stroke}
          strokeWidth={strokeWidth}
        />
        <Text at={{ x: face.x, y: textY }} text={item.name} size={10} muted />
        <Tags at={{ x: face.x, y: tagsY }} tags={tagItems} />
      </>
    );
  }

  const load = info?.load ?? null;
  const pool = item.layers?.pools ?? [];
  // 받침: 로컬 풀마다 한 조각(블록 앞 바닥), 색은 가장 나쁜 사용률(준비 안 됨은 따로 빗금)
  const segment = (i: number): Box => {
    const w = (b.w - (pool.length - 1) * 4) / pool.length;
    return { x: b.x + i * (w + 4), y: b.y + b.d + 6, z: b.z, w, d: 10, h: 0 };
  };
  const worst = pool.some((p) => p.tone === "hot")
    ? "hot"
    : pool.some((p) => p.tone === "warm")
      ? "warm"
      : "cool";
  return (
    <>
      {marks.ring && (
        <path
          d={facesPath([
            boxFace({ ...b, x: b.x - 8, y: b.y - 8, w: b.w + 16, d: b.d + 16, h: 0 }, "top"),
          ])}
          fill="none"
          stroke="var(--muted-foreground)"
          strokeWidth={1.4}
          strokeDasharray="4 4"
        />
      )}
      {marks.pedestal && (
        <path
          d={facesPath(pool.map((_, i) => boxFace(segment(i), "top")))}
          style={{ fill: POOL_COLOR[worst] }}
        />
      )}
      {marks.poolBad && (
        <path
          d={facesPath(
            pool.flatMap((p, i) => (p.tone === "bad" ? [boxFace(segment(i), "top")] : [])),
          )}
          fill={`url(#${defs}-bad)`}
        />
      )}
      {marks.sharedTick && (
        <path
          d={facesPath([
            boxFace({ x: b.x + b.w + 4, y: b.y + b.d + 4, z: b.z, w: 5, d: 5, h: 16 }, "left"),
          ])}
          style={{ fill: "var(--warning)" }}
        />
      )}
      {lod === "summary" ? (
        <>
          {(["left", "right"] as const).map((face) => (
            <polygon
              key={face}
              points={pointsAttr(boxFace(b, face))}
              style={{ fill: shade(base, face) }}
              stroke={stroke}
              strokeWidth={strokeWidth}
            />
          ))}
        </>
      ) : (
        <>
          <Gauge box={b} face="left" fraction={loadOn ? (load?.mem ?? 0) / 100 : 0} base={base} />
          <Gauge box={b} face="right" fraction={loadOn ? (load?.cpu ?? 0) / 100 : 0} base={base} />
        </>
      )}
      <polygon
        className="cm-top"
        points={pointsAttr(boxFace(b, "top"))}
        style={{ fill: topFill }}
        stroke={stroke}
        strokeWidth={strokeWidth}
        strokeLinejoin="round"
      />
      {marks.stripe && (
        <polygon
          points={pointsAttr(
            boxFace({ x: b.x, y: b.y + b.d - 9, z: top, w: b.w, d: 9, h: 0 }, "top"),
          )}
          fill={`url(#${defs}-stripes)`}
        />
      )}
      {marks.overcommit && (
        <polygon
          points={pointsAttr(boxFace({ ...b, z: top - 5, h: 5 }, "left"))}
          style={{ fill: "var(--danger)" }}
        />
      )}
      {marks.uplink && item.layers?.uplink && (
        <path
          d={facesPath([boxFace({ ...b, h: 5 }, "left"), boxFace({ ...b, h: 5 }, "right")])}
          style={{ fill: UPLINK_COLOR[item.layers.uplink] }}
        />
      )}
      <Text at={{ x: face.x, y: textY }} text={item.name} strong />
      {lod === "detail" && (
        <Text
          at={{ x: face.x, y: textY + 13 }}
          text={
            load
              ? t("clusterMap.loadShort", { cpu: Math.round(load.cpu), mem: Math.round(load.mem) })
              : t("clusterMap.label.loadUnknown")
          }
          size={10}
          muted
        />
      )}
      <Tags at={{ x: face.x, y: tagsY }} tags={tagItems} />
    </>
  );
});

/** 옆면 하나를 채움(아래)과 나머지(위) 두 다각형으로 */
function Gauge({
  box,
  face,
  fraction,
  base,
}: {
  box: Box;
  face: "left" | "right";
  fraction: number;
  base: string;
}) {
  const g = gaugeFaces(box, face, fraction);
  const pct = fraction * 100;
  const fill = GAUGE_COLOR[pct >= 90 ? "hot" : pct >= 70 ? "warm" : "cool"];
  return (
    <>
      <polygon points={pointsAttr(g.rest)} style={{ fill: shade(base, face) }} />
      <polygon points={pointsAttr(g.fill)} style={{ fill: shade(fill, face) }} />
    </>
  );
}

type Translate = (key: string, values?: Record<string, unknown>) => string;

function tagLabel(t: Translate, tag: MarkTag, info: MapNode | undefined): string {
  // 상태 꼬리표 글은 화면 읽기 이름(`clusterMap.label.*`)의 낱말을 다시 쓴다(첫 화면 묶음의 i18n을 줄임)
  switch (tag) {
    case "draining":
      return t("clusterMap.label.draining", {
        moving: info?.outgoing ?? 0,
        total: info?.pinnedRunning ?? 0,
      });
    case "alerts":
      return `⚠ ${info?.alerts ?? 0}`;
    case "overcommit":
      return t("clusterMap.tagAlloc", { pct: info?.allocatedPct ?? 0 });
    case "leader":
      return `★ ${t("clusterMap.label.leader")}`;
    case "gateway":
      return "GW";
    case "fence":
      return t("clusterMap.tagFence");
    default:
      return t(`clusterMap.label.${tag}`);
  }
}

/** 경로 조각 하나(타일·막대·그늘·HA 테두리·실패 ×) */
const Piece = memo(function Piece({
  piece,
  defs,
  haLayer,
}: {
  piece: PathPiece;
  defs: string;
  haLayer: boolean;
}) {
  switch (piece.kind) {
    case "shade":
      return <path d={piece.d} fill="black" fillOpacity={SHADE_OPACITY} aria-hidden />;
    case "ha":
      return (
        <path
          d={piece.d}
          fill="none"
          stroke={haLayer ? "var(--brand)" : "color-mix(in oklab, var(--card) 70%, white)"}
          strokeWidth={haLayer ? 2 : 1.2}
          aria-hidden
        />
      );
    case "cross":
      return (
        <path
          d={piece.d}
          fill="none"
          stroke="var(--background)"
          strokeWidth={1.6}
          strokeLinecap="round"
          aria-hidden
        />
      );
    case "bar":
      return (
        <path
          d={piece.d}
          style={{ fill: shade(TILE_COLOR[piece.tone!], piece.face!) }}
          aria-hidden
        />
      );
    case "tile": {
      const tone = piece.tone as keyof typeof TILE_COLOR;
      const outline = TILE_STROKE[tone as keyof typeof TILE_STROKE];
      const face = piece.face === "top" ? "top" : "left";
      const fill =
        tone === "ghost"
          ? "none"
          : piece.stale && face === "top"
            ? `url(#${defs}-unknown)`
            : shade(TILE_COLOR[tone], face);
      return (
        <path
          d={piece.d}
          className={tone === "busy" ? "cm-pulse" : undefined}
          style={{ fill }}
          fillOpacity={piece.stale && face !== "top" ? 0.55 : undefined}
          stroke={outline}
          strokeWidth={outline ? 1.2 : undefined}
          strokeDasharray={tone === "ghost" ? "3 2" : undefined}
          aria-hidden
        />
      );
    }
  }
});

/** 칸 위 글(자세히: 이름 앞 8자, "+N")과 규칙 어김 삼각형 */
function CellMarks({
  node,
  lod,
  vmNames,
}: {
  node: NodeItem;
  lod: Lod;
  vmNames: ReadonlyMap<string, string>;
}) {
  const cells = node.grid?.cells ?? [];
  return (
    <>
      {cells.map((cell) =>
        cellText(node, cell, lod) ? (
          <CellText
            key={cell.key}
            cell={cell}
            lod={lod}
            text={
              cell.kind === "more" || cell.kind === "hidden"
                ? `+${cell.count}`
                : (vmNames.get(cell.vm ?? "") ?? "").slice(0, 8)
            }
          />
        ) : null,
      )}
    </>
  );
}

function CellText({ cell, lod, text }: { cell: TileCell; lod: Lod; text: string }) {
  const c = topCenter(cell.box);
  const top = boxFace(cell.box, "top");
  return (
    <>
      <text
        x={round(c.x)}
        y={round(c.y + 2.5)}
        textAnchor="middle"
        fontSize={7}
        fontWeight={600}
        style={{ fill: "var(--foreground)" }}
        stroke="var(--background)"
        strokeWidth={2}
        paintOrder="stroke"
        aria-hidden
      >
        {text}
      </text>
      {lod === "detail" && cell.warn && (
        <polygon
          points={pointsAttr([
            top[0]!,
            { x: top[0]!.x + 5, y: top[0]!.y + 8 },
            { x: top[0]!.x - 5, y: top[0]!.y + 8 },
          ])}
          style={{ fill: "var(--warning)" }}
          stroke="var(--foreground)"
          strokeWidth={0.6}
          aria-hidden
        />
      )}
    </>
  );
}

/** 화살표 하나: 휜 선 + 가운데 글(마이그레이션은 흐르는 점선, 추천은 멈춘 회색 점선) */
function Link({ link, text, defs }: { link: MapLink; text: string; defs: string }) {
  const migration = link.kind === "migration";
  return (
    <>
      <path
        d={linkPath(link.from, link.to, link.lift)}
        data-link={link.key}
        className={migration ? "cm-flow" : undefined}
        fill="none"
        style={{ stroke: LINK_COLOR[link.kind] }}
        strokeWidth={migration ? 2.6 : 1.8}
        strokeLinecap="round"
        strokeDasharray={migration ? undefined : "4 5"}
        markerEnd={migration ? `url(#${defs}-arrow)` : undefined}
        pointerEvents="stroke"
        cursor="pointer"
      />
      <Text
        at={{
          x: (link.from.x + link.to.x) / 2,
          y: (link.from.y + link.to.y) / 2 - link.lift / 2 - 6,
        }}
        text={text}
        size={11}
        strong={migration}
        muted={!migration}
        link={link.key}
      />
    </>
  );
}

export interface MapSceneProps {
  scene: Scene;
  /** 열쇠 → 노드 모델(카드·꼬리표 글) */
  nodes: ReadonlyMap<string, MapNode>;
  /** 열쇠 → 화면 읽기 이름(`mapLabels`) */
  labels: ReadonlyMap<string, string>;
  /** VM id → 이름(자세히 단계의 타일 글) */
  vmNames: ReadonlyMap<string, string>;
  layers: readonly MapLayer[];
  /** 판 이름(번역한 것) */
  slabName(slab: SlabItem): string;
  /** "문제만": 문제 없는 노드를 흐리게(원소는 그대로) */
  problemsOnly?: boolean;
  /** 키 안내 글의 id(`aria-describedby`) */
  describedBy?: string;
  /**
   * 주소의 `?focus=`나 목록에서 돌아올 때 맞출 원소 — `seq`가 바뀔 때마다 맞춘다. `dom`이면 화면 초점도 그리로(주소),
   * 아니면 탭 차례 원소만(목록에서 돌아옴 — 화면 초점은 전환 단추에).
   */
  focusRequest?: { focus: Focus; seq: number; dom: boolean } | null;
  onOpen(open: MapOpen): void;
  /** Shift+Enter·더블클릭·Ctrl/⌘+클릭 — 상세 화면(없는 것은 서랍, 그래서 주인 원소도) */
  onDetail(open: MapOpen): void;
  /** 요약 단계에서 `]`: 그 노드의 VM 목록 */
  onSummaryEnter(nodeKey: string, owner: Element | null): void;
  onFocusChange?(focus: Focus | null): void;
  onLod?(lod: Lod): void;
  /** 가리킨·초점 원소의 카드 몸통 */
  renderCard(key: string): ReactNode;
  /** 끌어 옮기기(계획 71 §10) — 쪽이 그림을 `DndProvider` 안에 두고 준다. 없으면 끌기가 없다. */
  drag?: MapDrag;
}

/** 지도 끌어 옮기기에서 쪽이 주는 것 */
export interface MapDrag {
  /**
   * VM 타일 열쇠 → 끌 수 있으면 끌 것(VM id·이름), 끌 수 없으면 `m`이 읽을 까닭(번역한 글), VM 타일이 아니면(`+N`) null
   */
  item(tileKey: string): Item | string | null;
  /** 노드 열쇠 → 메모리 여유 글(키보드 옮기기가 노드마다 읽음, 모르면 null) */
  free(nodeKey: string): string | null;
  /** 든 VM이 바뀜(쪽이 그 VM의 CPU·NUMA 맞음을 묻는다) */
  onHold(vm: string | null): void;
}

/**
 * 2.5D 클러스터 지도(SVG 하나 — 계획 70 §5): 판 → 띠마다 노드 블록과 그 띠의 합친 타일 경로 → 화살표. 노드는
 * `<g role="button">`이고 탭 차례는 하나(나머지 `tabindex=-1`), 화살표가 이웃 노드로, `]`·`[`가 VM 타일 단계로 들고 난다.
 * 타일은 원소가 아니라 경로 조각이라 가리키기·누르기는 적중 셈(`hitTest`)이 정하고, 초점·가리킨 것만 따로 테두리를 그린다.
 * 가리키기·초점·끌기·확대는 블록·경로를 다시 그리지 않는다(블록은 `memo`, 밝히기는 `<style>` 한 줄).
 */
export function MapScene(props: MapSceneProps) {
  const { scene, nodes, labels, layers } = props;
  const { t } = useTranslation();
  const defs = useId().replace(/:/g, "");
  const svgId = `${defs}-map`;
  const reduced = useReducedMotion();
  const [wrap, setWrap] = useState<HTMLDivElement | null>(null);
  const size = useSize(wrap);
  const fit = useMemo(() => fitBox(scene.bounds, 40), [scene.bounds]);
  const viewport = useIsoViewport(fit);
  const { view, attach, handlers, zoomIn, zoomOut, reset, reveal, pan } = viewport;

  // 자세히 보기 단계(문턱 둘레 이력 — 지난 단계를 기억)
  const [prevLod, setPrevLod] = useState<Lod | null>(null);
  const lod = lodFor(
    { tilePx: tilePx(view, size), vms: scene.tiles.size, inView: tilesInView(scene, view) },
    prevLod ?? undefined,
  );
  if (lod !== prevLod) {
    setPrevLod(lod);
  }
  const paths = useMemo(() => scenePaths(scene, lod), [scene, lod]);
  const layersKey = layers.join(",");
  const haLayer = layers.includes("ha");
  const showRecs = layers.includes("recommendations");

  const [focus, setFocus] = useState<Focus | null>(null);
  const [hover, setHover] = useState<string | null>(null);
  const [focused, setFocused] = useState(false);
  const [announce, setAnnounce] = useState("");
  const order = useMemo(() => readingOrder(scene), [scene]);
  // 초점이 장면에서 사라졌으면(노드를 지움) 첫 노드
  const focusNode =
    focus && scene.byKey.has(focus.kind === "node" ? focus.key : focus.node)
      ? focus.kind === "node"
        ? focus.key
        : focus.node
      : (order[0]?.key ?? null);
  const focusKey =
    focus && focus.kind === "tile" && scene.tiles.has(focus.key) ? focus.key : focusNode;

  const svgRef = useRef<SVGSVGElement | null>(null);
  const press = useRef<{ x: number; y: number } | null>(null);
  const latest = useRef(props);
  // 포인터 사건·프레임 사이에 읽는 지금 값(끌기의 칸 찾기·가장자리 보기 옮기기)
  const now = useRef({ lod, view, pan, reduced });
  useLayoutEffect(() => {
    latest.current = props;
    now.current = { lod, view, pan, reduced };
  });
  const { onLod, onFocusChange } = props;
  useEffect(() => onLod?.(lod), [onLod, lod]);
  useEffect(() => onFocusChange?.(focus), [onFocusChange, focus]);

  const nodeEl = (key: string): SVGGElement | null =>
    svgRef.current?.querySelector<SVGGElement>(`g[data-key="${key}"]`) ?? null;

  const moveTo = (next: Focus, dom = true, quiet = false) => {
    setFocus(next);
    const key = next.kind === "node" ? next.key : next.key;
    if (!quiet) {
      setAnnounce(labels.get(key) ?? "");
    }
    const at = anchorOf(scene, key);
    if (at) {
      reveal(at.x, at.y);
    }
    const el = nodeEl(next.kind === "node" ? next.key : next.node);
    if (dom && el && document.activeElement !== el) {
      el.focus();
    }
  };

  // 주소(`?focus=`)·목록에서 돌아옴: 그 원소로 맞추고 초점을 준다(서랍은 열지 않음). 요청(`seq`)이 바뀔 때만 —
  // 장면이 바뀌어도 다시 맞추지 않는다. 초점 상태는 그리는 동안 고치고, 보기 옮기기·화면 초점은 그린 뒤에.
  const request = props.focusRequest;
  const [handled, setHandled] = useState<typeof request>(null);
  if (request && request.seq !== handled?.seq) {
    setHandled(request);
    setFocus(request.focus);
  }
  useEffect(() => {
    if (!handled) {
      return;
    }
    const f = handled.focus;
    const at = anchorOf(latest.current.scene, f.key);
    if (at) {
      reveal(at.x, at.y);
    }
    if (handled.dom) {
      svgRef.current
        ?.querySelector<SVGGElement>(`g[data-key="${f.kind === "node" ? f.key : f.node}"]`)
        ?.focus();
    }
    // 요청마다 한 번
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [handled]);

  const hitAt = (e: React.PointerEvent | React.MouseEvent): string | null => {
    const target = e.target as Element;
    const link = target.closest?.("[data-link]")?.getAttribute("data-link");
    if (link) {
      return link;
    }
    const svg = svgRef.current;
    const p = svg ? toScene(svg, e.clientX, e.clientY) : null;
    const hit = p ? hitTest(scene, lod, p) : null;
    if (hit) {
      return hit.key;
    }
    return target.closest?.("g[data-key]")?.getAttribute("data-key") ?? null;
  };

  const open = (key: string, detail: boolean) => {
    const owner = ownerOf(scene, key);
    const at = { key, owner: owner ? nodeEl(owner) : null };
    if (detail) {
      latest.current.onDetail(at);
    } else {
      latest.current.onOpen(at);
    }
  };

  // 끌어 옮기기(계획 71 §10): 켜진 VM 타일에서 시작한 누르기만 끌기이고 나머지는 보기 움직이기 그대로다. 놓을 자리는
  // 노드(`hitTest` — 타일을 맞히면 그 노드), 판정은 쪽의 `accepts`(틀의 `verdict`), 놓으면 쪽의 `onDrop`(대화상자).
  const dnd = useDndMaybe();
  const drag = dnd && props.drag ? props.drag : null;
  const ds = dnd?.state;
  const heldState = ds?.phase === "dragging" || ds?.phase === "picked" ? ds : null;
  const held = heldState?.item ?? null;
  const over = heldState?.over ?? null;
  /** 키보드 옮기기 모드(`m`) — 누르기 들기는 지도에 없으므로 든 채(`picked`)는 늘 이것이다 */
  const moving = ds?.phase === "picked";
  const [origin, setOrigin] = useState<{ node: string; tile: Focus } | null>(null);
  /** 끌기를 시작할 수 있는 누르기의 포인터 */
  const dragPointer = useRef<number | null>(null);
  /** 이 누르기가 끌기였음(뒤따르는 click은 서랍을 열지 않음) */
  const dragged = useRef(false);
  const heldId = held?.id ?? null;
  useEffect(() => {
    latest.current.drag?.onHold(heldId);
  }, [heldId]);

  /** 화면 점 → 놓을 노드 열쇠(타일을 맞히면 그 노드, "배치 안 됨" 판은 아님) */
  const findNode = useCallback((p: DndPoint): string | null => {
    const svg = svgRef.current;
    const at = svg ? toScene(svg, p.x, p.y) : null;
    const hit = at ? hitTest(latest.current.scene, now.current.lod, at) : null;
    const key = hit ? (hit.kind === "node" ? hit.key : hit.node) : null;
    return key === "unplaced" ? null : key;
  }, []);
  const hasDrag = drag !== null;
  const surface = dnd?.surface;
  useEffect(() => {
    if (!hasDrag || !surface) {
      return;
    }
    return surface({
      find: findNode,
      ids: () => latest.current.scene.nodes.filter((n) => n.kind !== "unplaced").map((n) => n.key),
      label: (id) => latest.current.scene.byKey.get(id)?.name ?? id,
    });
    // 틀의 `surface`는 상태마다 새 함수지만 하는 일(같은 모음에 넣기)은 같다 — 끌기가 있는 동안 한 번 단다.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hasDrag, findNode]);

  // 끄는 동안 그림 가장자리 48px 안이면 보기를 그쪽으로 옮긴다(동작 줄이기면 미끄러지지 않고 한 번에 뛴다).
  const dragging = ds?.phase === "dragging";
  const send = dnd?.send;
  useEffect(() => {
    const svg = svgRef.current;
    if (!dragging || !svg || !send) {
      return;
    }
    let p: DndPoint | null = null;
    let frame = 0;
    let jumped = -Infinity;
    const track = (e: PointerEvent) => {
      p = { x: e.clientX, y: e.clientY };
    };
    const tick = () => {
      frame = requestAnimationFrame(tick);
      const r = svg.getBoundingClientRect();
      const { dx, dy } = p ? scrollStep(r, p) : { dx: 0, dy: 0 };
      if (!p || (dx === 0 && dy === 0) || r.width === 0) {
        return;
      }
      const { view: v, pan: move, reduced: still } = now.current;
      if (still) {
        const at = performance.now();
        if (at - jumped < JUMP_MS) {
          return;
        }
        jumped = at;
        move((Math.sign(dx) * v.w) / 3, (Math.sign(dy) * v.h) / 3);
      } else {
        move((dx * v.w) / r.width, (dy * v.h) / r.width);
      }
      send({ type: "move", point: p, slot: findNode(p) });
    };
    addEventListener("pointermove", track);
    frame = requestAnimationFrame(tick);
    return () => {
      removeEventListener("pointermove", track);
      cancelAnimationFrame(frame);
    };
  }, [dragging, send, findNode]);

  /** 누른 곳의 VM 타일(요약 단계에는 타일이 없다) */
  const tileAt = (e: React.PointerEvent): { key: string; node: string } | null => {
    const svg = svgRef.current;
    const p = svg ? toScene(svg, e.clientX, e.clientY) : null;
    const hit = p ? hitTest(scene, lod, p) : null;
    return hit?.kind === "tile" ? hit : null;
  };
  /** 들기 시작: 원래 노드를 기억하고(꼬리표·Esc로 돌아올 곳), 취소하면 틀이 초점을 그 노드로 돌린다. */
  const hold = (item: Item, tile: { key: string; node: string }) => {
    setOrigin({ node: tile.node, tile: { kind: "tile", node: tile.node, key: tile.key } });
    dnd!.item(item.id, nodeEl(tile.node));
  };
  /** 노드 위에서 읽을 말: 놓을 수 있음·경고·까닭, 놓을 수 있으면 메모리 여유 */
  const overText = (item: Item, key: string): string => {
    const v: Verdict = dnd!.verdict(item, key);
    const slot = scene.byKey.get(key)?.name ?? key;
    const why = (r?: { key: string; values?: Record<string, string | number> }) =>
      r ? t(r.key, r.values) : "";
    const text = v.ok
      ? v.warn
        ? t("dnd.overWarn", { slot, warn: why(v.warn) })
        : t("dnd.overOk", { slot })
      : v.reason
        ? t("dnd.overNo", { slot, reason: why(v.reason) })
        : t("dnd.overNoPlain", { slot });
    const free = v.ok ? drag?.free(key) : null;
    return free ? `${text} ${free}` : text;
  };

  /** 옮기기 모드의 키(§10.3): 화살표·Home·End로 노드, Enter·Space로 놓기, Esc로 취소, Tab은 취소하고 나감 */
  const onMoveKey = (e: React.KeyboardEvent<SVGGElement>, item: Item) => {
    const at = over ?? origin?.node ?? null;
    const back = () => {
      if (origin) {
        setFocus(origin.tile);
      }
    };
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      dnd!.send({ type: "cancel" });
      back();
    } else if (e.key === "Tab") {
      dnd!.send({ type: "cancel" });
      back();
    } else if ((e.key === "Enter" || e.key === " ") && at) {
      e.preventDefault();
      e.stopPropagation();
      dnd!.send({ type: "drop", slot: at });
    } else if (at && isNavKey(e.key) && e.key !== "[" && e.key !== "]") {
      e.preventDefault();
      e.stopPropagation();
      const next = neighbor(scene, { kind: "node", key: at }, e.key);
      if (next?.kind === "node" && next.key !== "unplaced") {
        moveTo(next, true, true);
        dnd!.send({ type: "over", slot: next.key });
        dnd!.announce(overText(item, next.key));
      }
    }
  };

  /** `m`(VM 타일 단계): 옮기기 모드. 끌 수 없는 VM이면 까닭만 읽는다. */
  const startMove = (tile: Focus & { kind: "tile" }): boolean => {
    const it = drag?.item(tile.key) ?? null;
    if (it === null) {
      return false;
    }
    if (typeof it === "string") {
      dnd!.announce(it);
      return true;
    }
    hold(it, tile);
    dnd!.send({ type: "pick", item: it, key: true });
    setFocus({ kind: "node", key: tile.node });
    const count = scene.nodes.filter(
      (n) => n.kind !== "unplaced" && dnd!.verdict(it, n.key).ok,
    ).length;
    dnd!.announce(
      count > 0
        ? t("clusterMap.drag.start", { vm: it.label, count })
        : t("clusterMap.drag.none", { vm: it.label }),
    );
    return true;
  };

  const onKey = (e: React.KeyboardEvent<SVGGElement>, nodeKey: string) => {
    if (moving && held) {
      onMoveKey(e, held);
      return;
    }
    const current: Focus = focus && focusNode === nodeKey ? focus : { kind: "node", key: nodeKey };
    const key = current.kind === "tile" ? current.key : current.key;
    if (
      e.key === "m" &&
      !e.altKey &&
      !e.ctrlKey &&
      !e.metaKey &&
      drag &&
      current.kind === "tile" &&
      startMove(current)
    ) {
      e.preventDefault();
      e.stopPropagation();
      return;
    }
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      open(key, e.key === "Enter" && e.shiftKey);
      return;
    }
    // Ctrl/⌘/Alt와 함께면 브라우저의 것(Ctrl+`-`·`+`·`0` 쪽 확대는 저시력 사용자가 쓴다 — WCAG 1.4.4)
    if (e.altKey || e.ctrlKey || e.metaKey) {
      return;
    }
    if (e.key === "+" || e.key === "=") {
      e.preventDefault();
      zoomIn();
      return;
    }
    if (e.key === "-") {
      e.preventDefault();
      zoomOut();
      return;
    }
    if (e.key === "0") {
      e.preventDefault();
      reset();
      return;
    }
    if (!isNavKey(e.key)) {
      return;
    }
    // Esc는 타일 단계에서만 그림이 받는다(노드 단계면 쪽의 Esc 그대로)
    if (e.key === "Escape" && current.kind === "node") {
      return;
    }
    // `[`·`]`는 전역 단축키(사이드바 접기)와 겹친다 — 그림에 초점이 있으면 그림이 받는다.
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "]" && current.kind === "node" && lod === "summary") {
      latest.current.onSummaryEnter(nodeKey, e.currentTarget);
      return;
    }
    const next = neighbor(scene, current, e.key);
    if (next) {
      moveTo(next);
    }
  };

  // 들고 있는 동안은 포인터 아래(키보드면 고른) 노드를 둘레로, 가리키기 밝히기 대신 놓을 수 있음 밝히기
  const lit = held ? (over ?? origin?.node ?? null) : (hover ?? (focused ? focusKey : null));
  const levels =
    held && dnd
      ? scene.nodes
          .filter((n) => n.kind !== "unplaced" && n.key !== origin?.node)
          .map(
            (n) =>
              [n.key, levelOf(dnd.verdict(held, n.key))] as [string, ReturnType<typeof levelOf>],
          )
      : [];
  const here = held && origin ? scene.byKey.get(origin.node) : undefined;
  const outlineBox = (() => {
    if (!lit) {
      return null;
    }
    const tile = scene.tiles.get(lit);
    if (tile) {
      return tile.cell.box;
    }
    // 배치 안 됨 판(납작한 상자)도 — 초점이 보이게(WCAG 2.4.7)
    return scene.byKey.get(lit)?.box ?? null;
  })();

  // 장면이 바뀔 때만(가리키기·끌기에는 그대로)
  const floor = useMemo(
    () =>
      scene.slabs.map((s) => (
        <Slab
          key={s.key}
          slab={s}
          name={props.slabName(s)}
          shared={
            layers.includes("storage") && scene.shared.length > 0 && s.kind !== "unplaced"
              ? t("clusterMap.shared", { names: scene.shared.join(", ") })
              : null
          }
        />
      )),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [scene, layersKey, t],
  );
  const bandPieces = useMemo(
    () =>
      new Map(
        paths.map((band) => [
          band.band,
          <>
            {band.pieces.map((p) => (
              <Piece key={p.key} piece={p} defs={defs} haLayer={haLayer} />
            ))}
            {lod !== "summary" &&
              band.nodes.map((n) => (
                <CellMarks key={`t:${n.key}`} node={n} lod={lod} vmNames={props.vmNames} />
              ))}
          </>,
        ]),
      ),
    [paths, defs, haLayer, lod, props.vmNames],
  );
  const linkEls = useMemo(
    () =>
      scene.links
        .filter((l) => l.kind === "migration" || showRecs)
        .map((l) => <Link key={l.key} link={l} text={labels.get(l.key) ?? l.vmName} defs={defs} />),
    [scene.links, showRecs, labels, defs],
  );

  const card = held
    ? over && <p className="font-medium">{overText(held, over)}</p>
    : lit
      ? props.renderCard(lit)
      : null;
  const cardAt = lit ? anchorOf(scene, lit) : null;
  const px = cardAt ? toPixel(view, size, cardAt) : null;

  return (
    <div ref={setWrap} className="relative">
      <svg
        ref={(el) => {
          svgRef.current = el;
          attach(el);
        }}
        id={svgId}
        role="group"
        aria-label={t("clusterMap.pictureLabel")}
        aria-describedby={props.describedBy}
        viewBox={viewBoxAttr(view)}
        className="block h-[min(70vh,560px)] min-h-72 w-full touch-none rounded-lg border bg-gradient-to-b from-surface-1 to-background select-none"
        {...handlers}
        onPointerDown={(e) => {
          press.current = { x: e.clientX, y: e.clientY };
          dragged.current = false;
          // 다른 손가락이 닿으면 끌기를 그만두고 확대(보기 쪽)
          if (dnd && dragPointer.current !== null && dragPointer.current !== e.pointerId) {
            dragPointer.current = null;
            dnd.send({ type: "cancel" });
          }
          const tile =
            drag && dnd?.current().phase === "idle" && e.isPrimary && e.button === 0
              ? tileAt(e)
              : null;
          const item = tile ? drag!.item(tile.key) : null;
          if (tile && item && typeof item === "object") {
            dragPointer.current = e.pointerId;
            hold(item, tile);
            dnd!.send({
              type: "down",
              item,
              point: { x: e.clientX, y: e.clientY },
              touch: e.pointerType === "touch",
            });
            // 마우스·펜은 보기를 움직이지 않는다. 터치는 길게 누르기 전에 움직이면 보기 움직이기라 넘겨 둔다.
            if (e.pointerType !== "touch") {
              return;
            }
          }
          handlers.onPointerDown(e);
        }}
        onPointerMove={(e) => {
          // 끌기를 기다리거나 끄는 손가락은 보기를 움직이지 않는다(길게 누르기 전에 움직여 틀이 놓으면 다시 보기).
          if (dragPointer.current !== e.pointerId || dnd?.current().phase === "idle") {
            handlers.onPointerMove(e);
          }
          if (e.pointerType !== "touch" && !held) {
            const key = hitAt(e);
            setHover((was) => (was === key ? was : key));
          }
        }}
        onPointerUp={(e) => {
          if (dragPointer.current === e.pointerId) {
            dragPointer.current = null;
            const phase = dnd?.current().phase;
            dragged.current = phase === "dragging";
            // 끌지 않고 뗐다: 지도에는 누르기 들기가 없다 — 누르기는 서랍(click)이다.
            if (phase === "pressing") {
              dnd!.send({ type: "cancel" });
            }
          }
          handlers.onPointerUp(e);
        }}
        onPointerLeave={() => setHover(null)}
        // 길게 누르기(터치 끌기)의 문맥 메뉴를 막는다
        onContextMenu={(e) => dragPointer.current !== null && e.preventDefault()}
        onClick={(e) => {
          const from = press.current;
          press.current = null;
          if (dragged.current) {
            dragged.current = false;
            return;
          }
          if (from && Math.hypot(e.clientX - from.x, e.clientY - from.y) > CLICK_SLOP) {
            return;
          }
          const key = hitAt(e);
          if (key) {
            open(key, e.ctrlKey || e.metaKey);
          }
        }}
        onDoubleClick={(e) => {
          const key = hitAt(e);
          if (key) {
            open(key, true);
          }
        }}
      >
        <style>{glowStyle(svgId, scene, held ? null : lit, props.problemsOnly === true)}</style>
        {held && <style>{dragStyle(svgId, defs, levels, over)}</style>}
        <style>{motionStyle(svgId, reduced)}</style>
        <defs>
          {(
            [
              ["unknown", 8, NEUTRAL, "var(--muted-foreground)", 2.5],
              ["bad", 6, "var(--danger-soft)", "var(--danger)", 2.5],
              ["stripes", 10, "var(--warning)", "var(--foreground)", 4],
            ] as const
          ).map(([name, size, fill, line, width]) => (
            <pattern
              key={name}
              id={`${defs}-${name}`}
              width={size}
              height={size}
              patternUnits="userSpaceOnUse"
              patternTransform="rotate(45)"
            >
              <rect width={size} height={size} style={{ fill }} />
              <path d={`M0,0 V${size}`} style={{ stroke: line }} strokeWidth={width} />
            </pattern>
          ))}
          <marker
            id={`${defs}-arrow`}
            viewBox="0 0 10 10"
            refX="8"
            refY="5"
            markerWidth="6"
            markerHeight="6"
            orient="auto-start-reverse"
          >
            <path d="M0,0 L10,5 L0,10 z" style={{ fill: LINK_COLOR.migration }} />
          </marker>
        </defs>
        <g aria-hidden>{floor}</g>
        <g>
          {paths.map((band) => (
            <Fragment key={band.band}>
              {band.nodes.map((n) => (
                <g
                  key={n.key}
                  data-key={n.key}
                  data-problem={problem(n) ? "" : undefined}
                  role="button"
                  tabIndex={n.key === focusNode ? 0 : -1}
                  aria-label={labels.get(n.key) ?? n.name}
                  className="cursor-pointer motion-safe:transition-opacity"
                  onKeyDown={(e) => onKey(e, n.key)}
                  onFocus={() => {
                    setFocused(true);
                    if (focusNode !== n.key) {
                      setFocus({ kind: "node", key: n.key });
                    }
                  }}
                  onBlur={() => setFocused(false)}
                >
                  <NodeShape
                    item={n}
                    info={nodes.get(n.key)}
                    lod={lod}
                    layersKey={layersKey}
                    defs={defs}
                  />
                </g>
              ))}
              {bandPieces.get(band.band)}
            </Fragment>
          ))}
        </g>
        <g>{linkEls}</g>
        {here && (
          <g aria-hidden pointerEvents="none">
            <Tags
              at={{ x: nodeTop(here).x, y: nodeTop(here).y - 24 }}
              tags={[{ key: "here", text: t("clusterMap.drag.hereTag"), tone: "info" }]}
            />
          </g>
        )}
        <path
          d={outlineBox ? hullPath(outlineBox) : ""}
          fill="none"
          stroke="var(--foreground)"
          strokeWidth={2}
          strokeLinejoin="round"
          pointerEvents="none"
          aria-hidden
        />
      </svg>
      <div className="absolute top-2 right-2 flex flex-col gap-1">
        {(
          [
            ["zoomIn", Plus, zoomIn],
            ["zoomOut", Minus, zoomOut],
            ["fit", Maximize2, reset],
          ] as const
        ).map(([key, Icon, run]) => (
          <Button
            key={key}
            variant="outline"
            size="icon-sm"
            aria-label={t(`topology.${key}`)}
            onClick={run}
          >
            <Icon />
          </Button>
        ))}
      </div>
      {card && px && (
        <div
          aria-hidden
          data-map-card=""
          className="pointer-events-none absolute z-10 w-64 max-w-[calc(100%-1rem)] rounded-md border bg-popover p-3 text-xs text-popover-foreground shadow-md"
          style={{
            left: Math.max(8, Math.min(px.x + 16, size.width - 264)),
            top: Math.max(8, Math.min(px.y + 16, size.height - 160)),
          }}
        >
          {card}
        </div>
      )}
      <p aria-live="polite" className="sr-only">
        {announce}
      </p>
    </div>
  );
}

/** "문제만" 거르기의 문제: 오프라인·유지보수·경보·부하 90% 이상·실패 VM */
function problem(n: NodeItem): boolean {
  return (
    n.tags.includes("offline") ||
    n.tags.includes("maintenance") ||
    n.tags.includes("alerts") ||
    n.heat === "hot" ||
    n.counts.failed > 0
  );
}
