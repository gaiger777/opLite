import { hashKey, type QueryClient } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { toast } from "sonner";

import { type List, listKey } from "@/api/live";
import type { Task } from "@/api/types";

/** 결과를 기다리는 Task: id → 알림에 쓸 설명 */
const pending = new Map<string, string>();

let tracked = () => {};

/** 지켜볼 Task가 생기면 부를 함수를 정한다(`TaskToasts`). 해제 함수를 돌려준다. */
export function onTrack(listener: () => void) {
  tracked = listener;
  return () => {
    // 그 사이 다른 리스너로 바뀌었으면 그대로 둔다.
    if (tracked === listener) {
      tracked = () => {};
    }
  };
}

/** 지켜보던 Task를 모두 잊고 그 알림을 닫는다. 로그아웃하거나 세션이 끝나면 부른다. */
export function resetTasks() {
  for (const id of pending.keys()) {
    toast.dismiss(id);
  }
  pending.clear();
}

/**
 * API가 202로 돌려준 Task의 진행을 알림으로 보여 준다. 결과는 watch로 들어온 Task 목록에서
 * `settleTasks`가 찾는다.
 */
export function trackTask(task: Task, label: string) {
  pending.set(task.id, label);
  toast.loading(label, { id: task.id });
  tracked();
}

/** 끝난 Task의 알림을 결과로 바꾼다. Task 목록이 바뀔 때마다 부른다. */
export function settleTasks(tasks: Task[], t: TFunction) {
  for (const task of tasks) {
    const label = pending.get(task.id);
    if (label === undefined) {
      continue;
    }
    if (task.status.state === "succeeded") {
      pending.delete(task.id);
      toast.success(t("tasks.done", { label }), { id: task.id });
    } else if (task.status.state === "failed") {
      pending.delete(task.id);
      toast.error(t("tasks.failed", { label, error: task.status.error ?? "" }), {
        id: task.id,
        duration: Infinity,
        closeButton: true,
      });
    }
  }
}

/** Task를 사람이 읽을 문장으로: "web-01 시작" */
export function describeTask(task: Task, t: TFunction, target: string): string {
  const kind = task.spec.op.kind;
  // 모르는 작업(새 서버)은 번역 키 대신 종류를 그대로 보여 준다.
  return t(`tasks.op.${kind}`, { target, defaultValue: `${kind} ${target}` });
}

/** watch로 들어온 Task 목록에서 그 Task가 끝나기를 기다린다. 실패하면 Task의 오류로 거부한다. */
export function waitForTask(queryClient: QueryClient, id: string): Promise<Task> {
  const key = hashKey(listKey("task"));
  return new Promise((resolve, reject) => {
    let unsubscribe = () => {};
    const check = () => {
      const task = queryClient
        .getQueryData<List<Task>>(listKey("task"))
        ?.items.find((t) => t.id === id);
      if (task?.status.state === "succeeded") {
        unsubscribe();
        resolve(task);
      } else if (task?.status.state === "failed") {
        unsubscribe();
        reject(new Error(task.status.error ?? "task failed"));
      }
    };
    unsubscribe = queryClient.getQueryCache().subscribe((event) => {
      if (event.query.queryHash === key) {
        check();
      }
    });
    check();
  });
}
