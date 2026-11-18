import { useQuery } from "@tanstack/react-query";
import { Plus } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { type Column, DataTable, type Filter, useIsPhone } from "@/components/data-table/DataTable";
import { useTableState } from "@/components/data-table/state";
import { PageHeader } from "@/components/PageHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { FirstSteps } from "@/dashboard/FirstSteps";
import { formatMib } from "@/lib/format";
import { isTemplate } from "@/templates/templates";
import { type BulkAction, BULK_ACTIONS, bulkReason, snapshotVms } from "@/vms/bulk";
import { haEnabled } from "@/vms/ha";
import { VmBulkDialog } from "@/vms/VmBulkDialog";
import { VmMenu } from "@/vms/VmMenu";
import { VmPeek } from "@/vms/VmPeek";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";
import { VmPowerButton } from "@/vms/VmPowerButton";

export function vcpus(vm: Vm): number {
  return (vm.spec.cpu.sockets ?? 1) * vm.spec.cpu.cores;
}

const PHASES = ["running", "stopped", "failed", "starting", "stopping", "migrating"] as const;
/** 주소 쿼리의 거름 키 */
const KEYS = ["phase", "node", "template"];

export function VmListPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const vms = useList("vm");
  const nodes = useList("node");
  const snapshots = useList("snapshot");
  const taskIndex = useTaskIndex();
  // 머리줄의 클러스터 전환이 읽은 값만 본다("이 노드" 보기). 따로 묻지 않는다.
  const cluster = useQuery<{ node?: string }>({ queryKey: ["cluster"], enabled: false });
  const state = useTableState("vms", KEYS);
  // 고른 VM은 id로만 둔다. 대화상자가 실시간 목록에서 지금 값을 찾는다.
  const [bulk, setBulk] = useState<{ action: BulkAction; ids: string[]; clear(): void } | null>(
    null,
  );
  const operate = can(me.role, "operate", "vm");
  const write = can(me.role, "write", "vm");
  const templatesOnly = state.get("template") === "1";
  const phone = useIsPhone();

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
        </>
      ),
    },
    {
      id: "phase",
      header: t("common.state"),
      value: (vm) => vm.status.phase,
      cell: (vm) => <VmPhaseBadge vm={vm} />,
    },
    {
      id: "node",
      header: t("common.node"),
      value: (vm) => nodeName(vm.spec.node ?? vm.status.node),
      cell: (vm) => nodeName(vm.spec.node ?? vm.status.node),
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
    ...(here ? [{ name: t("vms.view.thisNode"), query: `node=${encodeURIComponent(here)}` }] : []),
  ];

  const withSnapshots = useMemo(() => snapshotVms(snapshots.data), [snapshots.data]);
  const ctx = { tasks: taskIndex, nodes: nodes.data ?? [], snapshotVms: withSnapshots };
  const bulkBar = (chosen: Vm[], clear: () => void) =>
    BULK_ACTIONS.filter((a) => (a === "delete" ? write : operate)).map((action) => {
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
    });

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="vms-title"
        title={t("nav.vms")}
        actions={
          write && (
            <Button asChild>
              <Link to="/vms/-new">
                <Plus />
                {t("common.create")}
              </Link>
            </Button>
          )
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
        labelledBy="vms-title"
        state={state}
        search={(vm) => vm.name}
        searchLabel={t("vms.search")}
        filters={filters}
        views={views}
        bulk={operate || write ? bulkBar : undefined}
        rowHref={(vm) => `/vms/${encodeURIComponent(vm.name)}`}
        peek={(vm) => <VmPeek vm={vm} nodeName={nodeName} />}
        keys
        toolbar={
          <Button
            variant={templatesOnly ? "secondary" : "outline"}
            size="sm"
            aria-pressed={templatesOnly}
            onClick={() => state.set("template", templatesOnly ? "" : "1")}
          >
            {t("vms.templatesOnly")}
          </Button>
        }
      />
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
