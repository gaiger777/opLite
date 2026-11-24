// VM 일괄 동작(설계 §5.3): VM마다 할 수 있는지와 그 이유, 스냅샷 이름, 동시 요청 상한.
import type { Node, Snapshot, Task, Vm } from "@/api/types";
import type { Privilege } from "@/auth/permissions";
import { availability, isBusy, isMigrating } from "@/vms/actions";
import { migrationTargets } from "@/vms/migrate";

export type BulkAction =
  "start" | "stop" | "force-stop" | "reboot" | "snapshot" | "migrate" | "delete";

/** 일괄 막대의 순서 */
export const BULK_ACTIONS: readonly BulkAction[] = [
  "start",
  "stop",
  "force-stop",
  "reboot",
  "snapshot",
  "migrate",
  "delete",
];

/** 동작마다 VM에 있어야 하는 권한(계획 62 경로표) */
export const BULK_PRIVILEGE: Record<BulkAction, Privilege> = {
  start: "vm.power",
  stop: "vm.power",
  "force-stop": "vm.power",
  reboot: "vm.power",
  snapshot: "vm.snapshot",
  migrate: "vm.migrate",
  delete: "vm.delete",
};

/** 되돌릴 수 없어 고른 수를 입력해야 하는 동작(설계 §5.8) */
export const TYPED: readonly BulkAction[] = ["delete", "force-stop"];

/** 한 번에 보내는 요청 수 */
export const BULK_LIMIT = 4;

export interface BulkContext {
  /** VM id → 그 VM의 Task(`useTaskIndex`) */
  tasks: Map<string, Task[]>;
  nodes: readonly Node[];
  /** 스냅샷이 있는 VM id(`snapshotVms`). 고른 VM마다 스냅샷 목록 전체를 훑지 않게 한 번 모아 둔다. */
  snapshotVms: ReadonlySet<string>;
  /** 옮길 노드 id */
  target?: string;
  /** 이 VM에 이 권한이 있는지(폴더·객체 grant). 없으면 모두 있는 것으로 본다. */
  allowed?(vm: Vm, privilege: Privilege): boolean;
}

/** 이 VM에 이 동작을 할 수 없는 이유(번역 키), 할 수 있으면 null */
export function bulkReason(vm: Vm, action: BulkAction, ctx: BulkContext): string | null {
  if (ctx.allowed && !ctx.allowed(vm, BULK_PRIVILEGE[action])) {
    return "vms.bulk.reason.denied";
  }
  const vmTasks = ctx.tasks.get(vm.id) ?? [];
  if (action === "snapshot") {
    if (isMigrating(vm, vmTasks)) {
      return "vms.reason.migrating";
    }
    if (vm.spec.template === true) {
      return "vms.reason.template";
    }
    return isBusy(vm) ? "vms.reason.busy" : null;
  }
  const able = availability(vm, action, vmTasks);
  if (!able.ok) {
    return able.reason;
  }
  if (action === "delete" && ctx.snapshotVms.has(vm.id)) {
    return "vms.bulk.reason.snapshots";
  }
  if (action === "migrate") {
    if (!ctx.target) {
      return "vms.bulk.reason.noTarget";
    }
    if ((vm.spec.node ?? vm.status.node) === ctx.target) {
      return "vms.bulk.reason.sameNode";
    }
    if (!migrationTargets(vm, ctx.nodes).some((n) => n.id === ctx.target)) {
      return "vms.bulk.reason.target";
    }
  }
  return null;
}

/** 스냅샷이 하나라도 있는 VM id */
export function snapshotVms(snapshots: readonly Snapshot[] | undefined): ReadonlySet<string> {
  return new Set(snapshots?.map((s) => s.spec.vm));
}

/** 일괄 스냅샷 이름 `bulk-YYYYMMDD-HHMM`(이 브라우저의 시각) */
export function snapshotName(now: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `bulk-${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(
    now.getHours(),
  )}${pad(now.getMinutes())}`;
}

/** `items`마다 `run`을 부르되 동시에 `limit`개까지만 돈다. 결과는 `items` 순서다. */
export async function runLimited<T, R>(
  items: readonly T[],
  limit: number,
  run: (item: T) => Promise<R>,
): Promise<PromiseSettledResult<R>[]> {
  const results: PromiseSettledResult<R>[] = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const index = next++;
      try {
        results[index] = { status: "fulfilled", value: await run(items[index]!) };
      } catch (reason) {
        results[index] = { status: "rejected", reason };
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return results;
}
