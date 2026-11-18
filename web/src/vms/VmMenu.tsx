import { MoreHorizontal } from "lucide-react";
import { useRef, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { trackTask } from "@/tasks/tasks";
import { DeployDialog } from "@/templates/DeployDialog";
import { convertReason, isTemplate, revertReason } from "@/templates/templates";
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
import { useVmActionHost } from "@/vms/actionHost";
import { VmMigrateDialog } from "@/vms/VmMigrateDialog";
import { VmSnapshotDialog } from "@/vms/VmSnapshots";

/** 목록 행의 동작 메뉴 */
export function VmMenu({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const navigate = useNavigate();
  const { run } = useVmActions();
  const host = useVmActionHost();
  const trigger = useRef<HTMLButtonElement>(null);
  const setConfirm = (which: VmDialog) => host.open({ vm, which, returnFocus: trigger });
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const { convert, revert } = useTemplateReasons(vm);
  const template = isTemplate(vm);
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
            <DropdownMenuItem onSelect={() => setConfirm(template ? "deploy" : "clone")}>
              {template ? t("templates.deploy") : t("vms.action.clone")}
            </DropdownMenuItem>
          )}
          {write && (
            <ReasonMenuItem
              reason={template ? revert : convert}
              onSelect={() => setConfirm(template ? "untemplate" : "template")}
            >
              {template ? t("templates.revert") : t("templates.convert")}
            </ReasonMenuItem>
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
    </>
  );
}

/**
 * 템플릿으로 바꿀 수 없는 이유와 VM으로 되돌릴 수 없는 이유(번역 키). 스냅샷·볼륨 목록을 모르면 그것으로는 막지 않는다
 * (되돌리기는 연결 복제를 모르면 막는다).
 */
export function useTemplateReasons(vm: Vm): { convert: string | null; revert: string | null } {
  const snapshots = useList("snapshot");
  const volumes = useList("volume");
  return {
    convert: convertReason(vm, snapshots.data, volumes.data),
    revert: revertReason(vm, volumes.data),
  };
}

/** 메뉴·상세 버튼이 여는 대화상자 */
export type VmDialog =
  | "force-stop"
  | "delete"
  | "clone"
  | "migrate"
  | "deploy"
  | "template"
  | "untemplate"
  // 팔레트가 바로 하지 않고 확인을 받는 전원 동작과 스냅샷(설계 §5.2). 시작은 이름이 정확히 같지 않을 때만.
  | "start"
  | "stop"
  | "reboot"
  | "snapshot";

/** 강제 정지·삭제의 확인과 복제·마이그레이션 대화상자 */
export function VmConfirm({
  vm,
  which,
  node,
  onClose,
  onRun,
  onRemove,
  afterRemove,
  returnFocus,
}: {
  vm: Vm;
  which: VmDialog | null;
  /** 옮기기 대화상자에 미리 고를 노드 id */
  node?: string;
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
      {(["start", "stop", "reboot"] as const).map((action) => (
        <ConfirmDialog
          key={action}
          open={which === action}
          onOpenChange={(open) => !open && onClose()}
          title={t(`vms.${action}Title`, { name: vm.name })}
          description={t(`vms.${action}Description`)}
          confirmLabel={t(`vms.action.${action}`)}
          returnFocus={returnFocus}
          onConfirm={() => onRun(vm, action)}
        />
      ))}
      {which === "snapshot" && (
        <VmSnapshotDialog vm={vm} onClose={onClose} returnFocus={returnFocus} />
      )}
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
        <VmMigrateDialog vm={vm} initialNode={node} onClose={onClose} returnFocus={returnFocus} />
      )}
      {which === "deploy" && (
        <DeployDialog template={vm} onClose={onClose} returnFocus={returnFocus} />
      )}
      <ConfirmDialog
        open={which === "template"}
        onOpenChange={(open) => !open && onClose()}
        title={t("templates.convertTitle", { name: vm.name })}
        description={t("templates.convertDescription")}
        confirmLabel={t("templates.convert")}
        returnFocus={returnFocus}
        onConfirm={async () => {
          await call(api.POST("/vms/{key}/template", { params: { path: { key: vm.id } } }));
          toast.success(t("templates.converted", { name: vm.name }));
        }}
      />
      <ConfirmDialog
        open={which === "untemplate"}
        onOpenChange={(open) => !open && onClose()}
        title={t("templates.revertTitle", { name: vm.name })}
        description={t("templates.revertDescription")}
        confirmLabel={t("templates.revert")}
        returnFocus={returnFocus}
        onConfirm={async () => {
          const task = await call(
            api.DELETE("/vms/{key}/template", { params: { path: { key: vm.id } } }),
          );
          trackTask(task, t("tasks.op.vm-untemplate", { target: vm.name }));
        }}
      />
    </>
  );
}
