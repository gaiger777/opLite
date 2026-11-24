import type { KeyboardEvent, PointerEvent } from "react";
import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { useDnd } from "@/dnd/DndProvider";
import type { Item } from "@/dnd/types";

/**
 * 끌 수 있는 원소(부품·블록·지도 밖의 끌 것)의 속성: 포인터 누르기(마우스 4px·터치 길게 누르기는 틀이 정함),
 * `Space`·`Enter`로 들기(다시 누르면 내려놓기), 화면 읽기 이름(`aria-roledescription`·키 안내). 원소는 `button`이어야
 * 한다. `role`은 "끌 수 있는 부품" 같은 이름(없으면 틀의 기본).
 */
export function useDraggable<I extends Item>(item: I, role?: string) {
  const { t } = useTranslation();
  const dnd = useDnd();
  const s = dnd.state;
  const held = (s.phase === "dragging" || s.phase === "picked") && s.item.id === item.id;
  const register = dnd.item;
  const ref = useCallback((el: HTMLElement | null) => register(item.id, el), [register, item.id]);
  return {
    ref,
    "data-dnd-item": "",
    "data-held": held ? "" : undefined,
    "aria-roledescription": role ?? t("dnd.role"),
    "aria-describedby": dnd.hint,
    onPointerDown(e: PointerEvent<HTMLElement>) {
      if (e.button !== 0 || !e.isPrimary) {
        return;
      }
      // 든 채 놓을 수 있는 칸 안의 다른 블록을 누르면 그 칸에 놓기다(칸의 click이 받는다).
      if (
        s.phase === "picked" &&
        s.item.id !== item.id &&
        e.currentTarget.closest("[data-drop=ok]")
      ) {
        return;
      }
      dnd.send({
        type: "down",
        item,
        point: { x: e.clientX, y: e.clientY },
        touch: e.pointerType === "touch",
      });
    },
    onKeyDown(e: KeyboardEvent<HTMLElement>) {
      if (e.key === " " || e.key === "Enter") {
        e.preventDefault();
        dnd.send({ type: "pick", item, key: true });
      }
    },
    onContextMenu(e: { preventDefault(): void }) {
      // 길게 누르기의 문맥 메뉴를 막는다.
      if (s.phase === "pressing") {
        e.preventDefault();
      }
    },
  };
}
