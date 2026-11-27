import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { AlertList } from "@/alerts/AlertList";
import { useAlerts } from "@/alerts/alerts";
import { SEVERITIES } from "@/alerts/severity";
import { useList } from "@/api/live";
import type { Task } from "@/api/types";
import { ACTION_ICONS } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/i18n/errors";
import { formatTime, uuidTime } from "@/lib/format";
import { isActive, isRinging, setCenterOpen } from "@/tasks/center";
import { failureTitle } from "@/tasks/failures/failure";
import { useFailureStrings } from "@/tasks/failures/strings";
import { TaskErrorText } from "@/tasks/TaskErrorText";
import { useTaskHref } from "@/tasks/taskLink";
import { alertHref } from "@/tasks/target";
import { describeProgress, describeTask } from "@/tasks/tasks";
import { newestFirst, TaskStateBadge, useTaskTarget } from "@/tasks/TasksPage";
import { retryLabel } from "@/tasks/retry";
import { useRetry } from "@/tasks/useRetry";

/** 최근(끝난) Task를 몇 개 보일지 */
export const RECENT_LIMIT = 10;

/** 진행률(바이트를 아는 단계만, 0–100) */
function percent(task: Task): number | null {
  const progress = task.status.progress;
  if (!progress || progress.total_bytes <= 0) {
    return null;
  }
  return Math.floor(
    (Math.min(progress.done_bytes, progress.total_bytes) * 100) / progress.total_bytes,
  );
}

/**
 * 작업·알림 센터의 본문(설계 §5.4): 도는 Task(진행률), 최근에 끝난 Task(실패는 다시 하기), 활성 경보. 데이터는 이미
 * 받는 `useList("task")`와 `useAlerts()`다. 서랍이 열려 있을 때만 그린다.
 */
export function NotificationCenter() {
  const { t } = useTranslation();
  const tasks = useList("task");
  const alerts = useAlerts();
  const all = useMemo(() => newestFirst(tasks.data ?? []), [tasks.data]);
  const running = all.filter(isActive);
  const recent = all.filter((task) => !isActive(task)).slice(0, RECENT_LIMIT);
  const active = useMemo(
    () =>
      [...(alerts.data?.alerts ?? [])].sort(
        (a, b) => SEVERITIES.indexOf(a.severity) - SEVERITIES.indexOf(b.severity),
      ),
    [alerts.data],
  );
  return (
    // 안의 링크를 누르면 서랍을 닫고 그 화면으로 간다.
    <div
      className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto px-4 pb-4"
      onClick={(e) => {
        if (e.target instanceof Element && e.target.closest("a")) {
          setCenterOpen(false);
        }
      }}
    >
      <Section id="center-running" title={t("center.running", { n: running.length })}>
        {running.length === 0 ? (
          <Empty>{t("center.noRunning")}</Empty>
        ) : (
          <TaskItems tasks={running} labelledBy="center-running" />
        )}
      </Section>
      <Section id="center-recent" title={t("center.recent")}>
        {recent.length === 0 ? (
          <Empty>{t("center.noRecent")}</Empty>
        ) : (
          <TaskItems tasks={recent} labelledBy="center-recent" />
        )}
        <Link to="/tasks" className="text-sm text-brand underline-offset-4 hover:underline">
          {t("center.allTasks")}
        </Link>
      </Section>
      <Section
        id="center-alerts"
        title={t("center.alerts", { n: active.filter(isRinging).length })}
      >
        {alerts.isError ? (
          <Empty>{errorMessage(t, alerts.error)}</Empty>
        ) : alerts.data ? (
          <AlertList alerts={active} compact label={t("alerts.active")} subjectHref={alertHref} />
        ) : (
          <Empty>{t("common.loading")}</Empty>
        )}
        <Link to="/alerts" className="text-sm text-brand underline-offset-4 hover:underline">
          {t("alerts.viewAll")}
        </Link>
      </Section>
    </div>
  );
}

function Section({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section aria-labelledby={id} className="flex flex-col gap-2">
      <h3 id={id} className="text-xs font-medium text-muted-foreground">
        {title}
      </h3>
      {children}
    </section>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return <p className="text-sm text-muted-foreground">{children}</p>;
}

/** Task 목록: 상태 칩, 상세 서랍을 여는 설명, 진행률, 시각과 오류(2줄 + 펼침), 실패한 전원·백업 작업의 다시 하기 */
function TaskItems({ tasks, labelledBy }: { tasks: Task[]; labelledBy: string }) {
  const { t, i18n } = useTranslation();
  const target = useTaskTarget();
  const taskHref = useTaskHref();
  const nodes = useList("node");
  const { of, retry, busy } = useRetry();
  // 실패 원인 사전(계획 77-2): 원인 코드가 있는 실패가 보일 때만 받는다.
  useFailureStrings(tasks.some((task) => task.status.failure));
  const nodeName = (id: string) => nodes.data?.find((n) => n.id === id)?.name;

  return (
    <ul aria-labelledby={labelledBy} className="flex flex-col divide-y">
      {tasks.map((task) => {
        const { name } = target(task);
        const label = describeTask(task, t, name, nodeName);
        const progress = describeProgress(task, t);
        const pct = percent(task);
        const again = of(task);
        const time = uuidTime(task.id);
        return (
          <li key={task.id} className="flex flex-col gap-1 py-2">
            <div className="flex items-start gap-2">
              <TaskStateBadge task={task} />
              {/* 누르면 그 Task의 상세 서랍(계획 77). 이름은 자르지 않고 줄바꿈한다. */}
              <Link
                to={taskHref(task.id)}
                className="min-w-0 font-medium break-words underline-offset-4 hover:underline"
              >
                {label}
              </Link>
              <time
                dateTime={time.toISOString()}
                className="ml-auto shrink-0 text-xs text-muted-foreground"
              >
                {formatTime(time, i18n.language)}
              </time>
            </div>
            {pct !== null && (
              <div
                role="progressbar"
                aria-label={label}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={pct}
                className="h-1.5 overflow-hidden rounded-full bg-muted"
              >
                <div className="h-full bg-brand" style={{ width: `${pct}%` }} />
              </div>
            )}
            {progress && <span className="text-xs text-muted-foreground">{progress}</span>}
            {task.status.error && (
              <TaskErrorText
                text={task.status.error}
                summary={failureTitle(task, t)}
                warning={task.status.state === "succeeded"}
                subject={label}
              />
            )}
            {again && (
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={again.reason !== undefined || busy.has(task.id)}
                  aria-label={
                    again.fixFirst
                      ? t("center.retryAfterFixNamed", { label })
                      : t("center.retryNamed", { label })
                  }
                  onClick={(e) => void retry(task, again, e.currentTarget)}
                >
                  <ACTION_ICONS.retry />
                  {retryLabel(again, t)}
                </Button>
                {again.reason && (
                  <span className="text-xs text-muted-foreground">{t(again.reason)}</span>
                )}
              </div>
            )}
          </li>
        );
      })}
    </ul>
  );
}
