// Task 상세 서랍(계획 77)을 여는 주소: 지금 화면 그대로 `?task=<id>`를 더한다(VM 상세의 `?tab=tasks`도 남는다).
import type { TFunction } from "i18next";
import { useCallback } from "react";
import { useLocation } from "react-router";

import type { Task } from "@/api/types";
import { TASK_PARAM } from "@/tasks/center";
import { type Named, TASK_LABELS } from "@/tasks/target";

/** 지금 화면에서 그 Task의 서랍을 여는 주소 */
export function useTaskHref(): (id: string) => string {
  const { pathname, search } = useLocation();
  return useCallback(
    (id: string) => {
      const params = new URLSearchParams(search);
      params.set(TASK_PARAM, id);
      return `${pathname}?${params.toString()}`;
    },
    [pathname, search],
  );
}

/**
 * 요청한 사람: 서버가 단 이름, 이름이 없는 옛 Task는 이미 받은 사용자 목록(`named`)에서 찾고 없으면 사용자 id의 끝, 사람이
 * 요청하지 않았으면 "자동"
 */
export function requesterName(task: Task, t: TFunction, named?: Map<string, Named>): string {
  const name = task.labels?.[TASK_LABELS.requestedByName];
  if (name) {
    return name;
  }
  const id = task.labels?.[TASK_LABELS.requestedBy];
  return id ? (named?.get(id)?.name ?? id.slice(-8)) : t("tasks.automatic");
}
