import { cn } from "cn";
import { useId } from "react";
import { useTranslation } from "react-i18next";

import { useTaskIndex } from "@/api/taskIndex";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ACTION_ICONS } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { useVmActionHost } from "@/vms/actionHost";
import { availability, isStopped, useVmActions } from "@/vms/actions";

/**
 * 당직 보기의 전원 단추 하나(설계 §5.9): 꺼진 VM은 시작(바로), 아니면 정지(확인 대화상자). 막히면 흐리고 이유를
 * 이름 뒤 설명으로 읽는다. operate 권한이 없으면 그리지 않는다. 터치 대상은 40px이다.
 */
export function VmPowerButton({ vm, className }: { vm: Vm; className?: string }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const { run } = useVmActions();
  const host = useVmActionHost();
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const reasonId = useId();
  if (!can(me.role, "operate", "vm")) {
    return null;
  }
  const action = isStopped(vm) ? "start" : "stop";
  const able = availability(vm, action, vmTasks);
  const Icon = action === "start" ? ACTION_ICONS.start : ACTION_ICONS.stop;
  return (
    <>
      <Button
        variant="outline"
        size="icon"
        className={cn("size-10", !able.ok && "opacity-50", className)}
        aria-label={t(`vms.powerButton.${action}`, { name: vm.name })}
        aria-disabled={able.ok ? undefined : true}
        aria-describedby={able.ok ? undefined : reasonId}
        title={able.ok ? undefined : t(able.reason)}
        onClick={() => {
          if (!able.ok) {
            return;
          }
          if (action === "start") {
            void run(vm, "start");
          } else {
            host.open({ vm, which: "stop" });
          }
        }}
      >
        <Icon />
      </Button>
      {!able.ok && (
        <span id={reasonId} className="sr-only">
          {t(able.reason)}
        </span>
      )}
    </>
  );
}
