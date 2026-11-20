import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";

import { useList } from "@/api/live";
import type { Node, Vm } from "@/api/types";
import { MetricCharts } from "@/charts/MetricChart";
import { NODE_CHARTS, useNodeHistory } from "@/charts/metrics";
import { RangePicker } from "@/charts/RangePicker";
import { useTimeRange } from "@/charts/useTimeRange";
import { ListPending } from "@/components/ListPending";
import { NodeCpuSection } from "@/cpu/NodeCpuSection";
import { NodeTopologySection } from "@/numa/NodeTopologySection";
import { TableMessage } from "@/components/TableMessage";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatMib } from "@/lib/format";
import { NodeHeader } from "@/nodes/NodeHeader";
import { NodeSdnSection } from "@/nodes/NodeSdnSection";
import { vcpus } from "@/vms/VmListPage";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";

/**
 * 노드 상세(`/nodes/:name`): 머리, 지표 차트 넷(범위 고르기), CPU(계획 63), NUMA 토폴로지(계획 64), SDN physnet·게이트웨이
 * (계획 66), 그 노드의 VM
 */
export function NodeDetailPage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const nodes = useList("node");
  const vms = useList("vm");
  if (!nodes.data) {
    return <ListPending error={nodes.error} />;
  }
  const node = nodes.data.find((n) => n.name === name);
  if (!node) {
    return (
      <div className="flex flex-col items-start gap-2">
        <h1 className="text-xl font-semibold">{t("nodes.notFound", { name })}</h1>
        <Link to="/nodes" className="text-sm underline">
          {t("nodes.backToList")}
        </Link>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-6">
      <NodeHeader node={node} vms={vms.data ? vmsOn(node, vms.data).length : undefined} />
      <NodeMetrics node={node} />
      <NodeCpuSection node={node} />
      <NodeTopologySection node={node} />
      <NodeSdnSection node={node} />
      <NodeVms node={node} />
    </div>
  );
}

function NodeMetrics({ node }: { node: Node }) {
  const { t } = useTranslation();
  const [range, setRange] = useTimeRange();
  // 위트니스는 지표를 기록하지 않는다(API도 409).
  const history = useNodeHistory(node.id, range, !node.spec.witness);
  return (
    <section aria-labelledby="node-metrics" className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="node-metrics" className="font-medium">
          {t("metrics.title")}
        </h2>
        {!node.spec.witness && <RangePicker value={range} onChange={setRange} />}
      </div>
      {node.spec.witness ? (
        <p className="text-sm text-muted-foreground">{t("metrics.witness")}</p>
      ) : (
        <MetricCharts specs={NODE_CHARTS} series={history.data} range={range} query={history} />
      )}
    </section>
  );
}

/** 그 노드에서 돌거나 그 노드에 고정한 VM */
export function vmsOn(node: Node, vms: Vm[]): Vm[] {
  return vms
    .filter((vm) => (vm.status.node ?? vm.spec.node) === node.id)
    .sort((a, b) => a.name.localeCompare(b.name));
}

function NodeVms({ node }: { node: Node }) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const here = vmsOn(node, vms.data ?? []);
  return (
    <section aria-labelledby="node-vms" className="flex flex-col gap-2">
      <h2 id="node-vms" className="font-medium">
        {t("nav.vms")}
      </h2>
      <Table aria-labelledby="node-vms">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("vms.vcpus")}</TableHead>
            <TableHead>{t("vms.memory")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {here.length === 0 && <TableMessage colSpan={4} query={vms} empty={t("nodes.noVms")} />}
          {here.map((vm) => (
            <TableRow key={vm.id}>
              <TableCell className="font-medium">
                <Link
                  to={`/vms/${encodeURIComponent(vm.name)}`}
                  className="underline-offset-4 hover:underline"
                >
                  {vm.name}
                </Link>
              </TableCell>
              <TableCell>
                <VmPhaseBadge vm={vm} />
              </TableCell>
              <TableCell>{vcpus(vm)}</TableCell>
              <TableCell>{formatMib(vm.spec.memory_mib)}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </section>
  );
}
