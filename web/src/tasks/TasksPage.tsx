import { hashKey, useQueryClient } from "@tanstack/react-query";
import { ListChecks } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { listKey, useList } from "@/api/live";
import type { Task } from "@/api/types";
import { type Column, DataTable } from "@/components/data-table/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { PageHeader } from "@/components/PageHeader";
import { StatusChip, type StatusTone } from "@/components/StatusChip";
import { Checkbox } from "@/components/ui/checkbox";
import { Pager, usePage } from "@/components/Pager";
import { Label } from "@/components/ui/label";
import { formatTime, uuidTime } from "@/lib/format";
import { failureTitle } from "@/tasks/failures/failure";
import { useFailureStrings } from "@/tasks/failures/strings";
import { TaskErrorText } from "@/tasks/TaskErrorText";
import { requesterName, useTaskHref } from "@/tasks/taskLink";
import { type Named, type TargetLookup, taskTarget } from "@/tasks/target";
import { describeDetail, describeProgress, describeTask } from "@/tasks/tasks";

/** 한 페이지에 보일 Task 수. API에 페이지 나누기가 없어 모두 받아 화면에서 나눈다. */
export const TASK_PAGE_SIZE = 50;

/** Task 상태의 색. 도는 Task는 점이 깜박인다. */
export const TASK_TONE: Record<Task["status"]["state"], StatusTone> = {
  pending: "neutral",
  running: "info",
  succeeded: "success",
  failed: "danger",
};

export function TaskStateBadge({ task }: { task: Task }) {
  const { t } = useTranslation();
  const state = task.status.state;
  return (
    <StatusChip tone={TASK_TONE[state]} busy={state === "running"}>
      {t(`tasks.state.${state}`)}
    </StatusChip>
  );
}

/**
 * 대상 이름을 찾을 그 밖의 목록(계획 77): 이미 받은 것만 쓴다(이 때문에 새로 읽지 않는다 — 없으면 서버가 단 이름 레이블).
 * 감시 목록(`listKey`)과 화면이 읽는 보기 목록 모두 `{ items }`다.
 */
const CACHED_LISTS: { key: readonly unknown[]; kind: string }[] = [
  { key: listKey("network"), kind: "network" },
  { key: listKey("storage-pool"), kind: "storage-pool" },
  { key: listKey("backup-job"), kind: "backup-job" },
  { key: listKey("ha-group"), kind: "ha-group" },
  { key: ["backup-stores"], kind: "backup-store" },
  { key: ["apply-sets"], kind: "apply-set" },
  { key: ["vm-exports"], kind: "vm-export" },
  { key: ["certificates"], kind: "certificate" },
  { key: ["vmware-migrations"], kind: "vmware-migration" },
  { key: ["dr-plans"], kind: "recovery-plan" },
  { key: ["remotes"], kind: "remote" },
  // 옛 Task(요청자 이름 레이블 없음)의 요청자
  { key: ["users"], kind: "user" },
];
const CACHED_HASHES = new Set(CACHED_LISTS.map((c) => hashKey(c.key)));

/** 이미 받은 그 밖의 목록에서 id → 이름·종류. 그 목록이 바뀌면 다시 만든다. */
function useCachedNames(): Map<string, Named> {
  const queryClient = useQueryClient();
  const [tick, setTick] = useState(0);
  useEffect(
    () =>
      queryClient.getQueryCache().subscribe((event) => {
        if (event.type === "updated" && CACHED_HASHES.has(event.query.queryHash)) {
          setTick((n) => n + 1);
        }
      }),
    [queryClient],
  );
  return useMemo(() => {
    void tick;
    const names = new Map<string, Named>();
    for (const { key, kind } of CACHED_LISTS) {
      const items = queryClient.getQueryData<{ items?: unknown }>(key)?.items;
      if (!Array.isArray(items)) {
        continue;
      }
      for (const item of items as { id?: unknown; name?: unknown }[]) {
        if (typeof item?.id === "string" && typeof item.name === "string") {
          names.set(item.id, { name: item.name, kind });
        }
      }
    }
    return names;
  }, [queryClient, tick]);
}

/** Task 대상을 찾을 목록(`taskTarget`·`relatedLinks`). 이미 받은 목록으로 찾는다. */
export function useTargetLookup(): TargetLookup {
  const vms = useList("vm");
  const nodes = useList("node");
  const volumes = useList("volume");
  const backups = useList("backup");
  const imports = useList("vm-import");
  const named = useCachedNames();
  return useMemo(
    () => ({
      vm: new Map(vms.data?.map((v) => [v.id, v])),
      volume: new Map(volumes.data?.map((v) => [v.id, v])),
      backup: new Map(backups.data?.map((b) => [b.id, b])),
      import: new Map(imports.data?.map((i) => [i.id, i])),
      node: new Map(nodes.data?.map((n) => [n.id, n])),
      named,
    }),
    [vms.data, volumes.data, backups.data, imports.data, nodes.data, named],
  );
}

/** Task 대상의 이름과 화면(`taskTarget`). 이미 받은 목록으로 찾는다. */
export function useTaskTarget() {
  const lookup = useTargetLookup();
  return useMemo(() => (task: Task) => taskTarget(task, lookup), [lookup]);
}

/**
 * Task 표. VM 화면의 작업 탭도 쓴다. `labelledBy`는 표의 이름이 될 제목의 id, `empty`는 보일 Task가
 * 없을 때의 문장이다.
 */
export function TaskTable({
  tasks,
  labelledBy,
  empty,
  keys = false,
  compact = false,
}: {
  tasks: Task[];
  labelledBy?: string;
  empty?: string;
  /** 화면의 주 목록이면 목록 단축키를 받는다. */
  keys?: boolean;
  /** 좁은 카드(홈 위젯): 작업(아래에 시각)과 상태 두 열만 — 노드·요청자는 상세 서랍에(계획 77) */
  compact?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const lookup = useTargetLookup();
  const target = (task: Task) => taskTarget(task, lookup);
  const taskHref = useTaskHref();
  const nodes = useList("node");
  const nodeById = useMemo(() => new Map(nodes.data?.map((n) => [n.id, n])), [nodes.data]);
  // 실패 원인 사전(계획 77-2): 원인 코드가 있는 실패가 보일 때만 받는다.
  useFailureStrings(tasks.some((task) => task.status.failure));
  const timeOf = (task: Task) => {
    const time = uuidTime(task.id);
    return <time dateTime={time.toISOString()}>{formatTime(time, i18n.language)}</time>;
  };
  const all: Column<Task>[] = [
    {
      id: "time",
      header: t("tasks.time"),
      className: "whitespace-nowrap",
      cell: timeOf,
    },
    {
      // 작업 이름(누르면 상세 서랍), 기록 설명, 오류(2줄 + 펼침 — 좁은 폭·휴대폰 카드에도 보인다, 계획 77)
      id: "operation",
      header: t("tasks.operation"),
      cell: (task) => {
        const { name } = target(task);
        const label = describeTask(task, t, name, (id) => nodeById.get(id)?.name);
        const detail = describeDetail(task, t);
        const error = task.status.error;
        return (
          <div className="min-w-0 max-w-2xl">
            <Link
              to={taskHref(task.id)}
              className="font-medium break-words underline-offset-4 hover:underline"
            >
              {label}
            </Link>
            {compact && <span className="block text-xs text-muted-foreground">{timeOf(task)}</span>}
            {detail && <span className="block text-xs text-muted-foreground">{detail}</span>}
            {error && (
              <TaskErrorText
                text={error}
                summary={failureTitle(task, t)}
                warning={task.status.state === "succeeded"}
                subject={label}
                className="mt-0.5"
              />
            )}
          </div>
        );
      },
    },
    {
      id: "node",
      header: t("common.node"),
      priority: 2,
      cell: (task) => nodeById.get(task.spec.node)?.name ?? "—",
    },
    {
      id: "requester",
      header: t("tasks.requester"),
      priority: 3,
      className: "whitespace-nowrap",
      cell: (task) => requesterName(task, t, lookup.named),
    },
    {
      id: "state",
      header: t("common.state"),
      cell: (task) => {
        const progress = describeProgress(task, t);
        return (
          <>
            <TaskStateBadge task={task} />
            {progress && (
              <span className="ml-2 text-xs whitespace-nowrap text-muted-foreground">
                {progress}
              </span>
            )}
          </>
        );
      },
    },
  ];
  const columns = compact ? all.filter((c) => c.id === "operation" || c.id === "state") : all;
  return (
    <DataTable
      id={compact ? "tasks-compact" : "tasks"}
      rows={tasks}
      columns={columns}
      rowId={(task) => task.id}
      empty={empty ?? t("tasks.empty")}
      labelledBy={labelledBy}
      keys={keys}
    />
  );
}

/** 최근 순으로 정렬한다(id가 UUIDv7이라 문자열 순서가 시간 순서다). */
export function newestFirst(tasks: Task[]): Task[] {
  return [...tasks].sort((a, b) => (a.id < b.id ? 1 : -1));
}

export function TasksPage() {
  const { t } = useTranslation();
  const tasks = useList("task");
  const [failedOnly, setFailedOnly] = useState(false);
  const shown = useMemo(
    () =>
      newestFirst(tasks.data ?? []).filter((task) => !failedOnly || task.status.state === "failed"),
    [tasks.data, failedOnly],
  );
  const pager = usePage(shown, TASK_PAGE_SIZE);
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="tasks-title"
        title={t("nav.item.tasks")}
        help="tasks"
        intro={t("help.tasks.intro")}
        actions={
          <div className="flex items-center gap-2">
            <Checkbox
              id="failed-only"
              checked={failedOnly}
              onCheckedChange={(v) => {
                setFailedOnly(v === true);
                pager.setPage(0);
              }}
            />
            <Label htmlFor="failed-only">{t("tasks.failedOnly")}</Label>
          </div>
        }
      />
      {tasks.isSuccess && tasks.data.length === 0 ? (
        <EmptyState
          icon={ListChecks}
          title={t("tasks.empty")}
          description={t("help.tasks.empty")}
          help="tasks.page"
        />
      ) : (
        <TaskTable
          tasks={pager.pageItems}
          labelledBy="tasks-title"
          keys
          empty={failedOnly ? t("tasks.emptyFailed") : undefined}
        />
      )}
      <Pager {...pager} pageSize={TASK_PAGE_SIZE} total={shown.length} />
    </div>
  );
}
