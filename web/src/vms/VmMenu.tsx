import { MoreHorizontal } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ACTIONS, canDelete, canRun, useVmActions, type VmAction } from "@/vms/actions";

/** 목록 행의 동작 메뉴 */
export function VmMenu({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const navigate = useNavigate();
  const { run, remove } = useVmActions();
  const [confirm, setConfirm] = useState<"force-stop" | "delete" | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const operate = can(me.role, "operate", "vm");
  const write = can(me.role, "write", "vm");
  if (!operate && !write) {
    return null;
  }
  return (
    <>
      {/* 메뉴에서 대화상자를 열므로 모달로 두지 않는다(닫힌 메뉴가 화면 클릭을 막지 않게). */}
      <DropdownMenu modal={false}>
        <DropdownMenuTrigger asChild>
          <Button
            ref={trigger}
            variant="ghost"
            size="icon-sm"
            aria-label={t("vms.actionsFor", { name: vm.name })}
          >
            <MoreHorizontal />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          {operate &&
            ACTIONS.map((action) => (
              <DropdownMenuItem
                key={action}
                disabled={!canRun(vm, action)}
                onSelect={() =>
                  action === "force-stop" ? setConfirm("force-stop") : void run(vm, action)
                }
              >
                {t(`vms.action.${action}`)}
              </DropdownMenuItem>
            ))}
          {operate && (
            <DropdownMenuItem
              disabled={vm.status.phase !== "running"}
              onSelect={() => void navigate(`/vms/${encodeURIComponent(vm.name)}/console`)}
            >
              {t("vms.action.console")}
            </DropdownMenuItem>
          )}
          {write && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                variant="destructive"
                disabled={!canDelete(vm)}
                onSelect={() => setConfirm("delete")}
              >
                {t("vms.action.delete")}
              </DropdownMenuItem>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      <VmConfirm
        vm={vm}
        which={confirm}
        onClose={() => setConfirm(null)}
        onRun={run}
        onRemove={remove}
        returnFocus={trigger}
      />
    </>
  );
}

/** 강제 정지와 삭제의 확인 대화상자 */
export function VmConfirm({
  vm,
  which,
  onClose,
  onRun,
  onRemove,
  afterRemove,
  returnFocus,
}: {
  vm: Vm;
  which: "force-stop" | "delete" | null;
  onClose(): void;
  onRun(vm: Vm, action: VmAction): Promise<void>;
  onRemove(vm: Vm): Promise<void>;
  afterRemove?(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  return (
    <>
      <ConfirmDialog
        open={which === "force-stop"}
        onOpenChange={(open) => !open && onClose()}
        title={t("vms.forceStopTitle", { name: vm.name })}
        description={t("vms.forceStopDescription")}
        confirmLabel={t("vms.action.force-stop")}
        returnFocus={returnFocus}
        onConfirm={() => onRun(vm, "force-stop")}
      />
      <ConfirmDialog
        open={which === "delete"}
        onOpenChange={(open) => !open && onClose()}
        title={t("vms.deleteTitle", { name: vm.name })}
        description={t("vms.deleteDescription")}
        confirmLabel={t("vms.action.delete")}
        typeToConfirm={vm.name}
        returnFocus={returnFocus}
        onConfirm={async () => {
          await onRemove(vm);
          afterRemove?.();
        }}
      />
    </>
  );
}
