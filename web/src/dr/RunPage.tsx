import { useQueryClient } from "@tanstack/react-query";
import { cn } from "cn";
import { Ban } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import {
  type DrPhase,
  type DrRun,
  type DrStep,
  localized,
  oldNodes,
  refreshDr,
  usePlan,
  useRun,
} from "@/dr/api";
import { CLEANUP_DOTS, failedPhase, formatDuration, PHASE_DOTS, rtoClock } from "@/dr/model";
import { UpgradeNotice } from "@/dr/PlansPage";
import { cancelText, PhaseChip, PhaseDots, RunStateChip, useNow } from "@/dr/parts";
import { formatTime } from "@/lib/format";
import { TaskStateBadge } from "@/tasks/TasksPage";

/** 복원이 끝난 단계 */
const AFTER_RESTORE: ReadonlySet<DrPhase> = new Set(["starting", "waiting", "ready", "timed_out"]);

interface Band {
  name: string;
  index: number;
  wait?: string;
  steps: DrStep[];
}

/** 실행의 그룹 띠: 계획의 그룹 차례로 단계를 묶는다. 뒷정리 실행(그룹 없음)은 띠 하나다. */
function bands(run: DrRun): Band[] {
  const steps = run.status.steps ?? [];
  if (run.spec.kind === "cleanup") {
    return [{ name: "", index: 0, steps }];
  }
  return run.spec.plan_spec.groups.map((g, index) => ({
    name: g.name,
    index,
    wait: g.wait ?? "running",
    steps: steps.filter((s) => s.group === g.name),
  }));
}

/**
 * DR 실행 하나(계획 76 §4): 그룹 띠, VM마다 단계 점·단계 칩·새 VM·Task·오류, RTO 시계(도는 동안 1초마다 흐름). watch 사건
 * (`dr-run`)이 오면 다시 읽는다. 취소는 루트 `cluster.admin`이다. 늦게 읽는 조각이다.
 */
export function RunPage() {
  const { id = "" } = useParams();
  const run = useRun(id);
  if (!run.data) {
    return <ListPending error={run.error} />;
  }
  return <RunView run={run.data} />;
}

function RunView({ run }: { run: DrRun }) {
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const tasks = useList("task");
  const nodes = useList("node");
  const old = oldNodes(nodes.data);
  const [cancelling, setCancelling] = useState(false);
  const status = run.status;
  const cleanup = run.spec.kind === "cleanup";
  // 취소한 시험이 끝나면 서버가 뒷정리를 저절로 시작한다: 계획의 지금 실행으로 가는 길(리뷰 1 웹 4)
  const plan = usePlan(
    run.spec.kind === "test" && status.state === "cancelled" ? run.spec.plan : undefined,
  );
  const autoCleanup = plan.data?.status.run !== run.id ? plan.data?.status.run : undefined;
  const live = status.state === "running";
  const now = useNow(live);
  const rto = rtoClock(status, now);
  const time = (s: number) => formatTime(new Date(s * 1000), i18n.language);
  const taskOf = (id: string | undefined) => tasks.data?.find((task) => task.id === id);
  const dots = cleanup ? CLEANUP_DOTS : PHASE_DOTS;
  const canCancel = live && !status.cancel_requested && perms.can("operate", "recovery-plan");

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="dr-run-title"
        title={t("dr.run.title", { kind: t(`dr.kind.${run.spec.kind}`), plan: run.spec.plan_name })}
        description={
          <span className="flex flex-wrap items-center gap-2">
            <RunStateChip state={status.state} />
            <span>{t("dr.run.started", { time: time(status.started_at) })}</span>
            {status.finished_at !== undefined && (
              <span>{t("dr.run.finished", { time: time(status.finished_at) })}</span>
            )}
            <Link
              to={`/dr-plans/${encodeURIComponent(run.spec.plan_name)}`}
              className="underline-offset-4 hover:underline"
            >
              {t("dr.run.plan", { name: run.spec.plan_name })}
            </Link>
          </span>
        }
        actions={
          canCancel && (
            <Button variant="outline" onClick={() => setCancelling(true)}>
              <Ban />
              {t("dr.action.cancel")}
            </Button>
          )
        }
      />
      {live && <UpgradeNotice old={old} />}
      {autoCleanup && (
        <p role="status" className="text-sm">
          <Link to={`/dr-runs/${autoCleanup}`} className="underline-offset-4 hover:underline">
            {t("dr.run.openCleanup")}
          </Link>
        </p>
      )}
      <dl className="flex flex-wrap gap-x-8 gap-y-2 text-sm">
        <div>
          {/* 뒷정리에는 RTO가 없다: 걸린 시간(리뷰 1 웹 13) */}
          <dt className="text-muted-foreground">{t(cleanup ? "dr.run.elapsed" : "dr.runs.rto")}</dt>
          <dd
            className="text-2xl font-semibold tabular-nums"
            // 1초마다 바뀌는 시계를 화면 읽기 프로그램이 읽지 않게(상태 칩이 상태를 알린다)
            aria-live="off"
            data-testid="rto-clock"
          >
            {cleanup
              ? formatDuration((status.finished_at ?? now) - status.started_at)
              : rto !== undefined
                ? formatDuration(rto)
                : "—"}
          </dd>
        </div>
        {status.prepare_secs !== undefined && (
          <div>
            <dt className="text-muted-foreground">{t("dr.run.prepare")}</dt>
            <dd className="text-2xl font-semibold tabular-nums">
              {formatDuration(status.prepare_secs)}
            </dd>
          </div>
        )}
        {status.delay_until !== undefined && live && (
          <div>
            <dt className="text-muted-foreground">{t("dr.run.nextGroup")}</dt>
            <dd className="tabular-nums">{time(status.delay_until)}</dd>
          </div>
        )}
      </dl>
      {status.cancel_requested && live && (
        <p role="status" className="text-sm text-warning-fg">
          {t("dr.run.cancelling")}
        </p>
      )}
      {status.stopping && live && (
        <p role="status" className="text-sm text-warning-fg">
          {t("dr.run.stopping")}
        </p>
      )}
      {status.error && (
        <p role="alert" className="text-sm text-destructive">
          {status.error}
        </p>
      )}
      {(status.bubbles ?? []).length > 0 && (
        <section aria-labelledby="dr-bubbles" className="flex flex-col gap-1">
          <h2 id="dr-bubbles" className="text-base font-semibold">
            {t("dr.run.bubbles")}
          </h2>
          <ul className="flex flex-col gap-1 text-sm">
            {status.bubbles!.map((b) => (
              <li key={b.name} className="flex flex-wrap gap-x-2">
                <span className="font-medium">{b.name}</span>
                {b.source && (
                  <span className="text-muted-foreground">
                    {t("dr.run.bubbleFor", { network: b.source })}
                  </span>
                )}
                <span>{t(`dr.bubble.${b.state}`)}</span>
                {b.error && <span className="text-destructive">{b.error}</span>}
              </li>
            ))}
          </ul>
        </section>
      )}
      <ol className="flex flex-col gap-3">
        {bands(run).map((band) => {
          const current = live && status.group === band.index && run.spec.kind !== "cleanup";
          const label = band.name
            ? t("dr.run.group", { n: band.index + 1, name: band.name })
            : t("dr.kind.cleanup");
          return (
            <li
              key={band.index}
              aria-label={label}
              aria-current={current ? "step" : undefined}
              className={cn("rounded-lg border p-3", current && "border-info")}
            >
              <div className="flex flex-wrap items-center justify-between gap-2">
                <h2 className="text-sm font-semibold">{label}</h2>
                {band.wait && (
                  <span className="text-xs text-muted-foreground">
                    {t("dr.run.waitFor", { wait: t(`dr.wait.${band.wait}`) })}
                  </span>
                )}
              </div>
              {band.steps.length === 0 ? (
                <p className="mt-2 text-sm text-muted-foreground">{t("dr.run.noSteps")}</p>
              ) : (
                <ul className="mt-2 flex flex-col divide-y">
                  {band.steps.map((s) => {
                    const task = taskOf(s.task);
                    const failedAt = cleanup
                      ? "deleting"
                      : failedPhase(s, tasks.data, status.started_at);
                    // 새 VM은 복원이 끝난 뒤에만 이 실행의 것이다(이름이 겹쳐 실패했으면 남의 VM — 리뷰 1 웹 2).
                    const restored =
                      AFTER_RESTORE.has(s.phase) ||
                      (s.phase === "failed" && failedAt !== undefined && failedAt !== "restoring");
                    return (
                      <li
                        key={s.vm}
                        aria-label={s.vm}
                        className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2 text-sm"
                      >
                        <span className="min-w-28 font-medium">{s.vm}</span>
                        <PhaseDots phase={s.phase} dots={dots} last={failedAt} />
                        <PhaseChip phase={s.phase} />
                        {s.phase === "preparing" && s.prepare && (
                          <span className="text-xs text-muted-foreground">
                            {t(`dr.prepare.${s.prepare.stage}`)}
                            {s.prepare.source_backup &&
                              ` · ${t("dr.run.sourceBackup", { backup: s.prepare.source_backup })}`}
                          </span>
                        )}
                        {s.new_vm && !cleanup && restored && (
                          <Link
                            to={`/vms/${encodeURIComponent(s.new_vm)}`}
                            className="underline-offset-4 hover:underline"
                          >
                            {s.new_vm}
                          </Link>
                        )}
                        {s.backup && (
                          <span className="text-xs text-muted-foreground">
                            {t("dr.run.from", { backup: s.backup })}
                          </span>
                        )}
                        {s.task && (
                          <Link
                            to="/tasks"
                            className="inline-flex items-center gap-1 text-xs underline-offset-4 hover:underline"
                          >
                            {t("dr.run.task")}
                            {task && <TaskStateBadge task={task} />}
                          </Link>
                        )}
                        {s.error && (
                          <span className="basis-full text-xs text-destructive">{s.error}</span>
                        )}
                      </li>
                    );
                  })}
                </ul>
              )}
            </li>
          );
        })}
      </ol>
      <ConfirmDialog
        open={cancelling}
        onOpenChange={setCancelling}
        title={t("dr.cancel.title", { name: run.spec.plan_name })}
        description={cancelText(t, run.spec.kind)}
        confirmLabel={t("dr.action.cancel")}
        onConfirm={async () => {
          try {
            await call(
              api.POST("/dr-plans/{key}/actions/cancel", {
                params: { path: { key: run.spec.plan } },
              }),
            );
            toast.success(t("dr.cancel.done", { name: run.spec.plan_name }));
          } catch (err) {
            throw localized(t, err);
          } finally {
            await refreshDr(queryClient);
          }
        }}
      />
    </div>
  );
}
