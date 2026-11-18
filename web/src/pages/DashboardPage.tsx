import { useQuery } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useAlerts } from "@/alerts/alerts";
import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Node, Vm } from "@/api/types";
import { useIsPhone } from "@/components/data-table/DataTable";
import { PageHeader } from "@/components/PageHeader";
import { ActiveAlertsWidget } from "@/dashboard/ActiveAlertsWidget";
import { CapacityWidget } from "@/dashboard/CapacityWidget";
import { ClusterChartsWidget } from "@/dashboard/ClusterChartsWidget";
import { FavoritesWidget } from "@/dashboard/FavoritesWidget";
import { FirstSteps } from "@/dashboard/FirstSteps";
import { NodeStripWidget } from "@/dashboard/NodeStripWidget";
import { PoolUsageWidget } from "@/dashboard/PoolUsageWidget";
import { TopVmsWidget } from "@/dashboard/TopVmsWidget";
import { WidgetCard } from "@/dashboard/WidgetCard";
import { errorMessage } from "@/i18n/errors";
import { compareVersions } from "@/lib/version";
import { SubscriptionBanner } from "@/settings/SubscriptionBanner";
import { isRinging } from "@/tasks/center";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";

/** 노드들이 돌리는 서로 다른 버전(낮은 것부터). 버전을 모르는 노드(옛 노드)는 뺀다. */
function nodeVersions(nodes: Node[]): string[] {
  const versions = new Set(nodes.flatMap((n) => (n.status.version ? [n.status.version] : [])));
  return [...versions].sort(compareVersions);
}

/** 아직 받지 못한 데이터: 실패했으면 그 이유, 아니면 불러오는 중 */
function waiting(t: TFunction, ...queries: { isError: boolean; error: unknown }[]) {
  const failed = queries.find((q) => q.isError);
  return failed ? errorMessage(t, failed.error) : t("common.loading");
}

/** 건강 띠의 칸 하나: 작은 이름, 큰 값, 작은 설명 */
function Stat({
  title,
  value,
  detail,
}: {
  title: string;
  value: React.ReactNode;
  detail?: React.ReactNode;
}) {
  const id = useId();
  return (
    <div
      role="group"
      aria-labelledby={id}
      className="flex min-w-0 flex-col gap-1 rounded-lg border bg-surface-2 px-4 py-3"
    >
      <span id={id} className="text-xs text-muted-foreground">
        {title}
      </span>
      <div className="truncate text-xl font-semibold tabular-nums">{value}</div>
      {detail && <div className="text-xs text-muted-foreground">{detail}</div>}
    </div>
  );
}

/** 건강 띠(설계 §8.1): 노드 온라인, VM(실패), 울리는 경보, 리더. 혼합 버전 경고는 그 아래 줄이다. */
function HealthStrip() {
  const { t } = useTranslation();
  const nodes = useList("node");
  const vms = useList("vm");
  const alerts = useAlerts();
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const allNodes = nodes.data ?? [];
  const allVms = vms.data ?? [];
  const count = (phase: Vm["status"]["phase"]) =>
    allVms.filter((vm) => vm.status.phase === phase).length;
  const failed = count("failed");
  const versions = nodeVersions(allNodes);
  const ringing = alerts.data?.alerts.filter(isRinging).length;
  // 리더 이름: Node 리소스, 클러스터 멤버, 그래도 없으면(합류 중) id 끝 8자
  let leader = waiting(t, cluster);
  if (cluster.data) {
    const id = cluster.data.leader;
    leader = id
      ? (allNodes.find((n) => n.id === id)?.name ??
        cluster.data.members.find((m) => m.id === id)?.name ??
        id.slice(-8))
      : t("dashboard.noLeader");
  }
  return (
    <section aria-label={t("dashboard.health")} className="flex flex-col gap-(--gap)">
      <div className="grid grid-cols-2 gap-(--gap) lg:grid-cols-4">
        <Stat
          title={t("dashboard.nodes")}
          value={
            nodes.data
              ? `${allNodes.filter((n) => n.status.online).length} / ${allNodes.length}`
              : waiting(t, nodes)
          }
          detail={nodes.data && t("dashboard.online")}
        />
        <Stat
          title={t("dashboard.vms")}
          value={vms.data ? String(allVms.length) : waiting(t, vms)}
          detail={
            vms.data && (
              <>
                {t("dashboard.vmCounts", {
                  // 옮기는 중인 VM은 원본에서 계속 돈다.
                  running: count("running") + count("migrating"),
                  stopped: count("stopped"),
                })}
                {" · "}
                {failed > 0 ? (
                  <Link
                    to="/vms?phase=failed"
                    className="font-medium text-danger-fg underline underline-offset-4"
                  >
                    {t("dashboard.failedCount", { count: failed })}
                  </Link>
                ) : (
                  t("dashboard.failedCount", { count: 0 })
                )}
              </>
            )
          }
        />
        <Stat
          title={t("dashboard.alerts")}
          value={ringing ?? (alerts.isError ? "—" : t("common.loading"))}
          detail={
            <Link to="/alerts" className="underline-offset-4 hover:underline">
              {t("dashboard.ringing")}
            </Link>
          }
        />
        <Stat title={t("dashboard.leader")} value={leader} detail={cluster.data?.name} />
      </div>
      {/* 롤링 업그레이드 중이거나 멈춘 업그레이드가 있다. */}
      {versions.length > 1 && (
        <p
          role="alert"
          className="rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm"
        >
          {t("dashboard.mixedVersions", { versions: versions.join(", ") })}
        </p>
      )}
    </section>
  );
}

/** 최근 Task 5개(작업 표와 같은 칩·깊은 링크) */
function RecentTasksWidget() {
  const { t } = useTranslation();
  const tasks = useList("task");
  const id = useId();
  return (
    <WidgetCard
      title={t("dashboard.recentTasks")}
      titleId={id}
      action={
        <Link to="/tasks" className="text-sm underline-offset-4 hover:underline">
          {t("dashboard.allTasks")}
        </Link>
      }
    >
      <TaskTable tasks={newestFirst(tasks.data ?? []).slice(0, 5)} labelledBy={id} />
    </WidgetCard>
  );
}

/**
 * 홈(설계 §8.1): 첫 걸음, 건강 띠, 경보, 용량(클러스터·풀), 차트, 노드 부하·상위 VM, 최근 작업, 즐겨찾기.
 * 위젯은 카드 하나씩만 그리고 배치는 여기서 한다. 휴대폰(당직 보기, §5.9)은 경보 → 상태 요약 → 즐겨찾기를
 * 먼저 쌓는다(화면 읽기 순서도 같게 DOM 순서를 바꾼다).
 */
export function DashboardPage() {
  const { t } = useTranslation();
  const phone = useIsPhone();
  const grid2 = "grid grid-cols-1 gap-(--gap) lg:grid-cols-2";
  return (
    <div className="flex flex-col gap-(--gap)">
      <PageHeader title={t("nav.area.home")} />
      <SubscriptionBanner />
      <FirstSteps />
      {phone ? (
        <>
          <ActiveAlertsWidget />
          <HealthStrip />
          <FavoritesWidget />
          <CapacityWidget />
          <PoolUsageWidget />
          <RecentTasksWidget />
        </>
      ) : (
        <>
          <HealthStrip />
          <div className="grid grid-cols-1 gap-(--gap) lg:grid-cols-3">
            <ActiveAlertsWidget />
            <CapacityWidget />
            <PoolUsageWidget />
          </div>
          <ClusterChartsWidget />
          <div className={grid2}>
            <NodeStripWidget />
            <TopVmsWidget />
          </div>
          <div className={grid2}>
            <RecentTasksWidget />
            <FavoritesWidget />
          </div>
        </>
      )}
    </div>
  );
}
