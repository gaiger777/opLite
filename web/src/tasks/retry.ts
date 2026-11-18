// 실패한 Task의 "다시 하기"(설계 §5.4). Task에는 동작 파라미터가 없으므로 파라미터가 필요 없는 전원 동작과
// "백업 작업 지금 실행"만 같은 API로 다시 보낸다.
import type { Backup, BackupJob, Task, Vm } from "@/api/types";
import { objectTarget, type Permissions } from "@/auth/permissions";
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
  /** 내 권한. VM마다(폴더·객체 grant) 따로 본다. */
  perms: Pick<Permissions, "has">;
  vm: Map<string, Vm>;
  backup: Map<string, Backup>;
  job: Map<string, BackupJob>;
  /** 대상 id → 끝나지 않은 Task(`useTaskIndex`) */
  tasks: Map<string, Task[]>;
  /** `retryGroup`별 가장 새 Task id(`latestTasks`). 실패 뒤에 같은 대상에 새 Task가 있으면 다시 하지 않는다. */
  latest?: Map<string, string>;
}

/** 다시 하기를 견주는 묶음: 같은 VM의 전원 동작끼리, 같은 VM의 백업끼리 */
function retryGroup(task: Task): string | null {
  const kind = task.spec.op.kind;
  const group = POWER[kind] ? "power" : kind === "vm-backup" ? "backup" : null;
  return group && `${group}:${task.spec.target}`;
}

/** 묶음마다 가장 새 Task의 id(id는 시각 순 UUIDv7) */
export function latestTasks(tasks: readonly Task[]): Map<string, string> {
  const out = new Map<string, string>();
  for (const task of tasks) {
    const group = retryGroup(task);
    const seen = group && out.get(group);
    if (group && (!seen || task.id > seen)) {
      out.set(group, task.id);
    }
  }
  return out;
}

/**
 * 실패한 Task를 다시 할 수 있으면 그 요청을, 아니면 null을 준다. 권한이 없거나 대상(VM·백업 작업)이 사라졌거나
 * 뒤에 같은 묶음의 새 Task가 있으면 null,
 * 지금 상태로 막히면 `reason`을 단다(VM은 `availability()`, 작업은 도는 중이거나 꺼짐).
 */
export function retryOf(task: Task, ctx: RetryContext): Retry | null {
  if (task.status.state !== "failed") {
    return null;
  }
  // 그 뒤에 같은 대상에 다른 시도(성공한 시작, 일부러 한 정지 등)가 있으면 옛 실패를 다시 하지 않는다.
  const group = retryGroup(task);
  const newest = group && ctx.latest?.get(group);
  if (newest && newest !== task.id) {
    return null;
  }
  const op = task.spec.op;
  const action = POWER[op.kind];
  if (action) {
    const vm = ctx.vm.get(task.spec.target);
    if (!vm || !ctx.perms.has("vm.power", objectTarget("vm", vm))) {
      return null;
    }
    const ok = availability(vm, action, ctx.tasks.get(vm.id) ?? []);
    return { kind: "vm", vm, action, reason: ok.ok ? undefined : ok.reason };
  }
  if (op.kind === "vm-backup") {
    const jobId = ctx.backup.get(op.backup)?.spec.job;
    const job = jobId ? ctx.job.get(jobId) : undefined;
    if (!job || !ctx.perms.has("backup.job")) {
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
