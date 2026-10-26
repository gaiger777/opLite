import type { RefObject } from "react";

/** 닫힐 때 `returnFocus`가 아직 화면에 있으면 그리로 포커스를 돌린다. */
export function focusBack(e: Event, returnFocus?: RefObject<HTMLElement | null>) {
  const target = returnFocus?.current;
  if (target?.isConnected) {
    e.preventDefault();
    target.focus();
  }
}
