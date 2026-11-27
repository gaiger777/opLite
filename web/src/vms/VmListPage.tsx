import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Copy, Monitor, Plus } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Vm } from "@/api/types";
import { objectTarget, type Privilege } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { type Column, DataTable, type Filter, useIsPhone } from "@/components/data-table/DataTable";
import { useTableState } from "@/components/data-table/state";
import { EmptyState } from "@/components/EmptyState";
import { PageHeader } from "@/components/PageHeader";
import { ViewToggle } from "@/layout/PageTabs";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cpuPending } from "@/cpu/cpu";
import { FirstSteps } from "@/dashboard/FirstSteps";
import { folderPath, folderTree, membersOf } from "@/folders/folders";
import { MoveButton, MoveDialog } from "@/folders/MoveDialog";
import { formatMib } from "@/lib/format";
import { isTemplate } from "@/templates/templates";
import { type BulkAction, BULK_ACTIONS, BULK_PRIVILEGE, bulkReason, snapshotVms } from "@/vms/bulk";
import { haEnabled } from "@/vms/ha";
import { type GuestInfoResponse, guestAddresses } from "@/vms/guest";
import { VmBulkDialog } from "@/vms/VmBulkDialog";
import { VmMenu } from "@/vms/VmMenu";
import { VmPeek } from "@/vms/VmPeek";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";
import { VmPowerButton } from "@/vms/VmPowerButton";

export function vcpus(vm: Vm): number {
  return (vm.spec.cpu.sockets ?? 1) * vm.spec.cpu.cores;
}

const PHASES = ["running", "stopped", "failed", "starting", "stopping", "migrating"] as const;
/** 한 쪽에 보일 VM 수(계획 77-6 — 131행을 한 쪽 5600px로 그리던 것) */
const PAGE_SIZE = 50;
/** 주소 쿼리의 거름 키 */
const KEYS = ["phase", "node", "template", "folder", "cpu"];

export function VmListPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const vms = useList("vm");
  const nodes = useList("node");
  const snapshots = useList("snapshot");
  const folders = useList("folder");
  const tree = useMemo(() => folderTree(folders.data, "vm"), [folders.data]);
  const taskIndex = useTaskIndex();
  // 머리줄의 클러스터 전환이 읽은 값만 본다("이 노드" 보기). 따로 묻지 않는다.
  const cluster = useQuery<{ node?: string }>({ queryKey: ["cluster"], enabled: false });
  const state = useTableState("vms", KEYS);
  // 고른 VM은 id로만 둔다. 대화상자가 실시간 목록에서 지금 값을 찾는다.
  const [bulk, setBulk] = useState<{ action: BulkAction; ids: string[]; clear(): void } | null>(
    null,
  );
  const [moving, setMoving] = useState<{ vms: Vm[]; clear(): void } | null>(null);
  // VM마다 권한이 다르다(계획 62 폴더·객체 grant). 어딘가에 그 권한이 있으면 단추를 두고 VM마다 센다.
  const allowed = (vm: Vm, privilege: Privilege) => perms.has(privilege, objectTarget("vm", vm));
  const create = perms.anywhere("vm.create");
  const phone = useIsPhone();

  // 게스트 에이전트가 알린 주소는 이미 읽은 것만(VM 상세·팔레트가 읽어 둔 캐시) — 행마다 묻지 않는다.
  const queryClient = useQueryClient();
  const guestIps = new Map<string, string[]>();
  for (const [key, data] of queryClient.getQueriesData<GuestInfoResponse>({
    queryKey: ["guest"],
  })) {
    if (data?.info && typeof key[1] === "string") {
      guestIps.set(key[1], guestAddresses(data.info));
    }
  }
  const addresses = (vm: Vm): string[] => {
    const statics = (vm.spec.cloud_init?.networks ?? []).flatMap((net) =>
      [net.ipv4, net.ipv6].flatMap((c) => (c?.mode === "static" ? [c.address] : [])),
    );
    return [
      ...new Set([...statics, ...(guestIps.get(vm.id) ?? [])].map((a) => a.split("/")[0] ?? a)),
    ];
  };

  const nodeName = useMemo(() => {
    const names = new Map(nodes.data?.map((n) => [n.id, n.name]));
    return (id: string | null | undefined) => (id ? (names.get(id) ?? id.slice(-8)) : "—");
  }, [nodes.data]);

  const columns: Column<Vm>[] = [
    {
      id: "name",
      header: t("common.name"),
      value: (vm) => vm.name,
      cell: (vm) => (
        <>
          <Link
            to={`/vms/${encodeURIComponent(vm.name)}`}
            className="font-medium underline-offset-4 hover:underline"
          >
            {vm.name}
          </Link>
          {haEnabled(vm.spec) && (
            <Badge variant="outline" className="ml-2">
              {t("vms.ha")}
            </Badge>
          )}
          {isTemplate(vm) && (
            <Badge variant="secondary" className="ml-2">
              {t("vms.template")}
            </Badge>
          )}
          {cpuPending(vm) && (
            <Badge variant="outline" className="ml-2" title={cpuPending(vm)?.detail}>
              {t("cpu.pending.badge")}
            </Badge>
          )}
        </>
      ),
    },
    {
      id: "phase",
      header: t("common.state"),
      value: (vm) => vm.status.phase,
      cell: (vm) => <VmPhaseBadge vm={vm} />,
    },
    // 계획 62: 폴더가 하나라도 보이면 폴더 열을 둔다. 이름 뒤에 둔다(휴대폰 카드 제목은 첫 열이다).
    ...(tree.folders.length > 0
      ? [
          {
            id: "folder",
            header: t("folders.folder"),
            value: (vm: Vm) => folderPath(tree, vm.spec.folder),
            cell: (vm: Vm) => folderPath(tree, vm.spec.folder),
            priority: 2 as const,
          },
        ]
      : []),
    {
      id: "node",
      header: t("common.node"),
      value: (vm) => nodeName(vm.spec.node ?? vm.status.node),
      cell: (vm) => nodeName(vm.spec.node ?? vm.status.node),
    },
    // 운영자가 먼저 찾는 주소(진단 §2.2): cloud-init 고정 주소와 이미 읽은 게스트 정보(새로 묻지 않는다)
    {
      id: "ip",
      header: t("vms.addresses"),
      value: (vm) => addresses(vm)[0] ?? "",
      cell: (vm) => {
        const list = addresses(vm);
        return list.length > 0 ? (
          <span className="font-mono text-xs" title={list.join(", ")}>
            {list[0]}
            {list.length > 1 && ` +${list.length - 1}`}
          </span>
        ) : (
          <span className="text-muted-foreground">—</span>
        );
      },
      priority: 2,
    },
    { id: "cpu", header: t("vms.vcpus"), value: vcpus, cell: vcpus, priority: 2 },
    {
      id: "memory",
      header: t("vms.memory"),
      value: (vm) => vm.spec.memory_mib,
      cell: (vm) => formatMib(vm.spec.memory_mib),
      priority: 2,
    },
    {
      id: "menu",
      header: t("vms.actions"),
      actions: true,
      // 휴대폰 카드(당직 보기)에는 전원 단추 하나를 앞에 둔다(설계 §5.9).
      cell: (vm) =>
        phone ? (
          <div className="flex items-center gap-1">
            <VmPowerButton vm={vm} />
            <VmMenu vm={vm} />
          </div>
        ) : (
          <VmMenu vm={vm} />
        ),
    },
  ];

  const filters: Filter<Vm>[] = [
    {
      id: "phase",
      label: t("common.state"),
      options: PHASES.map((p) => ({ value: p, label: t(`vms.phase.${p}`) })),
      test: (vm, value) => vm.status.phase === value,
    },
    {
      id: "node",
      label: t("common.node"),
      options: (nodes.data ?? []).map((n) => ({ value: n.id, label: n.name })),
      test: (vm, value) => (vm.spec.node ?? vm.status.node) === value,
    },
    {
      id: "folder",
      label: t("folders.folder"),
      options: tree.folders.map((f) => ({ value: f.id, label: folderPath(tree, f.id) })),
      // 하위 폴더의 VM도 보인다.
      test: (vm, value) => membersOf(tree, [vm], value, true).length > 0,
    },
    // 보류 `cpu`(다음 전원 껐다 켜기에 CPU가 바뀜, 계획 63). 설정의 숨김 목록 저장 뒤 이 거름으로 온다.
    {
      id: "cpu",
      label: t("cpu.pending.filter"),
      options: [{ value: "pending", label: t("cpu.pending.badge") }],
      test: (vm, value) => value !== "pending" || cpuPending(vm) !== undefined,
    },
    {
      id: "template",
      label: t("vms.templatesOnly"),
      options: [],
      test: (vm, value) => value !== "1" || isTemplate(vm),
    },
  ];
  const here = cluster.data?.node;
  const views = [
    { name: t("vms.view.running"), query: "phase=running" },
    { name: t("vms.view.failed"), query: "phase=failed" },
    { name: t("cpu.pending.view"), query: "cpu=pending" },
    ...(here ? [{ name: t("vms.view.thisNode"), query: `node=${encodeURIComponent(here)}` }] : []),
  ];

  const withSnapshots = useMemo(() => snapshotVms(snapshots.data), [snapshots.data]);
  const ctx = { tasks: taskIndex, nodes: nodes.data ?? [], snapshotVms: withSnapshots, allowed };
  // 노드를 읽을 수 없으면(폴더 사용자) 옮길 노드를 고를 수 없다.
  const actions = BULK_ACTIONS.filter(
    (a) => perms.anywhere(BULK_PRIVILEGE[a]) && (a !== "migrate" || perms.reads("node")),
  );
  const mover = perms.anywhere("inventory.move") && tree.folders.length > 0;
  const bulkBar = (chosen: Vm[], clear: () => void) => [
    ...actions.map((action) => {
      // 옮기기는 대상 노드를 대화상자에서 고르므로 노드와 상관없는 까닭만 센다.
      const able = chosen.filter((vm) => {
        const reason = bulkReason(vm, action, ctx);
        return reason === null || reason === "vms.bulk.reason.noTarget";
      }).length;
      const label = t(`vms.bulk.action.${action}`);
      return (
        <Button
          key={action}
          size="sm"
          variant={action === "delete" ? "destructive" : "outline"}
          disabled={able === 0}
          aria-label={t("vms.bulk.button", {
            action: label,
            able,
            blocked: chosen.length - able,
          })}
          onClick={() => setBulk({ action, ids: chosen.map((vm) => vm.id), clear })}
        >
          {label}
          <span aria-hidden className="text-xs tabular-nums opacity-70">
            {able}/{chosen.length}
          </span>
        </Button>
      );
    }),
    ...(mover
      ? [
          <MoveButton
            key="move"
            items={chosen}
            allowed={(vm) => allowed(vm, "inventory.move")}
            onClick={() => setMoving({ vms: chosen, clear })}
          />,
        ]
      : []),
  ];

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="vms-title"
        title={t("nav.vms")}
        help="vms"
        intro={t("help.vms.intro")}
        actions={
          <>
            <ViewToggle />
            {create && (
              // 머리줄의 "만들기 ▾"(모든 것)와 겹치지 않게 이 화면 단추는 무엇을 만드는지 말한다(계획 77-6).
              <Button asChild>
                <Link to="/vms/-new">
                  <Plus />
                  {t("vms.createVm")}
                </Link>
              </Button>
            )}
          </>
        }
      />
      <FirstSteps />
      <DataTable
        id="vms"
        rows={vms.data ?? []}
        columns={columns}
        rowId={(vm) => vm.id}
        rowName={(vm) => vm.name}
        query={vms}
        empty={t("vms.empty")}
        emptyState={
          <EmptyState
            icon={Monitor}
            title={t("vms.empty")}
            description={t("help.vms.empty")}
            help="vms.page"
            action={
              create && (
                <div className="flex flex-wrap justify-center gap-2">
                  <Button asChild>
                    <Link to="/vms/-new">
                      <Plus />
                      {t("vms.createFirst")}
                    </Link>
                  </Button>
                  <Button asChild variant="outline">
                    <Link to="/templates">
                      <Copy />
                      {t("vms.fromTemplate")}
                    </Link>
                  </Button>
                </div>
              )
            }
          />
        }
        labelledBy="vms-title"
        state={state}
        search={(vm) => vm.name}
        searchLabel={t("vms.search")}
        filters={filters}
        views={views}
        bulk={actions.length > 0 || mover ? bulkBar : undefined}
        rowHref={(vm) => `/vms/${encodeURIComponent(vm.name)}`}
        peek={(vm) => <VmPeek vm={vm} nodeName={nodeName} />}
        keys
        pageSize={PAGE_SIZE}
      />
      {moving && (
        <MoveDialog
          kind="vm"
          items={moving.vms}
          onClose={() => setMoving(null)}
          onDone={moving.clear}
        />
      )}
      {bulk && (
        <VmBulkDialog
          action={bulk.action}
          ids={bulk.ids}
          onClose={() => setBulk(null)}
          onStarted={bulk.clear}
        />
      )}
    </div>
  );
}
