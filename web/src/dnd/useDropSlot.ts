import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { speak, useDnd } from "@/dnd/DndProvider";

/**
 * 놓을 칸(DOM 원소). 들고 있으면 `data-drop="ok|no"`(놓을 수 있는 칸만 밝힘 — 모양은 쓰는 쪽), 그 위면 `data-over`,
 * 누르기·키보드로 든 채면 놓을 수 있는 칸에 초점이 가고(`tabIndex=-1`) 누르면 놓는다. `label`은 알림 줄의 칸 이름이다.
 */
export function useDropSlot<E extends HTMLElement = HTMLElement>(id: string, label: string) {
  const { t } = useTranslation();
  const dnd = useDnd();
  const register = dnd.slot;
  // 콜백 ref(React 19의 정리 함수): 원소가 붙으면 달고, 떨어지거나 id·이름이 바뀌면 뗀다.
  const ref = useCallback(
    (el: E | null) => (el ? register(id, el, label) : undefined),
    [register, id, label],
  );
  const s = dnd.state;
  const active = s.phase === "dragging" || s.phase === "picked";
  const verdict = active ? dnd.verdict(s.item, id) : null;
  const over = active && s.over === id;
  const description = !verdict
    ? undefined
    : verdict.ok
      ? speak(t, { key: verdict.warn ? "dnd.warn" : "dnd.ok", values: { warn: verdict.warn } })
      : speak(t, {
          key: verdict.reason ? "dnd.no" : "dnd.noPlain",
          values: { reason: verdict.reason },
        });
  return {
    ref,
    verdict,
    over,
    props: {
      "data-drop": verdict ? (verdict.ok ? "ok" : "no") : undefined,
      "data-over": over ? "" : undefined,
      tabIndex: s.phase === "picked" && verdict?.ok ? -1 : undefined,
      "aria-description": description,
      onClick:
        s.phase === "picked"
          ? () => {
              dnd.send({ type: "drop", slot: id });
            }
          : undefined,
    },
  };
}
