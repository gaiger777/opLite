import { Maximize2, Minus, Plus } from "lucide-react";
import {
  Fragment,
  memo,
  type ReactNode,
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

/** 그림 상자의 크기를 모를 때(첫 그리기·jsdom) 쓰는 크기 */
const DEFAULT_SIZE = { width: 960, height: 560 };
/** 끌기로 본 움직임(px) — 이보다 많이 움직였으면 누르기가 아니다 */
const CLICK_SLOP = 4;

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
  const { view, attach, handlers, zoomIn, zoomOut, reset, reveal } = viewport;

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
  useLayoutEffect(() => {
    latest.current = props;
  });
  const { onLod, onFocusChange } = props;
  useEffect(() => onLod?.(lod), [onLod, lod]);
  useEffect(() => onFocusChange?.(focus), [onFocusChange, focus]);

  const nodeEl = (key: string): SVGGElement | null =>
    svgRef.current?.querySelector<SVGGElement>(`g[data-key="${key}"]`) ?? null;

  const moveTo = (next: Focus, dom = true) => {
    setFocus(next);
    const key = next.kind === "node" ? next.key : next.key;
    setAnnounce(labels.get(key) ?? "");
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

  const onKey = (e: React.KeyboardEvent<SVGGElement>, nodeKey: string) => {
    const current: Focus = focus && focusNode === nodeKey ? focus : { kind: "node", key: nodeKey };
    const key = current.kind === "tile" ? current.key : current.key;
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

  const lit = hover ?? (focused ? focusKey : null);
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

  const card = lit ? props.renderCard(lit) : null;
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
          handlers.onPointerDown(e);
        }}
        onPointerMove={(e) => {
          handlers.onPointerMove(e);
          if (e.pointerType !== "touch") {
            const key = hitAt(e);
            setHover((was) => (was === key ? was : key));
          }
        }}
        onPointerLeave={() => setHover(null)}
        onClick={(e) => {
          const from = press.current;
          press.current = null;
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
        <style>{glowStyle(svgId, scene, lit, props.problemsOnly === true)}</style>
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
