// 자세히 판(계획 75 §4.1): 고른 것 없음 → 클러스터 요약, 노드 → 네 칸, 포트 → 그 노드의 네트워크 칸 + 포트 강조, VM → 지금
// `VmPeek` + "이 VM의 경로", 레일(운반자) → 그것을 쓰는 네트워크마다 노드와 상태. 넓은 화면은 지도 오른쪽 열, 좁은 화면은 서랍(`OperatorBoard`).
import { useQueries } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { StatusChip } from "@/components/StatusChip";
import { Sheet } from "@/components/ui/sheet";
import { NodeDetail } from "@/dashboard/NodeDetail";
import { pickFault } from "@/dashboard/pick";
import type { MapSelection } from "@/dashboard/selection";
import { cpuText, cpuUseText, memoryText, storageText } from "@/dashboard/texts";
import { UsageBar } from "@/dashboard/UsageBar";
import { errorMessage } from "@/i18n/errors";
import { formatMib } from "@/lib/format";
import { useNodeTopology } from "@/numa/numa";
import type { Capacity, NetRail, PhysicalTopology } from "@/physical/api";
import { capacityRow } from "@/physical/capacity";
import { faultList } from "@/physical/chain";
import { ForgetPort } from "@/physical/ForgetPort";
import { vmPath } from "@/physical/vmPath";
import { VmPeek } from "@/vms/VmPeek";

const RAIL_TONE = {
  up: "success",
  degraded: "warning",
  down: "danger",
  absent: "neutral",
  unknown: "neutral",
} as const;

function ClusterSummary({
  capacity,
  physical,
  physicalError,
  physicalStale,
  maintenance,
  onRetry,
  onSelect,
}: {
  capacity: Capacity;
  physical?: PhysicalTopology;
  physicalError?: unknown;
  physicalStale?: boolean;
  maintenance?: ReadonlySet<string>;
  onRetry?(): void;
  onSelect(s: MapSelection): void;
}) {
  const { t } = useTranslation();
  const totals = capacity.totals;
  // 합계도 노드 한 행과 같은 셈(공유 스레드·할당·사용)
  const row = capacityRow({
    id: "",
    node: "",
    online: true,
    cpu: totals.cpu,
    memory: totals.memory,
    storage: [],
  });
  const storage = {
    total: totals.storage.total_bytes,
    allocated: totals.storage.provisioned_bytes,
    used: totals.storage.used_bytes,
  };
  const faults = faultList(physical?.nodes ?? [], maintenance);
  const rails = physical?.networks ?? [];
  const railName = new Map(rails.map((r) => [r.id, r.name]));
  const broken = rails.filter((r) =>
    r.nodes.some((n) => n.state === "down" || n.state === "degraded"),
  );
  return (
    <div className="flex flex-col gap-3">
      <h2 className="text-base font-semibold">{t("physical.panel.cluster")}</h2>
      <p className="text-xs text-muted-foreground">
        {t("physical.panel.clusterHint", { count: totals.nodes })}
      </p>
      <UsageBar
        label={t("physical.table.cpu")}
        m={row.cpu}
        text={cpuText(t, row.cpu, row.cpuOvercommit, row.cpuDedicated)}
        note={cpuUseText(t, row.cpuUsePct)}
      />
      <UsageBar
        label={t("physical.table.memory")}
        m={row.memory}
        text={memoryText(t, row.memory, row.memoryOvercommit)}
      />
      <UsageBar label={t("physical.table.storage")} m={storage} text={storageText(t, storage)} />
      <section aria-label={t("physical.panel.faults")} className="flex flex-col gap-1">
        <h3 className="text-sm font-semibold">{t("physical.panel.faults")}</h3>
        {!physical ? (
          // 받지 못했으면 "장애 없음"이라 말하지 않는다(리뷰 1).
          physicalError === undefined ? (
            <p className="text-sm text-muted-foreground">{t("common.loading")}</p>
          ) : (
            <div className="flex flex-col items-start gap-1">
              <p className="text-sm text-muted-foreground">
                <span aria-hidden>? </span>
                {t("physical.panel.physicalUnknown")}
              </p>
              <p className="text-xs text-muted-foreground">{errorMessage(t, physicalError)}</p>
              <button type="button" className="text-sm underline" onClick={onRetry}>
                {t("physical.panel.retry")}
              </button>
            </div>
          )
        ) : faults.length === 0 ? (
          // 다시 읽지 못했으면 "장애 없음"이라 말하지 않는다(리뷰 2).
          <p className="text-sm text-muted-foreground">
            {t(physicalStale ? "physical.panel.staleRefresh" : "physical.panel.noFaults")}
          </p>
        ) : (
          <ul className="flex flex-col gap-1 text-sm">
            {physicalStale && (
              <li role="status" className="text-xs text-warning-fg">
                {t("physical.panel.staleRefresh")}
              </li>
            )}
            {faults.map((f) => (
              <li
                key={`${f.node}/${f.port ?? f.network}`}
                className="flex flex-wrap items-center gap-x-2"
              >
                <StatusChip
                  tone={f.stale ? "neutral" : f.severity === "down" ? "danger" : "warning"}
                >
                  {f.severity === "down" ? "× " : ""}
                  {t(`physical.health.${f.severity}`)}
                </StatusChip>
                <button
                  type="button"
                  className="underline-offset-4 hover:underline"
                  onClick={() => onSelect(pickFault(f))}
                >
                  {f.port !== undefined
                    ? t("physical.panel.faultAt", { node: f.node, port: f.port })
                    : t("physical.panel.unreachableAt", {
                        node: f.node,
                        network: railName.get(f.network ?? "") ?? f.network,
                      })}
                </button>
                <span className="text-xs text-muted-foreground">
                  {[
                    // 본딩으로 접은 구성 포트, 사라진 포트
                    ...f.members.map(
                      (m) => `${m.port} ${t(`physical.health.${m.missing ? "missing" : "down"}`)}`,
                    ),
                    f.members.length === 0 && f.missing ? t("physical.health.missing") : undefined,
                    // 주소를 가진 포트(스토리지·클러스터 링크망)는 VM 수 대신 쓰임·주소(리뷰 2)
                    ...(f.vms === 0 && (f.uses.length > 0 || f.addresses.length > 0)
                      ? [...f.uses.map((u) => t(`physical.use.${u}`)), ...f.addresses]
                      : [t("physical.chain.vms", { count: f.vms })]),
                    f.maintenance ? t("physical.panel.maintenance") : undefined,
                    f.stale ? t("physical.panel.stale") : undefined,
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </span>
                {(f.members.length > 0
                  ? f.members.filter((m) => m.missing).map((m) => m.port)
                  : f.missing && f.port
                    ? [f.port]
                    : []
                ).map((port) => (
                  <ForgetPort key={port} node={f.node} port={port} />
                ))}
              </li>
            ))}
          </ul>
        )}
      </section>
      <p className="text-xs text-muted-foreground">
        {t("physical.panel.rails", { count: rails.length, broken: broken.length })}
      </p>
    </div>
  );
}

/** 레일(운반자 하나 — 브리지·physnet·underlay)의 네트워크마다 노드와 상태 */
function RailDetail({
  carrier,
  rails,
  onSelect,
  ids,
}: {
  carrier: string;
  rails: readonly NetRail[];
  onSelect(s: MapSelection): void;
  ids: Map<string, string>;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-3">
      <h2 className="text-base font-semibold">
        {t("physical.panel.rail", {
          // VXLAN·오버레이의 운반자 글 `underlay`는 말로 바꾼다(지도의 `railName`과 같은 뜻).
          name:
            carrier === "underlay" &&
            rails.some((r) => r.kind === "vxlan" || r.kind === "sdn-overlay")
              ? t("physical.rail.underlay")
              : carrier,
        })}
      </h2>
      {rails.map((rail) => (
        <section key={rail.id} aria-label={rail.name} className="flex flex-col gap-1">
          <h3 className="text-sm font-semibold">{rail.name}</h3>
          <p className="text-xs text-muted-foreground">
            {t(`physical.rail.${rail.kind}`)}
            {rail.vlan != null && ` · VLAN ${rail.vlan}`}
          </p>
          <ul
            aria-label={t("physical.panel.railNodes", { name: rail.name })}
            className="flex flex-col gap-1 text-sm"
          >
            {rail.nodes.map((n) => {
              const id = ids.get(n.node);
              return (
                <li key={n.node} className="flex items-center justify-between gap-2">
                  {id ? (
                    <button
                      type="button"
                      className="underline-offset-4 hover:underline"
                      onClick={() => onSelect({ kind: "node", id })}
                    >
                      {n.node}
                    </button>
                  ) : (
                    <span>{n.node}</span>
                  )}
                  <StatusChip tone={RAIL_TONE[n.state]}>
                    {n.state === "down" ? "× " : ""}
                    {t(`physical.health.${n.state}`)}
                  </StatusChip>
                </li>
              );
            })}
          </ul>
        </section>
      ))}
    </div>
  );
}

function VmDetail({
  vm,
  capacity,
  physical,
  nodeName,
}: {
  vm: Vm;
  capacity: Capacity;
  physical?: PhysicalTopology;
  nodeName(id?: string | null): string;
}) {
  const { t } = useTranslation();
  const nodeId = vm.status.node ?? undefined;
  const cap = capacity.nodes.find((n) => n.id === nodeId);
  const phys = physical?.nodes.find((n) => n.id === nodeId);
  const nodeKey = cap?.node ?? phys?.node ?? "";
  const topology = useNodeTopology(nodeKey, nodeId !== undefined && nodeKey !== "");
  // 디스크의 볼륨만 하나씩 묻는다(클러스터의 볼륨 목록 전체를 지켜보지 않게 — 리뷰 1).
  const volumes = useQueries({
    queries: (vm.spec.disks ?? []).map((d) => ({
      queryKey: ["volumes", "one", d.volume],
      queryFn: () => call(api.GET("/volumes/{key}", { params: { path: { key: d.volume } } })),
      staleTime: 60_000,
      retry: false,
    })),
  });
  const path = vmPath(vm, {
    topology: topology.data,
    capacity: cap,
    physical: phys,
    rails: physical?.networks ?? [],
    volumes: volumes.flatMap((q) => (q.data ? [q.data] : [])),
    sharedPools: capacity.shared_pools,
  });
  return (
    <div className="flex flex-col gap-3">
      {/* VmPeek은 서랍 머리(SheetTitle)를 쓴다 — 열린 채 내용 없는 Sheet가 그 맥락만 준다(넓은 화면 판). */}
      <Sheet open modal={false}>
        <VmPeek vm={vm} nodeName={nodeName} />
      </Sheet>
      <section aria-label={t("physical.vm.path")} className="flex flex-col gap-1 text-sm">
        <h3 className="font-semibold">{t("physical.vm.path")}</h3>
        {nodeId === undefined && (
          <p className="text-muted-foreground">{t("physical.vm.notPlaced")}</p>
        )}
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
          <dt className="text-muted-foreground">{t("physical.vm.cpu")}</dt>
          <dd>
            {path.pinned.length > 0
              ? t("physical.cpu.pinned", { count: path.vcpus, cpus: path.pinned.join(", ") })
              : t("physical.cpu.floating", { count: path.vcpus })}
          </dd>
          <dt className="text-muted-foreground">{t("physical.vm.memory")}</dt>
          <dd>
            {formatMib(path.memoryMib)} ·{" "}
            {path.numa.length > 0
              ? t("physical.vm.numa", { nodes: path.numa.join(", ") })
              : t("physical.vm.unbound")}
            {path.hugepages && ` · hugepages ${path.hugepages}`}
          </dd>
          {path.disks.map((d) => (
            <div key={d.volume} className="contents">
              <dt className="text-muted-foreground">{t("physical.vm.disk")}</dt>
              <dd>
                {d.volume} → {d.pool ?? "—"}
                {d.ready === false && ` (${t("physical.storage.notReady")})`}
              </dd>
            </div>
          ))}
          {path.nics.map((n, i) => (
            <div key={`${n.network}-${i}`} className="contents">
              <dt className="text-muted-foreground">{t("physical.vm.nic")}</dt>
              <dd>
                {[
                  n.network,
                  n.carrier?.label,
                  n.ports
                    .map(
                      (p) =>
                        `${p.health === "down" ? "× " : ""}${p.label} (${t(`physical.health.${p.health}`)})`,
                    )
                    .join(", ") || undefined,
                ]
                  .filter(Boolean)
                  .join(" → ")}
                {n.state && n.state !== "up" && (
                  // 이 노드에 없는 네트워크·끊김은 노드 판과 같이 위험 표시(리뷰 2)
                  <span
                    className={
                      n.state === "absent" || n.state === "down" ? "text-danger-fg" : undefined
                    }
                  >
                    {" · "}
                    {n.state === "absent" || n.state === "down" ? "× " : ""}
                    {t(`physical.health.${n.state}`)}
                  </span>
                )}
              </dd>
            </div>
          ))}
        </dl>
      </section>
    </div>
  );
}

export function DetailPanel({
  selection,
  capacity,
  physical,
  physicalError,
  physicalStale,
  maintenance,
  onRetry,
  onSelect,
}: {
  selection: MapSelection | null;
  capacity: Capacity;
  physical?: PhysicalTopology;
  /** 물리 관계를 받지 못한 까닭(받은 적이 있으면 없음) */
  physicalError?: unknown;
  /** 다시 읽지 못해 마지막 답을 보이는 중 */
  physicalStale?: boolean;
  /** 유지보수 노드 id */
  maintenance?: ReadonlySet<string>;
  onRetry?(): void;
  onSelect(s: MapSelection | null): void;
}) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const nodes = useList("node");
  const nodeName = (id?: string | null) =>
    (id &&
      (nodes.data?.find((n) => n.id === id)?.name ??
        capacity.nodes.find((n) => n.id === id)?.node)) ||
    "—";
  const ids = new Map(capacity.nodes.map((n) => [n.node, n.id]));
  switch (selection?.kind) {
    case "node":
    case "port": {
      const cap =
        selection.kind === "node"
          ? capacity.nodes.find((n) => n.id === selection.id)
          : capacity.nodes.find((n) => n.id === selection.node);
      const phys =
        selection.kind === "node"
          ? physical?.nodes.find((n) => n.id === selection.id)
          : physical?.nodes.find((n) => n.id === selection.node);
      if (!cap && !phys) break;
      return (
        <NodeDetail
          cap={cap}
          physical={phys}
          capacity={capacity}
          rails={physical?.networks ?? []}
          port={selection.kind === "port" ? selection.port : undefined}
          onSelect={onSelect}
        />
      );
    }
    case "vm": {
      const vm = vms.data?.find((v) => v.id === selection.id);
      // 목록을 아직 받지 못했으면 "사라짐"이 아니라 기다림이다(리뷰 2).
      if (!vm && !vms.data) {
        return <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
      }
      if (!vm) break;
      return <VmDetail vm={vm} capacity={capacity} physical={physical} nodeName={nodeName} />;
    }
    case "rail": {
      // 레일 id는 운반자다 — 그 운반자를 쓰는 네트워크들
      const rails = (physical?.networks ?? []).filter((r) => r.carrier === selection.id);
      if (rails.length === 0) break;
      return <RailDetail carrier={selection.id} rails={rails} onSelect={onSelect} ids={ids} />;
    }
    default:
      return (
        <ClusterSummary
          capacity={capacity}
          physical={physical}
          physicalError={physicalError}
          physicalStale={physicalStale}
          maintenance={maintenance}
          onRetry={onRetry}
          onSelect={onSelect}
        />
      );
  }
  // 고른 것이 사라졌다(지운 VM·빠진 노드).
  return (
    <div className="flex flex-col gap-2">
      <p className="text-sm text-muted-foreground">{t("physical.panel.gone")}</p>
      <button type="button" className="self-start text-sm underline" onClick={() => onSelect(null)}>
        {t("physical.panel.back")}
      </button>
    </div>
  );
}
