import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { Vm } from "@/api/types";
import { errorMessage } from "@/i18n/errors";
import { trackTask } from "@/tasks/tasks";

export type VmAction = "start" | "stop" | "force-stop" | "reboot";

/** 지금 상태에서 할 수 있는 동작인지. 서버도 확인하지만 할 수 없는 버튼은 막아 둔다. */
export function canRun(vm: Vm, action: VmAction): boolean {
  const phase = vm.status.phase;
  switch (action) {
    case "start":
      return phase === "stopped" || phase === "failed";
    case "stop":
      return phase === "running" || phase === "starting";
    case "force-stop":
      return phase === "running" || phase === "starting" || phase === "stopping";
    case "reboot":
      return phase === "running";
  }
}

/** 지울 수 있는 상태인지(서버는 실행 중인 VM을 지우지 않는다) */
export function canDelete(vm: Vm): boolean {
  return vm.status.phase === "stopped" || vm.status.phase === "failed";
}

export function useVmActions() {
  const { t } = useTranslation();

  /** 동작을 요청하고 그 Task를 알림으로 지켜본다. 실패는 알림으로 보여 준다. */
  async function run(vm: Vm, action: VmAction) {
    try {
      const task = await call(
        api.POST("/vms/{key}/actions/{action}", {
          params: {
            path: { key: vm.id, action: action === "force-stop" ? "stop" : action },
            query: action === "force-stop" ? { mode: "force" } : {},
          },
        }),
      );
      trackTask(task, t(`tasks.op.vm-${action}`, { target: vm.name }));
    } catch (err) {
      toast.error(errorMessage(t, err));
    }
  }

  /** VM을 지운다. 실패하면 던진다(확인 대화상자가 보여 준다). 볼륨은 남는다. */
  async function remove(vm: Vm) {
    await call(api.DELETE("/vms/{key}", { params: { path: { key: vm.id } } }));
    toast.success(t("vms.deleted", { name: vm.name }));
  }

  return { run, remove };
}
