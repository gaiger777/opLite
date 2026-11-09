import { useQuery, useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import { applyEvent, listKey, useList, type List } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { codeMessage, errorMessage } from "@/i18n/errors";
import { formatCapacity } from "@/lib/format";
import { NodeUpdatesDialog } from "@/nodes/NodeUpdatesDialog";
import { PoolStatus } from "@/storage/PoolStatus";
import { trackTask } from "@/tasks/tasks";

/** 그 노드에 고정했거나 그 노드에서 마지막으로 돈 VM의 수 */
function vmCount(node: Node, vms: Vm[]): number {
  return vms.filter((vm) => (vm.spec.node ?? vm.status.node) === node.id).length;
}

/** 비우기가 옮길 VM의 수: 그 노드에 고정돼 켜 둔 VM(서버의 비우기 Task와 같은 기준) */
function runningCount(node: Node, vms: Vm[]): number {
  return vms.filter((vm) => vm.spec.node === node.id && vm.spec.power === "running").length;
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

  return (
    <div className="flex flex-col gap-4">
      <h1 id="nodes-title" className="text-2xl font-semibold">
        {t("nav.nodes")}
      </h1>
      <p className="text-sm text-muted-foreground">{t("nodes.joinNote")}</p>
      <Table aria-labelledby="nodes-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("nodes.address")}</TableHead>
            <TableHead>{t("nodes.version")}</TableHead>
            <TableHead>{t("nodes.updates.column")}</TableHead>
            <TableHead>{t("nodes.underlay")}</TableHead>
            <TableHead>{t("nodes.hypervisor")}</TableHead>
            <TableHead>{t("nodes.arch")}</TableHead>
            <TableHead>{t("nodes.fencing")}</TableHead>
            <TableHead>{t("nodes.cpus")}</TableHead>
            <TableHead>{t("nodes.memory")}</TableHead>
            <TableHead>{t("nodes.bridges")}</TableHead>
            <TableHead>{t("nodes.pools")}</TableHead>
            <TableHead>{t("nodes.vms")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(nodes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 15 : 14} query={nodes} empty={t("dashboard.noNodes")} />
          )}
          {(nodes.data ?? []).map((node) => (
            <TableRow key={node.id}>
              <TableCell className="font-medium">
                {node.name}
                {node.id === cluster.data?.node && (
                  <span className="ml-2 text-xs text-muted-foreground">{t("nodes.thisNode")}</span>
                )}
              </TableCell>
              <TableCell>
                <div className="flex flex-wrap gap-1">
                  <Badge variant={node.status.online ? "default" : "destructive"}>
                    {t(node.status.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
                  </Badge>
                  {node.spec.maintenance && (
                    <Badge variant="outline">{t("dashboard.maintenance")}</Badge>
                  )}
                  {node.spec.witness && <Badge variant="outline">{t("dashboard.witness")}</Badge>}
                </div>
              </TableCell>
              <TableCell className="font-mono text-xs">{node.spec.address}</TableCell>
              {/* 옛 버전으로 등록한 노드는 모른다. */}
              <TableCell className="font-mono text-xs">{node.status.version ?? "—"}</TableCell>
              {/* 업데이트를 확인하지 않는 노드(위트니스, 꺼 둔 노드)는 `updates`가 없다. */}
              <TableCell>
                {node.status.updates ? (
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
                )}
              </TableCell>
              {/* VXLAN 패킷을 주고받는 주소. 위트니스와 정하지 못한 노드는 없다. */}
              <TableCell className="font-mono text-xs">{node.status.underlay ?? "—"}</TableCell>
              <TableCell>{node.status.hypervisor}</TableCell>
              {/* 옛 버전으로 등록한 노드는 모른다. */}
              <TableCell>{node.status.arch ?? "—"}</TableCell>
              {/* 펜싱이 꺼진 노드(위트니스 포함)는 HA VM을 돌리지 않는다. */}
              <TableCell>
                {node.status.ha ? t(`nodes.fencingMode.${node.status.ha.fencing}`) : "—"}
              </TableCell>
              <TableCell>{node.status.cpus}</TableCell>
              <TableCell>{formatCapacity(node.status.memory_mib)}</TableCell>
              <TableCell className="font-mono text-xs">
                {node.status.bridges.map((b) => b.name).join(", ") || "—"}
              </TableCell>
              <TableCell>
                {Object.keys(node.status.pools).length === 0
                  ? "—"
                  : Object.entries(node.status.pools)
                      .sort(([a], [b]) => a.localeCompare(b))
                      .map(([name, state]) => (
                        <PoolStatus
                          key={name}
                          name={name}
                          state={state}
                          offline={!node.status.online}
                        />
                      ))}
              </TableCell>
              {/* VM 목록을 다 읽기 전에는 모른다. */}
              <TableCell>{vms.isSuccess ? vmCount(node, vms.data) : "—"}</TableCell>
              {write && (
                <TableCell className="text-right">
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
                        {t(
                          node.spec.maintenance ? "nodes.endMaintenance" : "nodes.startMaintenance",
                        )}
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
                </TableCell>
              )}
            </TableRow>
          ))}
        </TableBody>
      </Table>
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
          await call(api.DELETE("/nodes/{key}", { params: { path: { key: node.id } } }));
          toast.success(t("nodes.removed", { name: node.name }));
        }}
      />
    </div>
  );
}
