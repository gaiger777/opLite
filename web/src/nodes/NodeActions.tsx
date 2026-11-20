import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import { listKey, useList, type List } from "@/api/live";
import { optimistic, putResource } from "@/api/optimistic";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, Vm } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { parseLabels } from "@/backups/jobs";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { Button } from "@/components/ui/button";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { codeMessage, errorMessage } from "@/i18n/errors";
import { undoToast } from "@/lib/undo";
import { NodeDisksDialog } from "@/nodes/NodeDisksDialog";
import { NodeFenceDialog } from "@/nodes/NodeFenceDialog";
import { NodeInterfacesDialog } from "@/nodes/NodeInterfacesDialog";
import { NodeUpdatesDialog } from "@/nodes/NodeUpdatesDialog";
import { callWarned } from "@/sdn/sdn";
import { trackTask } from "@/tasks/tasks";

/** 그 노드에 고정했거나 그 노드에서 마지막으로 돈 VM의 수 */
export function vmCount(node: Node, vms: Vm[]): number {
  return vms.filter((vm) => (vm.spec.node ?? vm.status.node) === node.id).length;
}

/** 비우기가 옮길 VM의 수: 그 노드에 고정돼 켜 둔 VM(서버의 비우기 Task와 같은 기준) */
function runningCount(node: Node, vms: Vm[]): number {
  return vms.filter((vm) => vm.spec.node === node.id && vm.spec.power === "running").length;
}

/** 서버의 `NodeStatus::frozen`: 종료 정책 `freeze`로 꺼진 채 돌아오지 않은 노드. 리더가 HA VM을 옮기지 않는다. */
export function isFrozen(node: Node): boolean {
  return !node.status.online && node.status.shutdown?.action === "freeze";
}

/** 레이블을 `KEY=VALUE, …`로(키 순서) */
export function labelText(labels: Node["labels"]): string {
  return Object.entries(labels ?? {})
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([k, v]) => `${k}=${v}`)
    .join(", ");
}

/** 노드 동작이 여는 대화상자 */
type NodeDialog =
  "updates" | "interfaces" | "disks" | "fence" | "labels" | "drain" | "end" | "unfreeze" | "remove";

/**
 * 노드 동작(유지보수·비우기·업데이트·인터페이스·디스크·레이블·펜스·얼림 풀기·지우기)과 그 대화상자. 노드 목록의 행
 * 메뉴와 노드 상세 머리(`ResourceHeader`)가 함께 쓴다. `items`는 메뉴 항목, `dialogs`는 열린 대화상자다.
 * 대화상자를 닫으면 마지막으로 누른 메뉴 단추(`trigger`)로 포커스가 돌아간다.
 */
export function useNodeActions({ onRemoved }: { onRemoved?(node: Node): void } = {}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const vms = useList("vm");
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const tasks = useTaskIndex();
  const [open, setOpen] = useState<{ dialog: NodeDialog; node: Node } | null>(null);
  // 지우기가 409 `sdn_central_on_node`(OVN 중앙)로 거절된 노드: 대화상자에 SDN에서 빼기를 보인다(리뷰 2 웹 I-3).
  const [central, setCentral] = useState<string | null>(null);
  const [leaving, setLeaving] = useState(false);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const write = perms.can("write", "node");
  const close = () => setOpen(null);
  const show = (dialog: NodeDialog, node: Node) => setOpen({ dialog, node });
  const node = open?.node;
  const opened = (dialog: NodeDialog) => (open?.dialog === dialog ? open.node : null);

  /** 그 노드의 비우기 Task가 끝나지 않았는지 */
  const isDraining = (node: Node) =>
    (tasks.get(node.id) ?? []).some((task) => task.spec.op.kind === "node-drain");

  async function setMaintenance(node: Node, maintenance: boolean) {
    // 서버 Task가 없는 쓰기라 먼저 보이고, 실패하면 되돌린다(설계 §5.5).
    const undo = optimistic(queryClient, "node", node.id, (n) => ({
      ...n,
      spec: { ...n.spec, maintenance },
    }));
    try {
      const updated = await call(
        api.PUT("/nodes/{key}", {
          params: { path: { key: node.id }, header: { "If-Match": node.revision } },
          body: { labels: node.labels, spec: { ...node.spec, maintenance } },
        }),
      );
      // watch 이벤트보다 먼저 반영한다(곧바로 다시 바꿔도 새 revision으로 보낸다).
      putResource(queryClient, "node", updated);
      toast.success(
        t(maintenance ? "nodes.maintenanceOn" : "nodes.maintenanceOff", { name: node.name }),
      );
    } catch (err) {
      undo();
      toast.error(t("vms.actionFailed", { name: node.name, error: errorMessage(t, err) }));
    }
  }

  /** 유지보수 켜기·끄기. 끝내면 진행 중인 비우기가 멈추므로 먼저 묻는다. */
  const toggleMaintenance = (node: Node) =>
    node.spec.maintenance && isDraining(node)
      ? show("end", node)
      : void setMaintenance(node, !node.spec.maintenance);

  const drainReason = (node: Node) =>
    !node.status.online
      ? "nodes.drain.reason.offline"
      : node.status.hypervisor === "qemu-hvf"
        ? "nodes.drain.reason.hvf"
        : isDraining(node)
          ? "nodes.drain.reason.draining"
          : null;

  /**
   * 메뉴 항목. `primary`가 거짓이면 머리에 단추로 둔 유지보수·비우기를 뺀다. 쓰기 권한이 없으면 업데이트·인터페이스만
   * 보인다.
   */
  function items(node: Node, { primary = true }: { primary?: boolean } = {}) {
    const witness = node.spec.witness;
    return (
      <>
        {write && primary && (
          <DropdownMenuItem onSelect={() => toggleMaintenance(node)}>
            {t(node.spec.maintenance ? "nodes.endMaintenance" : "nodes.startMaintenance")}
          </DropdownMenuItem>
        )}
        {/* 위트니스는 VM을 돌리지 않는다. */}
        {write && primary && !witness && (
          <ReasonMenuItem reason={drainReason(node)} onSelect={() => show("drain", node)}>
            {t("nodes.drain.menu")}
          </ReasonMenuItem>
        )}
        {node.status.updates && (
          <DropdownMenuItem onSelect={() => show("updates", node)}>
            {t("nodes.updates.menu")}
          </DropdownMenuItem>
        )}
        {!witness && (
          <DropdownMenuItem onSelect={() => show("interfaces", node)}>
            {t("nodes.interfaces.menu")}
          </DropdownMenuItem>
        )}
        {write && !witness && (
          <DropdownMenuItem onSelect={() => show("disks", node)}>
            {t("nodes.disks.menu")}
          </DropdownMenuItem>
        )}
        {write && (
          <DropdownMenuItem onSelect={() => show("labels", node)}>
            {t("nodes.labelsMenu")}
          </DropdownMenuItem>
        )}
        {/* 위트니스는 HA VM을 돌리지 않아 끌 일이 없다. */}
        {write && !witness && (
          <DropdownMenuItem onSelect={() => show("fence", node)}>
            {t("nodes.fence.menu")}
          </DropdownMenuItem>
        )}
        {write && isFrozen(node) && (
          <DropdownMenuItem onSelect={() => show("unfreeze", node)}>
            {t("nodes.unfreeze.menu")}
          </DropdownMenuItem>
        )}
        {write && (
          <>
            <DropdownMenuSeparator />
            <ReasonMenuItem
              variant="destructive"
              // 살아 있는 노드는 그 노드에서 `oplctl cluster leave`로 뺀다(서버도 막는다).
              reason={
                node.id === cluster.data?.node
                  ? "nodes.reason.thisNode"
                  : node.status.online
                    ? "nodes.reason.online"
                    : null
              }
              onSelect={() => show("remove", node)}
            >
              {t("nodes.remove")}
            </ReasonMenuItem>
          </>
        )}
      </>
    );
  }

  const removing = opened("remove");
  const draining = opened("drain");
  const ending = opened("end");
  const unfreezing = opened("unfreeze");
  const dialogs = (
    <>
      {open?.dialog === "updates" && (
        <NodeUpdatesDialog node={open.node} returnFocus={trigger} onClose={close} />
      )}
      {open?.dialog === "interfaces" && (
        <NodeInterfacesDialog node={open.node} returnFocus={trigger} onClose={close} />
      )}
      {open?.dialog === "disks" && (
        <NodeDisksDialog node={open.node} returnFocus={trigger} onClose={close} />
      )}
      {open?.dialog === "fence" && (
        <NodeFenceDialog node={open.node} returnFocus={trigger} onClose={close} />
      )}
      {open?.dialog === "labels" && (
        <LabelsDialog node={open.node} returnFocus={trigger} onClose={close} />
      )}
      <ConfirmDialog
        open={draining !== null}
        onOpenChange={(o) => !o && close()}
        title={t("nodes.drain.title", { name: draining?.name ?? node?.name })}
        description={t("nodes.drain.description")}
        confirmLabel={t("nodes.drain.confirm")}
        returnFocus={trigger}
        onConfirm={async () => {
          const node = draining!;
          try {
            const task = await call(
              api.POST("/nodes/{key}/drain", { params: { path: { key: node.id } } }),
            );
            trackTask(task, t("tasks.op.node-drain", { target: node.name }));
          } catch (err) {
            throw new Error(
              err instanceof ApiError
                ? codeMessage(t, err.code, err.message, "nodes.drain.errors")
                : errorMessage(t, err),
              { cause: err },
            );
          }
        }}
      >
        {/* VM 목록을 다 읽기 전에는 세지 않는다. */}
        {draining && vms.isSuccess && (
          <p className="text-sm">
            {t("nodes.drain.running", { count: runningCount(draining, vms.data) })}
          </p>
        )}
        <p className="text-sm text-muted-foreground">{t("nodes.drain.localNote")}</p>
      </ConfirmDialog>
      <ConfirmDialog
        open={ending !== null}
        onOpenChange={(o) => !o && close()}
        title={t("nodes.drain.endTitle", { name: ending?.name ?? node?.name })}
        description={t("nodes.drain.endDescription")}
        confirmLabel={t("nodes.endMaintenance")}
        returnFocus={trigger}
        onConfirm={() => setMaintenance(ending!, false)}
      />
      <ConfirmDialog
        open={unfreezing !== null}
        onOpenChange={(o) => !o && close()}
        title={t("nodes.unfreeze.title", { name: unfreezing?.name ?? node?.name })}
        description={t("nodes.unfreeze.description")}
        confirmLabel={t("nodes.unfreeze.confirm")}
        returnFocus={trigger}
        onConfirm={async () => {
          const node = unfreezing!;
          try {
            await call(api.POST("/nodes/{key}/unfreeze", { params: { path: { key: node.id } } }));
          } catch (err) {
            throw new Error(
              err instanceof ApiError
                ? codeMessage(t, err.code, err.message, "nodes.unfreeze.errors")
                : errorMessage(t, err),
              { cause: err },
            );
          }
          toast.success(t("nodes.unfreeze.done", { name: node.name }));
        }}
      />
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(o) => {
          if (!o) {
            setCentral(null);
            close();
          }
        }}
        title={t("nodes.removeTitle", { name: removing?.name ?? node?.name })}
        description={t("nodes.removeDescription")}
        confirmLabel={t("nodes.remove")}
        typeToConfirm={removing?.name}
        impact={
          removing && vms.isSuccess
            ? [t("impact.vmsAssigned", { count: vmCount(removing, vms.data) })]
            : undefined
        }
        returnFocus={trigger}
        onConfirm={async () => {
          const node = removing!;
          let removed;
          try {
            removed = await call(
              api.DELETE("/nodes/{key}", { params: { path: { key: node.id } } }),
            );
          } catch (err) {
            if (err instanceof ApiError && err.code === "sdn_central_on_node") {
              setCentral(node.id);
              throw new Error(t("nodes.sdnCentral", { name: node.name }), { cause: err });
            }
            throw err;
          }
          toast.success(t("nodes.removed", { name: node.name }));
          // 손으로 할 일(예: Ceph 데몬 노드였으면 Ceph에서 치우기)
          for (const warning of removed?.warnings ?? []) {
            toast.warning(warning, { duration: Infinity });
          }
          onRemoved?.(node);
        }}
      >
        {removing && central === removing.id && perms.can("write", "sdn") && (
          <div>
            <Button
              variant="outline"
              size="sm"
              disabled={leaving}
              onClick={() => {
                const node = removing;
                setLeaving(true);
                void callWarned(
                  api.DELETE("/sdn/centrals/{node}", { params: { path: { node: node.id } } }),
                )
                  .then(({ data, warning }) => {
                    trackTask(data, t("tasks.op.sdn-central-remove", { node: node.name }));
                    if (warning) {
                      toast.warning(warning);
                    }
                    setCentral(null);
                    close();
                  })
                  .catch((err: unknown) => toast.error(errorMessage(t, err)))
                  .finally(() => setLeaving(false));
              }}
            >
              {t("nodes.sdnCentralRemove")}
            </Button>
          </div>
        )}
      </ConfirmDialog>
    </>
  );

  return {
    write,
    items,
    dialogs,
    trigger,
    /** 메뉴를 연 단추를 기억한다(`onPointerDown`·`onKeyDown`). */
    rememberTrigger: (e: React.SyntheticEvent<HTMLButtonElement>) => {
      trigger.current = e.currentTarget;
    },
    show,
    toggleMaintenance,
    drainReason,
  };
}

/** 노드 레이블 고치기. 열 때만 그린다. 배치 규칙의 `node_tag`(예: `zone=rack-a`)가 이 레이블을 본다. */
function LabelsDialog({
  node,
  returnFocus,
  onClose,
}: {
  node: Node;
  returnFocus: React.RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [text, setText] = useState(() => labelText(node.labels));
  const [error, setError] = useState<string | undefined>();
  const parsed = parseLabels(text);
  const unchanged = parsed !== null && labelText(parsed) === labelText(node.labels);
  /** 저장할 때의 실시간 목록에 있는 노드(노드 status가 자주 바뀌어 revision이 오른다) */
  const latest = () =>
    queryClient.getQueryData<List<Node>>(listKey("node"))?.items.find((n) => n.id === node.id) ??
    node;

  /** 레이블을 쓴다. 먼저 목록에 보이고 실패하면 되돌린다. */
  async function save(current: Node, labels: Record<string, string>) {
    const undo = optimistic(queryClient, "node", node.id, (n) => ({ ...n, labels }));
    try {
      const updated = await call(
        api.PUT("/nodes/{key}", {
          params: { path: { key: node.id }, header: { "If-Match": current.revision } },
          body: { labels, spec: current.spec },
        }),
      );
      putResource(queryClient, "node", updated);
    } catch (err) {
      undo();
      throw err;
    }
  }

  async function submit() {
    if (parsed === null) {
      setError("nodes.errors.labels");
      return false;
    }
    const current = latest();
    // 연 사이 다른 곳에서 레이블이 바뀌었으면 덮지 않는다.
    if (labelText(current.labels) !== labelText(node.labels)) {
      setError("nodes.errors.labelsChanged");
      return false;
    }
    const before = current.labels ?? {};
    await save(current, parsed);
    // 10초 동안 이전 레이블로 되돌린다. 그 사이 다른 곳에서 바꿨으면 덮지 않는다.
    undoToast(t, t("nodes.labelsSaved", { name: node.name }), async () => {
      const now = latest();
      if (labelText(now.labels) !== labelText(parsed)) {
        toast.error(t("nodes.errors.labelsChanged"));
        return;
      }
      try {
        await save(now, before);
        toast.success(t("common.undone"));
      } catch (err) {
        toast.error(t("vms.actionFailed", { name: node.name, error: errorMessage(t, err) }));
      }
    });
  }

  return (
    <FormDialog
      title={t("nodes.labelsTitle", { name: node.name })}
      description={t("nodes.labelsDescription")}
      submitLabel={t("common.save")}
      submitDisabled={unchanged}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="node-labels" label={t("nodes.labels")} error={error} hint={t("nodes.labelsHint")}>
        {(a11y) => (
          <Input
            id="node-labels"
            placeholder="zone=rack-a, ssd=true"
            value={text}
            onChange={(e) => setText(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}
