import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { type List, listKey, useList } from "@/api/live";
import type { Task, Vm } from "@/api/types";
import { onTrack, settleTasks, type TaskToastAction } from "@/tasks/tasks";
import { availability, useVmActions } from "@/vms/actions";

/** watch로 바뀐 Task 목록을 보고 진행 중 알림을 결과로 바꾼다. */
export function TaskToasts() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const tasks = useList("task");
  const { run } = useVmActions();
  // `run`은 그릴 때마다 새로 만들어지므로 ref로 넘겨 `actionFor`(와 아래 두 효과의 구독)가 매번 바뀌지 않게 한다.
  const runRef = useRef(run);
  useEffect(() => {
    runRef.current = run;
  });
  // VM 정지(강제 정지 포함)가 끝나면 알림에 "다시 켜기"를 단다(설계 §5.8). 지켜보는 Task는 이 사용자가 요청한
  // 것이므로 켤 권한도 있다. VM을 이미 받은 목록에서 찾지 못하면 단추를 달지 않는다.
  const actionFor = useCallback(
    (task: Task): TaskToastAction | undefined => {
      const kind = task.spec.op.kind;
      if (kind !== "vm-stop" && kind !== "vm-force-stop") {
        return undefined;
      }
      const find = () =>
        queryClient
          .getQueryData<List<Vm>>(listKey("vm"))
          ?.items.find((v) => v.id === task.spec.target);
      return (
        find() && {
          label: t("vms.startAgain"),
          // 누를 때의 VM으로 다시 본다(그 사이 켜졌거나 지워졌으면 보내지 않고 까닭을 알린다).
          onClick: () => {
            const vm = find();
            const able = vm && availability(vm, "start", []);
            if (vm && able?.ok) {
              void runRef.current(vm, "start");
            } else {
              toast.error(t(able && !able.ok ? able.reason : "vms.bulk.reason.gone"));
            }
          },
        }
      );
    },
    [queryClient, t],
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
