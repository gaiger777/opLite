// NUMA·전용 CPU·hugepages(계획 64): 서버 응답을 묻는 훅과 화면이 쓰는 순수 함수.
import { useQuery } from "@tanstack/react-query";
import type { TFunction } from "i18next";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { PendingChange, RunningHardware, Vm, VmSpec } from "@/api/types";

type Schemas = components["schemas"];
export type CpuPolicy = Schemas["CpuPolicy"];
export type ThreadPolicy = Schemas["ThreadPolicy"];
export type EmulatorPolicy = Schemas["EmulatorPolicy"];
export type MemoryPolicy = Schemas["MemoryPolicy"];
export type PageSize = Schemas["PageSize"];
export type PageKind = Schemas["PageKind"];
export type NumaSpec = Schemas["NumaSpec"];
export type HostResources = Schemas["HostResources"];
export type HugepageReserve = Schemas["HugepageReserve"];
export type NodeTopologyInfo = Schemas["NodeTopologyInfo"];
export type NodeUsageSummary = Schemas["NodeUsageSummary"];
export type NumaNodeFit = Schemas["NumaNodeFit"];

/** 서버의 `MAX_NUMA_NODES` */
export const MAX_NUMA_NODES = 8;
/** 서버의 `MAX_CPU_ID` */
export const MAX_CPU_ID = 4095;

/** NUMA 질의의 캐시 키 머리. 노드 설정을 저장하면 이 아래를 모두 다시 읽는다. */
export const NUMA_KEY = ["numa"] as const;

/** 노드의 토폴로지·세 집합·사용(`GET /nodes/{key}/topology`, 루트 `cluster.read`) */
export function useNodeTopology(key: string, enabled = true) {
  return useQuery({
    queryKey: [...NUMA_KEY, "node", key],
    queryFn: () => call(api.GET("/nodes/{key}/topology", { params: { path: { key } } })),
    staleTime: 10_000,
    retry: false,
    enabled,
  });
}

/** 노드마다 전용 CPU·hugepages 사용 요약(`GET /topology/usage`, 루트 `cluster.read`) */
export function useTopologyUsage(enabled = true) {
  return useQuery({
    queryKey: [...NUMA_KEY, "usage"],
    queryFn: () => call(api.GET("/topology/usage")),
    staleTime: 10_000,
    retry: false,
    enabled,
  });
}

/** VM의 NUMA와 노드마다 받을 수 있는지(`GET /vms/{key}/numa`, 마이그레이션 대화상자) */
export function useVmNuma(key: string, enabled = true) {
  return useQuery({
    queryKey: [...NUMA_KEY, "vm", key],
    queryFn: () => call(api.GET("/vms/{key}/numa", { params: { path: { key } } })),
    staleTime: 5_000,
    retry: false,
    enabled,
  });
}

/** Linux cpulist(`0-3,8,10-11`)를 번호로. 틀린 글이면 null, 빈 글이면 빈 목록이다. */
export function parseCpuList(text: string): number[] | null {
  const out = new Set<number>();
  const s = text.trim();
  if (s === "") {
    return [];
  }
  for (const part of s.split(",")) {
    const m = /^\s*(\d+)\s*(?:-\s*(\d+)\s*)?$/.exec(part);
    if (!m) {
      return null;
    }
    const a = Number(m[1]);
    const b = m[2] === undefined ? a : Number(m[2]);
    if (b < a || b > MAX_CPU_ID) {
      return null;
    }
    for (let i = a; i <= b; i++) {
      out.add(i);
    }
  }
  return [...out].sort((x, y) => x - y);
}

/** 번호를 맞춘 cpulist로(오름차순, 범위 합침) — 서버의 `CpuSet` 꼴 */
export function formatCpuList(cpus: Iterable<number>): string {
  const sorted = [...new Set(cpus)].sort((a, b) => a - b);
  const parts: string[] = [];
  for (let i = 0; i < sorted.length;) {
    let j = i;
    while (j + 1 < sorted.length && sorted[j + 1] === sorted[j]! + 1) {
      j++;
    }
    parts.push(j === i ? String(sorted[i]) : `${sorted[i]}-${sorted[j]}`);
    i = j + 1;
  }
  return parts.join(",");
}

/** 이 VM 스펙이 계획 64 칸을 하나라도 쓴다(서버의 `numa::in_use`) */
export function numaInUse(spec: VmSpec): boolean {
  return (
    (spec.cpu.policy ?? "shared") !== "shared" ||
    spec.cpu.thread_policy != null ||
    spec.cpu.emulator != null ||
    spec.numa != null ||
    spec.hugepages != null
  );
}

/** 전용 CPU나 hugepages를 쓴다(만들기·바꾸기·복제에 `vm.dedicated`가 든다, 설계 §12) */
export function usesHostResources(spec: VmSpec): boolean {
  return (spec.cpu.policy ?? "shared") === "dedicated" || spec.hugepages != null;
}

/** 보류 `numa`(다음 전원 껐다 켜기 때 배치가 바뀜). 없으면 undefined */
export function numaPending(vm: Vm): PendingChange | undefined {
  return (vm.status.hardware?.pending ?? []).find((c) => c.field === "numa");
}

/** VM 폼의 vNUMA 나누기: 없음·자동·노드 수·노드마다 직접 */
export type NumaMode = "off" | "auto" | "count" | "list";

/** 직접 나누기의 게스트 노드 하나(vCPU 자리 수와 MiB) */
export interface NumaNodeRow {
  cpus: string;
  memory: string;
}

/** VM 폼·복제·배포·가져오기의 CPU 배치와 NUMA 칸. 빈 문자열은 "주지 않음"(서버 기본)이다. */
export interface NumaValues {
  policy: CpuPolicy;
  threadPolicy: ThreadPolicy | "";
  emulator: EmulatorPolicy | "";
  mode: NumaMode;
  /** `mode`가 `count`일 때 게스트 노드 수 */
  count: string;
  /** `mode`가 `list`일 때 노드마다 */
  list: NumaNodeRow[];
  memory: MemoryPolicy | "";
  hugepages: PageSize | "";
}

export function emptyNuma(): NumaValues {
  return {
    policy: "shared",
    threadPolicy: "",
    emulator: "",
    mode: "off",
    count: "1",
    list: [],
    memory: "",
    hugepages: "",
  };
}

/** VM 스펙의 칸을 폼 값으로 */
export function numaFrom(spec: VmSpec): NumaValues {
  const nodes = spec.numa ? (spec.numa.nodes ?? "auto") : null;
  return {
    policy: spec.cpu.policy ?? "shared",
    threadPolicy: spec.cpu.thread_policy ?? "",
    emulator: spec.cpu.emulator ?? "",
    mode:
      nodes === null
        ? "off"
        : nodes === "auto"
          ? "auto"
          : typeof nodes === "number"
            ? "count"
            : "list",
    count: typeof nodes === "number" ? String(nodes) : "1",
    list: Array.isArray(nodes)
      ? nodes.map((n) => ({ cpus: String(n.cpus), memory: String(n.memory_mib) }))
      : [],
    memory: spec.numa?.memory ?? "",
    hugepages: spec.hugepages ?? "",
  };
}

/** 폼 값을 스펙 칸으로. 모두 기본이면 빈 객체 — 옛 클러스터가 아는 스펙 그대로다(설계 §1 "바이트까지 같다"). */
export function numaSpec(v: NumaValues): {
  cpu: Pick<Schemas["CpuSpec"], "policy" | "thread_policy" | "emulator">;
  numa?: NumaSpec;
  hugepages?: PageSize;
} {
  const dedicated = v.policy === "dedicated";
  const nodes: NumaSpec["nodes"] | undefined =
    v.mode === "auto"
      ? "auto"
      : v.mode === "count"
        ? Number(v.count)
        : v.mode === "list"
          ? v.list.map((n) => ({ cpus: Number(n.cpus), memory_mib: Number(n.memory) }))
          : undefined;
  return {
    cpu: dedicated
      ? {
          policy: "dedicated",
          ...(v.threadPolicy !== "" && { thread_policy: v.threadPolicy }),
          ...(v.emulator !== "" && { emulator: v.emulator }),
        }
      : {},
    ...(nodes !== undefined && {
      numa: { nodes, ...(v.memory !== "" && { memory: v.memory }) },
    }),
    ...(v.hugepages !== "" && { hugepages: v.hugepages }),
  };
}

/** 소켓 수의 약수 가운데 게스트 노드 수로 고를 수 있는 것(1~8, 노드는 소켓 단위 — 설계 §4) */
export function nodeCounts(sockets: number): number[] {
  const out: number[] = [];
  for (let d = 1; d <= Math.min(sockets, MAX_NUMA_NODES); d++) {
    if (sockets % d === 0) {
      out.push(d);
    }
  }
  return out;
}

/** 입력 오류(필드 → 번역 키): 서버의 `numa::validate`의 흔한 것. 나머지는 서버가 400 `invalid_numa`로 알린다. */
export function numaErrors(
  v: NumaValues,
  vm: { sockets: number; cores: string; maxVcpus: string; memory: string; maxMemory: string },
): Record<string, string> {
  const errors: Record<string, string> = {};
  const sockets = Math.max(1, vm.sockets);
  const slots = vm.maxVcpus !== "" ? Number(vm.maxVcpus) : sockets * Number(vm.cores);
  const perSocket = slots / sockets;
  if (
    v.policy === "dedicated" &&
    v.threadPolicy === "require" &&
    Number.isInteger(perSocket) &&
    perSocket % 2 !== 0
  ) {
    errors["numa.threads"] = "numa.errors.requireOdd";
  }
  if (v.hugepages === "1G" && vm.maxMemory !== "" && Number(vm.maxMemory) > Number(vm.memory)) {
    errors["numa.hugepages"] = "numa.errors.hugepagesHotplug";
  }
  if (v.mode === "count" && !nodeCounts(sockets).includes(Number(v.count))) {
    errors["numa.count"] = "numa.errors.count";
  }
  if (v.mode === "list") {
    if (v.list.length < 1 || v.list.length > MAX_NUMA_NODES) {
      errors["numa.list"] = "numa.errors.listCount";
      return errors;
    }
    let cpus = 0;
    let memory = 0;
    v.list.forEach((n, i) => {
      const c = Number(n.cpus);
      if (!/^\d+$/.test(n.cpus) || c === 0 || !Number.isInteger(perSocket) || c % perSocket !== 0) {
        errors[`numa.list.${i}.cpus`] = "numa.errors.listCpus";
      }
      if (!/^\d+$/.test(n.memory) || Number(n.memory) === 0) {
        errors[`numa.list.${i}.memory`] = "numa.errors.listMemory";
      }
      cpus += c;
      memory += Number(n.memory);
    });
    if (Object.keys(errors).length === 0 && cpus !== slots) {
      errors["numa.list"] = "numa.errors.listCpusSum";
    } else if (Object.keys(errors).length === 0 && memory !== Number(vm.memory)) {
      errors["numa.list"] = "numa.errors.listMemorySum";
    }
  }
  return errors;
}

/** 노드 수를 "2 nodes"처럼 */
function nodesText(t: TFunction, count: number): string {
  return t("numa.summary.nodes", { count });
}

/** 스펙의 배치를 짧게: `2 nodes, dedicated (isolate threads), hugepages 1G`(서버의 `describe_spec`). 안 쓰면 "없음" */
export function describeNuma(t: TFunction, spec: VmSpec): string {
  if (!numaInUse(spec)) {
    return t("numa.summary.none");
  }
  const nodes = spec.numa?.nodes ?? "auto";
  const parts = [
    nodes === "auto"
      ? t("numa.summary.auto")
      : nodesText(t, typeof nodes === "number" ? nodes : nodes.length),
  ];
  if ((spec.cpu.policy ?? "shared") === "dedicated") {
    parts.push(policyText(t, spec.cpu.thread_policy, spec.cpu.emulator));
  }
  if (spec.hugepages) {
    parts.push(t("numa.summary.hugepages", { size: spec.hugepages }));
  }
  if (spec.numa?.memory) {
    parts.push(t(`numa.memory.${spec.numa.memory}`));
  }
  return parts.join(", ");
}

function policyText(
  t: TFunction,
  thread: ThreadPolicy | undefined,
  emulator: EmulatorPolicy | undefined,
): string {
  const extra = [
    ...(thread && thread !== "prefer" ? [t(`numa.threads.${thread}`)] : []),
    ...(emulator === "isolate" ? [t("numa.emulator.isolate")] : []),
  ];
  return extra.length > 0
    ? `${t("numa.policy.dedicated")} (${extra.join(", ")})`
    : t("numa.policy.dedicated");
}

/** 켤 때 기록한 게스트 모양을 짧게: `2 nodes × (4 vCPU slots, 8 GiB), dedicated, hugepages 1G` */
export function describeRunning(t: TFunction, hw: RunningHardware): string | null {
  const numa = hw.numa;
  if (!numa) {
    return null;
  }
  const perSocket = slotsPerSocket(hw.cpu);
  const parts = [
    t("numa.summary.running", {
      count: numa.nodes.length,
      nodes: numa.nodes
        .map((n) =>
          t("numa.summary.node", { cpus: n.sockets.length * perSocket, memory: n.memory_mib }),
        )
        .join(" + "),
    }),
  ];
  if (numa.policy === "dedicated") {
    parts.push(policyText(t, numa.thread_policy, numa.emulator));
  }
  if (numa.page) {
    parts.push(t("numa.summary.hugepages", { size: numa.page }));
  }
  if (numa.memory) {
    parts.push(t(`numa.memory.${numa.memory}`));
  }
  return parts.join(", ");
}

/** 소켓마다 vCPU 자리 수(핫플러그 상한 기준, 서버의 `slots_per_socket`) */
export function slotsPerSocket(cpu: Schemas["CpuSpec"]): number {
  const sockets = Math.max(1, cpu.sockets ?? 1);
  return Math.max(1, Math.floor((cpu.max_vcpus ?? sockets * cpu.cores) / sockets));
}

/** vCPU 고정 표의 한 줄: 자리 번호, 게스트 노드, 호스트 NUMA 노드, pCPU(공유 VM은 없음) */
export interface PinRow {
  slot: number;
  guestNode: number | null;
  hostNode: number | null;
  pcpu: number | null;
}

/** 기록(`hardware.numa`·`host_placement`)으로 vCPU 자리마다 고정을 */
export function pinningRows(hw: RunningHardware): PinRow[] {
  const placement = hw.host_placement;
  const perSocket = slotsPerSocket(hw.cpu);
  const slots = Math.max(placement?.vcpus?.length ?? 0, (hw.cpu.sockets ?? 1) * perSocket);
  const guestOf = (slot: number) => {
    const socket = Math.floor(slot / perSocket);
    const i = (hw.numa?.nodes ?? []).findIndex((n) => n.sockets.includes(socket));
    return i === -1 ? null : i;
  };
  return Array.from({ length: slots }, (_, slot) => {
    const guestNode = guestOf(slot);
    return {
      slot,
      guestNode,
      hostNode: guestNode === null ? null : (placement?.host_nodes?.[guestNode] ?? null),
      pcpu: placement?.vcpus?.[slot] ?? null,
    };
  });
}

/** 전용 VM이 잡을 pCPU 수 어림: vCPU 자리(상한까지) + 에뮬레이터 `isolate` 1(설계 §8 `demand`, 형제는 노드를 모르므로 빼고) */
export function dedicatedNeed(spec: VmSpec): number {
  if ((spec.cpu.policy ?? "shared") !== "dedicated") {
    return 0;
  }
  const sockets = spec.cpu.sockets ?? 1;
  return (
    (spec.cpu.max_vcpus ?? sockets * spec.cpu.cores) + (spec.cpu.emulator === "isolate" ? 1 : 0)
  );
}

/**
 * `/topology/usage`로 이 스펙이 들어갈 만한 노드(이름). 전용 CPU는 남은 수, hugepages는 그 크기 풀(`any`는 두 크기
 * 합)의 남은 MiB만 견주는 어림이다 — 호스트 NUMA 노드 나눔은 서버가 켤 때 본다.
 */
export function roomOn(usage: readonly NodeUsageSummary[], spec: VmSpec): string[] {
  const need = dedicatedNeed(spec);
  const page = spec.hugepages;
  return usage
    .filter((n) => {
      if (need > 0 && (!n.pinning || n.dedicated_total - n.dedicated_used < need)) {
        return false;
      }
      if (page) {
        const free = n.hugepages
          .filter((p) => page === "any" || p.size === page)
          .reduce((sum, p) => sum + p.total_mib - p.used_mib, 0);
        if (free < spec.memory_mib) {
          return false;
        }
      }
      return true;
    })
    .map((n) => n.node);
}

/** 복제·배포·가져오기의 덮어쓰기 칸(서버의 `NumaOverrides`) */
export interface NumaOverride {
  cpu_policy?: CpuPolicy;
  cpu_thread_policy?: ThreadPolicy;
  cpu_emulator?: EmulatorPolicy;
  numa?: NumaSpec;
  no_numa?: boolean;
  hugepages?: PageSize;
  no_hugepages?: boolean;
}

/**
 * 요청에 넣을 덮어쓰기: `from`(원본)과 다른 칸만이다(바꾸지 않았으면 빈 객체). `clear`가 거짓이면(가져오기 — 새 VM이라
 * 지울 것이 없다) `no_numa`·`no_hugepages`를 쓰지 않는다. 스레드·에뮬레이터를 기본으로 되돌리면 그 기본값(`prefer`·
 * `shared`)을 보낸다(덮어쓰기에는 "지움"이 없다). 공유로 바꾸면 서버가 둘을 지운다.
 */
export function numaOverride(
  pick: NumaValues | null,
  from: NumaValues,
  clear = true,
): NumaOverride {
  if (!pick) {
    return {};
  }
  const a = numaSpec(pick);
  const b = numaSpec(from);
  const out: NumaOverride = {};
  if (pick.policy !== from.policy) {
    out.cpu_policy = pick.policy;
  }
  if (pick.policy === "dedicated") {
    if (a.cpu.thread_policy !== b.cpu.thread_policy) {
      out.cpu_thread_policy = a.cpu.thread_policy ?? "prefer";
    }
    if (a.cpu.emulator !== b.cpu.emulator) {
      out.cpu_emulator = a.cpu.emulator ?? "shared";
    }
  }
  if (JSON.stringify(a.numa) !== JSON.stringify(b.numa)) {
    if (a.numa) {
      out.numa = a.numa;
    } else if (clear) {
      out.no_numa = true;
    }
  }
  if (a.hugepages !== b.hugepages) {
    if (a.hugepages) {
      out.hugepages = a.hugepages;
    } else if (clear) {
      out.no_hugepages = true;
    }
  }
  return out;
}

/** hugepage 크기(KiB)의 이름: 2048 → `2M`, 1048576 → `1G` */
export function pageName(kib: number): string {
  return kib >= 1024 * 1024 && kib % (1024 * 1024) === 0
    ? `${kib / (1024 * 1024)}G`
    : kib >= 1024 && kib % 1024 === 0
      ? `${kib / 1024}M`
      : `${kib}K`;
}

/** `PageKind`의 KiB */
export function pageKib(kind: PageKind): number {
  return kind === "1G" ? 1024 * 1024 : 2048;
}

/** 호스트 자원 설정 대화상자의 hugepages 예약 행 */
export interface ReserveRow {
  size: PageKind;
  count: string;
  /** 비면 커널이 노드에 나눈다. */
  hostNode: string;
}

/** 호스트 자원 설정 대화상자의 값 */
export interface ResourcesValues {
  reserved: string;
  dedicated: string;
  hugepages: ReserveRow[];
}

export function resourcesFrom(r: HostResources | undefined): ResourcesValues {
  return {
    reserved: r?.reserved_cpus ?? "",
    dedicated: r?.dedicated_cpus ?? "",
    hugepages: (r?.hugepages ?? []).map((h) => ({
      size: h.size,
      count: String(h.count),
      hostNode: h.host_node == null ? "" : String(h.host_node),
    })),
  };
}

/**
 * 대화상자 값을 검사하고 요청 칸으로. 오류(필드 → 번역 키)가 있으면 `errors`만 있다. 모두 비우면 `{}` — 서버가
 * 설정을 지운다(칸을 빼면 그대로 둔다).
 */
export function resourcesTo(
  v: ResourcesValues,
  online: readonly number[] | undefined,
): { errors: Record<string, string>; value?: HostResources } {
  const errors: Record<string, string> = {};
  const reserved = parseCpuList(v.reserved);
  const dedicated = parseCpuList(v.dedicated);
  const known = online ? new Set(online) : undefined;
  if (reserved === null) {
    errors.reserved = "numa.resources.errors.cpuList";
  } else if (known && reserved.some((c) => !known.has(c))) {
    errors.reserved = "numa.resources.errors.offline";
  }
  if (dedicated === null) {
    errors.dedicated = "numa.resources.errors.cpuList";
  } else if (known && dedicated.some((c) => !known.has(c))) {
    errors.dedicated = "numa.resources.errors.offline";
  } else if (reserved && dedicated.some((c) => reserved.includes(c))) {
    errors.dedicated = "numa.resources.errors.overlap";
  }
  if (
    online &&
    reserved &&
    dedicated &&
    online.every((c) => reserved.includes(c) || dedicated.includes(c))
  ) {
    errors.dedicated ??= "numa.resources.errors.noShared";
  }
  const seen = new Set<string>();
  v.hugepages.forEach((h, i) => {
    if (!/^\d+$/.test(h.count)) {
      errors[`hugepages.${i}.count`] = "numa.resources.errors.count";
    }
    const key = `${h.size}@${h.hostNode}`;
    if (seen.has(key)) {
      errors[`hugepages.${i}.count`] = "numa.resources.errors.duplicate";
    }
    seen.add(key);
  });
  if (Object.keys(errors).length > 0) {
    return { errors };
  }
  return {
    errors,
    value: {
      ...(reserved!.length > 0 && { reserved_cpus: formatCpuList(reserved!) }),
      ...(dedicated!.length > 0 && { dedicated_cpus: formatCpuList(dedicated!) }),
      ...(v.hugepages.length > 0 && {
        hugepages: v.hugepages.map((h) => ({
          size: h.size,
          count: Number(h.count),
          ...(h.hostNode !== "" && { host_node: Number(h.hostNode) }),
        })),
      }),
    },
  };
}

/** 격자의 CPU 하나가 속한 쪽 */
export type CpuRole = "reserved" | "shared" | "free" | "used";

/** 토폴로지 응답으로 CPU마다 쪽과 그것을 잡은 VM 이름 */
export function cpuRoles(info: NodeTopologyInfo): Map<number, { role: CpuRole; vm?: string }> {
  const out = new Map<number, { role: CpuRole; vm?: string }>();
  const parse = (s: string | undefined) => parseCpuList(s ?? "") ?? [];
  const reserved = new Set(parse(info.sets?.reserved));
  const dedicated = new Set(parse(info.sets?.dedicated));
  const owner = new Map<number, string>();
  for (const v of info.vms) {
    for (const c of [...v.vcpus, ...parse(v.siblings), ...parse(v.emulator)]) {
      owner.set(c, v.vm);
    }
  }
  for (const c of parse(info.topology?.cpus)) {
    const vm = owner.get(c);
    const role: CpuRole = reserved.has(c)
      ? "reserved"
      : dedicated.has(c)
        ? vm !== undefined
          ? "used"
          : "free"
        : "shared";
    out.set(c, vm === undefined ? { role } : { role, vm });
  }
  return out;
}

/**
 * 격자 묶음: 호스트 NUMA 노드 › L3 묶음 › 코어(형제 스레드). L3를 모르면 노드 전체가 한 묶음, 형제를 모르면 CPU마다
 * 코어 하나다.
 */
export function cpuGrid(
  topology: Schemas["NodeTopology"],
): { node: number; l3: { cores: number[][] }[] }[] {
  const parse = (s: string) => parseCpuList(s) ?? [];
  const siblings = topology.siblings.map(parse);
  const l3 = topology.l3.map(parse);
  return topology.nodes.map((n) => {
    const cpus = parse(n.cpus);
    const groups = l3.map((g) => g.filter((c) => cpus.includes(c))).filter((g) => g.length > 0);
    const rest = cpus.filter((c) => !groups.some((g) => g.includes(c)));
    const all = rest.length > 0 ? [...groups, rest] : groups;
    return {
      node: n.id,
      l3: all.map((group) => {
        const cores: number[][] = [];
        const done = new Set<number>();
        for (const c of group) {
          if (done.has(c)) {
            continue;
          }
          const core = (siblings.find((s) => s.includes(c)) ?? [c]).filter((x) =>
            group.includes(x),
          );
          core.forEach((x) => done.add(x));
          cores.push(core);
        }
        return { cores };
      }),
    };
  });
}
