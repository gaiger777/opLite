import { useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";

import { type List, listKey, useList } from "@/api/live";
import type { Task } from "@/api/types";
import { onTrack, settleTasks } from "@/tasks/tasks";

/** watch로 바뀐 Task 목록을 보고 진행 중 알림을 결과로 바꾼다. */
export function TaskToasts() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const tasks = useList("task");
  useEffect(() => {
    if (tasks.data) {
      settleTasks(tasks.data, t);
    }
  }, [tasks.data, t]);
  // 202 응답보다 watch가 알린 결과가 먼저 올 수 있으므로 지켜보기 시작할 때도 지금 목록을 본다.
  useEffect(
    () =>
      onTrack(() => {
        const list = queryClient.getQueryData<List<Task>>(listKey("task"));
        if (list) {
          settleTasks(list.items, t);
        }
      }),
    [queryClient, t],
  );
  return null;
}
