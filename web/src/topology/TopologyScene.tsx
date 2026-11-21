import { Maximize2, Minus, Plus } from "lucide-react";
import { memo, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  fitBox,
  iso,
  IsoBlock,
  IsoDefs,
  IsoLink,
  IsoPrism,
  IsoShadow,
  IsoSlab,
  IsoTag,
  round,
  useIsoViewport,
  viewBoxAttr,
} from "@/components/iso";
import { Button } from "@/components/ui/button";
import {
  type Direction,
  neighbor,
  related,
  type Scene,
  type SceneItem,
  type Tone,
} from "@/topology/layout";

/** 뜻 → 디자인 토큰 색(밝음·어둠 테마가 같은 이름) */
export const TONE_COLOR: Record<Tone | "link", string> = {
  overlay: "color-mix(in oklab, var(--brand) 42%, var(--card))",
  provider: "color-mix(in oklab, var(--chart-2) 45%, var(--card))",
  simple: "color-mix(in oklab, var(--muted-foreground) 30%, var(--card))",
  router: "color-mix(in oklab, var(--brand) 55%, var(--foreground))",
  running: "var(--success)",
  stopped: "color-mix(in oklab, var(--muted-foreground) 55%, var(--card))",
  failed: "var(--danger)",
  busy: "var(--info)",
  online: "var(--success)",
  offline: "var(--danger)",
  unchecked: "color-mix(in oklab, var(--muted-foreground) 70%, var(--card))",
  missing: "var(--warning)",
  warning: "var(--warning)",
  fip: "var(--info)",
  lb: "var(--chart-4)",
  link: "var(--muted-foreground)",
};

/** 선 색: 판 색은 옅어 선에는 진한 색을 쓴다 */
const LINK_COLOR: Partial<Record<Tone | "link", string>> = {
  router: "color-mix(in oklab, var(--brand) 70%, var(--foreground))",
  provider: "var(--chart-2)",
};

const KEYS: Record<string, Direction> = {
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
};

/** 밝힌 원소·선만 진하게, 나머지는 옅게 — 원소를 다시 그리지 않고 `<style>` 한 줄만 바꾼다(리뷰 1 I5). */
function glowStyle(svg: string, glow: ReadonlySet<string> | null): string {
  if (!glow) {
    return "";
  }
  const keys = [...glow];
  const items = keys.map((k) => `#${svg} g[data-key="${k}"]`).join(",");
  const links = keys.map((k) => `#${svg} g[data-link="${k}"]`).join(",");
  return (
    `#${svg} g[data-key]{opacity:.22}#${svg} g[data-link]{opacity:.12}` +
    (items ? `${items}{opacity:1}` : "") +
    (links ? `${links}{opacity:1}` : "")
  );
}

/**
 * 2.5D 토폴로지 그림(SVG 하나): 깊이 순으로 판·블록·기둥·꼬리표를 그리고, 가리키거나 초점이 간 원소와 이어진 길을 밝힌다.
 * 원소마다 `role="button"`이고 초점은 하나만 탭으로 들어오며(나머지는 `tabindex=-1`) 화살표가 이웃으로 옮긴다.
 * 가리키기·초점·끌기는 원소를 다시 그리지 않는다(원소는 `memo`, 밝히기는 `<style>`, 이름은 미리 센 `labels`).
 */
export function TopologyScene({
  scene,
  labels,
  onOpen,
  showVmNames,
}: {
  scene: Scene;
  /** 원소 열쇠 → 화면 읽기 이름 */
  labels: ReadonlyMap<string, string>;
  onOpen(item: SceneItem): void;
  /** VM 블록 아래 이름을 적는다(작은 그림만) */
  showVmNames: boolean;
}) {
  const { t } = useTranslation();
  const defs = useId().replace(/:/g, "");
  const svgId = `${defs}-scene`;
  const fit = useMemo(() => fitBox(scene.bounds), [scene.bounds]);
  const { view, attach, handlers, zoomIn, zoomOut, reset, reveal } = useIsoViewport(fit);
  const [active, setActive] = useState<string | null>(null);
  const [lit, setLit] = useState<string | null>(null);
  const current =
    active && scene.items.some((it) => it.key === active) ? active : scene.items[0]?.key;
  const glow = useMemo(() => (lit ? related(scene, lit) : null), [scene, lit]);
  // 원소의 손잡이는 열쇠만 받는 한 벌(바뀌지 않음) — 지금 장면·`reveal`·`onOpen`은 참조로 읽는다.
  const latest = useRef({ scene, reveal, onOpen });
  useLayoutEffect(() => {
    latest.current = { scene, reveal, onOpen };
  });
  const handlersFor = useMemo(
    () => ({
      open(key: string) {
        const item = latest.current.scene.items.find((it) => it.key === key);
        if (item) {
          latest.current.onOpen(item);
        }
      },
      light(key: string, on: boolean) {
        setLit((was) => (on ? key : was === key ? null : was));
      },
      focus(key: string) {
        setActive(key);
      },
      move(key: string, dir: Direction, el: SVGGElement) {
        const { scene, reveal } = latest.current;
        const next = neighbor(scene.items, key, dir);
        if (!next) {
          return;
        }
        setActive(next);
        setLit(next);
        const item = scene.items.find((it) => it.key === next);
        if (item) {
          reveal(item.center.x, item.center.y);
        }
        el.ownerSVGElement?.querySelector<SVGGElement>(`[data-key="${next}"]`)?.focus();
      },
    }),
    [],
  );
  // 그림자·선은 장면이 바뀔 때만 다시 만든다(끌기·가리키기에는 그대로).
  const shadows = useMemo(
    () => (
      <g aria-hidden>
        {scene.items
          .filter(
            (it) =>
              it.kind === "router" ||
              it.kind === "network" ||
              it.kind === "provider" ||
              it.kind === "simple",
          )
          .map((it) => (
            <IsoShadow
              key={it.key}
              box={it.box}
              defs={defs}
              offset={it.kind === "router" ? 30 : 10}
            />
          ))}
      </g>
    ),
    [scene.items, defs],
  );
  const links = useMemo(
    () => (
      <g aria-hidden>
        {scene.links.map((l) => (
          <g
            key={l.key}
            data-link={l.key}
            opacity={0.75}
            className="motion-safe:transition-opacity"
          >
            <IsoLink
              from={l.from}
              to={l.to}
              color={LINK_COLOR[l.tone] ?? TONE_COLOR[l.tone]}
              dashed={l.dashed}
              lift={l.lift}
              width={l.kind === "interface" || l.kind === "external" ? 2.2 : 1.4}
              arrow={l.kind === "migration" ? `${defs}-arrow` : undefined}
            />
            {l.label && (
              <Label
                x={(l.from.x + l.to.x) / 2}
                y={(l.from.y + l.to.y) / 2 - l.lift / 2 - 6}
                text={l.label}
                strong
              />
            )}
          </g>
        ))}
      </g>
    ),
    [scene.links, defs],
  );
  const layer = (n: number) =>
    scene.items
      .filter((it) => it.layer === n)
      .map((it) => (
        <Item
          key={it.key}
          item={it}
          defs={defs}
          active={current === it.key}
          strong={lit === it.key}
          label={labels.get(it.key) ?? it.label}
          showName={showVmNames}
          handlers={handlersFor}
        />
      ));

  return (
    <div className="relative">
      <svg
        ref={attach}
        id={svgId}
        role="group"
        aria-label={t("topology.pictureLabel")}
        viewBox={viewBoxAttr(view)}
        className="block h-[min(70svh,720px)] min-h-72 w-full touch-none rounded-lg border bg-gradient-to-b from-surface-1 to-background select-none"
        {...handlers}
      >
        <style>{glowStyle(svgId, glow)}</style>
        <IsoDefs id={defs} />
        <defs>
          <marker
            id={`${defs}-arrow`}
            viewBox="0 0 10 10"
            refX="8"
            refY="5"
            markerWidth="7"
            markerHeight="7"
            orient="auto-start-reverse"
          >
            <path d="M0,0 L10,5 L0,10 z" fill="var(--warning)" />
          </marker>
        </defs>
        {/* 그림자는 모든 것 아래 */}
        {shadows}
        {/* 바닥 판 */}
        {layer(0)}
        {/* 이음선(판 위, 블록 아래) */}
        {links}
        {/* 판 위의 것(깊이 순) */}
        {layer(1)}
      </svg>
      <div className="absolute top-2 right-2 flex flex-col gap-1">
        <Button variant="outline" size="icon-sm" aria-label={t("topology.zoomIn")} onClick={zoomIn}>
          <Plus />
        </Button>
        <Button
          variant="outline"
          size="icon-sm"
          aria-label={t("topology.zoomOut")}
          onClick={zoomOut}
        >
          <Minus />
        </Button>
        <Button variant="outline" size="icon-sm" aria-label={t("topology.fit")} onClick={reset}>
          <Maximize2 />
        </Button>
      </div>
    </div>
  );
}

/** 원소의 손잡이(열쇠를 받는 한 벌 — 원소마다 새 함수를 만들지 않는다) */
interface ItemHandlers {
  open(key: string): void;
  light(key: string, on: boolean): void;
  focus(key: string): void;
  move(key: string, dir: Direction, el: SVGGElement): void;
}

/** 글 하나(배경색 테두리로 선 위에서도 읽힘) */
function Label({
  x,
  y,
  text,
  strong = false,
  muted = false,
  anchor = "middle",
}: {
  x: number;
  y: number;
  text: string;
  strong?: boolean;
  muted?: boolean;
  anchor?: "start" | "middle" | "end";
}) {
  return (
    <text
      x={round(x)}
      y={round(y)}
      textAnchor={anchor}
      className={
        muted
          ? "fill-muted-foreground text-[10px]"
          : strong
            ? "fill-foreground text-[12px] font-semibold"
            : "fill-foreground text-[11px]"
      }
      stroke="var(--background)"
      strokeWidth={3}
      paintOrder="stroke"
      strokeLinejoin="round"
    >
      {text}
    </text>
  );
}

const Item = memo(function Item({
  item,
  defs,
  active,
  strong,
  label,
  showName,
  handlers: h,
}: {
  item: SceneItem;
  defs: string;
  active: boolean;
  strong: boolean;
  label: string;
  showName: boolean;
  handlers: ItemHandlers;
}) {
  const color = TONE_COLOR[item.tone];
  const b = item.box;
  return (
    <g
      data-key={item.key}
      role="button"
      tabIndex={active ? 0 : -1}
      aria-label={label}
      className="cursor-pointer outline-none motion-safe:transition-opacity"
      onClick={() => h.open(item.key)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          h.open(item.key);
        } else if (KEYS[e.key]) {
          e.preventDefault();
          h.move(item.key, KEYS[e.key]!, e.currentTarget);
        }
      }}
      onFocus={() => {
        h.focus(item.key);
        h.light(item.key, true);
      }}
      onBlur={() => h.light(item.key, false)}
      onPointerEnter={() => h.light(item.key, true)}
      onPointerLeave={() => h.light(item.key, false)}
    >
      <title>{label}</title>
      {item.kind === "network" || item.kind === "provider" || item.kind === "simple" ? (
        <>
          <IsoSlab
            box={b}
            color={color}
            defs={defs}
            strong={strong}
            band={
              item.kind === "provider"
                ? "var(--chart-2)"
                : item.kind === "simple"
                  ? "var(--muted-foreground)"
                  : "var(--brand)"
            }
          />
          {/* 판 이름은 앞 왼쪽 가장자리 아래, CIDR은 그 아래 */}
          <Label
            x={iso(b.x, b.y + b.d, 0).x + 6}
            y={iso(b.x, b.y + b.d, 0).y + 16}
            text={item.label}
            strong
            anchor="start"
          />
          {item.detail && (
            <Label
              x={iso(b.x, b.y + b.d, 0).x + 6}
              y={iso(b.x, b.y + b.d, 0).y + 29}
              text={item.detail}
              muted
              anchor="start"
            />
          )}
        </>
      ) : item.kind === "lb" ? (
        <>
          <IsoPrism
            cx={b.x + b.w / 2}
            cy={b.y + b.d / 2}
            z={b.z}
            r={b.w / 2}
            h={b.h}
            color={color}
            strong={strong}
          />
          <Label x={item.center.x} y={item.center.y - 6} text={item.detail ?? item.label} strong />
        </>
      ) : item.kind === "fip" || item.kind === "vip" ? (
        <>
          <IsoTag x={b.x} y={b.y} z={b.z} color={color} strong={strong} />
          <Label x={item.center.x + 24} y={item.center.y + 2} text={item.label} anchor="start" />
        </>
      ) : (
        <>
          <IsoBlock
            box={b}
            color={color}
            defs={defs}
            sheen={item.kind === "router"}
            strong={strong}
          />
          {item.kind === "router" && (
            <>
              <Label x={item.center.x} y={item.center.y - 10} text={item.label} strong />
              {item.detail && (
                // 활성 게이트웨이 칩(노드 이름)
                <Label
                  x={item.center.x}
                  y={iso(b.x + b.w, b.y + b.d, b.z).y + 14}
                  text={item.detail}
                  muted
                />
              )}
            </>
          )}
          {item.kind === "more" && (
            <Label x={item.center.x} y={item.center.y + 4} text={item.label} strong />
          )}
          {item.kind === "vm" && showName && (
            <Label
              x={item.center.x}
              y={iso(b.x + b.w, b.y + b.d, b.z).y + 12}
              text={item.label}
              muted
            />
          )}
        </>
      )}
    </g>
  );
});
