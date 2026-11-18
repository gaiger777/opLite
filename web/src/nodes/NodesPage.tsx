import { useQuery, useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import { applyEvent, listKey, useList, type List } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { parseLabels } from "@/backups/jobs";
import { cephRoles, useCeph } from "@/ceph/ceph";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { type Column, DataTable } from "@/components/data-table/DataTable";
import { useTableState } from "@/components/data-table/state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { codeMessage, errorMessage } from "@/i18n/errors";
import { formatCapacity } from "@/lib/format";
import { failingChecks } from "@/ha-checks/checks";
import { NodeDisksDialog } from "@/nodes/NodeDisksDialog";
import { NodeFenceDialog } from "@/nodes/NodeFenceDialog";
import { NodeInterfacesDialog } from "@/nodes/NodeInterfacesDialog";
import { NodePeek } from "@/nodes/NodePeek";
import { NodeUpdatesDialog } from "@/nodes/NodeUpdatesDialog";
import { PoolStatus } from "@/storage/PoolStatus";
import { trackTask } from "@/tasks/tasks";
import { replicasOn } from "@/vms/replication";

/** 그 노드에 고정했거나 그 노드에서 마지막으로 돈 VM의 수 */
function vmCount(node: Node, vms: Vm[]): number {
  return vms.filter((vm) => (vm.spec.node ?? vm.status.node) === node.id).length;
}

/** 비우기가 옮길 VM의 수: 그 노드에 고정돼 켜 둔 VM(서버의 비우기 Task와 같은 기준) */
function runningCount(node: Node, vms: Vm[]): number {
  return vms.filter((vm) => vm.spec.node === node.id && vm.spec.power === "running").length;
}

/** 서버의 `NodeStatus::frozen`: 종료 정책 `freeze`로 꺼진 채 돌아오지 않은 노드. 리더가 HA VM을 옮기지 않는다. */
function isFrozen(node: Node): boolean {
  return !node.status.online && node.status.shutdown?.action === "freeze";
}

/** 그 노드가 복제본을 둔 VM의 수. 누르면 VM 이름과 복제 상태를 펼친다. */
function ReplicasCell({ node, vms }: { node: Node; vms: Vm[] }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const replicas = replicasOn(node.id, vms);
  if (replicas.length === 0) {
    return <>0</>;
  }
  const list = `node-${node.id}-replicas`;
  return (
    <>
      <Button
        variant="ghost"
        size="sm"
        aria-expanded={open}
        aria-controls={list}
        aria-label={t("nodes.replicasOpen", { name: node.name, count: replicas.length })}
        onClick={() => setOpen(!open)}
      >
        {replicas.length}
      </Button>
      {open && (
        <ul id={list} className="text-xs">
          {replicas.map(({ vm, state }) => (
            <li key={vm.id}>
              {vm.name} · {t(`vms.replication.state.${state}`)}
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

/** 레이블을 `KEY=VALUE, …`로(키 순서) */
function labelText(labels: Node["labels"]): string {
  return Object.entries(labels ?? {})
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([k, v]) => `${k}=${v}`)
    .join(", ");
}

/** 배지 버튼의 접근 이름에 넣는 확인 상태 */
function updatesState(
  t: ReturnType<typeof useTranslation>["t"],
  updates: NonNullable<Node["status"]["updates"]>,
): string {
  if (updates.error) {
    return t("nodes.updates.stateFailed");
  }
  if (!updates.checked_at) {
    return t("nodes.updates.stateNotChecked");
  }
  return t("nodes.updates.stateCount", {
    n: `${updates.packages.length}${updates.truncated ? "+" : ""}`,
  });
}

/** 올릴 수 있는 패키지 수. 확인이 실패했으면 경고, 아직 확인하지 않았으면 비어 있다. */
function UpdatesBadge({ updates }: { updates: NonNullable<Node["status"]["updates"]> }) {
  const { t } = useTranslation();
  if (updates.error) {
    return <Badge variant="destructive">{t("nodes.updates.failedBadge")}</Badge>;
  }
  if (!updates.checked_at) {
    return <span className="text-muted-foreground">—</span>;
  }
  const count = `${updates.packages.length}${updates.truncated ? "+" : ""}`;
  return <Badge variant={updates.packages.length > 0 ? "default" : "outline"}>{count}</Badge>;
}

/** 노드의 HA 검사 결과 배지: 실패한 검사 수, 아니면 degraded나 정상. 제목에 실패·degraded 검사의 이름과 오류 */
function ChecksBadge({ node }: { node: Node }) {
  const { t } = useTranslation();
  const checks = node.status.checks ?? [];
  const failing = failingChecks(node);
  const degraded = checks.filter((c) => c.state === "degraded");
  const title = [...failing, ...degraded]
    .map((c) => (c.error ? `${c.name}: ${c.error}` : c.name))
    .join("\n");
  if (failing.length > 0) {
    return (
      <Badge variant="destructive" title={title}>
        {t("nodes.checksFailing", { count: failing.length })}
      </Badge>
    );
  }
  if (degraded.length > 0) {
    return (
      <Badge variant="secondary" title={title}>
        {t("nodes.checkState.degraded")}
      </Badge>
    );
  }
  return <Badge variant="outline">{t("nodes.checkState.ok")}</Badge>;
}

/** 배지 버튼의 접근 이름에 넣는 검사 상태 */
function checksState(t: ReturnType<typeof useTranslation>["t"], node: Node): string {
  const checks = node.status.checks ?? [];
  const failing = failingChecks(node);
  if (failing.length > 0) {
    return t("nodes.checksFailing", { count: failing.length });
  }
  if (checks.length === 0) {
    return t("nodes.checksNone");
  }
  return t(
    checks.some((c) => c.state === "degraded")
      ? "nodes.checkState.degraded"
      : "nodes.checkState.ok",
  );
}

export function NodesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const vms = useList("vm");
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const [removing, setRemoving] = useState<Node | null>(null);
  const [showingUpdates, setShowingUpdates] = useState<Node | null>(null);
  const [draining, setDraining] = useState<Node | null>(null);
  const [ending, setEnding] = useState<Node | null>(null);
  const [labeling, setLabeling] = useState<Node | null>(null);
  const [unfreezing, setUnfreezing] = useState<Node | null>(null);
  const [fencing, setFencing] = useState<Node | null>(null);
  const [showingInterfaces, setShowingInterfaces] = useState<Node | null>(null);
  const [showingDisks, setShowingDisks] = useState<Node | null>(null);
  const ceph = useCeph();
  const tasks = useTaskIndex();
  /** 그 노드의 비우기 Task가 끝나지 않았는지 */
  const isDraining = (node: Node) =>
    (tasks.get(node.id) ?? []).some((task) => task.spec.op.kind === "node-drain");
  // 메뉴에서 연 대화상자를 닫으면 메뉴 항목이 사라지므로, 메뉴를 연 버튼으로 포커스를 돌린다.
  const trigger = useRef<HTMLButtonElement | null>(null);
  const rememberTrigger = (e: React.SyntheticEvent<HTMLButtonElement>) => {
    trigger.current = e.currentTarget;
  };
  const write = can(me.role, "write", "node");
  const state = useTableState("nodes");

  async function setMaintenance(node: Node, maintenance: boolean) {
    try {
      const updated = await call(
        api.PUT("/nodes/{key}", {
          params: { path: { key: node.id }, header: { "If-Match": node.revision } },
          body: { labels: node.labels, spec: { ...node.spec, maintenance } },
        }),
      );
      // watch 이벤트보다 먼저 반영한다(곧바로 다시 바꿔도 새 revision으로 보낸다).
      queryClient.setQueryData<List<Node>>(
        listKey("node"),
        (list) => list && applyEvent(list, { type: "put", kind: "node", resource: updated }),
      );
      toast.success(
        t(maintenance ? "nodes.maintenanceOn" : "nodes.maintenanceOff", { name: node.name }),
      );
    } catch (err) {
      toast.error(t("vms.actionFailed", { name: node.name, error: errorMessage(t, err) }));
    }
  }

  const columns: Column<Node>[] = [
    {
      id: "name",
      header: t("common.name"),
      value: (node) => node.name,
      className: "font-medium",
      cell: (node) => (
        <>
          <Link
            to={`/nodes/${encodeURIComponent(node.name)}`}
            className="underline-offset-4 hover:underline"
          >
            {node.name}
          </Link>
          {node.id === cluster.data?.node && (
            <span className="ml-2 text-xs text-muted-foreground">{t("nodes.thisNode")}</span>
          )}
        </>
      ),
    },
    {
      id: "state",
      header: t("common.state"),
      value: (node) => (node.status.online ? 1 : 0),
      cell: (node) => (
        <div className="flex flex-wrap gap-1">
          <Badge variant={node.status.online ? "default" : "destructive"}>
            {t(node.status.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
          </Badge>
          {node.spec.maintenance && <Badge variant="outline">{t("dashboard.maintenance")}</Badge>}
          {node.spec.witness && <Badge variant="outline">{t("dashboard.witness")}</Badge>}
          {/* 리더는 얼린 노드의 HA VM을 옮기지 않는다(돌아오면 그 노드에서 켠다). */}
          {isFrozen(node) && <Badge variant="secondary">{t("nodes.frozen")}</Badge>}
        </div>
      ),
    },
    {
      id: "address",
      header: t("nodes.address"),
      value: (node) => node.spec.address,
      className: "font-mono text-xs",
      priority: 2,
      cell: (node) => node.spec.address,
    },
    {
      id: "version",
      header: t("nodes.version"),
      className: "font-mono text-xs",
      priority: 3,
      // 옛 버전으로 등록한 노드는 모른다.
      cell: (node) => node.status.version ?? "—",
    },
    {
      id: "updates",
      header: t("nodes.updates.column"),
      priority: 3,
      // 업데이트를 확인하지 않는 노드(위트니스, 꺼 둔 노드)는 `updates`가 없다.
      cell: (node) =>
        node.status.updates ? (
          <Button
            variant="ghost"
            size="sm"
            aria-label={t("nodes.updates.open", {
              name: node.name,
              state: updatesState(t, node.status.updates),
            })}
            onPointerDown={rememberTrigger}
            onKeyDown={rememberTrigger}
            onClick={() => setShowingUpdates(node)}
          >
            <UpdatesBadge updates={node.status.updates} />
          </Button>
        ) : (
          "—"
        ),
    },
    {
      id: "underlay",
      header: t("nodes.underlay"),
      className: "font-mono text-xs",
      priority: 3,
      // VXLAN 패킷을 주고받는 주소. 위트니스와 정하지 못한 노드는 없다.
      cell: (node) => node.status.underlay ?? "—",
    },
    {
      id: "hypervisor",
      header: t("nodes.hypervisor"),
      priority: 3,
      cell: (node) => (
        <>
          {node.status.hypervisor}
          {/* 실행 중 vCPU·메모리 핫플러그. 디스크·NIC는 이 기록이 있는 모든 노드가 한다. 옛 노드·위트니스는 없다. */}
          {node.status.hotplug && (
            <span className="block text-xs text-muted-foreground">
              {t("nodes.hotplug", {
                cpu: t(node.status.hotplug.cpu ? "nodes.yes" : "nodes.no"),
                memory: t(node.status.hotplug.memory ? "nodes.yes" : "nodes.no"),
              })}
            </span>
          )}
        </>
      ),
    },
    {
      id: "arch",
      header: t("nodes.arch"),
      priority: 3,
      // 옛 버전으로 등록한 노드는 모른다.
      cell: (node) => node.status.arch ?? "—",
    },
    {
      id: "firmware",
      header: t("nodes.firmware"),
      priority: 3,
      // TPM·Secure Boot 능력. 옛 노드는 알리지 않고(모름), 위트니스는 VM을 돌리지 않는다.
      cell: (node) =>
        node.spec.witness ? (
          "—"
        ) : node.status.firmware_caps ? (
          t("nodes.firmwareCaps", {
            tpm: t(node.status.firmware_caps.tpm ? "nodes.yes" : "nodes.no"),
            secureBoot: t(node.status.firmware_caps.secure_boot ? "nodes.yes" : "nodes.no"),
          })
        ) : (
          <span className="text-muted-foreground">{t("nodes.firmwareUnknown")}</span>
        ),
    },
    {
      id: "fencing",
      header: t("nodes.fencing"),
      priority: 3,
      // 펜싱이 꺼진 노드(위트니스 포함)는 HA VM을 돌리지 않는다.
      cell: (node) => (
        <>
          {node.status.ha ? (
            <>
              {t(`nodes.fencingMode.${node.status.ha.fencing}`)}
              {/* `[ha] profile`이 정한, 리더가 이 노드의 HA VM을 옮기기까지의 시간 */}
              <span className="block text-xs text-muted-foreground">
                {t("nodes.recoverAfter", { secs: node.status.ha.recover_after_secs })}
              </span>
            </>
          ) : (
            "—"
          )}
          {/* 이웃 노드가 이 노드의 BMC·명령으로 전원을 읽지 못했다. */}
          {node.status.fence_check?.error && (
            <Badge variant="destructive" title={node.status.fence_check.error}>
              {t("nodes.fenceUnreachable")}
            </Badge>
          )}
        </>
      ),
    },
    {
      id: "checks",
      header: t("nodes.checks"),
      priority: 2,
      // HA 검사를 하지 않는 노드(위트니스, 옛 버전)는 결과도 인터페이스도 없다.
      cell: (node) =>
        (node.status.checks ?? []).length > 0 || (node.status.interfaces ?? []).length > 0 ? (
          <Button
            variant="ghost"
            size="sm"
            aria-label={t("nodes.interfaces.open", {
              name: node.name,
              state: checksState(t, node),
            })}
            onPointerDown={rememberTrigger}
            onKeyDown={rememberTrigger}
            onClick={() => setShowingInterfaces(node)}
          >
            {(node.status.checks ?? []).length > 0 ? <ChecksBadge node={node} /> : "—"}
          </Button>
        ) : (
          "—"
        ),
    },
    {
      id: "cpus",
      header: t("nodes.cpus"),
      value: (node) => node.status.cpus,
      priority: 2,
      cell: (node) => node.status.cpus,
    },
    {
      id: "memory",
      header: t("nodes.memory"),
      value: (node) => node.status.memory_mib,
      priority: 2,
      cell: (node) => formatCapacity(node.status.memory_mib),
    },
    {
      id: "bridges",
      header: t("nodes.bridges"),
      className: "font-mono text-xs",
      priority: 3,
      cell: (node) => node.status.bridges.map((b) => b.name).join(", ") || "—",
    },
    {
      id: "pools",
      header: t("nodes.pools"),
      priority: 3,
      cell: (node) =>
        Object.keys(node.status.pools).length === 0
          ? "—"
          : Object.entries(node.status.pools)
              .sort(([a], [b]) => a.localeCompare(b))
              .map(([name, state]) => (
                <PoolStatus key={name} name={name} state={state} offline={!node.status.online} />
              )),
    },
    {
      id: "labels",
      header: t("nodes.labels"),
      className: "font-mono text-xs",
      priority: 3,
      // 배치 규칙의 노드 태그(`zone=rack-a`)가 보는 레이블
      cell: (node) => labelText(node.labels) || "—",
    },
    {
      id: "vms",
      header: t("nodes.vms"),
      value: (node) => (vms.isSuccess ? vmCount(node, vms.data) : -1),
      // VM 목록을 다 읽기 전에는 모른다.
      cell: (node) => (vms.isSuccess ? vmCount(node, vms.data) : "—"),
    },
    {
      id: "replicas",
      header: t("nodes.replicas"),
      priority: 3,
      // 그 노드가 대상인 복제(VM 목록으로 센다)
      cell: (node) => (vms.isSuccess ? <ReplicasCell node={node} vms={vms.data} /> : "—"),
    },
    {
      id: "ceph",
      header: t("nodes.ceph"),
      className: "font-mono text-xs",
      priority: 3,
      // 관리 Ceph에서 이 노드가 맡은 데몬(mon·mgr·osd)
      cell: (node) => cephRoles(ceph.data, node.id).join(", ") || "—",
    },
  ];
  if (write) {
    columns.push({
      id: "menu",
      header: t("vms.actions"),
      actions: true,
      cell: (node) => (
        <DropdownMenu modal={false}>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t("vms.actionsFor", { name: node.name })}
              onPointerDown={rememberTrigger}
              onKeyDown={rememberTrigger}
            >
              <MoreHorizontal />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem
              onSelect={() =>
                // 유지보수를 끝내면 진행 중인 비우기가 멈추므로 먼저 묻는다.
                node.spec.maintenance && isDraining(node)
                  ? setEnding(node)
                  : void setMaintenance(node, !node.spec.maintenance)
              }
            >
              {t(node.spec.maintenance ? "nodes.endMaintenance" : "nodes.startMaintenance")}
            </DropdownMenuItem>
            {/* 위트니스는 VM을 돌리지 않는다. */}
            {!node.spec.witness && (
              <ReasonMenuItem
                reason={
                  !node.status.online
                    ? "nodes.drain.reason.offline"
                    : node.status.hypervisor === "qemu-hvf"
                      ? "nodes.drain.reason.hvf"
                      : isDraining(node)
                        ? "nodes.drain.reason.draining"
                        : null
                }
                onSelect={() => setDraining(node)}
              >
                {t("nodes.drain.menu")}
              </ReasonMenuItem>
            )}
            {node.status.updates && (
              <DropdownMenuItem onSelect={() => setShowingUpdates(node)}>
                {t("nodes.updates.menu")}
              </DropdownMenuItem>
            )}
            {!node.spec.witness && (
              <DropdownMenuItem onSelect={() => setShowingInterfaces(node)}>
                {t("nodes.interfaces.menu")}
              </DropdownMenuItem>
            )}
            {!node.spec.witness && (
              <DropdownMenuItem onSelect={() => setShowingDisks(node)}>
                {t("nodes.disks.menu")}
              </DropdownMenuItem>
            )}
            <DropdownMenuItem onSelect={() => setLabeling(node)}>
              {t("nodes.labelsMenu")}
            </DropdownMenuItem>
            {/* 위트니스는 HA VM을 돌리지 않아 끌 일이 없다. */}
            {!node.spec.witness && (
              <DropdownMenuItem onSelect={() => setFencing(node)}>
                {t("nodes.fence.menu")}
              </DropdownMenuItem>
            )}
            {isFrozen(node) && (
              <DropdownMenuItem onSelect={() => setUnfreezing(node)}>
                {t("nodes.unfreeze.menu")}
              </DropdownMenuItem>
            )}
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
              onSelect={() => setRemoving(node)}
            >
              {t("nodes.remove")}
            </ReasonMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      ),
    });
  }

  return (
    <div className="flex flex-col gap-4">
      <h1 id="nodes-title" className="text-xl font-semibold">
        {t("nav.nodes")}
      </h1>
      <p className="text-sm text-muted-foreground">{t("nodes.joinNote")}</p>
      <DataTable
        id="nodes"
        rows={nodes.data ?? []}
        columns={columns}
        rowId={(node) => node.id}
        rowName={(node) => node.name}
        query={nodes}
        empty={t("dashboard.noNodes")}
        labelledBy="nodes-title"
        state={state}
        search={(node) => `${node.name} ${node.spec.address}`}
        rowHref={(node) => `/nodes/${encodeURIComponent(node.name)}`}
        peek={(node) => (
          <NodePeek node={node} vms={vms.isSuccess ? vmCount(node, vms.data) : undefined} />
        )}
        keys
      />
      {write && (
        <>
          <p className="text-sm text-muted-foreground">{t("nodes.maintenanceNote")}</p>
          <p className="text-sm text-muted-foreground">{t("nodes.removeNote")}</p>
        </>
      )}
      {showingUpdates && (
        <NodeUpdatesDialog
          node={showingUpdates}
          returnFocus={trigger}
          onClose={() => setShowingUpdates(null)}
        />
      )}
      <ConfirmDialog
        open={draining !== null}
        onOpenChange={(open) => !open && setDraining(null)}
        title={t("nodes.drain.title", { name: draining?.name })}
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
        onOpenChange={(open) => !open && setEnding(null)}
        title={t("nodes.drain.endTitle", { name: ending?.name })}
        description={t("nodes.drain.endDescription")}
        confirmLabel={t("nodes.endMaintenance")}
        returnFocus={trigger}
        onConfirm={() => setMaintenance(ending!, false)}
      />
      {showingInterfaces && (
        <NodeInterfacesDialog
          node={showingInterfaces}
          returnFocus={trigger}
          onClose={() => setShowingInterfaces(null)}
        />
      )}
      {showingDisks && (
        <NodeDisksDialog
          node={showingDisks}
          returnFocus={trigger}
          onClose={() => setShowingDisks(null)}
        />
      )}
      {fencing && (
        <NodeFenceDialog node={fencing} returnFocus={trigger} onClose={() => setFencing(null)} />
      )}
      {labeling && (
        <LabelsDialog node={labeling} returnFocus={trigger} onClose={() => setLabeling(null)} />
      )}
      <ConfirmDialog
        open={unfreezing !== null}
        onOpenChange={(open) => !open && setUnfreezing(null)}
        title={t("nodes.unfreeze.title", { name: unfreezing?.name })}
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
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("nodes.removeTitle", { name: removing?.name })}
        description={t("nodes.removeDescription")}
        confirmLabel={t("nodes.remove")}
        typeToConfirm={removing?.name}
        returnFocus={trigger}
        onConfirm={async () => {
          const node = removing!;
          const removed = await call(
            api.DELETE("/nodes/{key}", { params: { path: { key: node.id } } }),
          );
          toast.success(t("nodes.removed", { name: node.name }));
          // 손으로 할 일(예: Ceph 데몬 노드였으면 Ceph에서 치우기)
          for (const warning of removed?.warnings ?? []) {
            toast.warning(warning, { duration: Infinity });
          }
        }}
      />
    </div>
  );
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

  async function submit() {
    if (parsed === null) {
      setError("nodes.errors.labels");
      return false;
    }
    // 노드 status가 자주 바뀌어 revision이 오르므로 연 때가 아니라 저장할 때의 실시간 목록에 있는 노드로 쓴다.
    const current =
      queryClient.getQueryData<List<Node>>(listKey("node"))?.items.find((n) => n.id === node.id) ??
      node;
    // 연 사이 다른 곳에서 레이블이 바뀌었으면 덮지 않는다.
    if (labelText(current.labels) !== labelText(node.labels)) {
      setError("nodes.errors.labelsChanged");
      return false;
    }
    const updated = await call(
      api.PUT("/nodes/{key}", {
        params: { path: { key: node.id }, header: { "If-Match": current.revision } },
        body: { labels: parsed, spec: current.spec },
      }),
    );
    queryClient.setQueryData<List<Node>>(
      listKey("node"),
      (list) => list && applyEvent(list, { type: "put", kind: "node", resource: updated }),
    );
    toast.success(t("nodes.labelsSaved", { name: node.name }));
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
