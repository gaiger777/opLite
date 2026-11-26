// 자세히 판(노드, 계획 75 §4.1): 네 탭이 아니라 한 열의 네 칸 — CPU(스레드 격자·오버커밋), 메모리(NUMA별·hugepages·상위 VM),
// 스토리지(풀 막대·씬 비율), 네트워크(사슬도). 포트를 고르면 네트워크 칸만 보이고 그 포트를 강조한다.
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { StatusChip } from "@/components/StatusChip";
import { ChainDiagram } from "@/dashboard/ChainDiagram";
import type { MapSelection } from "@/dashboard/selection";
import { cpuText, memoryText, storageText } from "@/dashboard/texts";
import { UsageBar } from "@/dashboard/UsageBar";
import { formatCapacity, formatMib } from "@/lib/format";
import { useNodeTopology } from "@/numa/numa";
import {
  type Capacity,
  type NetRail,
  type NodeCapacity,
  type NodePhysical,
  useNodeSample,
} from "@/physical/api";
import { capacityRow } from "@/physical/capacity";
import { buildChain } from "@/physical/chain";
import { THREAD_TOKEN, tokenColor } from "@/physical/colors";
import { type ThreadCell, threadGroups, threadsOf } from "@/physical/threads";
import { vcpus } from "@/vms/VmListPage";

/** QEMU가 떠 있는 단계(용량의 `vcpus_allocated`와 같은 규칙) */
const PLACED: Vm["status"]["phase"][] = ["starting", "running", "stopping", "migrating"];

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  const id = useId();
  return (
    // 랜드마크가 아니라 묶음이다(홈의 차트 칸과 이름이 겹친다).
    <section
      role="group"
      aria-labelledby={id}
      className="flex flex-col gap-2 border-t pt-3 first:border-t-0 first:pt-0"
    >
      <h3 id={id} className="text-sm font-semibold">
        {title}
      </h3>
      {children}
    </section>
  );
}

function hugepageText(pages: readonly { size_kib: number; total: number; used: number }[]) {
  return pages
    .map(
      (h) =>
        `${h.size_kib >= 1048576 ? `${h.size_kib / 1048576}G` : `${h.size_kib / 1024}M`} ${h.used}/${h.total}`,
    )
    .join(", ");
}

function CpuSection({ cap, vms, nodeName }: { cap: NodeCapacity; vms: Vm[]; nodeName: string }) {
  const { t } = useTranslation();
  const topology = useNodeTopology(nodeName);
  const [hover, setHover] = useState<string | null>(null);
  const groups = threadGroups(cap, topology.data);
  const row = capacityRow(cap);
  const pinned = hover ? new Set(threadsOf(topology.data, hover)) : null;
  // 공유 VM이면 공유 칸 전체가 그 VM의 자리다.
  const lit = (c: ThreadCell) =>
    pinned !== null && (pinned.size > 0 ? pinned.has(c.cpu) : c.kind === "shared");
  const count = (kind: ThreadCell["kind"]) =>
    groups.reduce((s, g) => s + g.cells.filter((c) => c.kind === kind).length, 0);
  const summary = t("physical.cpu.gridSummary", {
    threads: cap.cpu.threads,
    reserved: count("reserved") || cap.cpu.reserved,
    dedicated: count("dedicated") || cap.cpu.dedicated_total,
    used: cap.cpu.dedicated_used,
    shared: count("shared"),
  });
  const top = [...vms]
    .sort((a, b) => vcpus(b) - vcpus(a) || a.name.localeCompare(b.name))
    .slice(0, 8);
  return (
    <Section title={t("physical.cpu.title")}>
      <div role="img" aria-label={summary} className="flex flex-col gap-2">
        {groups.map((g) => (
          <div key={g.numa ?? "all"} className="flex flex-col gap-1">
            {g.numa !== undefined && (
              <span className="text-xs text-muted-foreground">
                {t("physical.cpu.numa", { id: g.numa })}
              </span>
            )}
            <div className="flex flex-wrap gap-0.5">
              {g.cells.map((c) => (
                <span
                  key={c.cpu}
                  data-cpu={c.cpu}
                  data-kind={c.kind}
                  data-lit={lit(c) || undefined}
                  title={`CPU ${c.cpu} — ${t(`physical.cpu.kind.${c.kind}`)}${c.vm ? ` (${c.vm})` : ""}`}
                  className="size-3.5 rounded-[2px] border data-[lit]:ring-2 data-[lit]:ring-ring"
                  style={{
                    borderColor: tokenColor(THREAD_TOKEN[c.kind]),
                    background:
                      c.kind === "shared"
                        ? "transparent"
                        : c.kind === "reserved"
                          ? `repeating-linear-gradient(135deg, ${tokenColor(THREAD_TOKEN.reserved)} 0 2px, transparent 2px 4px)`
                          : c.vm
                            ? tokenColor(THREAD_TOKEN.dedicated)
                            : `color-mix(in oklab, ${tokenColor(THREAD_TOKEN.dedicated)} 30%, transparent)`,
                  }}
                />
              ))}
            </div>
          </div>
        ))}
      </div>
      <p className="text-xs text-muted-foreground">{summary}</p>
      <p className="text-xs text-muted-foreground">{t("physical.cpu.legend")}</p>
      <p className="text-sm">
        {t("physical.cpu.shared", {
          vcpus: cap.cpu.vcpus_shared,
          threads: row.cpu.total,
          times: cap.cpu.overcommit === undefined ? "—" : `×${cap.cpu.overcommit.toFixed(1)}`,
        })}
      </p>
      <UsageBar
        label={t("physical.table.cpu")}
        m={row.cpu}
        text={cpuText(t, row.cpu, row.cpuOvercommit, row.cpuUsePct, row.cpuDedicated)}
      />
      {top.length > 0 && (
        <ul aria-label={t("physical.cpu.vms")} className="flex flex-col gap-0.5 text-xs">
          {top.map((vm) => {
            const pins = threadsOf(topology.data, vm.name);
            return (
              <li key={vm.id}>
                <button
                  type="button"
                  className="underline-offset-4 hover:underline"
                  onMouseEnter={() => setHover(vm.name)}
                  onMouseLeave={() => setHover(null)}
                  onFocus={() => setHover(vm.name)}
                  onBlur={() => setHover(null)}
                >
                  {vm.name}
                </button>{" "}
                <span className="text-muted-foreground">
                  {pins.length > 0
                    ? t("physical.cpu.pinned", { count: vcpus(vm), cpus: pins.join(", ") })
                    : t("physical.cpu.floating", { count: vcpus(vm) })}
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </Section>
  );
}

function MemorySection({ cap, vms }: { cap: NodeCapacity; vms: Vm[] }) {
  const { t } = useTranslation();
  const row = capacityRow(cap);
  const top = [...vms]
    .sort((a, b) => b.spec.memory_mib - a.spec.memory_mib || a.name.localeCompare(b.name))
    .slice(0, 5);
  return (
    <Section title={t("physical.memory.title")}>
      <UsageBar
        label={t("physical.table.memory")}
        m={row.memory}
        text={memoryText(t, row.memory, row.memoryOvercommit)}
      />
      {(cap.memory.hugepages ?? []).length > 0 && (
        <p className="text-xs text-muted-foreground">
          {t("physical.memory.hugepages", {
            pages: hugepageText(cap.memory.hugepages ?? []),
            reserved: formatCapacity(cap.memory.reserved_mib),
          })}
        </p>
      )}
      {(cap.numa ?? []).map((n) => (
        <div key={n.id} className="flex flex-col gap-0.5">
          <UsageBar
            label={t("physical.cpu.numa", { id: n.id })}
            m={{ total: n.memory_mib, allocated: n.bound_mib }}
            text={t("physical.memory.numa", {
              bound: formatCapacity(n.bound_mib),
              total: formatCapacity(n.memory_mib),
            })}
          />
          {(n.hugepages ?? []).length > 0 && (
            <span className="text-xs text-muted-foreground">
              {t("physical.memory.numaPages", { pages: hugepageText(n.hugepages ?? []) })}
            </span>
          )}
        </div>
      ))}
      {top.length > 0 && (
        <ul aria-label={t("physical.memory.vms")} className="flex flex-col gap-0.5 text-xs">
          {top.map((vm) => (
            <li key={vm.id} className="flex justify-between gap-2">
              <span>{vm.name}</span>
              <span className="tabular-nums text-muted-foreground">
                {formatMib(vm.spec.memory_mib)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

export function PoolBars({
  pools,
}: {
  pools: readonly {
    pool: string;
    total_bytes: number;
    used_bytes: number;
    provisioned_bytes: number;
    ready: boolean;
    note?: string;
  }[];
}) {
  const { t } = useTranslation();
  return pools.map((p) => {
    const m = { total: p.total_bytes, allocated: p.provisioned_bytes, used: p.used_bytes };
    const thin = p.total_bytes > 0 ? p.provisioned_bytes / p.total_bytes : undefined;
    return (
      <div key={p.pool} className="flex flex-col gap-0.5">
        <div className="flex items-center gap-2 text-sm">
          <span className="font-medium">{p.pool}</span>
          <StatusChip tone={p.ready ? "success" : "danger"}>
            {t(p.ready ? "physical.storage.ready" : "physical.storage.notReady")}
          </StatusChip>
          {p.note && <span className="text-xs text-muted-foreground">{p.note}</span>}
        </div>
        <UsageBar
          compact
          label={t("physical.storage.poolOf", { pool: p.pool })}
          m={m}
          text={[
            storageText(t, m),
            thin === undefined ? undefined : t("physical.storage.thin", { times: thin.toFixed(2) }),
          ]
            .filter(Boolean)
            .join(" · ")}
        />
      </div>
    );
  });
}

function StorageSection({ cap, shared }: { cap: NodeCapacity; shared: Capacity["shared_pools"] }) {
  const { t } = useTranslation();
  return (
    <Section title={t("physical.storage.title")}>
      {cap.storage.length === 0 && shared.length === 0 && (
        <p className="text-sm text-muted-foreground">{t("physical.storage.none")}</p>
      )}
      <PoolBars pools={cap.storage} />
      {shared.length > 0 && (
        <>
          <span className="text-xs text-muted-foreground">{t("physical.storage.shared")}</span>
          <PoolBars
            pools={shared.map((p) => ({
              ...p,
              ready: p.nodes_ready > 0,
              note: t("physical.storage.nodesReady", { ready: p.nodes_ready, nodes: p.nodes }),
            }))}
          />
        </>
      )}
    </Section>
  );
}

export function NetworkSection({
  node,
  rails,
  vms,
  highlight,
}: {
  node: NodePhysical;
  rails: readonly NetRail[];
  vms: Vm[];
  highlight?: string;
}) {
  const { t } = useTranslation();
  const sample = useNodeSample(node.node, node.online);
  const byName = new Map(rails.map((r) => [r.name, r.id]));
  const counts = new Map<string, number>();
  for (const vm of vms) {
    const nets = new Set((vm.spec.nics ?? []).flatMap((n) => byName.get(n.network) ?? []));
    for (const id of nets) counts.set(id, (counts.get(id) ?? 0) + 1);
  }
  const chain = buildChain(node, rails, counts);
  const names = new Map(rails.map((r) => [r.id, r.name]));
  return (
    <Section title={t("physical.network.title")}>
      {node.legacy && <p className="text-xs text-warning-fg">{t("physical.network.legacy")}</p>}
      {!node.online && (
        <p className="text-xs text-muted-foreground">
          {t("physical.table.offline")} · {t("physical.panel.stale")}
        </p>
      )}
      {(node.unreachable ?? []).map((u) => (
        <p key={u.network} className="text-xs text-danger-fg">
          {"× "}
          {t("physical.panel.unreachableAt", {
            node: node.node,
            network: names.get(u.network) ?? u.network,
          })}{" "}
          · {t("physical.chain.vms", { count: u.vms })}
        </p>
      ))}
      <ChainDiagram chain={chain} highlight={highlight} samples={sample.data?.interfaces} />
    </Section>
  );
}

/** 노드 하나의 판. `cap`·`node`가 없으면(용량·물리 응답에 없는 노드) 그 칸을 건너뛴다. */
export function NodeDetail({
  cap,
  physical,
  capacity,
  rails,
  port,
  onSelect,
}: {
  cap?: NodeCapacity;
  physical?: NodePhysical;
  capacity: Capacity;
  rails: readonly NetRail[];
  /** 포트를 골랐으면 그 이름(네트워크 칸만 보인다) */
  port?: string;
  onSelect(s: MapSelection): void;
}) {
  const { t } = useTranslation();
  const id = cap?.id ?? physical?.id;
  const name = cap?.node ?? physical?.node ?? "";
  const vms = (useList("vm").data ?? []).filter(
    (vm) => vm.status.node === id && PLACED.includes(vm.status.phase),
  );
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center justify-between gap-2">
        <h2 className="truncate text-base font-semibold">
          {port
            ? t("physical.panel.port", { node: name, port })
            : t("physical.panel.node", { node: name })}
        </h2>
        {port && id && (
          <button
            type="button"
            className="text-xs underline-offset-4 hover:underline"
            onClick={() => onSelect({ kind: "node", id })}
          >
            {t("physical.panel.wholeNode")}
          </button>
        )}
      </div>
      {cap && !cap.online && (
        <p className="text-sm text-muted-foreground">{t("physical.table.offline")}</p>
      )}
      {!port && cap && (
        <>
          <CpuSection cap={cap} vms={vms} nodeName={name} />
          <MemorySection cap={cap} vms={vms} />
          <StorageSection cap={cap} shared={capacity.shared_pools} />
        </>
      )}
      {physical ? (
        <NetworkSection node={physical} rails={rails} vms={vms} highlight={port} />
      ) : (
        <p className="text-sm text-muted-foreground">{t("physical.network.unknown")}</p>
      )}
    </div>
  );
}
