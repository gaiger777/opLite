import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import type { Task } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Pager, usePage } from "@/components/Pager";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatTime, uuidTime } from "@/lib/format";
import { describeTask } from "@/tasks/tasks";

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

/** Task 대상의 이름. VM이면 VM 화면으로 이어진다. */
export function useTaskTarget() {
  const vms = useList("vm");
  const volumes = useList("volume");
  return useMemo(() => {
    const vmById = new Map(vms.data?.map((v) => [v.id, v]));
    const volumeById = new Map(volumes.data?.map((v) => [v.id, v]));
    return (task: Task) => {
      const vm = vmById.get(task.spec.target);
      if (vm) {
        return { name: vm.name, href: `/vms/${encodeURIComponent(vm.name)}` };
      }
      const volume = volumeById.get(task.spec.target);
      // 지워진 대상은 id의 끝(UUIDv7의 무작위 부분)으로 구분한다. 앞부분은 시각이라 서로 비슷하다.
      return { name: volume?.name ?? task.spec.target.slice(-8), href: undefined };
    };
  }, [vms.data, volumes.data]);
}

/**
 * Task 표. VM 화면의 작업 탭도 쓴다. `labelledBy`는 표의 이름이 될 제목의 id, `empty`는 보일 Task가
 * 없을 때의 문장이다.
 */
export function TaskTable({
  tasks,
  labelledBy,
  empty,
}: {
  tasks: Task[];
  labelledBy?: string;
  empty?: string;
}) {
  const { t, i18n } = useTranslation();
  const target = useTaskTarget();
  const nodes = useList("node");
  const nodeById = useMemo(() => new Map(nodes.data?.map((n) => [n.id, n])), [nodes.data]);
  return (
    <Table aria-labelledby={labelledBy}>
      <TableHeader>
        <TableRow>
          <TableHead>{t("tasks.time")}</TableHead>
          <TableHead>{t("tasks.operation")}</TableHead>
          <TableHead>{t("common.node")}</TableHead>
          <TableHead>{t("common.state")}</TableHead>
          <TableHead>{t("tasks.error")}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {tasks.length === 0 && (
          <TableRow>
            <TableCell colSpan={5} className="text-center text-muted-foreground">
              {empty ?? t("tasks.empty")}
            </TableCell>
          </TableRow>
        )}
        {tasks.map((task) => {
          const { name, href } = target(task);
          const node = nodeById.get(task.spec.node);
          const time = uuidTime(task.id);
          return (
            <TableRow key={task.id}>
              <TableCell className="whitespace-nowrap">
                <time dateTime={time.toISOString()}>{formatTime(time, i18n.language)}</time>
              </TableCell>
              <TableCell>
                {href ? (
                  <Link to={href} className="underline-offset-4 hover:underline">
                    {describeTask(task, t, name)}
                  </Link>
                ) : (
                  describeTask(task, t, name)
                )}
              </TableCell>
              <TableCell>{node?.name ?? "—"}</TableCell>
              <TableCell>
                <TaskStateBadge task={task} />
              </TableCell>
              <TableCell>
                <div className="max-w-md truncate" title={task.status.error ?? undefined}>
                  {task.status.error ?? ""}
                </div>
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
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
      <div className="flex items-center justify-between gap-4">
        <h1 id="tasks-title" className="text-2xl font-semibold">
          {t("nav.tasks")}
        </h1>
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
      </div>
      <TaskTable
        tasks={pager.pageItems}
        labelledBy="tasks-title"
        empty={failedOnly ? t("tasks.emptyFailed") : undefined}
      />
      <Pager {...pager} pageSize={TASK_PAGE_SIZE} total={shown.length} />
    </div>
  );
}
