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
 * 막힌다(만들기·삭제·복제 중에는 받는다). `tasks`는 watch로 들어온 Task 목록이다.
 */
export function isReverting(vm: Vm, tasks: readonly Task[]): boolean {
  return tasks.some(
    (task) =>
      task.spec.target === vm.id &&
      task.spec.op.kind === "snapshot-revert" &&
      task.status.state !== "succeeded" &&
      task.status.state !== "failed",
  );
}

/** 지금 상태에서 할 수 있는 동작인지. 서버도 확인하지만 할 수 없는 버튼은 막아 둔다. */
export function canRun(vm: Vm, action: VmAction): boolean {
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
  }
}

/** 지울 수 있는 상태인지(서버는 멈추지 않은 VM을 지우지 않는다) */
export function canDelete(vm: Vm): boolean {
  return isStopped(vm);
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

  /** 여러 VM에 같은 동작을 한다. 건너뛰거나 실패한 것이 있으면 요약 알림을 하나 더 띄운다. */
  async function runMany(vms: Vm[], action: VmAction): Promise<BulkResult> {
    const able = vms.filter((vm) => canRun(vm, action));
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
