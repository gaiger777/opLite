import { useQueryClient } from "@tanstack/react-query";
import { cn } from "cn";
import { Plus, Trash2 } from "lucide-react";
import { type KeyboardEvent, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import { putResource } from "@/api/optimistic";
import type { Node } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { formatMib } from "@/lib/format";
import {
  cpuGrid,
  type CpuRole,
  cpuRoles,
  formatCpuList,
  type FullTopology,
  fullTopology,
  NUMA_KEY,
  type NodeTopologyInfo,
  type PageKind,
  pageKib,
  parseCpuList,
  resourcesFrom,
  type ResourcesValues,
  resourcesTo,
  useNodeTopology,
} from "@/numa/numa";

type Topology = FullTopology;

const ROLE_CLASS: Record<CpuRole, string> = {
  reserved: "bg-muted text-muted-foreground",
  shared: "bg-info-soft text-info-fg",
  free: "bg-success-soft text-success-fg",
  used: "bg-brand text-brand-foreground",
};
const ROLES: CpuRole[] = ["reserved", "shared", "free", "used"];

/** 409 중 "그래도 저장"으로 넘길 수 있는 것(서버가 `force`로 쓴다) */
const FORCEABLE = ["host_cpus_in_use", "hugepages_in_use"];

/**
 * 노드 상세의 "NUMA 토폴로지"(설계 §13, 루트 `cluster.read`): 요약과 세 집합, 경고 띠, 호스트 노드 › L3 › 코어 › 스레드
 * 격자(색: 예약·공유·빈 전용·쓰는 전용, 올리면 VM 이름), 호스트 노드 표(CPU·메모리·크기마다 hugepages), 거리 표,
 * "호스트 자원 설정"(admin). 위트니스는 VM을 돌리지 않아 없다.
 */
export function NodeTopologySection({ node }: { node: Node }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const readable = perms.has("cluster.read") && !node.spec.witness;
  const info = useNodeTopology(node.id, readable);
  const [editing, setEditing] = useState(false);
  if (!readable) {
    return null;
  }
  const data = info.data;
  // 서버는 빈 목록을 보내지 않는다(SMT·L3 없는 노드) — 채워 쓴다.
  const topology = data?.topology ? fullTopology(data.topology) : undefined;
  return (
    <section aria-labelledby="node-topology" className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="node-topology" className="font-medium">
          {t("numa.node.title")}
        </h2>
        {perms.has("cluster.admin") && topology && (
          <Button variant="outline" size="sm" onClick={() => setEditing(true)}>
            {t("numa.resources.open")}
          </Button>
        )}
      </div>
      {!data ? (
        <ListPending error={info.error} />
      ) : !topology ? (
        <p className="text-sm text-muted-foreground">{t("numa.node.none")}</p>
      ) : (
        <TopologyView info={data} topology={topology} />
      )}
      {editing && data && topology && (
        <HostResourcesDialog
          node={node}
          info={data}
          topology={topology}
          onClose={() => setEditing(false)}
        />
      )}
    </section>
  );
}

function TopologyView({ info, topology }: { info: NodeTopologyInfo; topology: Topology }) {
  const { t } = useTranslation();
  const roles = cpuRoles(info);
  const warnings = [
    ...(topology.error ? [topology.error] : []),
    ...info.warnings,
    ...(info.live && !info.live.affinity_ok ? [t("numa.node.affinity")] : []),
  ];
  return (
    <>
      <p className="text-sm">
        {t("numa.node.summary", {
          sockets: topology.sockets,
          nodes: topology.nodes.length,
          cpus: parseCpuList(topology.cpus)?.length ?? 0,
          threads: topology.threads_per_core,
        })}
        {" · "}
        {t(topology.pinning ? "numa.node.pinning" : "numa.node.noPinning")}
        {topology.cpuset && ` · ${t("numa.node.cpuset")}`}
      </p>
      {info.sets && (
        <dl className="grid grid-cols-1 gap-2 text-sm sm:grid-cols-3">
          {(["reserved", "dedicated", "shared"] as const).map((k) => (
            <div key={k}>
              <dt className="text-xs text-muted-foreground">{t(`numa.sets.${k}`)}</dt>
              <dd className="font-mono">{info.sets![k] || "—"}</dd>
            </div>
          ))}
        </dl>
      )}
      {warnings.length > 0 && (
        <div
          role="status"
          className="rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm"
        >
          <ul className="list-disc pl-5">
            {warnings.map((w) => (
              <li key={w}>{w}</li>
            ))}
          </ul>
        </div>
      )}
      <CpuGrid topology={topology} roles={roles} />
      <HostNodes info={info} />
      <VmsOnNode info={info} />
      <Distances topology={topology} />
    </>
  );
}

/** 격자: 호스트 노드 › L3 › 코어 › 스레드. `onPick`이 있으면 CPU를 눌러 고른다(호스트 자원 설정). */
function CpuGrid({
  topology,
  roles,
  onPick,
}: {
  topology: Topology;
  roles: Map<number, { role: CpuRole; vm?: string }>;
  onPick?(cpu: number): void;
}) {
  const { t } = useTranslation();
  const grid = cpuGrid(topology);
  // 고르는 격자는 Tab 한 번에 들어가고 화살표로 움직인다(roving tabindex, 리뷰 2 웹 — 256 스레드 노드에서 CPU마다 Tab이었다).
  const order = grid.flatMap((n) => n.l3.flatMap((g) => g.cores.flat()));
  const [active, setActive] = useState<number | undefined>(undefined);
  const current = active !== undefined && order.includes(active) ? active : order[0];
  const box = useRef<HTMLDivElement>(null);
  const move = (e: KeyboardEvent, cpu: number) => {
    const i = order.indexOf(cpu);
    const next = {
      ArrowRight: i + 1,
      ArrowDown: i + 1,
      ArrowLeft: i - 1,
      ArrowUp: i - 1,
      Home: 0,
      End: order.length - 1,
    }[e.key];
    if (next === undefined) {
      return;
    }
    e.preventDefault();
    const to = order[Math.max(0, Math.min(order.length - 1, next))]!;
    setActive(to);
    box.current?.querySelector<HTMLButtonElement>(`[data-cpu="${to}"]`)?.focus();
  };
  return (
    <div className="flex flex-col gap-2">
      <ul aria-label={t("numa.grid.legend")} className="flex flex-wrap gap-3 text-xs">
        {ROLES.map((r) => (
          <li key={r} className="flex items-center gap-1">
            <span className={cn("inline-block size-3 rounded-sm", ROLE_CLASS[r])} />
            {t(`numa.grid.${r}`)}
          </li>
        ))}
      </ul>
      {onPick && <p className="text-xs text-muted-foreground">{t("numa.grid.keys")}</p>}
      <div ref={box} className="flex flex-wrap gap-3">
        {grid.map((n) => (
          <div
            key={n.node}
            role="group"
            aria-label={t("numa.grid.node", { id: n.node })}
            className="flex flex-col gap-1 rounded-md border p-2"
          >
            <span className="text-xs font-medium">{t("numa.grid.node", { id: n.node })}</span>
            <div className="flex flex-wrap gap-2">
              {n.l3.map((group, gi) => (
                <div key={gi} className="flex flex-wrap gap-1 rounded border border-dashed p-1">
                  {group.cores.map((core) => (
                    <div key={core.join(",")} className="flex gap-px rounded bg-border p-px">
                      {core.map((cpu) => {
                        const r = roles.get(cpu) ?? { role: "shared" as const };
                        const label = r.vm
                          ? t("numa.grid.cpuVm", { cpu, role: t(`numa.grid.${r.role}`), vm: r.vm })
                          : t("numa.grid.cpu", { cpu, role: t(`numa.grid.${r.role}`) });
                        const cls = cn(
                          "flex h-6 min-w-7 items-center justify-center rounded-sm px-1 font-mono text-[10px]",
                          ROLE_CLASS[r.role],
                        );
                        return onPick ? (
                          <button
                            key={cpu}
                            type="button"
                            title={label}
                            aria-label={label}
                            data-cpu={cpu}
                            tabIndex={cpu === current ? 0 : -1}
                            className={cn(cls, "cursor-pointer")}
                            onFocus={() => setActive(cpu)}
                            onKeyDown={(e) => move(e, cpu)}
                            onClick={() => onPick(cpu)}
                          >
                            {cpu}
                          </button>
                        ) : (
                          <span
                            key={cpu}
                            title={label}
                            aria-label={label}
                            role="img"
                            className={cls}
                          >
                            {cpu}
                          </span>
                        );
                      })}
                    </div>
                  ))}
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/** 호스트 노드 표: CPU·메모리(빈 것)·전용 사용·크기마다 hugepages(전체·빈 것·Orbivisor·다른 것) */
function HostNodes({ info }: { info: NodeTopologyInfo }) {
  const { t } = useTranslation();
  const live = (id: number) => info.live?.nodes.find((n) => n.id === id);
  return (
    <Table aria-label={t("numa.node.hostNodes")}>
      <TableHeader>
        <TableRow>
          <TableHead>{t("numa.node.hostNode")}</TableHead>
          <TableHead>{t("numa.node.cpus")}</TableHead>
          <TableHead>{t("numa.node.memory")}</TableHead>
          <TableHead>{t("numa.node.dedicated")}</TableHead>
          <TableHead>{t("numa.node.hugepages")}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {info.host_nodes.map((n) => {
          const now = live(n.id);
          const dedicated = parseCpuList(n.dedicated)?.length ?? 0;
          const used = parseCpuList(n.dedicated_used)?.length ?? 0;
          return (
            <TableRow key={n.id}>
              <TableCell>{n.id}</TableCell>
              <TableCell className="font-mono text-xs">{n.cpus}</TableCell>
              <TableCell>
                {formatMib(n.memory_mib)}
                {now && (
                  <span className="block text-xs text-muted-foreground">
                    {t("numa.node.free", { free: formatMib(now.memory_free_mib) })}
                  </span>
                )}
              </TableCell>
              <TableCell>
                {dedicated > 0 ? t("numa.node.usedOf", { used, total: dedicated }) : "—"}
              </TableCell>
              <TableCell>
                {n.hugepages.length === 0
                  ? "—"
                  : n.hugepages.map((p) => {
                      const pool = now?.hugepages?.find((l) => l.size_kib === pageKib(p.size));
                      const other = pool ? Math.max(0, p.total - pool.free - p.used) : undefined;
                      return (
                        <span key={p.size} className="block">
                          {t("numa.node.pages", { size: p.size, total: p.total, used: p.used })}
                          {pool && (
                            <span className="text-xs text-muted-foreground">
                              {" "}
                              {t("numa.node.pagesLive", { free: pool.free, other })}
                            </span>
                          )}
                        </span>
                      );
                    })}
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}

/**
 * 이 노드에 자리 잡은 VM(`info.vms`): 호스트 노드·vCPU의 pCPU·비워 둔 형제·에뮬레이터·hugepages. 격자의 올림 글(title)은
 * 키보드·전화로 볼 수 없어 표로도 보인다(리뷰 2 웹). 없으면 보이지 않는다.
 */
function VmsOnNode({ info }: { info: NodeTopologyInfo }) {
  const { t } = useTranslation();
  if (info.vms.length === 0) {
    return null;
  }
  const list = (s: string) => s || "—";
  return (
    <Table aria-label={t("numa.node.vms")}>
      <TableHeader>
        <TableRow>
          <TableHead>{t("numa.node.vm")}</TableHead>
          <TableHead>{t("numa.vm.hostNodes")}</TableHead>
          <TableHead>{t("numa.node.vcpus")}</TableHead>
          <TableHead>{t("numa.vm.siblings")}</TableHead>
          <TableHead>{t("numa.vm.emulator")}</TableHead>
          <TableHead>{t("numa.node.hugepages")}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {info.vms.map((v) => (
          <TableRow key={v.vm}>
            <TableHead scope="row" className="font-normal">
              {v.vm}
            </TableHead>
            <TableCell>{v.host_nodes.length > 0 ? v.host_nodes.join(", ") : "—"}</TableCell>
            <TableCell className="font-mono text-xs">{list(formatCpuList(v.vcpus))}</TableCell>
            <TableCell className="font-mono text-xs">{list(v.siblings)}</TableCell>
            <TableCell className="font-mono text-xs">{list(v.emulator)}</TableCell>
            <TableCell>
              {v.page ? t("numa.node.vmPages", { count: v.pages, size: v.page }) : "—"}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

/** 호스트 노드 사이 거리(노드가 둘 이상이고 거리를 알 때) */
function Distances({ topology }: { topology: Topology }) {
  const { t } = useTranslation();
  if (topology.nodes.length < 2 || topology.distances.length !== topology.nodes.length) {
    return null;
  }
  return (
    <Table aria-label={t("numa.node.distances")}>
      <TableHeader>
        <TableRow>
          <TableHead>{t("numa.node.distances")}</TableHead>
          {topology.nodes.map((n) => (
            <TableHead key={n.id}>{n.id}</TableHead>
          ))}
        </TableRow>
      </TableHeader>
      <TableBody>
        {topology.nodes.map((n, i) => (
          <TableRow key={n.id}>
            <TableHead scope="row">{n.id}</TableHead>
            {topology.distances[i]!.map((d, j) => (
              <TableCell key={j}>{d}</TableCell>
            ))}
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

/**
 * 호스트 자원 설정(admin, `PUT /nodes/{key}`의 `host_resources`): 예약·전용 cpulist(격자를 눌러 고름), hugepages 예약
 * 행. 409 `host_cpus_in_use`·`hugepages_in_use`면 VM·모자란 것을 보이고 단추가 "그래도 저장"(`?force=true`)이 된다.
 * 모두 비우면 `{}`를 보내 설정을 지운다.
 */
function HostResourcesDialog({
  node,
  info,
  topology,
  onClose,
}: {
  node: Node;
  info: NodeTopologyInfo;
  topology: Topology;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 연 때의 노드(판·스펙·레이블): 그사이 다른 곳에서 바꿨으면 서버가 412로 알린다(리뷰 1 웹 — 지켜보기가 갱신한 판으로
  // 쓰면 남의 설정을 덮거나 지운다).
  const [opened] = useState(node);
  const [values, setValues] = useState<ResourcesValues>(() =>
    resourcesFrom(opened.spec.host_resources),
  );
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [target, setTarget] = useState<"reserved" | "dedicated">("dedicated");
  const [conflict, setConflict] = useState<{ message: string; items: string[] } | null>(null);
  // 모두 비워 설정을 지우기 전 한 번 묻는다.
  const [confirmClear, setConfirmClear] = useState(false);
  const online = parseCpuList(topology.cpus) ?? [];
  // 목록을 고치면 그 칸의 오류("먼저 고치세요" 포함)를 지운다(리뷰 2 웹 — 저장할 때까지 남았다).
  const edit = (field: "reserved" | "dedicated", text: string) => {
    change({ [field]: text });
    setErrors((e) => Object.fromEntries(Object.entries(e).filter(([k]) => k !== field)));
  };
  const change = (patch: Partial<ResourcesValues>) => {
    setValues((v) => ({ ...v, ...patch }));
    setConflict(null);
    setConfirmClear(false);
  };
  // 격자의 색은 입력한 값으로(쓰는 전용 CPU는 지금 응답의 VM)
  const reserved = parseCpuList(values.reserved) ?? [];
  const dedicated = parseCpuList(values.dedicated) ?? [];
  const roles = cpuRoles({
    ...info,
    sets: {
      reserved: formatCpuList(reserved),
      dedicated: formatCpuList(dedicated),
      shared: "",
    },
  });

  function pick(cpu: number) {
    const other = target === "reserved" ? "dedicated" : "reserved";
    const mine = parseCpuList(values[target]);
    const theirs = parseCpuList(values[other]);
    // 고치는 중인(틀린) 목록을 눌러 덮어쓰지 않는다: 먼저 고치라고 알린다.
    if (mine === null) {
      setErrors((e) => ({ ...e, [target]: "numa.resources.errors.fixFirst" }));
      return;
    }
    change({
      [target]: formatCpuList(mine.includes(cpu) ? mine.filter((c) => c !== cpu) : [...mine, cpu]),
      ...(theirs !== null && { [other]: formatCpuList(theirs.filter((c) => c !== cpu)) }),
    });
  }

  async function submit() {
    const { errors: found, value } = resourcesTo(values, online);
    setErrors(found);
    if (!value) {
      return false;
    }
    // `{}`는 지우기다: 설정이 없었으면 보낼 것이 없고, 있었으면 한 번 묻는다.
    if (Object.keys(value).length === 0) {
      if (!opened.spec.host_resources) {
        return;
      }
      if (!confirmClear) {
        setConfirmClear(true);
        return false;
      }
    }
    let response: Response | undefined;
    try {
      const updated = await call(
        api
          .PUT("/nodes/{key}", {
            params: {
              path: { key: opened.id },
              header: { "If-Match": opened.revision },
              ...(conflict && { query: { force: true } }),
            },
            body: { labels: opened.labels, spec: { ...opened.spec, host_resources: value } },
          })
          .then((result) => {
            response = result.response;
            return result;
          }),
      );
      putResource(queryClient, "node", updated);
    } catch (err) {
      if (err instanceof ApiError && FORCEABLE.includes(err.code) && !conflict) {
        const d = (err.details ?? {}) as { vms?: string[]; short?: string[] };
        setConflict({ message: err.message, items: d.vms ?? d.short ?? [] });
        return false;
      }
      throw err;
    }
    await queryClient.invalidateQueries({ queryKey: NUMA_KEY });
    const warning = response?.headers.get("orbivisor-warning");
    if (warning) {
      toast.warning(warning);
    } else {
      toast.success(t("numa.resources.saved", { name: node.name }));
    }
  }

  return (
    <FormDialog
      title={t("numa.resources.title", { name: node.name })}
      description={t("numa.resources.description")}
      submitLabel={
        conflict
          ? t("numa.resources.force")
          : confirmClear
            ? t("numa.resources.clear")
            : t("common.save")
      }
      destructive={conflict !== null || confirmClear}
      className="sm:max-w-3xl"
      onClose={onClose}
      onSubmit={submit}
    >
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <Field
          id="res-reserved"
          label={t("numa.sets.reserved")}
          error={errors.reserved}
          hint={t("numa.resources.reservedHint")}
        >
          {(a11y) => (
            <Input
              id="res-reserved"
              className="font-mono"
              placeholder="0-1"
              value={values.reserved}
              onChange={(e) => edit("reserved", e.target.value)}
              {...a11y}
            />
          )}
        </Field>
        <Field
          id="res-dedicated"
          label={t("numa.sets.dedicated")}
          error={errors.dedicated}
          hint={t("numa.resources.dedicatedHint")}
        >
          {(a11y) => (
            <Input
              id="res-dedicated"
              className="font-mono"
              placeholder="4-31"
              value={values.dedicated}
              onChange={(e) => edit("dedicated", e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      </div>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span id="res-pick">{t("numa.resources.pick")}</span>
        <ToggleGroup
          type="single"
          variant="outline"
          size="sm"
          aria-labelledby="res-pick"
          value={target}
          onValueChange={(v) => v && setTarget(v as "reserved" | "dedicated")}
        >
          <ToggleGroupItem value="reserved">{t("numa.sets.reserved")}</ToggleGroupItem>
          <ToggleGroupItem value="dedicated">{t("numa.sets.dedicated")}</ToggleGroupItem>
        </ToggleGroup>
      </div>
      <CpuGrid topology={topology} roles={roles} onPick={pick} />
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1 text-sm font-medium">{t("numa.resources.hugepages")}</legend>
        <p className="text-xs text-muted-foreground">{t("numa.resources.hugepagesHint")}</p>
        {values.hugepages.map((h, i) => {
          const set = (patch: Partial<(typeof values.hugepages)[number]>) =>
            change({
              hugepages: values.hugepages.map((r, j) => (j === i ? { ...r, ...patch } : r)),
            });
          return (
            <div
              key={i}
              role="group"
              aria-label={t("numa.resources.reserve", { n: i + 1 })}
              className="flex flex-wrap items-end gap-2"
            >
              <Field id={`res-hp-${i}-size`} label={t("numa.resources.size")}>
                <NativeSelect
                  id={`res-hp-${i}-size`}
                  value={h.size}
                  onChange={(e) => set({ size: e.target.value as PageKind })}
                >
                  <NativeSelectOption value="2M">2M</NativeSelectOption>
                  <NativeSelectOption value="1G">1G</NativeSelectOption>
                </NativeSelect>
              </Field>
              <Field
                id={`res-hp-${i}-count`}
                label={t("numa.resources.count")}
                error={errors[`hugepages.${i}.count`]}
              >
                {(a11y) => (
                  <Input
                    id={`res-hp-${i}-count`}
                    inputMode="numeric"
                    className="w-28"
                    value={h.count}
                    onChange={(e) => set({ count: e.target.value })}
                    {...a11y}
                  />
                )}
              </Field>
              <Field id={`res-hp-${i}-node`} label={t("numa.resources.hostNode")}>
                <NativeSelect
                  id={`res-hp-${i}-node`}
                  value={h.hostNode}
                  onChange={(e) => set({ hostNode: e.target.value })}
                >
                  <NativeSelectOption value="">{t("numa.resources.anyNode")}</NativeSelectOption>
                  {topology.nodes.map((n) => (
                    <NativeSelectOption key={n.id} value={String(n.id)}>
                      {n.id}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </Field>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={t("numa.resources.removeReserve", { n: i + 1 })}
                onClick={() => change({ hugepages: values.hugepages.filter((_, j) => j !== i) })}
              >
                <Trash2 />
              </Button>
            </div>
          );
        })}
        <div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() =>
              change({ hugepages: [...values.hugepages, { size: "2M", count: "", hostNode: "" }] })
            }
          >
            <Plus />
            {t("numa.resources.addReserve")}
          </Button>
        </div>
      </fieldset>
      {confirmClear && (
        <p
          role="alert"
          className="rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm"
        >
          {t("numa.resources.clearConfirm")}
        </p>
      )}
      {conflict && (
        <div
          role="alert"
          className="rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm"
        >
          <p>{conflict.message}</p>
          {conflict.items.length > 0 && (
            <ul className="list-disc pl-5">
              {conflict.items.map((v) => (
                <li key={v}>{v}</li>
              ))}
            </ul>
          )}
          <p className="mt-1 text-xs">{t("numa.resources.forceHint")}</p>
        </div>
      )}
    </FormDialog>
  );
}
