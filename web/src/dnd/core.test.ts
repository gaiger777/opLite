import { IDLE, MOUSE_SLOP, nextSlot, slotAt, step, TOUCH_SLOP } from "@/dnd/core";
import type { Ctx, DndEvent, DndState, Item, Step } from "@/dnd/types";

const POOL: Item = { id: "pool", label: "ceph" };
const NET: Item = { id: "net", label: "lan" };
const SLOTS = ["disks", "nics", "compute"];
// 풀은 디스크 칸에만, 네트워크는 NIC 칸에만(계산 칸에는 까닭과 함께 못 놓음)
const ctx: Ctx = {
  accepts: (item, slot) =>
    (item.id === "pool" && slot === "disks") || (item.id === "net" && slot === "nics")
      ? slot === "nics"
        ? { ok: true, warn: { key: "w" } }
        : { ok: true }
      : slot === "compute"
        ? { ok: false, reason: { key: "r" } }
        : { ok: false },
  label: (slot) => `slot ${slot}`,
  slots: () => SLOTS,
};

/** 사건열을 차례로 넣은 걸음들 */
function run(events: DndEvent[], from: DndState = IDLE): Step[] {
  const out: Step[] = [];
  let s = from;
  for (const e of events) {
    const next = step(s, e, ctx);
    out.push(next);
    s = next.state;
  }
  return out;
}
const last = (steps: Step[]) => steps.at(-1)!;
const at = (x: number, y = 0) => ({ x, y });

describe("pointer drag (mouse)", () => {
  it("starts only after the pointer moves more than 4px, then drops on an accepting slot", () => {
    const steps = run([
      { type: "down", item: POOL, point: at(0), touch: false },
      { type: "move", point: at(MOUSE_SLOP), slot: null },
      { type: "move", point: at(MOUSE_SLOP + 1), slot: "nics" },
      { type: "move", point: at(20), slot: "disks" },
      { type: "move", point: at(21), slot: "disks" },
      { type: "up", slot: "disks" },
    ]);
    expect(steps[1]!.state.phase).toBe("pressing");
    expect(steps[2]!.state).toEqual({ phase: "dragging", item: POOL, over: "nics" });
    expect(steps[2]!.say).toEqual({
      key: "dnd.overNoPlain",
      values: { slot: "slot nics", reason: undefined },
    });
    expect(steps[3]!.say?.key).toBe("dnd.overOk");
    // 같은 칸 위에서는 다시 읽지 않는다.
    expect(steps[4]!.say).toBeUndefined();
    expect(last(steps)).toEqual({
      state: IDLE,
      drop: { item: POOL, slot: "disks" },
      say: { key: "dnd.dropped", values: { item: "ceph", slot: "slot disks" } },
    });
  });

  it("cancels on a slot that refuses and reads why", () => {
    const steps = run([
      { type: "down", item: POOL, point: at(0), touch: false },
      { type: "move", point: at(10), slot: "compute" },
      { type: "up", slot: "compute" },
    ]);
    expect(steps[1]!.say).toEqual({
      key: "dnd.overNo",
      values: { slot: "slot compute", reason: { key: "r" } },
    });
    expect(last(steps).cancelled).toBe(POOL);
    expect(last(steps).drop).toBeUndefined();
    expect(last(steps).say?.key).toBe("dnd.overNo");
  });

  it("cancels outside any slot, on Escape and on pointercancel", () => {
    const drag: DndEvent[] = [
      { type: "down", item: POOL, point: at(0), touch: false },
      { type: "move", point: at(10), slot: null },
    ];
    expect(last(run([...drag, { type: "up", slot: null }])).say?.key).toBe("dnd.cancelled");
    expect(last(run([...drag, { type: "cancel" }])).cancelled).toBe(POOL);
  });

  it("announces the number of places when the drag starts outside a slot", () => {
    const steps = run([
      { type: "down", item: NET, point: at(0), touch: false },
      { type: "move", point: at(10), slot: null },
    ]);
    expect(steps[1]!.say).toEqual({ key: "dnd.picked", values: { item: "lan", count: 1 } });
  });

  it("warns on a slot that accepts with a warning", () => {
    const steps = run([
      { type: "down", item: NET, point: at(0), touch: false },
      { type: "move", point: at(10), slot: "nics" },
    ]);
    expect(steps[1]!.say).toEqual({
      key: "dnd.overWarn",
      values: { slot: "slot nics", warn: { key: "w" } },
    });
  });
});

describe("touch", () => {
  it("hands a move before the long press to scrolling", () => {
    const steps = run([
      { type: "down", item: POOL, point: at(0), touch: true },
      { type: "move", point: at(TOUCH_SLOP), slot: null },
      { type: "move", point: at(TOUCH_SLOP + 1), slot: "disks" },
    ]);
    expect(steps[1]!.state.phase).toBe("pressing");
    expect(last(steps)).toEqual({ state: IDLE });
  });

  it("drags after the long press", () => {
    const steps = run([
      { type: "down", item: POOL, point: at(0), touch: true },
      { type: "longpress" },
      { type: "move", point: at(30), slot: "disks" },
      { type: "up", slot: "disks" },
    ]);
    expect(steps[1]!.state).toMatchObject({ phase: "pressing", armed: true });
    expect(steps[2]!.state.phase).toBe("dragging");
    expect(last(steps).drop).toEqual({ item: POOL, slot: "disks" });
  });

  it("ignores the long-press timer of a mouse", () => {
    const steps = run([
      { type: "down", item: POOL, point: at(0), touch: false },
      { type: "longpress" },
    ]);
    expect(last(steps).state).toMatchObject({ phase: "pressing", armed: false });
  });
});

describe("tap to pick, tap to drop", () => {
  const tap = (item: Item): DndEvent[] => [
    { type: "down", item, point: at(0), touch: true },
    { type: "up", slot: null },
  ];

  it("picks on a tap and drops on a tapped slot", () => {
    const steps = run([...tap(POOL), { type: "drop", slot: "disks" }]);
    expect(steps[1]!.state).toEqual({ phase: "picked", item: POOL, over: null, key: false });
    expect(steps[1]!.say).toEqual({ key: "dnd.picked", values: { item: "ceph", count: 1 } });
    expect(last(steps).drop).toEqual({ item: POOL, slot: "disks" });
  });

  it("stays picked on a slot that refuses and reads why", () => {
    const steps = run([...tap(POOL), { type: "drop", slot: "compute" }]);
    expect(last(steps).state).toEqual({
      phase: "picked",
      item: POOL,
      over: "compute",
      key: false,
    });
    expect(last(steps).say?.key).toBe("dnd.overNo");
  });

  it("puts it down when the picked part is tapped again, and switches on another part", () => {
    expect(last(run([...tap(POOL), ...tap(POOL)])).cancelled).toBe(POOL);
    expect(last(run([...tap(POOL), ...tap(NET)])).state).toMatchObject({
      phase: "picked",
      item: NET,
    });
  });
});

describe("keyboard", () => {
  it("picks with the key instructions, moves over slots and drops", () => {
    const steps = run([
      { type: "pick", item: POOL, key: true },
      { type: "over", slot: "disks" },
      { type: "drop", slot: "disks" },
    ]);
    expect(steps[0]!.say).toEqual({ key: "dnd.pickedKey", values: { item: "ceph", count: 1 } });
    expect(steps[1]!.say?.key).toBe("dnd.overOk");
    expect(last(steps).drop).toEqual({ item: POOL, slot: "disks" });
  });

  it("puts it down with Space on the picked part or Escape", () => {
    const pick: DndEvent = { type: "pick", item: POOL, key: true };
    expect(last(run([pick, pick])).cancelled).toBe(POOL);
    expect(last(run([pick, { type: "cancel" }])).say?.key).toBe("dnd.cancelled");
  });
});

it("moves between accepting slots only and wraps around", () => {
  const order = ["a", "b", "c", "d"];
  const ok = (id: string) => id !== "b";
  expect(nextSlot(order, ok, null, 1)).toBe("a");
  expect(nextSlot(order, ok, null, -1)).toBe("d");
  expect(nextSlot(order, ok, "a", 1)).toBe("c");
  expect(nextSlot(order, ok, "d", 1)).toBe("a");
  expect(nextSlot(order, ok, "a", -1)).toBe("d");
  // 지금 칸이 놓을 수 없는 칸이면 처음부터
  expect(nextSlot(order, ok, "b", 1)).toBe("a");
  expect(nextSlot(order, () => false, null, 1)).toBeNull();
});

it("finds the innermost slot under a point", () => {
  const r = (left: number, top: number, right: number, bottom: number) => ({
    left,
    top,
    right,
    bottom,
  });
  const rects = [
    { id: "disks", r: r(0, 0, 100, 100) },
    { id: "disks.0", r: r(10, 10, 90, 30) },
    { id: "nics", r: r(0, 200, 100, 300) },
  ];
  expect(slotAt(rects, at(50, 20))).toBe("disks.0");
  expect(slotAt(rects, at(50, 50))).toBe("disks");
  expect(slotAt(rects, at(50, 250))).toBe("nics");
  expect(slotAt(rects, at(50, 150))).toBeNull();
});

// 성질 시험: 아무 사건열이든 놓기는 받는 칸에만, 놓기·취소 뒤는 늘 쉼, 끄는 중·든 채는 늘 든 것이 있다.
describe("properties over random event sequences", () => {
  function rng(seed: number) {
    let x = seed;
    return () => {
      x = (x * 1103515245 + 12345) % 2 ** 31;
      return x / 2 ** 31;
    };
  }
  const pickOne = <T>(r: () => number, list: readonly T[]) => list[Math.floor(r() * list.length)]!;

  function randomEvent(r: () => number): DndEvent {
    const item = pickOne(r, [POOL, NET]);
    const slot = pickOne(r, [null, ...SLOTS]);
    const point = at(Math.floor(r() * 40), Math.floor(r() * 40));
    switch (Math.floor(r() * 8)) {
      case 0:
        return { type: "down", item, point, touch: r() < 0.5 };
      case 1:
        return { type: "longpress" };
      case 2:
        return { type: "move", point, slot };
      case 3:
        return { type: "up", slot };
      case 4:
        return { type: "pick", item, key: r() < 0.5 };
      case 5:
        return { type: "over", slot };
      case 6:
        return { type: "drop", slot: slot ?? "disks" };
      default:
        return { type: "cancel" };
    }
  }

  it.each(Array.from({ length: 200 }, (_, i) => i + 1))("seed %i", (seed) => {
    const r = rng(seed);
    let s: DndState = IDLE;
    for (let i = 0; i < 60; i++) {
      const e = randomEvent(r);
      const out = step(s, e, ctx);
      if (out.drop) {
        expect(ctx.accepts(out.drop.item, out.drop.slot).ok).toBe(true);
        expect(s.phase === "dragging" || s.phase === "picked").toBe(true);
      }
      if (out.drop || out.cancelled) {
        expect(out.state).toEqual(IDLE);
        expect(out.say).toBeDefined();
      }
      if (out.state.phase !== "idle") {
        expect([POOL, NET]).toContain(out.state.item);
      }
      // 같은 상태·같은 사건은 같은 결과다(순수).
      expect(step(s, e, ctx)).toEqual(out);
      s = out.state;
    }
  });
});
