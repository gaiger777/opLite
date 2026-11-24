// 끌어 놓기 틀의 순수 상태 기계(계획 71 설계 §6.1): 포인터 끌기(마우스·펜은 4px 문턱, 터치는 350 ms 길게 누르기),
// 누르기로 들고 누르기로 놓기, 키보드로 들고 칸 사이를 옮겨 놓기. 같은 사건열은 같은 결과라 시험이 사건만 넣어 본다.
// 놓을 칸 판정(`accepts`)은 쓰는 쪽이 준다.
import type { Ctx, DndEvent, DndState, Item, Point, Say, Step } from "@/dnd/types";

/** 터치 길게 누르기(지도·빌더 같은 값, 설계 §11) */
export const LONG_PRESS_MS = 350;
/** 마우스·펜이 이만큼 넘게 움직이면 끌기다(계획 70 지도의 누르기 규칙과 같다). */
export const MOUSE_SLOP = 4;
/** 길게 누르기가 차기 전에 손가락이 이만큼 넘게 움직이면 스크롤에 넘긴다. */
export const TOUCH_SLOP = 8;

export const IDLE: DndState<never> = { phase: "idle" };

const far = (a: Point, b: Point, d: number) => Math.hypot(a.x - b.x, a.y - b.y) > d;

/** 칸 위에 있을 때 읽을 말: 놓을 수 있음(경고)·없음(까닭) */
function overSay<T extends Item>(ctx: Ctx<T>, item: T, slot: string): Say {
  const v = ctx.accepts(item, slot);
  const name = ctx.label(slot);
  return v.ok
    ? { key: v.warn ? "dnd.overWarn" : "dnd.overOk", values: { slot: name, warn: v.warn } }
    : {
        key: v.reason ? "dnd.overNo" : "dnd.overNoPlain",
        values: { slot: name, reason: v.reason },
      };
}

/** 들었을 때 읽을 말: 놓을 수 있는 칸 수(키보드면 키 안내까지) */
function pickSay<T extends Item>(ctx: Ctx<T>, item: T, key: boolean): Say {
  const count = ctx.slots().filter((s) => ctx.accepts(item, s).ok).length;
  return { key: key ? "dnd.pickedKey" : "dnd.picked", values: { item: item.label, count } };
}

/** 사건 하나로 다음 상태를 정한다. */
export function step<T extends Item>(s: DndState<T>, e: DndEvent<T>, ctx: Ctx<T>): Step<T> {
  const cancel = (item: T): Step<T> => ({
    state: IDLE,
    cancelled: item,
    say: { key: "dnd.cancelled", values: { item: item.label } },
  });
  const drop = (item: T, slot: string): Step<T> => ({
    state: IDLE,
    drop: { item, slot },
    say: { key: "dnd.dropped", values: { item: item.label, slot: ctx.label(slot) } },
  });
  const press = (again: boolean): Step<T> | null =>
    e.type === "down"
      ? {
          state: {
            phase: "pressing",
            item: e.item,
            start: e.point,
            touch: e.touch,
            armed: false,
            again,
          },
        }
      : null;
  switch (s.phase) {
    case "idle":
      if (e.type === "pick") {
        return {
          state: { phase: "picked", item: e.item, over: null, key: e.key },
          say: pickSay(ctx, e.item, e.key),
        };
      }
      return press(false) ?? { state: s };
    case "pressing":
      switch (e.type) {
        case "longpress":
          return { state: s.touch ? { ...s, armed: true } : s };
        case "move":
          if (s.touch && !s.armed) {
            // 길게 누르기 전에 움직이면 스크롤이다(아무것도 들지 않음).
            return { state: far(s.start, e.point, TOUCH_SLOP) ? IDLE : s };
          }
          if (!far(s.start, e.point, MOUSE_SLOP)) {
            return { state: s };
          }
          return {
            state: { phase: "dragging", item: s.item, over: e.slot },
            say: e.slot ? overSay(ctx, s.item, e.slot) : pickSay(ctx, s.item, false),
          };
        case "up":
          // 끌지 않고 뗐다: 누르기 들기(든 것을 다시 눌렀으면 내려놓기)
          return s.again
            ? cancel(s.item)
            : {
                state: { phase: "picked", item: s.item, over: null, key: false },
                say: pickSay(ctx, s.item, false),
              };
        case "cancel":
          return { state: IDLE };
        default:
          return { state: s };
      }
    case "dragging":
      switch (e.type) {
        case "move":
          return e.slot === s.over
            ? { state: s }
            : {
                state: { ...s, over: e.slot },
                say: e.slot ? overSay(ctx, s.item, e.slot) : undefined,
              };
        case "up":
          if (e.slot && ctx.accepts(s.item, e.slot).ok) {
            return drop(s.item, e.slot);
          }
          // 놓을 수 없는 칸이면 까닭을 읽고 취소한다.
          return e.slot ? { ...cancel(s.item), say: overSay(ctx, s.item, e.slot) } : cancel(s.item);
        case "cancel":
          return cancel(s.item);
        default:
          return { state: s };
      }
    case "picked":
      switch (e.type) {
        case "down":
          return press(e.item.id === s.item.id)!;
        case "pick":
          return e.item.id === s.item.id
            ? cancel(s.item)
            : {
                state: { phase: "picked", item: e.item, over: null, key: e.key },
                say: pickSay(ctx, e.item, e.key),
              };
        case "over":
          return {
            state: { ...s, over: e.slot },
            say: e.slot ? overSay(ctx, s.item, e.slot) : undefined,
          };
        case "drop":
          return ctx.accepts(s.item, e.slot).ok
            ? drop(s.item, e.slot)
            : { state: { ...s, over: e.slot }, say: overSay(ctx, s.item, e.slot) };
        case "cancel":
          return cancel(s.item);
        default:
          return { state: s };
      }
  }
}

/** 점을 담은 칸 가운데 가장 안쪽(넓이가 가장 작은) 칸 */
export function slotAt(
  rects: readonly { id: string; r: Pick<DOMRect, "left" | "top" | "right" | "bottom"> }[],
  p: Point,
): string | null {
  let best: string | null = null;
  let area = Infinity;
  for (const { id, r } of rects) {
    const a = (r.right - r.left) * (r.bottom - r.top);
    if (p.x >= r.left && p.x <= r.right && p.y >= r.top && p.y <= r.bottom && a < area) {
      best = id;
      area = a;
    }
  }
  return best;
}

/** 키보드로 옮길 다음 칸: 놓을 수 있는 칸 사이만, 끝에서 처음으로 돈다. 없으면 null */
export function nextSlot(
  order: readonly string[],
  ok: (id: string) => boolean,
  current: string | null,
  dir: 1 | -1,
): string | null {
  const valid = order.filter(ok);
  if (valid.length === 0) {
    return null;
  }
  const at = current === null ? -1 : valid.indexOf(current);
  if (at === -1) {
    return dir === 1 ? valid[0]! : valid[valid.length - 1]!;
  }
  return valid[(at + dir + valid.length) % valid.length]!;
}
