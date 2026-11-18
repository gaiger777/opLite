import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect } from "react";
import { useTranslation } from "react-i18next";

import { type List, listKey, useList } from "@/api/live";
import type { Task, Vm } from "@/api/types";
import { onTrack, settleTasks, type TaskToastAction } from "@/tasks/tasks";
import { useVmActions } from "@/vms/actions";

/** watch로 바뀐 Task 목록을 보고 진행 중 알림을 결과로 바꾼다. */
export function TaskToasts() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const tasks = useList("task");
  const { run } = useVmActions();
  // VM 정지(강제 정지 포함)가 끝나면 알림에 "다시 켜기"를 단다(설계 §5.8). 지켜보는 Task는 이 사용자가 요청한
  // 것이므로 켤 권한도 있다. VM을 이미 받은 목록에서 찾지 못하면 단추를 달지 않는다.
  const actionFor = useCallback(
    (task: Task): TaskToastAction | undefined => {
      const kind = task.spec.op.kind;
      if (kind !== "vm-stop" && kind !== "vm-force-stop") {
        return undefined;
      }
      const vm = queryClient
        .getQueryData<List<Vm>>(listKey("vm"))
        ?.items.find((v) => v.id === task.spec.target);
      return vm && { label: t("vms.startAgain"), onClick: () => void run(vm, "start") };
    },
    [queryClient, t, run],
  );
  useEffect(() => {
    const list = queryClient.getQueryData<List<Task>>(listKey("task"));
    if (tasks.data && list) {
      settleTasks(list, t, actionFor);
    }
  }, [tasks.data, queryClient, t, actionFor]);
  // 202 응답보다 watch가 알린 결과가 먼저 올 수 있으므로 지켜보기 시작할 때도 지금 목록을 본다.
  useEffect(
    () =>
      onTrack(() => {
        const list = queryClient.getQueryData<List<Task>>(listKey("task"));
        if (list) {
          settleTasks(list, t, actionFor);
        }
      }),
    [queryClient, t, actionFor],
  );
  return null;
}
