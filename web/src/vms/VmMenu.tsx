import { MoreHorizontal } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { useTaskIndex } from "@/api/taskIndex";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  ACTIONS,
  availability,
  useVmActions,
  type VmAction,
  type VmOperation,
} from "@/vms/actions";
import { VmCloneDialog } from "@/vms/VmCloneDialog";
import { VmDeleteDialog } from "@/vms/VmDeleteDialog";
import { VmMigrateDialog } from "@/vms/VmMigrateDialog";

/** 목록 행의 동작 메뉴 */
export function VmMenu({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const navigate = useNavigate();
  const { run, remove } = useVmActions();
  const [confirm, setConfirm] = useState<VmDialog | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  // Task 목록을 아직 모르면 Task 때문에는 막지 않는다(서버에 맡긴다).
  const reasonOf = (action: VmOperation) => {
    const able = availability(vm, action, vmTasks);
    return able.ok ? null : able.reason;
  };
  const operate = can(me.role, "operate", "vm");
  const write = can(me.role, "write", "vm");
  // 복제는 새 VM과 새 볼륨을 만든다.
  const clone = write && can(me.role, "write", "volume");
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
              <ReasonMenuItem
                key={action}
                reason={reasonOf(action)}
                onSelect={() =>
                  action === "force-stop" ? setConfirm("force-stop") : void run(vm, action)
                }
              >
                {t(`vms.action.${action}`)}
              </ReasonMenuItem>
            ))}
          {operate && (
            <ReasonMenuItem
              reason={reasonOf("console")}
              onSelect={() => void navigate(`/vms/${encodeURIComponent(vm.name)}/console`)}
            >
              {t("vms.action.console")}
            </ReasonMenuItem>
          )}
          {clone && (
            <DropdownMenuItem onSelect={() => setConfirm("clone")}>
              {t("vms.action.clone")}
            </DropdownMenuItem>
          )}
          {write && (
            <>
              <DropdownMenuSeparator />
              <ReasonMenuItem
                variant="destructive"
                reason={reasonOf("delete")}
                onSelect={() => setConfirm("delete")}
              >
                {t("vms.action.delete")}
              </ReasonMenuItem>
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

/** 메뉴·상세 버튼이 여는 대화상자 */
export type VmDialog = "force-stop" | "delete" | "clone" | "migrate";

/** 강제 정지·삭제의 확인과 복제·마이그레이션 대화상자 */
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
  which: VmDialog | null;
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
      {which === "delete" && (
        <VmDeleteDialog
          vm={vm}
          onClose={onClose}
          onRemove={onRemove}
          afterRemove={afterRemove}
          returnFocus={returnFocus}
        />
      )}
      {which === "clone" && <VmCloneDialog vm={vm} onClose={onClose} returnFocus={returnFocus} />}
      {which === "migrate" && (
        <VmMigrateDialog vm={vm} onClose={onClose} returnFocus={returnFocus} />
      )}
    </>
  );
}
