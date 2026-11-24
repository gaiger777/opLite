// 가리키기·초점 카드(계획 70 §10.1): 그림 위에 떠 있는 작은 카드(`pointer-events: none`, 화면 읽기에는 숨김 — 같은 글이
// 원소의 `aria-label`이다). 노드는 상태 칩·CPU·메모리 막대·할당·VM 수·경보·로컬 풀, VM은 상태·vCPU·메모리·노드·HA·옮기기.
import { useTranslation } from "react-i18next";

import type { Vm } from "@/api/types";
import type { NodeItem } from "@/clusterMap/layout";
import { isHa, type MapNode } from "@/clusterMap/model";
import { StatusChip } from "@/components/StatusChip";
import { formatCapacity, formatMib } from "@/lib/format";

function Meter({ label, pct }: { label: string; pct: number | null }) {
  const tone = pct === null ? "" : pct >= 90 ? "bg-danger" : pct >= 70 ? "bg-warning" : "bg-brand";
  return (
    <div className="flex items-center gap-2">
      <span className="w-14 text-muted-foreground">{label}</span>
      <span className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
        {pct !== null && (
          <span className={`block h-full ${tone}`} style={{ width: `${Math.min(100, pct)}%` }} />
        )}
      </span>
      <span className="w-10 text-right tabular-nums">
        {pct === null ? "—" : `${Math.round(pct)}%`}
      </span>
    </div>
  );
}

export function NodeCard({ node, item }: { node: MapNode; item: NodeItem }) {
  const { t } = useTranslation();
  const c = item.counts;
  const total = c.running + c.stopped + c.failed + c.busy;
  return (
    <div className="flex flex-col gap-1.5">
      <p className="text-sm font-semibold">{node.name}</p>
      <div className="flex flex-wrap gap-1">
        <StatusChip tone={node.online ? "success" : "danger"}>
          {t(node.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
        </StatusChip>
        {node.maintenance && <StatusChip tone="warning">{t("dashboard.maintenance")}</StatusChip>}
        {node.frozen && <StatusChip tone="info">{t("nodes.frozen")}</StatusChip>}
        {node.witness && <StatusChip tone="neutral">{t("dashboard.witness")}</StatusChip>}
        {node.leader && <StatusChip tone="neutral">{t("clusterMap.label.leader")}</StatusChip>}
      </div>
      {!node.witness && (
        <>
          {node.load ? (
            <>
              <Meter label={t("clusterMap.card.cpu")} pct={node.load.cpu} />
              <Meter label={t("clusterMap.card.memory")} pct={node.load.mem} />
            </>
          ) : (
            <p className="text-muted-foreground">{t("clusterMap.label.loadUnknown")}</p>
          )}
          <p>
            {t("clusterMap.card.size", {
              cpus: node.cpus,
              memory: formatCapacity(node.memoryMib),
            })}
            {node.allocatedPct !== null &&
              ` · ${t("clusterMap.card.allocated", { pct: node.allocatedPct })}`}
          </p>
          <p>
            {t("clusterMap.card.vms", { running: c.running, total })}
            {node.hiddenVms > 0 &&
              ` · ${t("clusterMap.label.hiddenCount", { count: node.hiddenVms })}`}
            {node.templates > 0 &&
              ` · ${t("clusterMap.card.templates", { count: node.templates })}`}
          </p>
          {node.pools.length > 0 && (
            <p>
              {t("clusterMap.card.pools")}:{" "}
              {node.pools
                .map((p) =>
                  p.ready
                    ? `${p.name} ${p.usedPct ?? "—"}%`
                    : `${p.name} (${t("clusterMap.card.notReady")})`,
                )
                .join(", ")}
            </p>
          )}
        </>
      )}
      {node.alerts > 0 && (
        <p className="font-medium text-danger-fg">
          {t("clusterMap.label.alerts", { count: node.alerts })}
        </p>
      )}
    </div>
  );
}

export function VmCard({
  vm,
  nodeName,
  progress,
}: {
  vm: Vm;
  nodeName(id: string | null | undefined): string;
  progress: number | null;
}) {
  const { t } = useTranslation();
  const phase = vm.status.phase;
  const tone =
    phase === "running"
      ? "success"
      : phase === "failed"
        ? "danger"
        : phase === "stopped"
          ? "neutral"
          : "info";
  const m = vm.status.migration;
  return (
    <div className="flex flex-col gap-1.5">
      <p className="text-sm font-semibold">{vm.name}</p>
      <div className="flex flex-wrap gap-1">
        <StatusChip tone={tone}>{t(`vms.phase.${phase}`)}</StatusChip>
        {isHa(vm) && <StatusChip tone="neutral">{t("clusterMap.label.ha")}</StatusChip>}
      </div>
      <p>
        {t("clusterMap.card.vmSize", {
          vcpus: (vm.spec.cpu.sockets ?? 1) * vm.spec.cpu.cores,
          memory: formatMib(vm.spec.memory_mib),
        })}
      </p>
      <p>{t("clusterMap.label.onNode", { node: nodeName(vm.status.node ?? vm.spec.node) })}</p>
      {m && (
        <p className="font-medium text-info-fg">
          {progress !== null
            ? t("clusterMap.label.movingPct", { node: nodeName(m.node), progress })
            : t("clusterMap.label.moving", { node: nodeName(m.node) })}
        </p>
      )}
    </div>
  );
}

/** 화살표·"+N" 따위: 화면 읽기 이름 그대로 */
export function TextCard({ text }: { text: string }) {
  return <p>{text}</p>;
}
