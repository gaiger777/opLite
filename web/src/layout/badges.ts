// 사이드바 항목 옆 숫자(계획 77-3). 머리줄 종이 이미 받는 Task 목록과 경보(30초)에서 센다 — 새 요청이 없다.
import { useAlerts } from "@/alerts/alerts";
import { useList } from "@/api/live";
import type { Task } from "@/api/types";
import type { NavItem } from "@/layout/nav";
import { isRinging } from "@/tasks/center";

/** 실패를 세는 기간. 오래된 실패가 숫자를 영영 붙잡지 않게 한다. */
export const FAILED_WINDOW_MS = 24 * 60 * 60 * 1000;

/** 최근(`FAILED_WINDOW_MS` 안에 끝난) 실패한 Task 수. 끝 시각이 없는 옛 노드의 Task는 세지 않는다. */
export function recentFailures(tasks: readonly Task[], now: number): number {
  return tasks.filter(
    (task) =>
      task.status.state === "failed" &&
      task.status.finished_at !== undefined &&
      now - task.status.finished_at < FAILED_WINDOW_MS,
  ).length;
}

export type Badges = Record<NonNullable<NavItem["badge"]>, number>;

export function useNavBadges(): Badges {
  const tasks = useList("task");
  const alerts = useAlerts();
  return {
    alerts: alerts.data?.alerts.filter(isRinging).length ?? 0,
    // 지금 시각 대신 목록을 마지막으로 받은 시각으로 잰다(그리기는 순수해야 한다 — 목록은 실시간으로 바뀐다).
    failedTasks: recentFailures(tasks.data ?? [], tasks.dataUpdatedAt),
  };
}
