// 아이소메트릭 그림의 보기 움직이기(계획 67 §12.2): 끌어 옮기기, Ctrl+휠·두 손가락 확대, 단추(+, −, 맞추기). SVG의
// `viewBox`만 바꾼다(원소는 그대로). 계획 70의 클러스터 보기가 같은 훅을 쓴다.
import { useCallback, useEffect, useState } from "react";

export interface ViewBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Bounds {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

/** 누르고 있는 손가락·마우스(끌기·두 손가락 확대) — 그리기에 쓰지 않는 바뀌는 값이라 SVG 원소마다 둔다. */
interface Gesture {
  pointers: Map<number, { x: number; y: number }>;
  pinch: number | null;
}
const gestures = new WeakMap<Element, Gesture>();

function gesture(el: Element): Gesture {
  let g = gestures.get(el);
  if (!g) {
    g = { pointers: new Map(), pinch: null };
    gestures.set(el, g);
  }
  return g;
}

/** 확대 한도(맞춘 크기의 배수) */
const MIN_SCALE = 0.25;
const MAX_SCALE = 6;

/** 그림 전체가 들어가는 viewBox(가장자리 `pad`) */
export function fitBox(bounds: Bounds, pad = 32): ViewBox {
  return {
    x: bounds.minX - pad,
    y: bounds.minY - pad,
    w: Math.max(1, bounds.maxX - bounds.minX + pad * 2),
    h: Math.max(1, bounds.maxY - bounds.minY + pad * 2),
  };
}

/**
 * 점 (px, py)(viewBox 좌표)를 그대로 둔 채 `factor`배 확대한다(1보다 크면 가까이). 맞춘 크기 `fit`의 한도를 넘지 않는다.
 */
export function zoomAt(
  view: ViewBox,
  factor: number,
  px: number,
  py: number,
  fit: ViewBox,
): ViewBox {
  const scale = fit.w / (view.w / factor);
  const clamped = Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
  const w = fit.w / clamped;
  const h = (view.h / view.w) * w;
  return {
    x: px - ((px - view.x) / view.w) * w,
    y: py - ((py - view.y) / view.h) * h,
    w,
    h,
  };
}

/**
 * viewBox 상태와 SVG에 붙일 손잡이. `fit`이 바뀌면(자료가 바뀜) 사용자가 움직이지 않았을 때만 다시 맞춘다. `ref`는 SVG에
 * 붙이는 콜백(`attach`)이다(Ctrl+휠은 쪽 확대를 막아야 해서 수동이 아닌 리스너로 받는다).
 */
export function useIsoViewport(fit: ViewBox) {
  const [state, setState] = useState<{ fit: ViewBox; view: ViewBox; moved: boolean }>({
    fit,
    view: fit,
    moved: false,
  });
  // 자료가 바뀌어 맞춘 크기가 달라졌다: 움직인 적이 없으면 새로 맞춘다(그리는 동안 상태를 고치는 React 꼴).
  if (!sameBox(state.fit, fit)) {
    setState({ fit, view: state.moved ? state.view : fit, moved: state.moved });
  }
  const view = state.view;
  const [svg, setSvg] = useState<SVGSVGElement | null>(null);
  // 누르고 있는 손가락·마우스(끌기·두 손가락 확대). 그리기에 쓰지 않는 바뀌는 값이다.

  const set = useCallback((next: (v: ViewBox) => ViewBox) => {
    setState((s) => ({ ...s, view: next(s.view), moved: true }));
  }, []);

  // 화면 픽셀 → viewBox 좌표(지금 viewBox `v` 기준)
  const toView = useCallback((el: Element, v: ViewBox, clientX: number, clientY: number) => {
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) {
      return { x: v.x + v.w / 2, y: v.y + v.h / 2 };
    }
    return {
      x: v.x + ((clientX - rect.left) / rect.width) * v.w,
      y: v.y + ((clientY - rect.top) / rect.height) * v.h,
    };
  }, []);

  // Ctrl+휠(트랙패드 확대 몸짓도 ctrlKey)만 확대한다 — 그냥 휠은 쪽을 내린다. React의 휠 리스너는 수동이라 막을 수 없다.
  useEffect(() => {
    if (!svg) {
      return;
    }
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.metaKey) {
        return;
      }
      e.preventDefault();
      setState((s) => {
        const at = toView(svg, s.view, e.clientX, e.clientY);
        return {
          ...s,
          view: zoomAt(s.view, e.deltaY < 0 ? 1.15 : 1 / 1.15, at.x, at.y, s.fit),
          moved: true,
        };
      });
    };
    svg.addEventListener("wheel", onWheel, { passive: false });
    return () => svg.removeEventListener("wheel", onWheel);
  }, [svg, toView]);

  const handlers = {
    onPointerDown(e: React.PointerEvent<SVGSVGElement>) {
      if (e.pointerType === "mouse" && e.button !== 0) {
        return;
      }
      const g = gesture(e.currentTarget);
      g.pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (g.pointers.size === 2) {
        const [a, b] = [...g.pointers.values()];
        g.pinch = Math.hypot(a!.x - b!.x, a!.y - b!.y);
      }
    },
    onPointerMove(e: React.PointerEvent<SVGSVGElement>) {
      const el = e.currentTarget;
      const g = gesture(el);
      const last = g.pointers.get(e.pointerId);
      if (!last) {
        return;
      }
      const now = { x: e.clientX, y: e.clientY };
      g.pointers.set(e.pointerId, now);
      if (g.pointers.size === 2 && g.pinch) {
        const [a, b] = [...g.pointers.values()];
        const dist = Math.hypot(a!.x - b!.x, a!.y - b!.y);
        const factor = dist / g.pinch;
        g.pinch = dist;
        set((v) => {
          const center = toView(el, v, (a!.x + b!.x) / 2, (a!.y + b!.y) / 2);
          return zoomAt(v, factor, center.x, center.y, state.fit);
        });
        return;
      }
      const rect = el.getBoundingClientRect();
      const scale = rect.width > 0 ? view.w / rect.width : 1;
      const dx = (now.x - last.x) * scale;
      const dy = (now.y - last.y) * scale;
      if (dx !== 0 || dy !== 0) {
        // 조금 움직인 뒤부터 끌기다(원소 누르기는 그대로 둔다).
        if (!el.hasPointerCapture?.(e.pointerId) && Math.hypot(dx, dy) > 2) {
          el.setPointerCapture?.(e.pointerId);
        }
        set((v) => ({ ...v, x: v.x - dx, y: v.y - dy }));
      }
    },
    onPointerUp(e: React.PointerEvent<SVGSVGElement>) {
      const g = gesture(e.currentTarget);
      g.pointers.delete(e.pointerId);
      if (g.pointers.size < 2) {
        g.pinch = null;
      }
    },
    onPointerCancel(e: React.PointerEvent<SVGSVGElement>) {
      const g = gesture(e.currentTarget);
      g.pointers.delete(e.pointerId);
      g.pinch = null;
    },
  };

  const center = () => ({ x: view.x + view.w / 2, y: view.y + view.h / 2 });
  return {
    view,
    /** SVG의 `ref`에 주는 콜백 */
    attach: setSvg,
    handlers,
    zoomIn: () => set((v) => zoomAt(v, 1.25, center().x, center().y, state.fit)),
    zoomOut: () => set((v) => zoomAt(v, 1 / 1.25, center().x, center().y, state.fit)),
    reset: () => setState((s) => ({ ...s, view: s.fit, moved: false })),
    /** 보기를 (dx, dy)(viewBox 좌표)만큼 옮긴다(지도 끌기의 가장자리 보기 옮기기 — 계획 71) */
    pan: (dx: number, dy: number) => set((v) => ({ ...v, x: v.x + dx, y: v.y + dy })),
    /** 화면 점(viewBox 좌표)이 보이게 옮긴다(키보드 초점이 그림 밖으로 나가면) */
    reveal(x: number, y: number) {
      if (x >= view.x && x <= view.x + view.w && y >= view.y && y <= view.y + view.h) {
        return;
      }
      set((v) => ({ ...v, x: x - v.w / 2, y: y - v.h / 2 }));
    },
  };
}

function sameBox(a: ViewBox, b: ViewBox) {
  return a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;
}

export function viewBoxAttr(v: ViewBox): string {
  return `${round(v.x)} ${round(v.y)} ${round(v.w)} ${round(v.h)}`;
}

function round(n: number) {
  return Math.round(n * 10) / 10;
}
