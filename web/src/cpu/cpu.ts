// 게스트 CPU 모델·플래그(계획 63): 서버 응답을 묻는 훅과 화면이 쓰는 순수 함수.
import { useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { PendingChange, Vm } from "@/api/types";

type Schemas = components["schemas"];
export type CpuModelEntry = Schemas["CpuModelEntry"];
export type CpuModelKind = Schemas["CpuModelKind"];
export type CpuFlagEntry = Schemas["CpuFlagEntry"];
export type CpuBaselines = Schemas["CpuBaselines"];
export type NodeCpuInfo = Schemas["NodeCpuInfo"];
export type ResolvedCpu = Schemas["ResolvedCpu"];

/** CPU 질의의 캐시 키 머리. 설정 저장·노드 다시 읽기 뒤 이 아래를 모두 다시 읽는다. */
export const CPU_KEY = ["cpu"] as const;

/** 머신 타입의 아키텍처(서버의 `MachineType::arch`). 비면(서버가 고름) 모른다. */
export function archOf(machine: Schemas["MachineType"] | ""): Schemas["Arch"] | undefined {
  return machine === "q35" ? "x86_64" : machine === "virt" ? "aarch64" : undefined;
}

/** 아키텍처의 모델 목록(`GET /cpu/models`). 아키텍처를 모르면 서버가 노드들의 아키텍처로 고른다. */
export function useCpuModels(arch: Schemas["Arch"] | undefined, enabled = true) {
  return useQuery({
    queryKey: [...CPU_KEY, "models", arch ?? ""],
    queryFn: () => call(api.GET("/cpu/models", { params: { query: arch ? { arch } : {} } })),
    staleTime: 60_000,
    retry: false,
    enabled,
  });
}

/** 아키텍처의 플래그 이름(`GET /cpu/flags`) */
export function useCpuFlags(arch: Schemas["Arch"] | undefined, enabled = true) {
  return useQuery({
    queryKey: [...CPU_KEY, "flags", arch ?? ""],
    queryFn: () => call(api.GET("/cpu/flags", { params: { query: arch ? { arch } : {} } })),
    staleTime: 60_000,
    retry: false,
    enabled,
  });
}

/** 클러스터 CPU 기준(`GET /cpu/baseline`, 루트 `cluster.read`) */
export function useCpuBaseline(enabled = true) {
  return useQuery({
    queryKey: [...CPU_KEY, "baseline"],
    queryFn: () => call(api.GET("/cpu/baseline")),
    staleTime: 10_000,
    retry: false,
    enabled,
  });
}

/** 노드의 CPU 탐침과 카탈로그(`GET /nodes/{key}/cpu`, 루트 `cluster.read`) */
export function useNodeCpu(key: string, enabled = true) {
  return useQuery({
    queryKey: [...CPU_KEY, "node", key],
    queryFn: () => call(api.GET("/nodes/{key}/cpu", { params: { path: { key } } })),
    staleTime: 30_000,
    retry: false,
    enabled,
  });
}

/** VM의 CPU와 노드마다 받을 수 있는지(`GET /vms/{key}/cpu`, 마이그레이션 대화상자) */
export function useVmCpu(key: string, enabled = true) {
  return useQuery({
    queryKey: [...CPU_KEY, "vm", key],
    queryFn: () => call(api.GET("/vms/{key}/cpu", { params: { path: { key } } })),
    staleTime: 5_000,
    retry: false,
    enabled,
  });
}

/**
 * 플래그 이름을 서버의 `normalize_flag`처럼 맞춘다: 소문자, `_` → `-`, 옛 이름 `sse4-1`·`sse4-2` → `sse4.1`·`sse4.2`.
 * 문법(`^[a-z0-9][a-z0-9.-]{0,63}$`)에 맞지 않으면 null이다.
 */
export function normalizeFlag(name: string): string | null {
  let n = name.trim().toLowerCase().replaceAll("_", "-");
  if (n === "sse4-1" || n === "sse4-2") {
    n = `sse4.${n.slice(-1)}`;
  }
  return /^[a-z0-9][a-z0-9.-]{0,63}$/.test(n) ? n : null;
}

/** 서버가 받는 VM 플래그 수 */
export const MAX_CPU_FLAGS = 64;

/** `"-avx512vbmi"`를 켜기·끄기와 이름으로 */
export function splitFlag(flag: string): { on: boolean; name: string } {
  return flag.startsWith("+")
    ? { on: true, name: flag.slice(1) }
    : { on: false, name: flag.startsWith("-") ? flag.slice(1) : flag };
}

/**
 * 플래그 하나를 더할 때 막는 까닭(번역 키), 되면 null. 입력은 `+aes`·`-aes`·`aes`(기본은 끄기 = 숨기기)다. `known`이
 * 있으면 그 이름(노드 QEMU가 아는 플래그)만 받는다.
 */
export function flagProblem(
  input: string,
  flags: readonly string[],
  known: ReadonlySet<string> | undefined,
): string | null {
  const { name } = splitFlag(input.trim());
  const n = normalizeFlag(name);
  if (n === null) {
    return "cpu.errors.flagName";
  }
  if (known && !known.has(n)) {
    return "cpu.errors.unknownFlag";
  }
  if (flags.some((f) => splitFlag(f).name === n)) {
    return "cpu.errors.duplicateFlag";
  }
  if (flags.length >= MAX_CPU_FLAGS) {
    return "cpu.errors.tooManyFlags";
  }
  return null;
}

/** 입력을 플래그 문자열로(`+`가 없으면 숨기기 `-`). 먼저 `flagProblem`이 null이어야 한다. */
export function toFlag(input: string, on?: boolean): string {
  const { on: plus, name } = splitFlag(input.trim());
  return `${(on ?? plus) ? "+" : "-"}${normalizeFlag(name) ?? name}`;
}

/** 모델 고르기의 묶음 순서 */
export const KIND_ORDER: readonly CpuModelKind[] = ["basic", "cluster", "level", "qemu"];

/**
 * 고를 수 있는 모델 묶음: 별칭은 빼고(저장할 때 버전 모델로 고정되므로) 가리키는 버전 모델의 찾기 이름에 더한다.
 * `query`는 이름·별칭·벤더에서 대소문자 없이 찾는다. 지금 값(`keep`)은 찾기와 상관없이 남긴다.
 */
export function modelGroups(
  models: readonly CpuModelEntry[],
  query: string,
  keep: string,
): { kind: CpuModelKind; models: CpuModelEntry[] }[] {
  const aliases = new Map<string, string[]>();
  for (const m of models) {
    if (m.alias_of) {
      aliases.set(m.alias_of, [...(aliases.get(m.alias_of) ?? []), m.name]);
    }
  }
  const q = query.trim().toLowerCase();
  const matches = (m: CpuModelEntry) =>
    m.name === keep ||
    q === "" ||
    [m.name, m.vendor ?? "", ...(aliases.get(m.name) ?? [])].some((s) =>
      s.toLowerCase().includes(q),
    );
  return KIND_ORDER.map((kind) => ({
    kind,
    models: models.filter((m) => m.kind === kind && !m.alias_of && matches(m)),
  })).filter((g) => g.models.length > 0);
}

/** 별칭이면 가리키는 버전 모델 이름(웹은 버전 이름만 고르게 한다, 설계 §8) */
export function pinnedName(models: readonly CpuModelEntry[], name: string): string {
  return models.find((m) => m.name === name)?.alias_of ?? name;
}

/**
 * 보류 `cpu`(다음 전원 껐다 켜기 때 CPU가 바뀜)나 `numa`(CPU 배치·NUMA가 바뀜, 계획 64) — 둘 다 전원 껐다 켜기로만
 * 적용된다. 없으면 undefined
 */
export function cpuPending(vm: Vm): PendingChange | undefined {
  return (vm.status.hardware?.pending ?? []).find((c) => c.field === "cpu" || c.field === "numa");
}

/** 해석된 CPU를 한 줄로: `Icelake-Server-v7 -avx512vbmi +pcid`(서버의 `describe`와 같은 모양) */
export function describeResolved(cpu: ResolvedCpu): string {
  const flags = Object.entries(cpu.flags ?? {})
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([name, on]) => `${on ? "+" : "-"}${name}`);
  return [cpu.model, ...flags].join(" ");
}

/** 기준의 모델·플래그 한 줄 */
export function describeBaseline(b: Schemas["Baseline"]): string {
  return describeResolved({ model: b.model, flags: b.flags });
}

/** VM 스펙의 모델과 플래그 한 줄(`host -avx512vbmi`) */
export function describeSpec(cpu: Schemas["CpuSpec"]): string {
  return [cpu.model ?? "host", ...(cpu.flags ?? [])].join(" ");
}
