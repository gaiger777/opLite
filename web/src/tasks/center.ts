// 작업·알림 센터(머리줄 종 서랍, 설계 §5.4)가 열렸는지. 서랍(`AppLayout`), Task 알림(`settleTasks`), 실패 알림의
// "보기"가 함께 쓰므로 맥락 대신 모듈 상태로 둔다.
import { useSyncExternalStore } from "react";

import type { AlertView } from "@/alerts/alerts";
import type { Task } from "@/api/types";

let open = false;
const listeners = new Set<() => void>();

/** 센터가 열려 있는지(알림 중복을 줄일 때 본다) */
export function isCenterOpen(): boolean {
  return open;
}

/** 센터를 열거나 닫는다. */
export function setCenterOpen(value: boolean) {
  if (open !== value) {
    open = value;
    for (const listener of listeners) {
      listener();
    }
  }
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** 센터가 열려 있는지(바뀌면 다시 그린다) */
export function useCenterOpen(): boolean {
  return useSyncExternalStore(subscribe, isCenterOpen);
}

/** 끝나지 않은 Task */
export function isActive(task: Task): boolean {
  return task.status.state === "pending" || task.status.state === "running";
}

/** 울리는 경보(critical·warning)인지. 종의 숫자에 센다. */
export function isRinging(alert: AlertView): boolean {
  return alert.state === "firing" && alert.severity !== "info";
}

/** Task 상세 서랍을 여는 함수(`TaskDrawer`가 정한다 — 주소에 `?task=<id>`). 서랍이 없으면 센터를 연다. */
let taskOpener: ((id: string) => void) | null = null;

/** 서랍을 여는 함수를 정한다. 해제 함수를 돌려준다. */
export function setTaskOpener(opener: (id: string) => void) {
  taskOpener = opener;
  return () => {
    if (taskOpener === opener) {
      taskOpener = null;
    }
  };
}

/** Task 상세 서랍을 연다(실패 알림의 "보기"). */
export function openTask(id: string) {
  if (taskOpener) {
    taskOpener(id);
  } else {
    setCenterOpen(true);
  }
}

/** Task 상세 서랍을 여는 주소의 쿼리 키 */
export const TASK_PARAM = "task";
