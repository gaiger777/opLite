import { useQuery } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Node, Vm } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatCapacity } from "@/lib/format";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";
import { vcpus } from "@/vms/VmListPage";

/** QEMU가 떠 있는 단계. 실패한 VM도 `status.node`가 남으므로 단계로 거른다. */
const ACTIVE: Vm["status"]["phase"][] = ["starting", "running", "stopping"];

/** 노드에서 시작 중·실행 중·정지 중인 VM이 쓰는 vCPU와 메모리 */
export function usage(node: Node, vms: Vm[]) {
  const here = vms.filter((vm) => vm.status.node === node.id && ACTIVE.includes(vm.status.phase));
  return {
    vms: here.length,
    vcpus: here.reduce((sum, vm) => sum + vcpus(vm), 0),
    memoryMib: here.reduce((sum, vm) => sum + vm.spec.memory_mib, 0),
  };
}

/** 아직 받지 못한 데이터: 실패했으면 그 이유, 아니면 불러오는 중 */
function waiting(t: TFunction, ...queries: { isError: boolean; error: unknown }[]) {
  const failed = queries.find((q) => q.isError);
  return failed ? errorMessage(t, failed.error) : t("common.loading");
}

function Stat({ title, value, detail }: { title: string; value: string; detail?: string }) {
  const id = useId();
  return (
    <Card role="group" aria-labelledby={id}>
      <CardHeader>
        <CardTitle id={id} className="text-sm font-normal text-muted-foreground">
          {title}
        </CardTitle>
      </CardHeader>
      <CardContent>
        <div className="text-2xl font-semibold">{value}</div>
        {detail && <div className="text-xs text-muted-foreground">{detail}</div>}
      </CardContent>
    </Card>
  );
}

export function DashboardPage() {
  const { t } = useTranslation();
  const nodes = useList("node");
  const vms = useList("vm");
  const tasks = useList("task");
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const allNodes = nodes.data ?? [];
  const allVms = vms.data ?? [];
  const count = (phase: Vm["status"]["phase"]) =>
    allVms.filter((vm) => vm.status.phase === phase).length;
  const failed = allVms.filter((vm) => vm.status.phase === "failed");

  // 리더 이름: Node 리소스, 클러스터 멤버, 그래도 없으면(합류 중) id 끝 8자
  let leader = waiting(t, cluster);
  if (cluster.data) {
    const id = cluster.data.leader;
    leader = id
      ? (allNodes.find((n) => n.id === id)?.name ??
        cluster.data.members.find((m) => m.id === id)?.name ??
        id.slice(-8))
      : t("dashboard.noLeader");
  }

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-semibold">{t("nav.dashboard")}</h1>
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
        <Stat
          title={t("dashboard.nodes")}
          value={
            nodes.data
              ? `${allNodes.filter((n) => n.status.online).length} / ${allNodes.length}`
              : waiting(t, nodes)
          }
          detail={nodes.data && t("dashboard.online")}
        />
        <Stat
          title={t("dashboard.vms")}
          value={vms.data ? String(allVms.length) : waiting(t, vms)}
          detail={
            vms.data &&
            t("dashboard.vmCounts", {
              running: count("running"),
              stopped: count("stopped"),
              failed: count("failed"),
            })
          }
        />
        <Stat title={t("dashboard.leader")} value={leader} detail={cluster.data?.name} />
      </div>

      <section className="flex flex-col gap-2">
        <h2 id="dashboard-nodes" className="font-medium">
          {t("dashboard.perNode")}
        </h2>
        <Table aria-labelledby="dashboard-nodes">
          <TableHeader>
            <TableRow>
              <TableHead>{t("vms.node")}</TableHead>
              <TableHead>{t("dashboard.state")}</TableHead>
              <TableHead>{t("dashboard.hypervisor")}</TableHead>
              <TableHead>{t("dashboard.activeVms")}</TableHead>
              <TableHead>{t("vms.vcpus")}</TableHead>
              <TableHead>{t("vms.memory")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(!nodes.data || !vms.data || allNodes.length === 0) && (
              <TableRow>
                <TableCell colSpan={6} className="text-center text-muted-foreground">
                  {nodes.data && vms.data ? t("dashboard.noNodes") : waiting(t, nodes, vms)}
                </TableCell>
              </TableRow>
            )}
            {(vms.data ? allNodes : []).map((node) => {
              const used = usage(node, allVms);
              return (
                <TableRow key={node.id}>
                  <TableCell className="font-medium">{node.name}</TableCell>
                  <TableCell>
                    {node.status.online ? (
                      <Badge>{t("dashboard.nodeOnline")}</Badge>
                    ) : (
                      <Badge variant="destructive">{t("dashboard.nodeOffline")}</Badge>
                    )}
                    {node.spec.maintenance && (
                      <Badge variant="outline" className="ml-1">
                        {t("dashboard.maintenance")}
                      </Badge>
                    )}
                  </TableCell>
                  <TableCell>{node.status.hypervisor}</TableCell>
                  <TableCell>{used.vms}</TableCell>
                  <TableCell>
                    {used.vcpus} / {node.status.cpus}
                  </TableCell>
                  <TableCell>
                    {formatCapacity(used.memoryMib)} / {formatCapacity(node.status.memory_mib)}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </section>

      {failed.length > 0 && (
        <section className="flex flex-col gap-2">
          <h2 className="font-medium">{t("dashboard.failedVms")}</h2>
          <ul className="flex flex-col gap-1 text-sm">
            {failed.map((vm) => (
              <li key={vm.id}>
                <Link to={`/vms/${encodeURIComponent(vm.name)}`} className="font-medium underline">
                  {vm.name}
                </Link>
                <span className="ml-2 text-muted-foreground">{vm.status.last_error}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="flex flex-col gap-2">
        <div className="flex items-center justify-between">
          <h2 id="dashboard-tasks" className="font-medium">
            {t("dashboard.recentTasks")}
          </h2>
          <Link to="/tasks" className="text-sm underline-offset-4 hover:underline">
            {t("dashboard.allTasks")}
          </Link>
        </div>
        <TaskTable tasks={newestFirst(tasks.data ?? []).slice(0, 5)} labelledBy="dashboard-tasks" />
      </section>
    </div>
  );
}
