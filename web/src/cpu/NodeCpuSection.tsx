import { useQueryClient } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { Node } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { ListPending } from "@/components/ListPending";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { CPU_KEY, useNodeCpu } from "@/cpu/cpu";
import { errorMessage } from "@/i18n/errors";
import { formatTime } from "@/lib/format";
import { trackTask, waitForTask } from "@/tasks/tasks";

/**
 * 노드 상세의 "CPU"(설계 §13): 탐침 결과(QEMU 버전·가속기·호스트 CPU·읽은 시각·오류), 클러스터 기준을 켤 수 없는
 * 까닭, 다시 읽기(admin, Task `node-cpu-probe` — 검사 회차를 기다려 10초 안팎), 모델 표(찾기). 루트
 * `cluster.read`가 있어야 보인다. 위트니스는 VM을 돌리지 않아 없다.
 */
export function NodeCpuSection({ node }: { node: Node }) {
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const readable = perms.has("cluster.read") && !node.spec.witness;
  const info = useNodeCpu(node.id, readable);
  const [query, setQuery] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  if (!readable) {
    return null;
  }

  async function refresh() {
    setRefreshing(true);
    try {
      const task = await call(
        api.POST("/nodes/{key}/cpu/refresh", { params: { path: { key: node.id } } }),
      );
      trackTask(task, t("tasks.op.node-cpu-probe", { target: node.name }));
      await waitForTask(queryClient, task.id);
      await queryClient.invalidateQueries({ queryKey: CPU_KEY });
    } catch (err) {
      toast.error(t("cpu.node.refreshFailed", { error: errorMessage(t, err) }));
    } finally {
      setRefreshing(false);
    }
  }

  const data = info.data;
  const cpu = data?.cpu;
  const q = query.trim().toLowerCase();
  const models = (data?.models ?? []).filter(
    (m) =>
      q === "" ||
      [m.name, m.vendor ?? "", m.alias_of ?? ""].some((s) => s.toLowerCase().includes(q)),
  );
  return (
    <section aria-labelledby="node-cpu" className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="node-cpu" className="font-medium">
          {t("cpu.node.title")}
        </h2>
        {perms.has("cluster.admin") && (
          <Button
            variant="outline"
            size="sm"
            disabled={refreshing || !node.status.online}
            onClick={() => void refresh()}
          >
            <RefreshCw className={refreshing ? "animate-spin" : undefined} />
            {refreshing ? t("cpu.node.refreshing") : t("cpu.node.refresh")}
          </Button>
        )}
      </div>
      {!data ? (
        <ListPending error={info.error} />
      ) : (
        <>
          {cpu ? (
            <dl className="grid grid-cols-1 gap-3 text-sm sm:grid-cols-2 lg:grid-cols-3">
              <Item label={t("cpu.node.qemu")}>{cpu.qemu || "—"}</Item>
              <Item label={t("cpu.node.accel")}>{cpu.accel || "—"}</Item>
              <Item label={t("cpu.node.vendor")}>{cpu.vendor || "—"}</Item>
              <Item label={t("cpu.node.modelId")}>{cpu.model_id || "—"}</Item>
              <Item label={t("cpu.node.probedAt")}>
                {cpu.probed_at ? formatTime(new Date(cpu.probed_at * 1000), i18n.language) : "—"}
              </Item>
              <Item label={t("cpu.node.counts")}>
                {t("cpu.node.countsValue", {
                  usable: data.models.filter((m) => m.usable).length,
                  models: data.models.length,
                  flags: data.flags.filter((f) => f.supported).length,
                })}
              </Item>
            </dl>
          ) : (
            <p className="text-sm text-muted-foreground">{t("cpu.node.none")}</p>
          )}
          {cpu?.error && (
            <p role="alert" className="text-sm text-destructive">
              {t("cpu.node.error", { error: cpu.error })}
            </p>
          )}
          {data.baseline_problem && (
            <p className="rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm">
              {t("cpu.node.baselineProblem", { problem: data.baseline_problem })}
            </p>
          )}
          {data.models.length > 0 && (
            <>
              <Input
                type="search"
                className="max-w-xs"
                aria-label={t("cpu.model.search")}
                placeholder={t("cpu.model.search")}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
              <div className="max-h-96 overflow-y-auto">
                <Table aria-labelledby="node-cpu">
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t("cpu.node.model")}</TableHead>
                      <TableHead>{t("cpu.node.vendor")}</TableHead>
                      <TableHead>{t("common.state")}</TableHead>
                      <TableHead>{t("cpu.node.missing")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {models.map((m) => (
                      <TableRow key={m.name}>
                        <TableCell className="font-mono text-xs">
                          {m.name}
                          {m.alias_of && (
                            <span className="ml-1 text-muted-foreground">→ {m.alias_of}</span>
                          )}
                        </TableCell>
                        <TableCell>{m.vendor ?? "—"}</TableCell>
                        <TableCell>
                          <StatusChip tone={m.usable ? "success" : "neutral"}>
                            {t(m.usable ? "cpu.node.usable" : "cpu.node.unusable")}
                          </StatusChip>
                        </TableCell>
                        <TableCell className="font-mono text-xs whitespace-normal">
                          {[...(m.missing ?? []), ...(m.missing_other ?? [])].join(", ") || "—"}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            </>
          )}
        </>
      )}
    </section>
  );
}

function Item({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-0.5">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="break-words">{children}</dd>
    </div>
  );
}
