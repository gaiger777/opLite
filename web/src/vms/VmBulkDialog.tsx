import { useQueryClient } from "@tanstack/react-query";
import { useId, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Task, Vm } from "@/api/types";
import { objectTarget, type Privilege } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { TypeToConfirm } from "@/components/ConfirmDialog";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { waitForTask } from "@/tasks/tasks";
import {
  type BulkAction,
  BULK_LIMIT,
  bulkReason,
  runLimited,
  snapshotName,
  snapshotVms,
  TYPED,
} from "@/vms/bulk";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";

/** VM 하나의 진행: 요청함(Task를 기다림), 끝남, 실패(까닭) */
type Outcome = { state: "requested" } | { state: "done" } | { state: "failed"; error: string };

/**
 * 고른 VM에 한 동작을 한다(설계 §5.3). 영향 목록(이름·노드·상태·못 하는 까닭)을 실시간 VM 목록의 지금 값으로 보이고,
 * 지우기·강제 정지는 실제로 할 수(가능 수)를 입력해야 한다. 요청은 동시에 `BULK_LIMIT`개씩 보내고 VM마다 Task가 끝나기를 기다려 이 목록에 보인 뒤, 결과를 알림
 * 하나로 묶는다. 닫아도 기다림과 알림은 이어진다.
 */
export function VmBulkDialog({
  action,
  ids,
  onClose,
  onStarted,
}: {
  action: BulkAction;
  /** 고른 VM id. 상태는 열려 있는 동안 실시간 목록에서 읽는다. */
  ids: string[];
  onClose(): void;
  /** 요청을 보내기 시작했다(고르기를 푼다). */
  onStarted(): void;
}) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const tasks = useTaskIndex();
  const perms = usePermissions();
  const nodes = useList("node");
  const snapshots = useList("snapshot");
  const live = useList("vm");
  const byId = useMemo(() => new Map(live.data?.map((vm) => [vm.id, vm])), [live.data]);
  // 연 때의 값: 목록에서 빠진 VM(지운 뒤 등)의 이름을 계속 보이는 데만 쓴다.
  const [opened] = useState(() => byId);
  const vms = ids.flatMap((id) => {
    const vm = byId.get(id) ?? opened.get(id);
    return vm ? [vm] : [];
  });
  const withSnapshots = useMemo(() => snapshotVms(snapshots.data), [snapshots.data]);
  const candidates = (nodes.data ?? []).filter(
    (n) => !n.spec.witness && n.status.online && !n.spec.maintenance,
  );
  const [target, setTarget] = useState(candidates[0]?.id ?? "");
  const [typed, setTyped] = useState("");
  const [outcomes, setOutcomes] = useState<Record<string, Outcome> | null>(null);
  const typeId = useId();
  const nodeName = (id: string | null | undefined) =>
    (id && nodes.data?.find((n) => n.id === id)?.name) || "—";
  const ctx = {
    tasks,
    nodes: nodes.data ?? [],
    snapshotVms: withSnapshots,
    target: target || candidates[0]?.id,
    allowed: (vm: Vm, privilege: Privilege) => perms.has(privilege, objectTarget("vm", vm)),
  };
  // 시작한 뒤에는 그때 고른 VM을 그대로 보인다(상태가 바뀌어도 까닭이 다시 붙지 않게).
  const [frozen, setFrozen] = useState<Set<string> | null>(null);
  const reasonOf = (vm: Vm) =>
    frozen
      ? frozen.has(vm.id)
        ? null
        : "vms.bulk.skippedReason"
      : byId.has(vm.id)
        ? bulkReason(vm, action, ctx)
        : "vms.bulk.reason.gone";
  const able = vms.filter((vm) => reasonOf(vm) === null);
  // 노드를 읽을 수 없는 폴더 사용자는 노드 목록 없이 한다(옮기기 말고는 노드가 필요 없다).
  const loaded =
    (nodes.isSuccess || !perms.reads("node")) && (action !== "delete" || snapshots.isSuccess);
  const label = t(`vms.bulk.action.${action}`);
  // 입력할 수는 실제로 할 VM 수다. 그 수가 바뀌면(누가 VM을 켰다 등) 다시 입력해야 한다.
  const typedOk = !TYPED.includes(action) || typed.trim() === String(able.length);
  const started = outcomes !== null;

  async function request(vm: Vm, name: string): Promise<Task | null> {
    const path = { key: vm.id };
    switch (action) {
      case "start":
      case "stop":
      case "reboot":
      case "force-stop":
        return call(
          api.POST("/vms/{key}/actions/{action}", {
            params: {
              path: { ...path, action: action === "force-stop" ? "stop" : action },
              query: action === "force-stop" ? { mode: "force" } : {},
            },
          }),
        );
      case "snapshot":
        return call(
          api.POST("/vms/{key}/snapshots", {
            params: { path },
            body: { name, description: "", memory: false },
          }),
        );
      case "migrate":
        return call(
          api.POST("/vms/{key}/migrate", { params: { path }, body: { node: ctx.target! } }),
        );
      case "delete":
        await call(api.DELETE("/vms/{key}", { params: { path } }));
        return null;
    }
  }

  async function start() {
    const chosen = able;
    const name = snapshotName(new Date());
    setFrozen(new Set(chosen.map((vm) => vm.id)));
    setOutcomes(Object.fromEntries(chosen.map((vm) => [vm.id, { state: "requested" }])));
    onStarted();
    const set = (id: string, outcome: Outcome) =>
      setOutcomes((prev) => ({ ...prev, [id]: outcome }));
    const toastId = toast.loading(t("vms.bulk.running", { action: label, count: chosen.length }));
    // VM마다 Task가 끝나면 참(성공), 요청이나 Task가 실패하면 거짓
    const waits: Promise<boolean>[] = [];
    const fail = (vm: Vm, err: unknown) => {
      set(vm.id, { state: "failed", error: errorMessage(t, err) });
      return false;
    };
    await runLimited(chosen, BULK_LIMIT, async (vm) => {
      try {
        const task = await request(vm, name);
        waits.push(
          (task ? waitForTask(queryClient, task.id) : Promise.resolve()).then(
            () => {
              set(vm.id, { state: "done" });
              return true;
            },
            (err: unknown) => fail(vm, err),
          ),
        );
      } catch (err) {
        waits.push(Promise.resolve(fail(vm, err)));
      }
    });
    const failed = (await Promise.all(waits)).filter((ok) => !ok).length;
    const summary = [
      t("vms.bulk.summary", {
        action: label,
        ok: chosen.length - failed,
        total: chosen.length,
        failed,
      }),
      vms.length > chosen.length
        ? t("vms.bulk.skipped", { count: vms.length - chosen.length })
        : "",
    ]
      .filter(Boolean)
      .join(" ");
    if (failed > 0) {
      toast.error(summary, {
        id: toastId,
        duration: Infinity,
        closeButton: true,
        action: { label: t("vms.bulk.view"), onClick: () => void navigate("/tasks") },
      });
    } else {
      toast.success(summary, { id: toastId });
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("vms.bulk.title", { action: label, count: vms.length })}</DialogTitle>
          <DialogDescription>
            {t("vms.bulk.counts", { able: able.length, blocked: vms.length - able.length })}
          </DialogDescription>
        </DialogHeader>
        {action === "migrate" && !started && (
          <div className="flex flex-col gap-2">
            <Label htmlFor={`${typeId}-node`}>{t("migrate.node")}</Label>
            <NativeSelect
              id={`${typeId}-node`}
              className="w-full"
              value={ctx.target ?? ""}
              onChange={(e) => setTarget(e.target.value)}
            >
              {candidates.map((n) => (
                <NativeSelectOption key={n.id} value={n.id}>
                  {n.name}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          </div>
        )}
        <div className="max-h-80 overflow-y-auto">
          <Table aria-label={t("vms.bulk.impact")}>
            <TableHeader>
              <TableRow>
                <TableHead>{t("common.name")}</TableHead>
                <TableHead>{t("common.node")}</TableHead>
                <TableHead>{t("common.state")}</TableHead>
                <TableHead>{t("vms.bulk.result")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {vms.map((vm) => {
                const reason = reasonOf(vm);
                const outcome = outcomes?.[vm.id];
                return (
                  <TableRow key={vm.id}>
                    <TableCell className="font-medium">{vm.name}</TableCell>
                    <TableCell>{nodeName(vm.spec.node ?? vm.status.node)}</TableCell>
                    <TableCell>
                      <VmPhaseBadge vm={vm} />
                    </TableCell>
                    <TableCell className="whitespace-normal">
                      {reason ? (
                        <StatusChip tone="neutral">{t(reason)}</StatusChip>
                      ) : outcome?.state === "failed" ? (
                        <StatusChip tone="danger" title={outcome.error}>
                          {t("vms.bulk.failed", { error: outcome.error })}
                        </StatusChip>
                      ) : outcome?.state === "done" ? (
                        <StatusChip tone="success">{t("vms.bulk.done")}</StatusChip>
                      ) : outcome ? (
                        <StatusChip tone="info" busy>
                          {t("vms.bulk.requested")}
                        </StatusChip>
                      ) : (
                        <StatusChip tone="info">{t("vms.bulk.ready")}</StatusChip>
                      )}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
        {action === "snapshot" && !started && (
          <p className="text-sm text-muted-foreground">
            {t("vms.bulk.snapshotName", { name: snapshotName(new Date()) })}
          </p>
        )}
        {TYPED.includes(action) && !started && (
          <div className="flex flex-col gap-2">
            <p className="text-sm">
              {t("vms.bulk.willRun", { able: able.length, total: vms.length })}
            </p>
            <TypeToConfirm id={typeId} expected={able.length} value={typed} onChange={setTyped} />
          </div>
        )}
        <DialogFooter>
          {started ? (
            <Button variant="outline" onClick={onClose}>
              {t("common.done")}
            </Button>
          ) : (
            <>
              <Button variant="outline" onClick={onClose}>
                {t("common.cancel")}
              </Button>
              <Button
                variant={TYPED.includes(action) ? "destructive" : "default"}
                disabled={!loaded || able.length === 0 || !typedOk}
                onClick={() => void start()}
              >
                {label}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
