import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import type { Task } from "@/api/types";
import { type Column, DataTable } from "@/components/data-table/DataTable";
import { PageHeader } from "@/components/PageHeader";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Pager, usePage } from "@/components/Pager";
import { Label } from "@/components/ui/label";
import { formatTime, uuidTime } from "@/lib/format";
import { describeDetail, describeProgress, describeTask } from "@/tasks/tasks";

/** 한 페이지에 보일 Task 수. API에 페이지 나누기가 없어 모두 받아 화면에서 나눈다. */
export const TASK_PAGE_SIZE = 50;

const STATE_VARIANT = {
  pending: "outline",
  running: "secondary",
  succeeded: "default",
  failed: "destructive",
} as const;

export function TaskStateBadge({ task }: { task: Task }) {
  const { t } = useTranslation();
  return (
    <Badge variant={STATE_VARIANT[task.status.state]}>
      {t(`tasks.state.${task.status.state}`)}
    </Badge>
  );
}

/** Task 대상의 이름. VM이면 VM 화면으로, 가져오기면 가져오기 화면으로 이어진다. */
export function useTaskTarget() {
  const vms = useList("vm");
  const nodes = useList("node");
  const volumes = useList("volume");
  const backups = useList("backup");
  const imports = useList("vm-import");
  return useMemo(() => {
    const vmById = new Map(vms.data?.map((v) => [v.id, v]));
    const volumeById = new Map(volumes.data?.map((v) => [v.id, v]));
    const backupById = new Map(backups.data?.map((b) => [b.id, b]));
    const importById = new Map(imports.data?.map((i) => [i.id, i]));
    const nodeById = new Map(nodes.data?.map((n) => [n.id, n]));
    return (task: Task) => {
      const vm = vmById.get(task.spec.target);
      if (vm) {
        return { name: vm.name, href: `/vms/${encodeURIComponent(vm.name)}` };
      }
      const op = task.spec.op;
      // 복원의 대상은 Task가 끝날 때 만들어지는 새 VM이다. 그 전에는 요청한 이름을 보인다.
      if (op.kind === "backup-restore") {
        return { name: op.name, href: undefined };
      }
      // 가져오기도 같다.
      if (op.kind === "vm-import") {
        return { name: op.run.name, href: undefined };
      }
      const imp = importById.get(task.spec.target);
      if (imp) {
        return { name: imp.name, href: `/imports/${encodeURIComponent(imp.name)}` };
      }
      // 노드 업데이트 확인·업그레이드의 대상은 노드다.
      const name =
        volumeById.get(task.spec.target)?.name ??
        backupById.get(task.spec.target)?.name ??
        nodeById.get(task.spec.target)?.name;
      // 지워진 대상은 id의 끝(UUIDv7의 무작위 부분)으로 구분한다. 앞부분은 시각이라 서로 비슷하다.
      return { name: name ?? task.spec.target.slice(-8), href: undefined };
    };
  }, [vms.data, volumes.data, backups.data, imports.data, nodes.data]);
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
}: {
  tasks: Task[];
  labelledBy?: string;
  empty?: string;
  /** 화면의 주 목록이면 목록 단축키를 받는다. */
  keys?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const target = useTaskTarget();
  const nodes = useList("node");
  const nodeById = useMemo(() => new Map(nodes.data?.map((n) => [n.id, n])), [nodes.data]);
  const columns: Column<Task>[] = [
    {
      id: "time",
      header: t("tasks.time"),
      className: "whitespace-nowrap",
      cell: (task) => {
        const time = uuidTime(task.id);
        return <time dateTime={time.toISOString()}>{formatTime(time, i18n.language)}</time>;
      },
    },
    {
      id: "operation",
      header: t("tasks.operation"),
      cell: (task) => {
        const { name, href } = target(task);
        const label = describeTask(task, t, name, (id) => nodeById.get(id)?.name);
        const detail = describeDetail(task, t);
        return (
          <>
            {href ? (
              <Link to={href} className="underline-offset-4 hover:underline">
                {label}
              </Link>
            ) : (
              label
            )}
            {detail && <span className="block text-xs text-muted-foreground">{detail}</span>}
          </>
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
    {
      id: "error",
      header: t("tasks.error"),
      priority: 2,
      cell: (task) => (
        <div className="max-w-md truncate" title={task.status.error ?? undefined}>
          {task.status.error ?? ""}
        </div>
      ),
    },
  ];
  return (
    <DataTable
      id="tasks"
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
        title={t("nav.tasks")}
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
      <TaskTable
        tasks={pager.pageItems}
        labelledBy="tasks-title"
        keys
        empty={failedOnly ? t("tasks.emptyFailed") : undefined}
      />
      <Pager {...pager} pageSize={TASK_PAGE_SIZE} total={shown.length} />
    </div>
  );
}
