import { useQuery } from "@tanstack/react-query";
import { MoreHorizontal } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Node, Vm } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { cephRoles, useCeph } from "@/ceph/ceph";
import { type Column, DataTable } from "@/components/data-table/DataTable";
import { useTableState } from "@/components/data-table/state";
import { PageHeader } from "@/components/PageHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { formatCapacity, formatMib } from "@/lib/format";
import { failingChecks } from "@/ha-checks/checks";
import { isFrozen, labelText, useNodeActions, vmCount } from "@/nodes/NodeActions";
import { NodeInterfacesDialog } from "@/nodes/NodeInterfacesDialog";
import { NodePeek } from "@/nodes/NodePeek";
import { NodeUpdatesDialog } from "@/nodes/NodeUpdatesDialog";
import { NodeSdnCell } from "@/sdn/chips";
import { strayNodes, useSdn } from "@/sdn/sdn";
import { PoolStatus } from "@/storage/PoolStatus";
import { TaskErrorText } from "@/tasks/TaskErrorText";
import { useTopologyUsage } from "@/numa/numa";
import { replicasOn } from "@/vms/replication";

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
  const nodes = useList("node");
  const vms = useList("vm");
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const ceph = useCeph();
  const perms = usePermissions();
  // 전용 CPU·hugepages 열(계획 64)은 한 번 읽는다(루트 `cluster.read`).
  const usage = useTopologyUsage(perms.has("cluster.read"));
  const usageOf = (node: Node) => usage.data?.nodes.find((n) => n.node === node.name);
  // 중앙 역할을 알린 노드가 있을 때만 `GET /sdn`으로 남은 중앙 DB인지 본다(리뷰 2 웹 I-3).
  const sdn = useSdn(false, perms.reads("sdn") && !!nodes.data?.some((n) => n.status.sdn?.central));
  const strays = strayNodes(sdn.data);
  const actions = useNodeActions();
  const [showingUpdates, setShowingUpdates] = useState<Node | null>(null);
  const [showingInterfaces, setShowingInterfaces] = useState<Node | null>(null);
  // 셀 단추에서 연 대화상자를 닫으면 그 단추로 포커스를 돌린다(메뉴는 `actions.trigger`).
  const trigger = actions.trigger;
  const rememberTrigger = actions.rememberTrigger;
  const state = useTableState("nodes");

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
      priority: 4,
      // VXLAN 패킷을 주고받는 주소. 위트니스와 정하지 못한 노드는 없다.
      cell: (node) => node.status.underlay ?? "—",
    },
    {
      id: "hypervisor",
      header: t("nodes.hypervisor"),
      priority: 4,
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
      priority: 4,
      // 옛 버전으로 등록한 노드는 모른다.
      cell: (node) => node.status.arch ?? "—",
    },
    {
      id: "firmware",
      header: t("nodes.firmware"),
      priority: 4,
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
      priority: 4,
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
            <>
              <Badge variant="destructive">{t("nodes.fenceUnreachable")}</Badge>
              {/* 읽지 못한 까닭: 마우스로만 보이는 title 대신 글로(계획 77-1 리뷰 I5) */}
              <TaskErrorText
                text={node.status.fence_check.error}
                subject={node.name}
                className="mt-0.5"
              />
            </>
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
      id: "dedicated",
      header: t("numa.columns.dedicated"),
      priority: 4,
      value: (node) => usageOf(node)?.dedicated_used ?? -1,
      // 고정할 수 없거나(macOS·위트니스·옛 노드) 전용 CPU가 없는 노드는 비어 있다.
      cell: (node) => {
        const u = usageOf(node);
        return u && u.dedicated_total > 0
          ? t("numa.node.usedOf", { used: u.dedicated_used, total: u.dedicated_total })
          : "—";
      },
    },
    {
      id: "hugepages",
      header: t("numa.columns.hugepages"),
      priority: 4,
      cell: (node) => {
        const pools = usageOf(node)?.hugepages ?? [];
        return pools.length === 0
          ? "—"
          : pools.map((p) => (
              <span key={p.size} className="block">
                {t("numa.columns.pages", {
                  size: p.size,
                  used: formatMib(p.used_mib),
                  total: formatMib(p.total_mib),
                })}
              </span>
            ));
      },
    },
    {
      id: "sdn",
      header: t("nodes.sdn"),
      priority: 4,
      // OVN chassis 상태와 중앙이면 NB Raft 역할(계획 65). SDN을 세우지 않은 노드는 없다.
      cell: (node) => <NodeSdnCell sdn={node.status.sdn} stray={strays.has(node.id)} />,
    },
    {
      id: "bridges",
      header: t("nodes.bridges"),
      className: "font-mono text-xs",
      priority: 4,
      cell: (node) => node.status.bridges.map((b) => b.name).join(", ") || "—",
    },
    {
      id: "pools",
      header: t("nodes.pools"),
      priority: 4,
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
      priority: 4,
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
      priority: 4,
      // 그 노드가 대상인 복제(VM 목록으로 센다)
      cell: (node) => (vms.isSuccess ? <ReplicasCell node={node} vms={vms.data} /> : "—"),
    },
    {
      id: "ceph",
      header: t("nodes.ceph"),
      className: "font-mono text-xs",
      priority: 4,
      // 관리 Ceph에서 이 노드가 맡은 데몬(mon·mgr·osd)
      cell: (node) => cephRoles(ceph.data, node.id).join(", ") || "—",
    },
  ];
  if (actions.write) {
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
          <DropdownMenuContent align="end">{actions.items(node)}</DropdownMenuContent>
        </DropdownMenu>
      ),
    });
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="nodes-title"
        title={t("nav.nodes")}
        help="nodes"
        intro={t("help.nodes.intro")}
      />
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
      {actions.write && (
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
      {showingInterfaces && (
        <NodeInterfacesDialog
          node={showingInterfaces}
          returnFocus={trigger}
          onClose={() => setShowingInterfaces(null)}
        />
      )}
      {actions.dialogs}
    </div>
  );
}
