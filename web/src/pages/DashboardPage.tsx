import { useQuery } from "@tanstack/react-query";
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
import { formatMib } from "@/lib/format";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";
import { vcpus } from "@/vms/VmListPage";

/** 노드에서 돌고 있는 VM이 쓰는 vCPU와 메모리 */
export function usage(node: Node, vms: Vm[]) {
  const here = vms.filter((vm) => vm.status.node === node.id && vm.status.phase !== "stopped");
  return {
    vms: here.length,
    vcpus: here.reduce((sum, vm) => sum + vcpus(vm), 0),
    memoryMib: here.reduce((sum, vm) => sum + vm.spec.memory_mib, 0),
  };
}

function Stat({ title, value, detail }: { title: string; value: string; detail?: string }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm font-normal text-muted-foreground">{title}</CardTitle>
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
  const leader = allNodes.find((n) => n.id === cluster.data?.leader);
  const failed = allVms.filter((vm) => vm.status.phase === "failed");

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-semibold">{t("nav.dashboard")}</h1>
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
        <Stat
          title={t("dashboard.nodes")}
          value={`${allNodes.filter((n) => n.status.online).length} / ${allNodes.length}`}
          detail={t("dashboard.online")}
        />
        <Stat
          title={t("dashboard.vms")}
          value={String(allVms.length)}
          detail={t("dashboard.vmCounts", {
            running: count("running"),
            stopped: count("stopped"),
            failed: count("failed"),
          })}
        />
        <Stat
          title={t("dashboard.leader")}
          value={leader?.name ?? "—"}
          detail={cluster.data?.name}
        />
      </div>

      <section className="flex flex-col gap-2">
        <h2 className="font-medium">{t("dashboard.perNode")}</h2>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t("vms.node")}</TableHead>
              <TableHead>{t("dashboard.state")}</TableHead>
              <TableHead>{t("dashboard.hypervisor")}</TableHead>
              <TableHead>{t("dashboard.runningVms")}</TableHead>
              <TableHead>{t("vms.vcpus")}</TableHead>
              <TableHead>{t("vms.memory")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {allNodes.map((node) => {
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
                    {formatMib(used.memoryMib)} / {formatMib(node.status.memory_mib)}
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
          <h2 className="font-medium">{t("dashboard.recentTasks")}</h2>
          <Link to="/tasks" className="text-sm underline-offset-4 hover:underline">
            {t("dashboard.allTasks")}
          </Link>
        </div>
        <TaskTable tasks={newestFirst(tasks.data ?? []).slice(0, 5)} />
      </section>
    </div>
  );
}
