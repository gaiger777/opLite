// 전역 단축키(설계 §5.7). 의존성 없는 순서 키 해석기와, 도움말이 함께 읽는 단축키 표다. 전원·지우기에는 한 글자
// 키를 두지 않는다.
import { useEffect, useRef } from "react";

/** 단축키 id. 도움말의 글은 `shortcuts.{id}`다. */
export type ShortcutId =
  | "palette"
  | "help"
  | "search"
  | "rail"
  | "create"
  | "favorite"
  | "center"
  | "go.home"
  | "go.vms"
  | "go.infra"
  | "go.protect"
  | "go.activity"
  | "go.settings"
  | "list.next"
  | "list.prev"
  | "list.select"
  | "list.peek"
  | "list.open";

export interface Shortcut {
  id: ShortcutId;
  /** 차례로 누를 키(`["g", "v"]`). `mod+k`는 ⌘K(맥)·Ctrl+K다. */
  keys: string[];
  group: "general" | "navigation" | "list";
}

/** 도움말 표의 순서이기도 하다. */
export const SHORTCUTS: readonly Shortcut[] = [
  { id: "palette", keys: ["mod+k"], group: "general" },
  { id: "help", keys: ["?"], group: "general" },
  { id: "search", keys: ["/"], group: "general" },
  { id: "create", keys: ["c"], group: "general" },
  { id: "rail", keys: ["["], group: "general" },
  { id: "favorite", keys: ["f"], group: "general" },
  // Shift+N(키 이름은 대문자 N)
  { id: "center", keys: ["N"], group: "general" },
  { id: "go.home", keys: ["g", "h"], group: "navigation" },
  { id: "go.vms", keys: ["g", "v"], group: "navigation" },
  { id: "go.infra", keys: ["g", "i"], group: "navigation" },
  { id: "go.protect", keys: ["g", "p"], group: "navigation" },
  { id: "go.activity", keys: ["g", "a"], group: "navigation" },
  { id: "go.settings", keys: ["g", "s"], group: "navigation" },
  // 목록 키는 공통 표(`DataTable`)가 받는다. 여기서는 도움말에만 보인다.
  { id: "list.next", keys: ["j"], group: "list" },
  { id: "list.prev", keys: ["k"], group: "list" },
  { id: "list.select", keys: ["x"], group: "list" },
  { id: "list.peek", keys: ["Space"], group: "list" },
  { id: "list.open", keys: ["Enter"], group: "list" },
];

/** 머리줄이 받는 단축키(목록 키는 표가 받는다) */
const GLOBAL = SHORTCUTS.filter((s) => s.group !== "list");

/** 순서 키의 둘째 키를 기다리는 시간 */
export const SEQUENCE_MS = 1000;

/**
 * 순서 키 해석기. `feed`에 키와 시각을 넣으면 끝난 단축키 id를, 아직 아니면 null을 준다. 앞 키 뒤 `timeout`이
 * 지나면 앞 키를 잊는다. 맞지 않는 키는 그 키로 새로 시작한다(`g x g v`의 끝은 `g v`).
 */
export function createSequence(shortcuts: readonly Shortcut[] = GLOBAL, timeout = SEQUENCE_MS) {
  let pending: string[] = [];
  let last = 0;
  const startsWith = (seq: string[]) =>
    shortcuts.some((s) => seq.length <= s.keys.length && seq.every((k, i) => s.keys[i] === k));
  return {
    feed(key: string, now: number): ShortcutId | null {
      if (pending.length > 0 && now - last > timeout) {
        pending = [];
      }
      last = now;
      for (const seq of [[...pending, key], [key]]) {
        const hit = shortcuts.find(
          (s) => s.keys.length === seq.length && s.keys.every((k, i) => seq[i] === k),
        );
        if (hit) {
          pending = [];
          return hit.id;
        }
        if (startsWith(seq)) {
          pending = seq;
          return null;
        }
      }
      pending = [];
      return null;
    },
    reset() {
      pending = [];
    },
  };
}

/** 글을 입력하는 곳이면 한 글자 단축키를 받지 않는다. */
export function isTyping(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }
  return (
    target.isContentEditable ||
    target.closest("input, textarea, select, [contenteditable=''], [contenteditable='true']") !==
      null
  );
}

/** 팔레트 말고 열린 대화상자·서랍이 있다(그 안에서는 단축키를 받지 않는다). */
export function dialogOpen(): boolean {
  return (
    document.querySelector("[role=dialog]:not([data-palette]), [role=alertdialog], [role=menu]") !==
    null
  );
}

/**
 * 눌린 글자. 한글 입력 상태에서는 `e.key`가 `ㅎ`처럼 영문이 아니므로, 영문이 아닌 한 글자이면 물리 키(`e.code`의
 * `KeyG`)의 영문자로 바꾼다(Shift면 대문자). 영문 자판(AZERTY 등)은 `e.key`를 그대로 쓴다.
 */
export function letterKey(e: Pick<KeyboardEvent, "key" | "code" | "shiftKey">): string {
  const letter = /^Key([A-Z])$/.exec(e.code ?? "")?.[1];
  if (e.key.length === 1 && e.key > "\x7f" && letter) {
    return e.shiftKey ? letter : letter.toLowerCase();
  }
  return e.key;
}

/** 키 이벤트를 표의 키 이름으로. 수정 키가 있으면 `mod+k` 말고는 null이다. */
export function keyName(e: KeyboardEvent): string | null {
  const key = letterKey(e);
  if ((e.metaKey || e.ctrlKey) && !e.altKey && key.toLowerCase() === "k") {
    return "mod+k";
  }
  if (e.metaKey || e.ctrlKey || e.altKey) {
    return null;
  }
  return key.length === 1 ? key : null;
}

/**
 * 문서에 단축키를 단다. `enabled`가 거짓이면(콘솔 화면) 아무것도 받지 않는다. ⌘K는 입력 칸 안에서도 받고, 한 글자
 * 키는 입력 칸·대화상자 밖에서만 받는다.
 */
export function useShortcuts(handlers: Partial<Record<ShortcutId, () => void>>, enabled = true) {
  const latest = useRef(handlers);
  useEffect(() => {
    latest.current = handlers;
  });
  useEffect(() => {
    if (!enabled) {
      return;
    }
    const sequence = createSequence();
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing) {
        return;
      }
      const key = keyName(e);
      if (key === null) {
        return;
      }
      if (key === "mod+k") {
        if (!document.querySelector("[role=dialog]:not([data-palette]), [role=alertdialog]")) {
          const run = latest.current.palette;
          if (run) {
            e.preventDefault();
            run();
          }
        }
        return;
      }
      if (isTyping(e.target) || dialogOpen()) {
        sequence.reset();
        return;
      }
      const id = sequence.feed(key, Date.now());
      const run = id ? latest.current[id] : undefined;
      if (run) {
        e.preventDefault();
        run();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [enabled]);
}

/** 맥에서는 ⌘, 그 밖에서는 Ctrl */
export function isMac(): boolean {
  return /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
}

/** 도움말·머리줄에 보이는 키 글 */
export function keyLabel(key: string): string {
  if (key === "mod+k") {
    return isMac() ? "⌘K" : "Ctrl K";
  }
  // 대문자는 Shift와 함께 누른다.
  if (/^[A-Z]$/.test(key)) {
    return isMac() ? `⇧${key}` : `Shift ${key}`;
  }
  return key;
}
