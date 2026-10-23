import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import type { Task } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
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

/** API에 페이지 나누기가 없어 모두 받아 최근 것만 보여 준다. */
const SHOWN = 200;

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
  return (task: Task) => {
    const vm = vms.data?.find((v) => v.id === task.spec.target);
    if (vm) {
      return { name: vm.name, href: `/vms/${encodeURIComponent(vm.name)}` };
    }
    const volume = volumes.data?.find((v) => v.id === task.spec.target);
    return { name: volume?.name ?? task.spec.target.slice(0, 8), href: undefined };
  };
}

/** Task 표. VM 화면의 작업 탭도 쓴다. */
export function TaskTable({ tasks }: { tasks: Task[] }) {
  const { t, i18n } = useTranslation();
  const target = useTaskTarget();
  const nodes = useList("node");
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>{t("tasks.time")}</TableHead>
          <TableHead>{t("tasks.operation")}</TableHead>
          <TableHead>{t("tasks.node")}</TableHead>
          <TableHead>{t("tasks.stateColumn")}</TableHead>
          <TableHead>{t("tasks.error")}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {tasks.length === 0 && (
          <TableRow>
            <TableCell colSpan={5} className="text-center text-muted-foreground">
              {t("tasks.empty")}
            </TableCell>
          </TableRow>
        )}
        {tasks.map((task) => {
          const { name, href } = target(task);
          const node = nodes.data?.find((n) => n.id === task.spec.node);
          return (
            <TableRow key={task.id}>
              <TableCell className="whitespace-nowrap">
                {formatTime(uuidTime(task.id), i18n.language)}
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
              <TableCell className="max-w-md truncate" title={task.status.error ?? undefined}>
                {task.status.error ?? ""}
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
      newestFirst(tasks.data ?? [])
        .filter((task) => !failedOnly || task.status.state === "failed")
        .slice(0, SHOWN),
    [tasks.data, failedOnly],
  );
  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-4">
        <h1 className="text-2xl font-semibold">{t("nav.tasks")}</h1>
        <div className="flex items-center gap-2">
          <Checkbox
            id="failed-only"
            checked={failedOnly}
            onCheckedChange={(v) => setFailedOnly(v === true)}
          />
          <Label htmlFor="failed-only">{t("tasks.failedOnly")}</Label>
        </div>
      </div>
      <TaskTable tasks={shown} />
    </div>
  );
}
