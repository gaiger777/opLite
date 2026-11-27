import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Task } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { errorMessage } from "@/i18n/errors";
import { latestTasks, type Retry, retryOf } from "@/tasks/retry";
import { useVmActionHost } from "@/vms/actionHost";
import { useVmActions } from "@/vms/actions";

/**
 * 실패한 Task의 "다시 하기"(센터와 상세 서랍이 함께 쓴다 — 계획 77-2). `of(task)`는 다시 낼 요청(없으면 null),
 * `retry`는 시작만 바로 보내고 정지·다시 시작·강제 정지는 다른 곳처럼 확인 대화상자를 거친다. `busy`는 요청 중인 Task.
 */
export function useRetry() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const vms = useList("vm");
  const backups = useList("backup");
  const jobs = useList("backup-job");
  const all = useList("task");
  const index = useTaskIndex();
  const { run } = useVmActions();
  const host = useVmActionHost();
  // 요청 중인 다시 하기(두 번 눌러 두 번 보내지 않게)
  const [busy, setBusy] = useState<ReadonlySet<string>>(() => new Set());
  const returnFocus = useRef<HTMLElement | null>(null);
  const ctx = useMemo(
    () => ({
      perms,
      vm: new Map(vms.data?.map((v) => [v.id, v])),
      backup: new Map(backups.data?.map((b) => [b.id, b])),
      job: new Map(jobs.data?.map((j) => [j.id, j])),
      tasks: index,
      latest: latestTasks(all.data ?? []),
    }),
    [perms, vms.data, backups.data, jobs.data, index, all.data],
  );

  async function retry(task: Task, r: Retry, button: HTMLElement) {
    if (r.kind === "vm" && r.action !== "start") {
      returnFocus.current = button;
      host.open({ vm: r.vm, which: r.action, returnFocus });
      return;
    }
    setBusy((prev) => new Set(prev).add(task.id));
    try {
      if (r.kind === "vm") {
        await run(r.vm, r.action);
        return;
      }
      await call(api.POST("/backup-jobs/{key}/run", { params: { path: { key: r.job.id } } }));
      toast.success(t("backupJobs.runRequested", { name: r.job.name }));
    } catch (err) {
      toast.error(errorMessage(t, err));
    } finally {
      setBusy((prev) => {
        const next = new Set(prev);
        next.delete(task.id);
        return next;
      });
    }
  }

  return { of: (task: Task) => retryOf(task, ctx), retry, busy };
}
