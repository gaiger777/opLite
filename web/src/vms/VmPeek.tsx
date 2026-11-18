import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useTaskIndex } from "@/api/taskIndex";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { PeekFacts } from "@/components/data-table/PeekFacts";
import { ACTION_ICONS } from "@/components/icons";
import { PinButton } from "@/components/PinButton";
import { Button } from "@/components/ui/button";
import { SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { formatMib } from "@/lib/format";
import { availability, useVmActions } from "@/vms/actions";
import { useVmActionHost } from "@/vms/actionHost";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";

/** VM 미리 보기(설계 §5.3): 요약, 주 동작(시작·정지·콘솔), 열기 */
export function VmPeek({ vm, nodeName }: { vm: Vm; nodeName(id?: string | null): string }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const host = useVmActionHost();
  const { run } = useVmActions();
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const operate = can(me.role, "operate", "vm");
  const href = `/vms/${encodeURIComponent(vm.name)}`;
  const ips = (vm.spec.cloud_init?.networks ?? []).flatMap((net) =>
    [net.ipv4, net.ipv6].flatMap((c) => (c?.mode === "static" ? [c.address] : [])),
  );
  const ok = (action: "start" | "stop" | "console") => availability(vm, action, vmTasks).ok;
  const Start = ACTION_ICONS.start;
  const Stop = ACTION_ICONS.stop;
  const Console = ACTION_ICONS.console;
  return (
    <>
      <SheetHeader>
        <div className="flex items-center gap-2 pr-8">
          <SheetTitle className="truncate">{vm.name}</SheetTitle>
          <PinButton resource={{ kind: "vm", id: vm.id, name: vm.name }} shortcut={false} />
        </div>
        <SheetDescription asChild>
          <div>
            <VmPhaseBadge vm={vm} />
          </div>
        </SheetDescription>
      </SheetHeader>
      <PeekFacts
        facts={[
          [t("common.node"), nodeName(vm.spec.node ?? vm.status.node)],
          [t("vms.vcpus"), (vm.spec.cpu.sockets ?? 1) * vm.spec.cpu.cores],
          [t("vms.memory"), formatMib(vm.spec.memory_mib)],
          ...(ips.length > 0
            ? [[t("table.addresses"), ips.join(", ")] as [string, ReactNode]]
            : []),
        ]}
      />
      <div className="flex flex-wrap gap-2 p-4">
        {operate && ok("start") && (
          <Button variant="outline" size="sm" onClick={() => void run(vm, "start")}>
            <Start />
            {t("vms.action.start")}
          </Button>
        )}
        {operate && ok("stop") && (
          <Button variant="outline" size="sm" onClick={() => host.open({ vm, which: "stop" })}>
            <Stop />
            {t("vms.action.stop")}
          </Button>
        )}
        {operate && ok("console") && (
          <Button variant="outline" size="sm" asChild>
            <Link to={`${href}/console`}>
              <Console />
              {t("vms.action.console")}
            </Link>
          </Button>
        )}
        <Button size="sm" asChild>
          <Link to={href}>{t("table.open")}</Link>
        </Button>
      </div>
    </>
  );
}
