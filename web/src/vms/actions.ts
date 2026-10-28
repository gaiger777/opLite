import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { Task, Vm } from "@/api/types";
import { errorMessage } from "@/i18n/errors";
import { trackTask } from "@/tasks/tasks";

export type VmAction = "start" | "stop" | "force-stop" | "reboot";

/** 메뉴와 상세 화면이 보여 주는 동작(이 순서로) */
export const ACTIONS: readonly VmAction[] = ["start", "stop", "force-stop", "reboot"];

/**
 * 서버의 `is_stopped`: 원하는 전원 상태도 꺼짐이고 실제로도 멈춰 있다. 삭제와 노드·디스크 변경은 이때만
 * 된다. 막 시작했거나 노드가 내려가 `power`만 켜짐인 VM은 멈춘 것이 아니다.
 */
export function isStopped(vm: Vm): boolean {
  const phase = vm.status.phase;
  return (vm.spec.power ?? "stopped") === "stopped" && (phase === "stopped" || phase === "failed");
}

/**
 * 서버의 `check_not_busy`: 시작·정지 중이거나 원하는 전원 상태에 아직 닿지 않았다. 이때 서버는 스냅샷·복제
 * 요청을 받지 않는다(vm_busy).
 */
export function isBusy(vm: Vm): boolean {
  const phase = vm.status.phase;
  if (phase === "starting" || phase === "stopping") {
    return true;
  }
  return vm.spec.power === "running" ? phase !== "running" : !isStopped(vm);
}

/**
 * 서버의 `check_no_revert_task`: 이 VM의 스냅샷 되돌리기 Task가 끝나지 않았다. 강제 정지는 이때만
 * 막힌다(만들기·삭제·복제 중에는 받는다). `vmTasks`는 이 VM을 대상으로 한 Task다(`useTaskIndex`).
 */
export function isReverting(vmTasks: readonly Task[]): boolean {
  return vmTasks.some(
    (task) =>
      task.spec.op.kind === "snapshot-revert" &&
      task.status.state !== "succeeded" &&
      task.status.state !== "failed",
  );
}

/** 할 수 있는지, 없으면 그 이유(번역 키) */
export type Availability = { ok: true } | { ok: false; reason: string };

/** 끝나지 않으면 다른 시작·정지·재시작을 막는 전원 Task */
const POWER_KINDS: readonly string[] = ["vm-start", "vm-stop", "vm-force-stop", "vm-reboot"];

/** 상태만 보고 할 수 있는 동작인지. 서버도 확인하지만 할 수 없는 버튼은 막아 둔다. */
function allowedByState(vm: Vm, action: VmAction | "console" | "delete"): boolean {
  const phase = vm.status.phase;
  const powerOn = vm.spec.power === "running";
  switch (action) {
    case "start":
      return isStopped(vm);
    case "stop":
      return powerOn || phase === "running" || phase === "starting";
    case "force-stop":
      // 서버는 어느 상태의 정지든 받는다. 끄려는 중이거나 켜려는 VM에만 보인다.
      return powerOn || phase === "running" || phase === "starting" || phase === "stopping";
    case "reboot":
      // 서버는 원하는 전원 상태가 켜짐인 실행 중 VM만 재시작한다.
      return powerOn && phase === "running";
    case "console":
      return phase === "running";
    case "delete":
      // 서버는 멈추지 않은 VM을 지우지 않는다.
      return isStopped(vm);
  }
}

/** 상태 때문에 막혔을 때의 이유: 실패, 멈춤, 켜져 실행 중, 그 사이(바뀌는 중) */
function stateReason(vm: Vm): string {
  if (vm.status.phase === "failed") {
    return "vms.reason.failed";
  }
  if (isStopped(vm)) {
    return "vms.reason.stopped";
  }
  return isBusy(vm) ? "vms.reason.busy" : "vms.reason.running";
}

/**
 * 지금 할 수 있는 동작인지와 막힌 이유. `vmTasks`는 이 VM을 대상으로 한 Task다(`useTaskIndex`). 전원 Task가
 * 끝나지 않았으면 시작·정지·재시작을 막는다(서버는 새 요청으로 앞 것을 대체하지만 웹에서는 겹치지 않게).
 * 강제 정지는 정지가 ACPI에서 멈췄을 때 빠져나갈 길이므로 되돌리기 중에만 막는다.
 */
export function availability(
  vm: Vm,
  action: VmAction | "console" | "delete",
  vmTasks: readonly Task[],
): Availability {
  const power = action === "start" || action === "stop" || action === "reboot";
  if (
    power &&
    vmTasks.some(
      (task) =>
        POWER_KINDS.includes(task.spec.op.kind) &&
        task.status.state !== "succeeded" &&
        task.status.state !== "failed",
    )
  ) {
    return { ok: false, reason: "vms.reason.powerPending" };
  }
  if (!allowedByState(vm, action)) {
    return { ok: false, reason: stateReason(vm) };
  }
  if (action === "force-stop" && isReverting(vmTasks)) {
    return { ok: false, reason: "vms.reason.reverting" };
  }
  return { ok: true };
}

/** 여럿에 한 동작의 결과: 요청이 받아들여진 수, 실패한 수, 할 수 없어 건너뛴 수 */
interface BulkResult {
  done: number;
  failed: number;
  skipped: number;
}

export function useVmActions() {
  const { t } = useTranslation();

  async function request(vm: Vm, action: VmAction) {
    const task = await call(
      api.POST("/vms/{key}/actions/{action}", {
        params: {
          path: { key: vm.id, action: action === "force-stop" ? "stop" : action },
          query: action === "force-stop" ? { mode: "force" } : {},
        },
      }),
    );
    trackTask(task, t(`tasks.op.vm-${action}`, { target: vm.name }));
  }

  /** 동작을 요청하고 그 Task를 알림으로 지켜본다. 실패는 VM 이름과 함께 알림으로 보여 준다. */
  async function run(vm: Vm, action: VmAction) {
    try {
      await request(vm, action);
    } catch (err) {
      toast.error(t("vms.actionFailed", { name: vm.name, error: errorMessage(t, err) }));
    }
  }

  /**
   * 여러 VM에 같은 동작을 한다. 할 수 없는 VM은 건너뛴다(`taskIndex`는 `useTaskIndex`). 건너뛰거나 실패한
   * 것이 있으면 요약 알림을 하나 더 띄운다.
   */
  async function runMany(
    vms: Vm[],
    action: VmAction,
    taskIndex: Map<string, Task[]>,
  ): Promise<BulkResult> {
    const able = vms.filter((vm) => availability(vm, action, taskIndex.get(vm.id) ?? []).ok);
    const results = await Promise.allSettled(
      able.map((vm) =>
        request(vm, action).catch((err: unknown) => {
          toast.error(t("vms.actionFailed", { name: vm.name, error: errorMessage(t, err) }));
          throw err;
        }),
      ),
    );
    const failed = results.filter((r) => r.status === "rejected").length;
    const result = { done: able.length - failed, failed, skipped: vms.length - able.length };
    if (result.failed > 0 || result.skipped > 0) {
      const summary = t("vms.bulkSummary", {
        action: t(`vms.action.${action}`),
        ...result,
      });
      if (result.failed > 0) {
        toast.error(summary);
      } else {
        toast.info(summary);
      }
    }
    return result;
  }

  /** VM을 지운다. 실패하면 던진다(확인 대화상자가 보여 준다). 볼륨은 남는다. */
  async function remove(vm: Vm) {
    await call(api.DELETE("/vms/{key}", { params: { path: { key: vm.id } } }));
    toast.success(t("vms.deleted", { name: vm.name }));
  }

  return { run, runMany, remove };
}
