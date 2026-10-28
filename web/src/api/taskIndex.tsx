import { createContext, useContext, useMemo } from "react";

import { useList } from "@/api/live";
import type { Task } from "@/api/types";

const TaskIndexContext = createContext<Map<string, Task[]>>(new Map());

/** 끝나지 않은 Task를 대상 id별로 모은 색인을 한 번 만든다. 행마다 Task 목록을 훑지 않게 한다. */
export function TaskIndexProvider({ children }: { children: React.ReactNode }) {
  const tasks = useList("task");
  const index = useMemo(() => {
    const byTarget = new Map<string, Task[]>();
    for (const task of tasks.data ?? []) {
      if (task.status.state === "succeeded" || task.status.state === "failed") {
        continue;
      }
      const list = byTarget.get(task.spec.target);
      if (list) {
        list.push(task);
      } else {
        byTarget.set(task.spec.target, [task]);
      }
    }
    return byTarget;
  }, [tasks.data]);
  return <TaskIndexContext value={index}>{children}</TaskIndexContext>;
}

/** 대상 id → 끝나지 않은 Task. Task 목록을 아직 모르면 비어 있다. */
export function useTaskIndex(): Map<string, Task[]> {
  return useContext(TaskIndexContext);
}
