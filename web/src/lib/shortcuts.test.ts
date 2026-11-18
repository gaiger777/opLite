import { createSequence, isTyping, keyLabel, keyName, SHORTCUTS } from "@/lib/shortcuts";

it("finishes a g sequence within a second", () => {
  const seq = createSequence();
  expect(seq.feed("g", 0)).toBeNull();
  expect(seq.feed("v", 500)).toBe("go.vms");
  expect(seq.feed("g", 1000)).toBeNull();
  expect(seq.feed("s", 1900)).toBe("go.settings");
});

it("forgets the first key after the timeout", () => {
  const seq = createSequence();
  expect(seq.feed("g", 0)).toBeNull();
  expect(seq.feed("v", 1001)).toBeNull();
});

it("starts over from a key that breaks a sequence", () => {
  const seq = createSequence();
  seq.feed("g", 0);
  expect(seq.feed("x", 10)).toBeNull();
  expect(seq.feed("g", 20)).toBeNull();
  expect(seq.feed("i", 30)).toBe("go.infra");
  seq.feed("g", 40);
  expect(seq.feed("?", 50)).toBe("help");
});

it("runs single keys right away", () => {
  const seq = createSequence();
  expect(seq.feed("?", 0)).toBe("help");
  expect(seq.feed("[", 0)).toBe("rail");
  expect(seq.feed("c", 0)).toBe("create");
  expect(seq.feed("v", 0)).toBeNull();
  // Shift+N(대문자)만 센터를 연다.
  expect(seq.feed("N", 0)).toBe("center");
  expect(seq.feed("n", 0)).toBeNull();
});

it("labels Shift keys", () => {
  expect(keyLabel("N")).toMatch(/^(⇧N|Shift N)$/);
  expect(keyLabel("n")).toBe("n");
});

it("has a single-key shortcut for no power or delete action", () => {
  const single = SHORTCUTS.filter((s) => s.keys.length === 1).map((s) => s.id);
  expect(single).toEqual([
    "palette",
    "help",
    "search",
    "create",
    "rail",
    "favorite",
    "center",
    "list.next",
    "list.prev",
    "list.select",
    "list.peek",
    "list.open",
  ]);
});

it("ignores typing targets", () => {
  const input = document.createElement("input");
  const div = document.createElement("div");
  const editable = document.createElement("div");
  editable.setAttribute("contenteditable", "true");
  document.body.append(input, div, editable);
  expect(isTyping(input)).toBe(true);
  expect(isTyping(editable)).toBe(true);
  expect(isTyping(div)).toBe(false);
  expect(isTyping(null)).toBe(false);
  input.remove();
  div.remove();
  editable.remove();
});

it("names keys", () => {
  const key = (init: KeyboardEventInit) => keyName(new KeyboardEvent("keydown", init));
  expect(key({ key: "k", metaKey: true })).toBe("mod+k");
  expect(key({ key: "K", ctrlKey: true })).toBe("mod+k");
  expect(key({ key: "g" })).toBe("g");
  expect(key({ key: "?", shiftKey: true })).toBe("?");
  expect(key({ key: "g", ctrlKey: true })).toBeNull();
  expect(key({ key: "Enter" })).toBeNull();
});
