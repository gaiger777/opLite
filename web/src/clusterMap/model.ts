// 클러스터 지도의 모델(계획 70 §5·§6.1): 노드·VM 목록과 `/placement`·`/cluster`·`/alerts`·Task 진행률을 순수 함수
// `toModel`로 합친다. 그림(배치)은 이 모델만 본다 — 같은 입력이면 같은 모델(이름 순)이다.
import type { AlertView } from "@/alerts/alerts";
import type { Node, PlacementMode, PlacementStatus, StoragePool, Vm } from "@/api/types";
import type { components } from "@/api/schema";
import { migrationPercent } from "@/tasks/tasks";

export type TaskProgress = components["schemas"]["TaskProgress"];
export type Severity = AlertView["severity"];
export type Phase = Vm["status"]["phase"];

/** 부하 열 색(§7.1): 70% 밑 조용함, 70~90% 경고, 90% 이상 위험, 신선한 요약이 없으면 모름 */
export type Heat = "cool" | "warm" | "hot" | "unknown";

/** 스토리지 레이어의 풀 받침 색(§7.3): 사용률 문턱은 부하와 같고, 준비 안 됨은 `bad` */
export type PoolTone = "cool" | "warm" | "hot" | "bad";

/** 노드 로컬 풀 하나(스토리지 레이어 받침의 한 조각) */
export interface MapPool {
  name: string;
  /** 사용률(%, 정수) — 크기를 모르면 `null` */
  usedPct: number | null;
  ready: boolean;
  error: string | null;
  tone: PoolTone;
}

/** 업링크(물리·본딩 인터페이스) 상태(네트워크 레이어 띠) */
export type Uplink = "up" | "degraded" | "down";

/** 노드 부하(%, 소수 한 자리) */
export interface NodeLoad {
  cpu: number;
  mem: number;
}

export interface MapNode {
  id: string;
  name: string;
  labels: Record<string, string>;
  online: boolean;
  maintenance: boolean;
  witness: boolean;
  frozen: boolean;
  /** `/cluster`를 읽을 수 있고 이 노드가 리더 */
  leader: boolean;
  cpus: number;
  memoryMib: number;
  /** 신선한 부하 요약(`age_secs`가 있을 때만). 없으면 "부하 모름" — 대체값을 부하처럼 보이지 않는다. */
  load: NodeLoad | null;
  /** 할당 메모리 / 전체(%) — `/placement`가 있을 때 */
  allocatedPct: number | null;
  /** 경보 중 firing인 것의 수와 가장 높은 심각도 */
  alerts: number;
  severity: Severity | null;
  /** 이 노드에 놓였지만 읽을 수 없는 VM 수(`/placement`의 `vms` − 보이는 같은 기준의 수) */
  hiddenVms: number;
  /** 템플릿 VM 수(타일로 그리지 않음 — 카드에 수만) */
  templates: number;
  /** 비우기가 옮길 VM 수: 이 노드에 고정돼 켜 둔 VM(서버의 비우기와 같은 기준) */
  pinnedRunning: number;
  /** 이 노드에서 나가는 진행 중 마이그레이션 수 */
  outgoing: number;
  /** 스토리지 레이어: 로컬 풀(공유 아닌 풀, 이름 순) */
  pools: MapPool[];
  /** 스토리지 레이어: 이 노드에서 준비 안 된 공유 풀 이름 */
  sharedDown: string[];
  /** 네트워크 레이어: 업링크 상태. 인터페이스 정보가 없으면(옛 노드·macOS) `null` — 그리지 않는다. */
  uplink: Uplink | null;
  /** 네트워크 레이어: SDN 게이트웨이 노드("GW" 꼬리표) */
  gateway: boolean;
  /** HA 레이어: 펜싱이 있는가. 증인·옛 노드(알리지 않음)는 `null` — 그리지 않는다. */
  fencing: boolean | null;
  /** HA 레이어: 이웃이 이 노드의 전원을 읽지 못한 이유(펜스 확인 실패) */
  fenceError: string | null;
}

export interface MapVm {
  id: string;
  name: string;
  /** 그리는 자리(알 수 없으면 `null` — "배치 안 됨" 판) */
  node: string | null;
  phase: Phase;
  ha: boolean;
  /** 어긴 배치 규칙 수 */
  violations: number;
  /** 진행 중 마이그레이션(대상 노드·Task) */
  migration: { to: string; task: string } | null;
}

/** 화살표 하나: 진행 중 마이그레이션 또는 최적 배치 추천 */
export interface MapMove {
  key: string;
  kind: "migration" | "recommendation";
  vm: string;
  vmName: string;
  from: string;
  to: string;
  /** 진행률(0~100 정수) — 디스크 복사 단계일 때만(메모리 단계의 바이트 수는 끝난 디스크 복사라 늘 100%) */
  progress: number | null;
  /** 진행 단계 id(퍼센트가 없을 때 — 글은 `migrationStageText`로 옮긴다) */
  stage: string | null;
  /** 마이그레이션 Task id */
  task: string | null;
  /** 추천 id(적용할 때) */
  recommendation: string | null;
}

export interface ClusterModel {
  /** 클러스터 이름(`/cluster`를 못 읽으면 `null`) */
  name: string | null;
  /** 이름 순 */
  nodes: MapNode[];
  /** 이름 순, 템플릿 뺌 */
  vms: MapVm[];
  moves: MapMove[];
  /** `/placement`를 읽었는가(503·403이면 아니다 — 추천 레이어를 막는다) */
  placement: boolean;
  /** 균형 조정기 모드(`/placement`를 읽었을 때). `off`면 추천 레이어를 막는다. */
  balancer: PlacementMode | null;
  /** 공유 풀 이름(스토리지 레이어 — 판 앞의 띠) */
  shared: string[];
}

export interface MapInput {
  nodes: readonly Node[];
  vms: readonly Vm[];
  /** `GET /placement`. 아직 없거나 503·403이면 `null` */
  placement?: PlacementStatus | null;
  /** `GET /cluster`. 403이면 `null`(리더 없이 그린다) */
  cluster?: { name: string; leader?: string | null } | null;
  alerts?: readonly AlertView[];
  /** Task id → 진행률 */
  progress?: ReadonlyMap<string, TaskProgress>;
  /** 스토리지 풀 목록(공유인지 알려고). 없으면 노드의 풀을 모두 로컬로 본다. */
  pools?: readonly StoragePool[];
  /**
   * 사용자가 읽을 수 없는 VM이 있을 수 있다(VM 읽기가 루트 아래 모두가 아님). 거짓(기본)이면 "볼 수 없는 VM"을 세지 않는다 —
   * 모두 보는 사용자에게 `/placement`(15초 주기)와 실시간 VM 목록의 시차가 가짜 "+N 볼 수 없음"이 되지 않게.
   */
  vmsLimited?: boolean;
}

/** 이름 차례: 숫자를 숫자로(`n2` < `n10`), 브라우저 언어와 상관없이 같은 차례 */
const collator = new Intl.Collator("en", { numeric: true, sensitivity: "variant" });

export function byName<T extends { name: string; id: string }>(a: T, b: T): number {
  return collator.compare(a.name, b.name) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
}

/** 값의 자연 차례(묶음 판 이름) */
export function naturalCompare(a: string, b: string): number {
  return collator.compare(a, b) || (a < b ? -1 : a > b ? 1 : 0);
}

/** `/placement`의 0.1% 단위 → %(소수 한 자리) */
export function permille(n: number): number {
  return Math.round(n) / 10;
}

/** 부하 열 색(§7.1): `max(cpu, mem)`의 문턱 70·90 */
export function heatTone(load: NodeLoad | null): Heat {
  if (!load) {
    return "unknown";
  }
  const heat = Math.max(load.cpu, load.mem);
  return heat >= 90 ? "hot" : heat >= 70 ? "warm" : "cool";
}

const SEVERITY: Record<Severity, number> = { info: 0, warning: 1, critical: 2 };

/** HA로 지키는 VM(`ha: true`나 `ignored`가 아닌 설정) — 타일·카드가 같은 기준 */
export function isHa(vm: Pick<Vm, "spec">): boolean {
  const ha = vm.spec.ha;
  return ha === true || (typeof ha === "object" && ha !== null && ha.state !== "ignored");
}

/** 풀 사용률 → 받침 색(부하와 같은 문턱 70·90) */
export function poolTone(ready: boolean, usedPct: number | null): PoolTone {
  if (!ready) {
    return "bad";
  }
  return usedPct === null ? "cool" : usedPct >= 90 ? "hot" : usedPct >= 70 ? "warm" : "cool";
}

/** 노드의 로컬 풀과 준비 안 된 공유 풀. 풀 목록을 알면 지운 풀(목록에 없는 이름)의 옛 상태는 뺀다. */
function nodePools(
  n: Node,
  pools: ReadonlyMap<string, StoragePool> | null,
): { local: MapPool[]; sharedDown: string[] } {
  const local: MapPool[] = [];
  const sharedDown: string[] = [];
  for (const [name, state] of Object.entries(n.status.pools ?? {})) {
    const pool = pools?.get(name);
    if (pools && !pool) {
      continue;
    }
    if (pool?.spec.shared) {
      if (!state.ready) {
        sharedDown.push(name);
      }
      continue;
    }
    const total = state.total_bytes ?? 0;
    const usedPct =
      total > 0 ? Math.round(((total - (state.available_bytes ?? 0)) / total) * 100) : null;
    const ready = state.ready === true;
    local.push({
      name,
      usedPct,
      ready,
      error: state.error ?? null,
      tone: poolTone(ready, usedPct),
    });
  }
  local.sort((a, b) => naturalCompare(a.name, b.name));
  sharedDown.sort(naturalCompare);
  return { local, sharedDown };
}

/**
 * 업링크 상태: 본딩, 그리고 본딩 포트가 아닌 이더넷 중 쓰는 것(브리지에 묶였거나 주소가 있는 것 — 꽂지 않은 남는 포트를
 * "끊김"으로 보이지 않게). 모두 up이고 링크가 있으면 `up`, 하나도 아니면 `down`, 섞이거나 본딩이 degraded면 `degraded`.
 */
export function uplinkState(n: Pick<Node, "status">): Uplink | null {
  const all = n.status.interfaces;
  if (!all) {
    return null;
  }
  const bonds = new Set(all.filter((i) => i.kind === "bond").map((i) => i.name));
  const used = all.filter(
    (i) =>
      i.kind === "bond" ||
      (i.kind === "ethernet" &&
        !(i.master !== undefined && bonds.has(i.master)) &&
        (i.master !== undefined || (i.addresses ?? []).length > 0)),
  );
  if (used.length === 0) {
    return null;
  }
  let up = 0;
  let degraded = false;
  for (const i of used) {
    const ok = i.state === "up" && i.carrier !== false;
    if (i.bond?.state === "degraded" && ok) {
      degraded = true;
    }
    if (ok && i.bond?.state !== "down") {
      up += 1;
    }
  }
  return up === 0 ? "down" : up < used.length || degraded ? "degraded" : "up";
}

/** 노드 레이블 키(묶기 도구의 "고른 레이블" 목록) */
export function labelKeys(nodes: readonly Pick<MapNode, "labels">[]): string[] {
  const keys = new Set<string>();
  for (const n of nodes) {
    for (const k of Object.keys(n.labels)) {
      keys.add(k);
    }
  }
  return [...keys].sort(naturalCompare);
}

/** VM을 그리는 노드: `status.node`(지금 도는 곳), 없거나 모르는 노드면 `spec.node`, 그것도 아니면 `null`(배치 안 됨) */
export function placeOf(
  vm: Pick<Vm, "spec" | "status">,
  known: ReadonlySet<string>,
): string | null {
  if (vm.status.node && known.has(vm.status.node)) {
    return vm.status.node;
  }
  if (vm.spec.node && known.has(vm.spec.node)) {
    return vm.spec.node;
  }
  return null;
}

/**
 * 질의 결과 → 모델. VM 자리는 `status.node`(지금 도는 곳), 없거나 모르는 노드면 `spec.node`, 그것도 아니면 배치 안 됨.
 * 템플릿은 타일이 아니라 노드의 수로만 센다.
 */
export function toModel(input: MapInput): ClusterModel {
  const placement = input.placement ?? null;
  const loads = new Map((placement?.nodes ?? []).map((l) => [l.node, l]));
  const known = new Set(input.nodes.map((n) => n.id));
  const where = (vm: Vm) => placeOf(vm, known);

  const vms: MapVm[] = [];
  const templates = new Map<string, number>();
  const visiblePlaced = new Map<string, number>();
  const pinnedRunning = new Map<string, number>();
  const outgoing = new Map<string, number>();
  const moves: MapMove[] = [];
  const bump = (map: Map<string, number>, key: string) => map.set(key, (map.get(key) ?? 0) + 1);

  for (const vm of input.vms) {
    const node = where(vm);
    if (vm.spec.template) {
      if (node) {
        bump(templates, node);
      }
      continue;
    }
    // `/placement`의 `vms`와 같은 기준(고정된 노드에 켜 둔 VM) — 볼 수 없는 수를 셈하려고
    if (vm.spec.node && vm.spec.power === "running") {
      bump(visiblePlaced, vm.spec.node);
      bump(pinnedRunning, vm.spec.node);
    }
    const m = vm.status.migration;
    const migration =
      m && node && known.has(m.node) && m.node !== node ? { to: m.node, task: m.task } : null;
    vms.push({
      id: vm.id,
      name: vm.name,
      node,
      phase: vm.status.phase,
      ha: isHa(vm),
      violations: vm.status.rule_violations?.length ?? 0,
      migration,
    });
    if (migration && node) {
      bump(outgoing, node);
      const p = input.progress?.get(migration.task);
      moves.push({
        key: `move:${vm.id}`,
        kind: "migration",
        vm: vm.id,
        vmName: vm.name,
        from: node,
        to: migration.to,
        progress: migrationPercent(p),
        stage: p?.stage ?? null,
        task: migration.task,
        recommendation: null,
      });
    }
  }
  vms.sort(byName);

  // 추천: 이미 Task가 있는 것(적용함·auto)은 진행 중 마이그레이션 화살표로 보이므로 뺀다. 같은 VM에 둘 그리지 않는다.
  const moving = new Set(moves.map((m) => m.vm));
  const recs = [...(placement?.recommendations ?? [])]
    .filter((r) => !r.task && !moving.has(r.vm) && known.has(r.from) && known.has(r.to))
    .sort((a, b) => naturalCompare(a.vm_name, b.vm_name) || naturalCompare(a.id, b.id));
  for (const r of recs) {
    if (moving.has(r.vm)) {
      continue;
    }
    moving.add(r.vm);
    moves.push({
      key: `rec:${r.id}`,
      kind: "recommendation",
      vm: r.vm,
      vmName: r.vm_name,
      from: r.from,
      to: r.to,
      progress: null,
      stage: null,
      task: null,
      recommendation: r.id,
    });
  }
  moves.sort((a, b) =>
    a.kind === b.kind ? naturalCompare(a.vmName, b.vmName) : a.kind === "migration" ? -1 : 1,
  );

  const alerts = new Map<string, { count: number; severity: Severity }>();
  const byNodeName = new Map(input.nodes.map((n) => [n.name, n.id]));
  for (const a of input.alerts ?? []) {
    if (a.state !== "firing") {
      continue;
    }
    const id =
      a.subject.kind === "node" && known.has(a.subject.id)
        ? a.subject.id
        : a.subject.node !== undefined
          ? byNodeName.get(a.subject.node)
          : undefined;
    if (!id) {
      continue;
    }
    const prev = alerts.get(id);
    alerts.set(id, {
      count: (prev?.count ?? 0) + 1,
      severity:
        prev && SEVERITY[prev.severity] >= SEVERITY[a.severity] ? prev.severity : a.severity,
    });
  }

  const leader = input.cluster?.leader ?? null;
  const poolMap = input.pools ? new Map(input.pools.map((p) => [p.name, p])) : null;
  const nodes: MapNode[] = input.nodes.map((n) => {
    const l = loads.get(n.id);
    const fresh = l && l.age_secs !== undefined;
    const { local, sharedDown } = nodePools(n, poolMap);
    const witness = n.spec.witness === true;
    return {
      id: n.id,
      name: n.name,
      labels: { ...(n.labels ?? {}) },
      online: n.status.online,
      maintenance: n.spec.maintenance === true,
      witness: n.spec.witness === true,
      frozen: !n.status.online && n.status.shutdown?.action === "freeze",
      leader: leader !== null && leader === n.id,
      cpus: n.status.cpus,
      memoryMib: n.status.memory_mib,
      load: fresh ? { cpu: permille(l.cpu_use_pct), mem: permille(l.mem_use_pct) } : null,
      allocatedPct:
        l && l.mem_total_mib > 0 ? Math.round((l.mem_allocated_mib / l.mem_total_mib) * 100) : null,
      alerts: alerts.get(n.id)?.count ?? 0,
      severity: alerts.get(n.id)?.severity ?? null,
      hiddenVms: l && input.vmsLimited ? Math.max(0, l.vms - (visiblePlaced.get(n.id) ?? 0)) : 0,
      templates: templates.get(n.id) ?? 0,
      pinnedRunning: pinnedRunning.get(n.id) ?? 0,
      outgoing: outgoing.get(n.id) ?? 0,
      pools: witness ? [] : local,
      sharedDown: witness ? [] : sharedDown,
      uplink: uplinkState(n),
      gateway: n.spec.sdn?.gateway !== undefined || n.status.sdn?.gateway === true,
      // 펜싱(`status.ha`)을 모르는 옛 노드(버전도 알리지 않음)는 그리지 않는다(빈 칸을 "없음"으로 보이지 않음).
      fencing: witness ? null : n.status.ha ? true : n.status.version === undefined ? null : false,
      fenceError: n.status.fence_check?.error ?? null,
    };
  });
  nodes.sort(byName);

  const shared = (input.pools ?? [])
    .filter((p) => p.spec.shared)
    .map((p) => p.name)
    .sort(naturalCompare);
  return {
    name: input.cluster?.name ?? null,
    nodes,
    vms,
    moves,
    placement: placement !== null,
    balancer: placement ? (placement.settings?.mode ?? "recommend") : null,
    shared,
  };
}
