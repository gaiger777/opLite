// 복구 계획 편집(계획 76 §4)의 순수 함수: 계획 ↔ 편집 초안, 검사(서버 `RecoveryPlanSpec::validate`의 형식 검사와 같은 범위),
// VM 고르기 후보, 실행 단계 점, 시간 글. 화면은 이것을 그리기만 한다.
import type { Backup } from "@/api/types";
import type {
  DrPhase,
  DrRunKind,
  DrRunState,
  DrWait,
  OnTimeout,
  RecoveryPlanSpec,
  VmProtection,
} from "@/dr/api";

/** 서버 `dr.rs`의 한도 */
export const MAX_GROUPS = 32;
export const MAX_PLAN_VMS = 500;
export const MIN_TIMEOUT_SECS = 10;
export const MAX_TIMEOUT_SECS = 86_400;
export const MAX_DELAY_SECS = 3600;
export const MAX_RPO_SECS = 365 * 86_400;
export const DEFAULT_RPO_SECS = 86_400;
export const DEFAULT_TIMEOUT_SECS = 600;
/** NIC 순번의 끝(서버 `vm::MAX_NICS` 16개) */
export const MAX_NIC = 15;
/** 시험 VM 이름의 꼬리(서버 `TEST_VM_SUFFIX`) */
export const TEST_VM_SUFFIX = "-drt";

export const WAITS: readonly DrWait[] = ["running", "guest", "ip"];
export const ON_TIMEOUT: readonly OnTimeout[] = ["continue", "stop"];

export interface Pair {
  from: string;
  to: string;
}

export interface IpDraft {
  nic: string;
  dhcp: boolean;
  address: string;
  gateway: string;
  /** 쉼표·빈칸으로 나눈 DNS 서버 */
  dns: string;
}

export interface VmDraft {
  vm: string;
  pool: string;
  node: string;
  live: boolean;
  ip: IpDraft[];
}

export interface GroupDraft {
  /** 화면의 React key(이름은 바뀔 수 있다) */
  key: string;
  name: string;
  wait: DrWait;
  timeout: string;
  delay: string;
  vms: VmDraft[];
}

export interface Draft {
  name: string;
  source: string;
  pool: string;
  folder: string;
  /** RPO 목표(시간, 소수 가능, 0 = 끔) */
  rpoHours: string;
  onTimeout: OnTimeout;
  networks: Pair[];
  securityGroups: Pair[];
  testNetworks: Pair[];
  groups: GroupDraft[];
}

let nextKey = 0;
function newKey() {
  nextKey += 1;
  return `g${nextKey}`;
}

export function newGroup(name: string): GroupDraft {
  return {
    key: newKey(),
    name,
    wait: "running",
    timeout: String(DEFAULT_TIMEOUT_SECS),
    delay: "0",
    vms: [],
  };
}

export function newVm(vm: string): VmDraft {
  return { vm, pool: "", node: "", live: false, ip: [] };
}

export function newIp(nic: number): IpDraft {
  return { nic: String(nic), dhcp: false, address: "", gateway: "", dns: "" };
}

/** 새 계획의 초안: 그룹 하나 */
export function emptyDraft(): Draft {
  return {
    name: "",
    source: "",
    pool: "",
    folder: "",
    rpoHours: String(DEFAULT_RPO_SECS / 3600),
    onTimeout: "continue",
    networks: [],
    securityGroups: [],
    testNetworks: [],
    groups: [newGroup("group-1")],
  };
}

const pairs = (map: Record<string, string> | undefined): Pair[] =>
  Object.entries(map ?? {}).map(([from, to]) => ({ from, to }));

export function toDraft(name: string, spec: RecoveryPlanSpec): Draft {
  return {
    name,
    source: spec.source,
    pool: spec.pool,
    folder: spec.folder ?? "",
    rpoHours: String(spec.rpo_target_secs / 3600),
    onTimeout: spec.on_timeout ?? "continue",
    networks: pairs(spec.networks),
    securityGroups: pairs(spec.security_groups),
    testNetworks: pairs(spec.test_networks),
    groups: spec.groups.map((g) => ({
      key: newKey(),
      name: g.name,
      wait: g.wait ?? "running",
      timeout: String(g.timeout_secs),
      delay: String(g.delay_secs),
      vms: g.vms.map((v) => ({
        vm: v.vm,
        pool: v.pool ?? "",
        node: v.node ?? "",
        live: v.live ?? false,
        ip: (v.ip ?? []).map((r) => ({
          nic: String(r.nic),
          dhcp: r.dhcp ?? false,
          address: r.address ?? "",
          gateway: r.gateway ?? "",
          dns: (r.dns ?? []).join(", "),
        })),
      })),
    })),
  };
}

const map = (list: Pair[]): Record<string, string> | undefined => {
  const out = Object.fromEntries(
    list.filter((p) => p.from.trim() && p.to.trim()).map((p) => [p.from.trim(), p.to.trim()]),
  );
  return Object.keys(out).length > 0 ? out : undefined;
};

const opt = (s: string) => (s.trim() ? s.trim() : undefined);

/** 초안 → 계획(검사를 통과한 초안만) */
export function toSpec(draft: Draft): RecoveryPlanSpec {
  return {
    source: draft.source,
    pool: draft.pool.trim(),
    folder: opt(draft.folder),
    rpo_target_secs: Math.round(Number(draft.rpoHours) * 3600),
    on_timeout: draft.onTimeout,
    networks: map(draft.networks),
    security_groups: map(draft.securityGroups),
    test_networks: map(draft.testNetworks),
    groups: draft.groups.map((g) => ({
      name: g.name.trim(),
      wait: g.wait,
      timeout_secs: Number(g.timeout),
      delay_secs: Number(g.delay),
      vms: g.vms.map((v) => ({
        vm: v.vm,
        pool: opt(v.pool),
        node: opt(v.node),
        live: v.live || undefined,
        ip:
          v.ip.length > 0
            ? v.ip.map((r) =>
                r.dhcp
                  ? { nic: Number(r.nic), dhcp: true }
                  : {
                      nic: Number(r.nic),
                      address: r.address.trim(),
                      gateway: opt(r.gateway),
                      dns: splitList(r.dns).length > 0 ? splitList(r.dns) : undefined,
                    },
              )
            : undefined,
      })),
    })),
  };
}

export function splitList(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

const NAME = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/;
const CIDR = /^[0-9a-fA-F:.]+\/\d{1,3}$/;

function intIn(text: string, min: number, max: number): boolean {
  if (!/^\d+$/.test(text.trim())) {
    return false;
  }
  const n = Number(text);
  return n >= min && n <= max;
}

/**
 * 초안 검사. 칸 → 오류 번역 키(`dr.invalid.*`). 칸 이름: `name`, `source`, `pool`, `rpo`, `groups`, `g{i}.name`, `g{i}.timeout`,
 * `g{i}.delay`, `g{i}.vms`, `g{i}.v{j}.ip{k}`, `vms`(겹침).
 */
export function validate(draft: Draft, isNew: boolean): Record<string, string> {
  const errors: Record<string, string> = {};
  if (isNew && !NAME.test(draft.name)) {
    errors.name = "dr.invalid.name";
  }
  if (!draft.source) {
    errors.source = "dr.invalid.required";
  }
  if (!NAME.test(draft.pool.trim())) {
    errors.pool = "dr.invalid.name";
  }
  const rpo = Number(draft.rpoHours);
  if (
    draft.rpoHours.trim() === "" ||
    !Number.isFinite(rpo) ||
    rpo < 0 ||
    rpo * 3600 > MAX_RPO_SECS
  ) {
    errors.rpo = "dr.invalid.rpo";
  }
  if (draft.groups.length === 0 || draft.groups.length > MAX_GROUPS) {
    errors.groups = "dr.invalid.groups";
  }
  const total = draft.groups.reduce((n, g) => n + g.vms.length, 0);
  if (total > MAX_PLAN_VMS) {
    errors.groups = "dr.invalid.tooManyVms";
  }
  const names = new Set<string>();
  draft.groups.forEach((g, i) => {
    const name = g.name.trim();
    if (!NAME.test(name)) {
      errors[`g${i}.name`] = "dr.invalid.name";
    } else if (names.has(name)) {
      errors[`g${i}.name`] = "dr.invalid.duplicateGroup";
    }
    names.add(name);
    if (!intIn(g.timeout, MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS)) {
      errors[`g${i}.timeout`] = "dr.invalid.timeout";
    }
    if (!intIn(g.delay, 0, MAX_DELAY_SECS)) {
      errors[`g${i}.delay`] = "dr.invalid.delay";
    }
    if (g.vms.length === 0) {
      errors[`g${i}.vms`] = "dr.invalid.emptyGroup";
    }
    g.vms.forEach((v, j) => {
      if (v.pool.trim() && !NAME.test(v.pool.trim())) {
        errors[`g${i}.v${j}.pool`] = "dr.invalid.name";
      }
      v.ip.forEach((r, k) => {
        const at = `g${i}.v${j}.ip${k}`;
        if (!intIn(r.nic, 0, MAX_NIC)) {
          errors[at] = "dr.invalid.nic";
        } else if (!r.dhcp && !CIDR.test(r.address.trim())) {
          errors[at] = "dr.invalid.address";
        }
      });
    });
  });
  const seen = new Set<string>();
  for (const g of draft.groups) {
    for (const v of g.vms) {
      if (seen.has(v.vm)) {
        errors.vms = "dr.invalid.duplicateVm";
      }
      seen.add(v.vm);
    }
  }
  return errors;
}

/** 초안의 모든 VM 이름 */
export function draftVms(draft: Draft): Set<string> {
  return new Set(draft.groups.flatMap((g) => g.vms.map((v) => v.vm)));
}

/** 그룹 `i`를 `by`(-1 위, +1 아래)만큼 옮긴다. 끝을 넘으면 그대로다. */
export function moveGroup<T>(list: readonly T[], i: number, by: -1 | 1): T[] {
  const j = i + by;
  if (j < 0 || j >= list.length) {
    return [...list];
  }
  const next = [...list];
  [next[i], next[j]] = [next[j]!, next[i]!];
  return next;
}

/** 고르기 후보 하나: 원격 VM 목록에 있는지, 이 클러스터에 그 원격의 사본이 있는지(가장 새 사본 시각) */
export interface Candidate {
  vm: string;
  remote: boolean;
  copyAt?: number;
}

/**
 * VM 고르기 후보(설계 §4 — 원격 VM 목록 + DR 사본): 원격 VM 이름과, 이 클러스터의 `ready` 사본 가운데 `origin.ca_fingerprint`가
 * 원격의 CA 지문인 것(서버 `from_source`와 같은 판단)의 VM 이름. 이름 순.
 */
export function candidates(
  remoteVms: readonly string[] | undefined,
  backups: readonly Backup[] | undefined,
  sourceCa: string | undefined,
): Candidate[] {
  const out = new Map<string, Candidate>();
  for (const vm of remoteVms ?? []) {
    out.set(vm, { vm, remote: true });
  }
  if (sourceCa) {
    for (const b of backups ?? []) {
      if (b.spec.origin?.ca_fingerprint !== sourceCa || b.status.phase !== "ready") {
        continue;
      }
      const name = b.spec.vm_name;
      const at = b.status.created_at ?? undefined;
      const prev = out.get(name) ?? { vm: name, remote: false };
      out.set(name, {
        ...prev,
        copyAt: at !== undefined && (prev.copyAt ?? 0) < at ? at : prev.copyAt,
      });
    }
  }
  return [...out.values()].sort((a, b) => a.vm.localeCompare(b.vm));
}

/** 보호 상태 수: 지키는 VM, 넘은 VM(복구 지점 없음 포함), 전체 */
export function rpoCounts(protection: readonly VmProtection[] | undefined) {
  const list = protection ?? [];
  const ok = list.filter((p) => p.ok).length;
  return { ok, bad: list.length - ok, total: list.length };
}

/** 실행 단계 점의 차례(설계 §4 "VM마다 단계 점") */
export const PHASE_DOTS: readonly DrPhase[] = ["restoring", "starting", "waiting", "ready"];
/** 뒷정리 실행의 단계 점 */
export const CLEANUP_DOTS: readonly DrPhase[] = ["deleting", "deleted"];

export type DotState = "done" | "current" | "failed" | "todo";

/**
 * 단계 점마다 상태. 끝난 단계는 `done`, 지금 단계는 `current`(실패·시간 초과면 그 앞까지 `done`이고 다음 점이 `failed`),
 * 건너뛴 VM과 아직 시작 전(`pending`·`preparing`)은 모두 `todo`다. 실패한 단계는 정확히 모르므로 마지막으로 본 단계
 * (`last`)가 있으면 그 점이 `failed`다.
 */
export function phaseDots(
  phase: DrPhase,
  dots: readonly DrPhase[] = PHASE_DOTS,
  last?: DrPhase,
): DotState[] {
  if (phase === "failed" || phase === "timed_out") {
    const at = phase === "timed_out" ? dots.indexOf("waiting") : last ? dots.indexOf(last) : 0;
    const failAt = at < 0 ? 0 : at;
    return dots.map((_, i) => (i < failAt ? "done" : i === failAt ? "failed" : "todo"));
  }
  const at = dots.indexOf(phase);
  if (at < 0) {
    return dots.map(() => "todo");
  }
  const end = dots.length - 1;
  return dots.map((_, i) =>
    i < at || (i === at && at === end) ? "done" : i === at ? "current" : "todo",
  );
}

export type Tone = "success" | "warning" | "danger" | "info" | "neutral";

export const RUN_TONE: Record<DrRunState, Tone> = {
  running: "info",
  succeeded: "success",
  partial: "warning",
  failed: "danger",
  cancelled: "neutral",
};

export const PHASE_TONE: Record<DrPhase, Tone> = {
  pending: "neutral",
  preparing: "info",
  restoring: "info",
  starting: "info",
  waiting: "info",
  ready: "success",
  failed: "danger",
  timed_out: "danger",
  skipped: "neutral",
  deleting: "info",
  deleted: "success",
};

/** 실행 종류가 장애 조치인지 */
export const isFailover = (kind: DrRunKind) => kind.startsWith("failover");

/** 초 → `1h 5m`·`3m 20s`·`45s` */
export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) {
    return `${s}s`;
  }
  if (s < 3600) {
    return `${Math.floor(s / 60)}m ${s % 60}s`;
  }
  if (s < 86_400) {
    return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  }
  return `${Math.floor(s / 86_400)}d ${Math.floor((s % 86_400) / 3600)}h`;
}

/** 실행의 RTO 시계: 끝났으면 서버가 잰 `rto_secs`, 도는 동안은 시작부터 지금까지(초) */
export function rtoClock(
  status: { state: DrRunState; started_at: number; rto_secs?: number; finished_at?: number },
  nowSecs: number,
): number | undefined {
  if (status.state === "running") {
    return Math.max(0, nowSecs - status.started_at);
  }
  return status.rto_secs;
}
