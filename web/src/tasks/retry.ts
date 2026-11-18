// 실패한 Task의 "다시 하기"(설계 §5.4). Task에는 동작 파라미터가 없으므로 파라미터가 필요 없는 전원 동작과
// "백업 작업 지금 실행"만 같은 API로 다시 보낸다.
import type { Backup, BackupJob, Role, Task, Vm } from "@/api/types";
import { can } from "@/auth/permissions";
import { availability, type VmAction } from "@/vms/actions";

/** 다시 보낼 요청. `reason`이 있으면 지금은 할 수 없다(번역 키). */
export type Retry =
  | { kind: "vm"; vm: Vm; action: VmAction; reason?: string }
  | { kind: "job"; job: BackupJob; reason?: string };

const POWER: Partial<Record<Task["spec"]["op"]["kind"], VmAction>> = {
  "vm-start": "start",
  "vm-stop": "stop",
  "vm-force-stop": "force-stop",
  "vm-reboot": "reboot",
};

export interface RetryContext {
  role: Role;
  vm: Map<string, Vm>;
  backup: Map<string, Backup>;
  job: Map<string, BackupJob>;
  /** 대상 id → 끝나지 않은 Task(`useTaskIndex`) */
  tasks: Map<string, Task[]>;
}

/**
 * 실패한 Task를 다시 할 수 있으면 그 요청을, 아니면 null을 준다. 권한이 없거나 대상(VM·백업 작업)이 사라졌으면 null,
 * 지금 상태로 막히면 `reason`을 단다(VM은 `availability()`, 작업은 도는 중이거나 꺼짐).
 */
export function retryOf(task: Task, ctx: RetryContext): Retry | null {
  if (task.status.state !== "failed") {
    return null;
  }
  const op = task.spec.op;
  const action = POWER[op.kind];
  if (action) {
    const vm = ctx.vm.get(task.spec.target);
    if (!vm || !can(ctx.role, "operate", "vm")) {
      return null;
    }
    const ok = availability(vm, action, ctx.tasks.get(vm.id) ?? []);
    return { kind: "vm", vm, action, reason: ok.ok ? undefined : ok.reason };
  }
  if (op.kind === "vm-backup") {
    const jobId = ctx.backup.get(op.backup)?.spec.job;
    const job = jobId ? ctx.job.get(jobId) : undefined;
    if (!job || !can(ctx.role, "write", "backup-job")) {
      return null;
    }
    // 서버는 도는 중이거나 꺼 둔 작업을 거절한다(백업 작업 화면의 "지금 실행"과 같음).
    const reason =
      job.status.run != null
        ? "center.jobRunning"
        : !job.spec.enabled
          ? "center.jobDisabled"
          : undefined;
    return { kind: "job", job, reason };
  }
  return null;
}
