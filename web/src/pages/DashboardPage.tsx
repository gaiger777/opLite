import { useQuery } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { lazy, Suspense, useId } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useAlerts } from "@/alerts/alerts";
import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePermissions } from "@/auth/usePermissions";
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
import { isUpgrading, useCapacity } from "@/physical/api";
import { SubscriptionBanner } from "@/settings/SubscriptionBanner";
import { isRinging } from "@/tasks/center";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";

// 운영자 대시보드(계획 75 §4.1 — 지도·자세히 판·할당 표)는 늦게 읽는 칸이다(첫 화면 묶음 < 3KB). 문구도 함께 받는다.
const OperatorBoard = lazy(async () => {
  const [board] = await Promise.all([
    import("@/dashboard/OperatorBoard"),
    import("@/physical/strings").then((s) => s.loadPhysicalStrings()),
  ]);
  return board;
});

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
function HealthStrip({ clusterRead }: { clusterRead: boolean }) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const vms = useList("vm");
  const alerts = useAlerts();
  const cluster = useQuery({
    queryKey: ["cluster"],
    queryFn: () => call(api.GET("/cluster")),
    enabled: clusterRead,
  });
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
        {/* 노드와 리더는 클러스터 읽기(루트 `cluster.read`)가 있어야 보인다(폴더 사용자는 VM과 경보만). */}
        {clusterRead && (
          <Stat
            title={t("dashboard.nodes")}
            value={
              nodes.data
                ? `${allNodes.filter((n) => n.status.online).length} / ${allNodes.length}`
                : waiting(t, nodes)
            }
            detail={nodes.data && t("dashboard.online")}
          />
        )}
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
        {clusterRead && (
          <Stat title={t("dashboard.leader")} value={leader} detail={cluster.data?.name} />
        )}
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
      <TaskTable tasks={newestFirst(tasks.data ?? []).slice(0, 5)} labelledBy={id} compact />
    </WidgetCard>
  );
}

/**
 * 홈(설계 §8.1, 계획 75 §4.1): 첫 걸음, 건강 띠, 운영자 대시보드([지도 | 자세히 판], 노드별 할당·사용 표), 경보, 차트, 상위 VM,
 * 최근 작업, 즐겨찾기. 위젯은 카드 하나씩만 그리고 배치는 여기서 한다. 휴대폰(당직 보기, §5.9)은 건강 띠 → 경보 → 지도 →
 * 표(카드)를 먼저 쌓는다(화면 읽기 순서도 같게 DOM 순서를 바꾼다). 섞인 판 클러스터(`/capacity`가 409)는 지난 홈(용량·풀·노드
 * 부하 위젯)과 안내 한 줄이다.
 */
export function DashboardPage() {
  const { t } = useTranslation();
  const phone = useIsPhone();
  // 용량·풀·차트·노드 부하·상위 VM·대시보드는 클러스터 지표(`/metrics/cluster`)와 노드 목록을 읽어 루트 `cluster.read`가
  // 있어야 한다. 없으면(폴더 사용자) 숨기고 한 줄로 알린다.
  const clusterRead = usePermissions().has("cluster.read");
  const capacity = useCapacity(clusterRead);
  const alerts = useAlerts();
  const legacy = isUpgrading(capacity.error);
  const grid2 = "grid grid-cols-1 gap-(--gap) lg:grid-cols-2";
  const hidden = !clusterRead && (
    <p className="text-sm text-muted-foreground">{t("dashboard.clusterHidden")}</p>
  );
  const upgrading = legacy && (
    <p className="text-sm text-muted-foreground">{t("dashboard.boardAfterUpgrade")}</p>
  );
  // 울리는 경보가 있으면 경보 위젯을 접지 않고 지도 위에 둔다(홈 한 화면의 "주의할 것", 77-6).
  const ringing = (alerts.data?.alerts.filter(isRinging).length ?? 0) > 0;
  const more = {
    names: [
      ...(ringing ? [] : [t("alerts.active")]),
      t("metrics.topVms"),
      t("metrics.clusterLoad"),
      t("dashboard.recentTasks"),
      t("favorites.title"),
    ],
    content: (
      <>
        <div className={grid2}>
          {!ringing && <ActiveAlertsWidget />}
          <TopVmsWidget />
        </div>
        <ClusterChartsWidget />
        <div className={grid2}>
          <RecentTasksWidget />
          <FavoritesWidget />
        </div>
      </>
    ),
  };
  const board = (
    <Suspense fallback={<p className="text-sm text-muted-foreground">{t("common.loading")}</p>}>
      <OperatorBoard phone={phone} more={phone ? undefined : more} />
    </Suspense>
  );
  const operator = clusterRead && !legacy;
  // 어느 홈인지 알 때까지(첫 `/capacity`) 클러스터 칸을 그리지 않는다 — 두 홈 사이를 오가며 위젯을 다시 올리지 않게.
  const pending = clusterRead && capacity.isPending;
  const loading = <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
  // 휴대폰은 홈마다 순서가 달라 열쇠로 같은 위젯을 지킨다.
  const phoneItems = pending
    ? [<HealthStrip key="health" clusterRead={clusterRead} />, <div key="loading">{loading}</div>]
    : operator
      ? [
          <HealthStrip key="health" clusterRead={clusterRead} />,
          <ActiveAlertsWidget key="alerts" />,
          <div key="board" className="contents">
            {board}
          </div>,
          <FavoritesWidget key="favorites" />,
          <RecentTasksWidget key="tasks" />,
        ]
      : [
          <ActiveAlertsWidget key="alerts" />,
          <HealthStrip key="health" clusterRead={clusterRead} />,
          <div key="upgrading" className="contents">
            {upgrading}
          </div>,
          <FavoritesWidget key="favorites" />,
          ...(clusterRead
            ? [<CapacityWidget key="capacity" />, <PoolUsageWidget key="pools" />]
            : []),
          <RecentTasksWidget key="tasks" />,
          <div key="hidden" className="contents">
            {hidden}
          </div>,
        ];
  return (
    <div className="flex flex-col gap-(--gap)">
      <PageHeader title={t("nav.area.home")} />
      <SubscriptionBanner />
      <FirstSteps />
      {phone ? (
        phoneItems
      ) : (
        <>
          <HealthStrip clusterRead={clusterRead} />
          {pending ? (
            loading
          ) : operator ? (
            <>
              {ringing && <ActiveAlertsWidget />}
              {board}
            </>
          ) : clusterRead ? (
            <>
              {upgrading}
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
            </>
          ) : (
            <ActiveAlertsWidget />
          )}
          {!pending && !operator && (
            <div className={grid2}>
              <RecentTasksWidget />
              <FavoritesWidget />
            </div>
          )}
          {hidden}
        </>
      )}
    </div>
  );
}
