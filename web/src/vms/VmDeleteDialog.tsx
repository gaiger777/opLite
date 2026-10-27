import { useQueryClient } from "@tanstack/react-query";
import { type RefObject, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { listKey, useList, type List } from "@/api/live";
import type { Snapshot, Vm } from "@/api/types";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ListPending } from "@/components/ListPending";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { trackTask, waitForTask } from "@/tasks/tasks";
import { isStopped } from "@/vms/actions";

/**
 * VM 삭제 확인. 열 때만 그린다. 서버는 스냅샷이 남은 VM을 지우지 않으므로(vm_has_snapshots), 스냅샷이
 * 있으면 함께 지울지 고르게 하고, 고르면 스냅샷 삭제 Task를 하나씩 기다린 뒤 VM을 지운다.
 */
export function VmDeleteDialog({
  vm,
  onClose,
  onRemove,
  afterRemove,
  returnFocus,
}: {
  vm: Vm;
  onClose(): void;
  onRemove(vm: Vm): Promise<void>;
  afterRemove?(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const snapshots = useList("snapshot");
  const [withSnapshots, setWithSnapshots] = useState(false);
  // 확인을 누를 때의 스냅샷. 지우는 동안 목록에서 빠져도 이것으로 그린다.
  const [deleting, setDeleting] = useState<Snapshot[] | null>(null);
  const checkboxId = useId();
  // 대화상자가 사라지면(다른 화면으로 가거나 로그아웃) 스냅샷 삭제를 그만 기다리고 VM도 지우지 않는다.
  const abort = useRef<AbortController | null>(null);
  useEffect(() => () => abort.current?.abort(), []);
  const mine = (snapshots.data ?? []).filter((s) => s.spec.vm === vm.id);
  // 만드는 중인 스냅샷은 지울 수 없다(snapshot_not_ready). 끝나기를 기다리게 한다.
  const pending = mine.some((s) => s.status.phase !== "ready");
  const shown = deleting ?? mine;

  async function confirm() {
    // 대화상자를 연 뒤 VM이 시작됐을 수 있다. 서버가 VM을 지우지 않을 것이면 스냅샷도 지우지 않는다.
    const latest =
      queryClient.getQueryData<List<Vm>>(listKey("vm"))?.items.find((v) => v.id === vm.id) ?? vm;
    if (!isStopped(latest)) {
      throw new Error(t("errors.vm_running"));
    }
    const controller = new AbortController();
    abort.current = controller;
    const targets = withSnapshots ? mine : [];
    setDeleting(targets);
    try {
      for (const snapshot of targets) {
        const task = await call(
          api.DELETE("/vms/{key}/snapshots/{name}", {
            params: { path: { key: vm.id, name: snapshot.id } },
          }),
        );
        trackTask(task, t("snapshots.op.delete", { name: snapshot.spec.name, vm: vm.name }));
        await waitForTask(queryClient, task.id, controller.signal);
      }
      await onRemove(vm);
      afterRemove?.();
    } finally {
      // 실패하면 다시 시도할 때 그때의 목록을 쓴다.
      setDeleting(null);
    }
  }

  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={t("vms.deleteTitle", { name: vm.name })}
      description={t("vms.deleteDescription")}
      confirmLabel={t("vms.action.delete")}
      typeToConfirm={vm.name}
      returnFocus={returnFocus}
      // 스냅샷 목록을 다 읽어야 스냅샷이 있는지 안다.
      confirmDisabled={!snapshots.isSuccess || pending || (mine.length > 0 && !withSnapshots)}
      onConfirm={confirm}
    >
      {!snapshots.isSuccess && <ListPending error={snapshots.error} />}
      {shown.length > 0 && (
        <>
          <div className="flex items-center gap-2">
            <Checkbox
              id={checkboxId}
              checked={withSnapshots}
              onCheckedChange={(v) => setWithSnapshots(v === true)}
            />
            <Label htmlFor={checkboxId}>{t("vms.deleteSnapshots", { count: shown.length })}</Label>
          </div>
          {!withSnapshots && (
            <p className="text-sm text-muted-foreground">{t("vms.hasSnapshots")}</p>
          )}
          {deleting === null && pending && (
            <p className="text-sm text-muted-foreground">{t("vms.snapshotsPending")}</p>
          )}
          {deleting !== null && deleting.length > 0 && (
            <p className="text-sm text-muted-foreground">{t("vms.deletingSnapshots")}</p>
          )}
        </>
      )}
    </ConfirmDialog>
  );
}
