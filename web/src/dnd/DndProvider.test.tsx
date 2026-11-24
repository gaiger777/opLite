import { act, fireEvent, render, screen } from "@testing-library/react";

import { useEffect } from "react";

import { DndProvider, useDnd } from "@/dnd/DndProvider";
import { LONG_PRESS_MS } from "@/dnd/core";
import type { Item, Verdict } from "@/dnd/types";
import { useShortcuts } from "@/lib/shortcuts";
import { useDraggable } from "@/dnd/useDraggable";
import { useDropSlot } from "@/dnd/useDropSlot";

const POOL: Item = { id: "pool", label: "ceph" };
const NET: Item = { id: "net", label: "lan" };

function accepts(item: Item, slot: string): Verdict {
  if (item.id === "pool" && slot === "disks") {
    return { ok: true };
  }
  return slot === "compute" ? { ok: false, reason: { key: "vmForm.errors.cores" } } : { ok: false };
}

function Part({ item }: { item: Item }) {
  const props = useDraggable(item);
  return <button {...props}>{item.label}</button>;
}

function Slot({ id, label, rect }: { id: string; label: string; rect: DOMRect }) {
  const slot = useDropSlot<HTMLDivElement>(id, label);
  return (
    <div
      ref={(el) => {
        if (el) {
          el.getBoundingClientRect = () => rect;
        }
        return slot.ref(el);
      }}
      role="group"
      aria-label={label}
      {...slot.props}
    >
      {label}
    </div>
  );
}

function setup() {
  const onDrop = vi.fn((item: Item) => (item.id === "pool" ? "1 disk." : undefined));
  render(
    <DndProvider accepts={accepts} onDrop={onDrop}>
      <Part item={POOL} />
      <Part item={NET} />
      <Slot id="disks" label="Disks" rect={new DOMRect(300, 100, 200, 100)} />
      <Slot id="nics" label="NICs" rect={new DOMRect(300, 300, 200, 100)} />
      <Slot id="compute" label="Compute" rect={new DOMRect(300, 500, 200, 100)} />
      <button>outside</button>
    </DndProvider>,
  );
  return { onDrop };
}

const part = (name: string) => screen.getByRole("button", { name });
const slot = (name: string) => screen.getByRole("group", { name });
const live = () => screen.getByRole("status");

function press(el: Element, x: number, y: number, pointerType = "mouse") {
  fireEvent.pointerDown(el, { button: 0, isPrimary: true, clientX: x, clientY: y, pointerType });
}
function moveTo(x: number, y: number) {
  fireEvent.pointerMove(window, { clientX: x, clientY: y });
}

it("names draggable parts for screen readers without aria-grabbed", () => {
  setup();
  const button = part("ceph");
  expect(button).toHaveAttribute("aria-roledescription", "draggable");
  expect(button).toHaveAccessibleDescription(/Press Space or Enter to pick up/);
  expect(button).not.toHaveAttribute("aria-grabbed");
  expect(live()).toHaveAttribute("aria-live", "polite");
});

it("drags with the mouse past the threshold, lights the slots and drops", () => {
  const { onDrop } = setup();
  press(part("ceph"), 50, 50);
  moveTo(52, 51);
  expect(slot("Disks")).not.toHaveAttribute("data-drop");
  moveTo(400, 150);
  expect(slot("Disks")).toHaveAttribute("data-drop", "ok");
  expect(slot("Disks")).toHaveAttribute("data-over");
  expect(slot("NICs")).toHaveAttribute("data-drop", "no");
  expect(part("ceph")).toHaveAttribute("data-held");
  expect(live()).toHaveTextContent("Disks. Can drop here.");
  // 그림자는 포인터를 따라가는 든 것의 이름
  expect(document.querySelector("[aria-hidden].fixed")).toHaveTextContent("ceph");
  fireEvent.pointerUp(window, { clientX: 400, clientY: 150 });
  expect(onDrop).toHaveBeenCalledWith(POOL, "disks");
  expect(live()).toHaveTextContent("Dropped ceph on Disks. 1 disk.");
  expect(slot("Disks")).not.toHaveAttribute("data-drop");
});

it("reads why a slot refuses and cancels a drop there", () => {
  const { onDrop } = setup();
  press(part("ceph"), 50, 50);
  moveTo(400, 550);
  expect(live()).toHaveTextContent("Compute. Cannot drop here: From 1 to 65535.");
  expect(slot("Compute")).toHaveAccessibleDescription(/Cannot drop here/);
  fireEvent.pointerUp(window, { clientX: 400, clientY: 550 });
  expect(onDrop).not.toHaveBeenCalled();
  expect(document.querySelector("[aria-hidden].fixed")).toBeNull();
});

it("picks with the keyboard, tabs only to accepting slots and drops with Enter", () => {
  const { onDrop } = setup();
  part("ceph").focus();
  fireEvent.keyDown(part("ceph"), { key: " " });
  expect(live()).toHaveTextContent(
    "Picked up ceph. 1 place to drop. Tab to choose, Space to drop, Escape to cancel.",
  );
  expect(slot("Disks")).toHaveAttribute("tabindex", "-1");
  expect(slot("NICs")).not.toHaveAttribute("tabindex");
  fireEvent.keyDown(document.activeElement!, { key: "Tab" });
  expect(slot("Disks")).toHaveFocus();
  // 놓을 수 있는 칸이 하나라 다시 Tab해도 그 칸이다.
  fireEvent.keyDown(document.activeElement!, { key: "Tab", shiftKey: true });
  expect(slot("Disks")).toHaveFocus();
  fireEvent.keyDown(document.activeElement!, { key: "Enter" });
  expect(onDrop).toHaveBeenCalledWith(POOL, "disks");
});

it("cancels with Escape and gives focus back to the part", () => {
  setup();
  part("ceph").focus();
  fireEvent.keyDown(part("ceph"), { key: "Enter" });
  fireEvent.keyDown(document.activeElement!, { key: "Tab" });
  expect(slot("Disks")).toHaveFocus();
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(part("ceph")).toHaveFocus();
  expect(live()).toHaveTextContent("Cancelled.");
  expect(slot("Disks")).not.toHaveAttribute("data-drop");
});

it("pauses global shortcuts while something is held", () => {
  const create = vi.fn();
  const palette = vi.fn();
  function Shortcuts() {
    useShortcuts({ create, palette });
    return null;
  }
  render(<Shortcuts />);
  setup();
  part("ceph").focus();
  fireEvent.keyDown(part("ceph"), { key: " " });
  expect(document.documentElement).toHaveAttribute("data-dnd-held");
  fireEvent.keyDown(document.activeElement!, { key: "c" });
  fireEvent.keyDown(document.activeElement!, { key: "k", metaKey: true });
  expect(create).not.toHaveBeenCalled();
  expect(palette).not.toHaveBeenCalled();
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(document.documentElement).not.toHaveAttribute("data-dnd-held");
  fireEvent.keyDown(document.body, { key: "c" });
  expect(create).toHaveBeenCalledTimes(1);
});

it("picks with a tap and drops with a tap on a lit slot", () => {
  const { onDrop } = setup();
  press(part("ceph"), 50, 50, "touch");
  fireEvent.pointerUp(window, { clientX: 50, clientY: 50 });
  expect(live()).toHaveTextContent("Picked up ceph. 1 place to drop.");
  expect(slot("Disks")).toHaveAttribute("data-drop", "ok");
  fireEvent.click(slot("NICs"));
  expect(onDrop).not.toHaveBeenCalled();
  expect(live()).toHaveTextContent("NICs. Cannot drop here.");
  fireEvent.click(slot("Disks"));
  expect(onDrop).toHaveBeenCalledWith(POOL, "disks");
});

it("puts a tapped part down on a tap elsewhere", () => {
  setup();
  press(part("lan"), 50, 50, "touch");
  fireEvent.pointerUp(window, { clientX: 50, clientY: 50 });
  expect(part("lan")).toHaveAttribute("data-held");
  fireEvent.pointerDown(screen.getByRole("button", { name: "outside" }));
  expect(part("lan")).not.toHaveAttribute("data-held");
  expect(live()).toHaveTextContent("Cancelled.");
});

describe("touch", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("drags only after a long press", () => {
    const { onDrop } = setup();
    press(part("ceph"), 50, 50, "touch");
    act(() => vi.advanceTimersByTime(LONG_PRESS_MS));
    moveTo(400, 150);
    expect(slot("Disks")).toHaveAttribute("data-over");
    fireEvent.pointerUp(window, { clientX: 400, clientY: 150 });
    expect(onDrop).toHaveBeenCalledWith(POOL, "disks");
  });

  it("leaves a quick swipe to scrolling", () => {
    const { onDrop } = setup();
    press(part("ceph"), 50, 50, "touch");
    moveTo(50, 80);
    act(() => vi.advanceTimersByTime(LONG_PRESS_MS));
    moveTo(400, 150);
    fireEvent.pointerUp(window, { clientX: 400, clientY: 150 });
    expect(onDrop).not.toHaveBeenCalled();
    expect(slot("Disks")).not.toHaveAttribute("data-drop");
  });
});

it("speaks Korean too", async () => {
  const { default: i18n } = await import("@/i18n");
  await act(() => i18n.changeLanguage("ko"));
  setup();
  part("ceph").focus();
  fireEvent.keyDown(part("ceph"), { key: " " });
  expect(live()).toHaveTextContent("ceph을(를) 들었습니다. 놓을 수 있는 곳 1개.");
});

/** 지도처럼 칸이 원소가 아닌 그림: 찾기 함수로 칸을 준다 */
function Surface() {
  const dnd = useDnd();
  const add = dnd.surface;
  useEffect(
    () =>
      add({
        find: (p) => (p.x > 700 ? "node:b" : null),
        ids: () => ["node:b"],
        label: () => "node-b",
      }),
    [add],
  );
  return null;
}

it("drops on a plugged-in surface (map) before DOM slots", () => {
  const onDrop = vi.fn();
  render(
    <DndProvider accepts={(_, slot) => ({ ok: slot === "node:b" })} onDrop={onDrop}>
      <Part item={NET} />
      <Surface />
    </DndProvider>,
  );
  press(part("lan"), 50, 50);
  moveTo(800, 100);
  expect(live()).toHaveTextContent("node-b. Can drop here.");
  fireEvent.pointerUp(window, { clientX: 800, clientY: 100 });
  expect(onDrop).toHaveBeenCalledWith(NET, "node:b");
});
