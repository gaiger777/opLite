// 끌어 놓기 틀의 React 쪽(계획 71 설계 §6): 상태 기계(`core.ts`)에 포인터·키보드 사건을 넣고, 칸 자리를 끌기 시작 때
// 한 번 모으며(스크롤·크기 바뀜 때 다시), 그림자를 `requestAnimationFrame`마다 옮기고 가장자리에서 스크롤하고, 알림 줄
// 하나로 읽는다. 쪽마다 하나 둔다.
import {
  createContext,
  type ReactNode,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import { Announcer } from "@/dnd/Announcer";
import { scrollStep } from "@/dnd/autoscroll";
import { IDLE, LONG_PRESS_MS, nextSlot, slotAt, step } from "@/dnd/core";
import type { Ctx, DndEvent, DndState, Item, Point, Say, Surface, Verdict } from "@/dnd/types";

type T = (key: string, values?: Record<string, unknown>) => string;

export interface DndApi {
  state: DndState;
  send(e: DndEvent): void;
  /** 든 것을 이 칸에 놓을 수 있는지 */
  verdict(item: Item, slot: string): Verdict;
  /** DOM 칸을 단다(`el`이 null이면 뗀다). 돌려준 함수로 뗀다. */
  slot(id: string, el: HTMLElement | null, label: string): () => void;
  /** 끌 수 있는 원소(취소하면 초점을 돌려줄 곳) */
  item(id: string, el: HTMLElement | null): void;
  /** 찾기 함수 그림(지도)을 단다. */
  surface(s: Surface): () => void;
  /** 알림 줄에 한 마디(빌더의 서버 확인 결과 등) */
  announce(text: string): void;
  /** 키 안내 글의 id(`aria-describedby`) */
  hint: string;
}

const DndContext = createContext<DndApi | null>(null);

export function useDnd(): DndApi {
  const api = useContext(DndContext);
  if (!api) {
    throw new Error("useDnd outside DndProvider");
  }
  return api;
}

/** 알림 줄 말을 옮긴다(값의 `reason`·`warn`도 키다). */
export function speak(t: T, say: Say): string {
  const values = Object.fromEntries(
    Object.entries(say.values).map(([k, v]) => [
      k,
      typeof v === "object" && v ? t(v.key, v.values) : v,
    ]),
  );
  return t(say.key, values);
}

export function DndProvider<I extends Item>({
  accepts,
  onDrop,
  children,
}: {
  accepts(item: I, slot: string): Verdict;
  /** 놓았다. 돌려준 글을 알림 줄 끝에 붙인다("디스크 2개"). */
  onDrop(item: I, slot: string): string | void;
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const hint = useId();
  const [state, setState] = useState<DndState>(IDLE);
  const [said, setSaid] = useState("");
  const live = useRef({ state: IDLE as DndState, accepts, onDrop, t: t as T });
  useLayoutEffect(() => {
    Object.assign(live.current, { accepts, onDrop, t });
  });
  const [m] = useState(() => {
    const slots = new Map<string, { el: HTMLElement; label: string }>();
    const items = new Map<string, HTMLElement>();
    const surfaces = new Set<Surface>();
    /** DOM 칸의 문서 차례 */
    const order = () =>
      [...slots]
        .sort(([, a], [, b]) =>
          a.el.compareDocumentPosition(b.el) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1,
        )
        .map(([id]) => id);
    const ctx: Ctx = {
      accepts: (item, slot) => (live.current.accepts as Ctx["accepts"])(item, slot),
      label: (id) =>
        slots.get(id)?.label ?? [...surfaces].find((s) => s.ids().includes(id))?.label(id) ?? id,
      slots: () => [...order(), ...[...surfaces].flatMap((s) => s.ids())],
    };
    const self = {
      slots,
      items,
      surfaces,
      order,
      ctx,
      point: { x: 0, y: 0 } as Point,
      rects: [] as { id: string; r: DOMRect }[],
      /** 칸 자리를 모은다(끌기 시작·스크롤·크기 바뀜 때만 — 움직일 때마다 재면 레이아웃을 강제한다). */
      measure() {
        self.rects = [...slots].map(([id, { el }]) => ({ id, r: el.getBoundingClientRect() }));
      },
      /** 점 아래 칸: 찾기 함수 그림이 먼저, 아니면 가장 안쪽 DOM 칸 */
      find(p: Point): string | null {
        for (const s of surfaces) {
          const id = s.find(p);
          if (id) {
            return id;
          }
        }
        return slotAt(self.rects, p);
      },
      say: (text: string) => setSaid((s) => (s === text ? `${text}\u00a0` : text)),
      send(e: DndEvent) {
        if (e.type === "down" || e.type === "move") {
          self.point = e.point;
        }
        const out = step(live.current.state, e, ctx);
        live.current.state = out.state;
        setState(out.state);
        let text = out.say ? speak(live.current.t, out.say) : "";
        if (out.drop) {
          const extra = (live.current.onDrop as (i: Item, s: string) => string | void)(
            out.drop.item,
            out.drop.slot,
          );
          text = extra ? `${text} ${extra}` : text;
        }
        if (out.cancelled) {
          items.get(out.cancelled.id)?.focus();
        }
        if (text) {
          self.say(text);
        }
      },
    };
    return self;
  });
  const ghost = useRef<HTMLDivElement>(null);

  const api = useMemo<DndApi>(
    () => ({
      state,
      send: m.send,
      verdict: (item, slot) => m.ctx.accepts(item, slot),
      slot(id, el, label) {
        if (el) {
          m.slots.set(id, { el, label });
        }
        return () => {
          if (m.slots.get(id)?.el === el) {
            m.slots.delete(id);
          }
        };
      },
      item(id, el) {
        if (el) {
          m.items.set(id, el);
        } else {
          m.items.delete(id);
        }
      },
      surface(s) {
        m.surfaces.add(s);
        return () => m.surfaces.delete(s);
      },
      announce: m.say,
      hint,
    }),
    [state, m, hint],
  );
  const { send } = m;

  const phase = state.phase;
  const touchPress = state.phase === "pressing" && state.touch;
  // 누르는 중·끄는 중: 창 전체의 포인터·키·스크롤
  useEffect(() => {
    if (phase !== "pressing" && phase !== "dragging") {
      return;
    }
    const measure = () => m.measure();
    const find = (p: Point) => m.find(p);
    const move = (e: PointerEvent) => {
      const point = { x: e.clientX, y: e.clientY };
      send({ type: "move", point, slot: find(point) });
    };
    const up = () => send({ type: "up", slot: find(m.point) });
    const cancel = () => send({ type: "cancel" });
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        cancel();
      }
    };
    // 길게 누른 뒤와 끄는 동안은 손가락이 화면을 스크롤하지 않는다.
    const touch = (e: TouchEvent) => {
      const s = live.current.state;
      if (s.phase === "dragging" || (s.phase === "pressing" && s.armed)) {
        e.preventDefault();
      }
    };
    measure();
    const timer = touchPress
      ? setTimeout(() => send({ type: "longpress" }), LONG_PRESS_MS)
      : undefined;
    let frame = 0;
    const tick = () => {
      const p = m.point;
      if (ghost.current) {
        ghost.current.style.transform = `translate3d(${p.x + 12}px,${p.y + 12}px,0)`;
      }
      let scrolled = false;
      const scroll = (el: Element | null) => {
        const r = el ? el.getBoundingClientRect() : new DOMRect(0, 0, innerWidth, innerHeight);
        const { dx, dy } = scrollStep(r, p);
        if (dx || dy) {
          (el ?? window).scrollBy(dx, dy);
          scrolled = true;
        }
      };
      document.querySelectorAll("[data-dnd-scroll]").forEach(scroll);
      scroll(null);
      if (scrolled) {
        measure();
        send({ type: "move", point: p, slot: find(p) });
      }
      frame = requestAnimationFrame(tick);
    };
    if (phase === "dragging") {
      tick();
    }
    const on: [string, EventListener, AddEventListenerOptions?][] = [
      ["pointermove", move as EventListener],
      ["pointerup", up],
      ["pointercancel", cancel],
      ["keydown", key as EventListener],
      ["touchmove", touch as EventListener, { passive: false }],
      ["scroll", measure, { capture: true }],
      ["resize", measure],
    ];
    on.forEach(([type, fn, opts]) => addEventListener(type, fn, opts));
    return () => {
      on.forEach(([type, fn, opts]) => removeEventListener(type, fn, opts));
      clearTimeout(timer);
      cancelAnimationFrame(frame);
    };
  }, [phase, touchPress, send, m]);

  // 든 채(누르기·키보드): 놓을 수 있는 칸 사이만 Tab·화살표, Space·Enter로 놓기, Esc·빈 곳 누르기로 취소
  useEffect(() => {
    if (phase !== "picked") {
      return;
    }
    const key = (e: KeyboardEvent) => {
      const s = live.current.state;
      if (s.phase !== "picked") {
        return;
      }
      const dir =
        e.key === "Tab"
          ? e.shiftKey
            ? -1
            : 1
          : e.key === "ArrowDown" || e.key === "ArrowRight"
            ? 1
            : e.key === "ArrowUp" || e.key === "ArrowLeft"
              ? -1
              : 0;
      const at = document.activeElement;
      const onSlot = !!s.over && m.slots.get(s.over)?.el === at;
      if (e.key === "Escape") {
        e.preventDefault();
        send({ type: "cancel" });
      } else if (dir && (e.key === "Tab" || onSlot || m.items.get(s.item.id) === at)) {
        e.preventDefault();
        const next = nextSlot(m.order(), (id) => m.ctx.accepts(s.item, id).ok, s.over, dir);
        const el = next && m.slots.get(next)?.el;
        if (el) {
          el.focus();
          el.scrollIntoView?.({ block: "nearest" });
          send({ type: "over", slot: next });
        }
      } else if ((e.key === " " || e.key === "Enter") && onSlot) {
        e.preventDefault();
        send({ type: "drop", slot: s.over! });
      }
    };
    const down = (e: PointerEvent) => {
      if (!(e.target as Element).closest?.("[data-drop],[data-dnd-item]")) {
        send({ type: "cancel" });
      }
    };
    document.addEventListener("keydown", key);
    document.addEventListener("pointerdown", down, true);
    return () => {
      document.removeEventListener("keydown", key);
      document.removeEventListener("pointerdown", down, true);
    };
  }, [phase, send, m]);

  return (
    <DndContext.Provider value={api}>
      {children}
      <p id={hint} hidden>
        {t("dnd.hint")}
      </p>
      {state.phase === "dragging" && (
        <div
          ref={ghost}
          aria-hidden
          className="pointer-events-none fixed top-0 left-0 z-50 rounded-md border bg-popover px-2 py-1 text-sm text-popover-foreground shadow-md"
        >
          {state.item.label}
        </div>
      )}
      <Announcer text={said} />
    </DndContext.Provider>
  );
}
