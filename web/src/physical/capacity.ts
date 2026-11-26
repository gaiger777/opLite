// 노드별 할당·사용 표(계획 75 §4.1)의 셈: 행(노드마다 CPU·RAM·스토리지의 총·할당·사용), 정렬, "문제만" 거르기. 서버
// `GET /capacity`와 `GET /topology/physical`(장애 포트)을 받는다. 그리기와 떨어진 순수 함수다.
import type { NodeCapacity, NodePhysical } from "@/physical/api";

/** 이만큼(%) 넘게 쓰면 문제다(설계 §4.1 "사용 > 90%") */
export const HOT_PERCENT = 90;

/**
 * 공유 vCPU 오버커밋이 이만큼 넘으면 문제다(리뷰 1 — KVM에서 2~4배는 흔한 정상 운영이라 ×1로는 "문제만"이 쓸모없어진다). 설정
 * 한도가 생기면 그 값으로 바꾼다. 메모리는 ×1을 넘으면 문제다.
 */
export const CPU_OVERCOMMIT_LIMIT = 4;

/** 막대 하나의 값: 총(바탕), 할당(테두리 칸), 사용(진한 칸). 모르면 없다. */
export interface Measure {
  total: number;
  allocated?: number;
  used?: number;
}

export interface CapacityRow {
  id: string;
  node: string;
  online: boolean;
  /**
   * 공유 스레드(예약·전용 뺀 것) 위의 공유 vCPU(사용 칸 없음 — 노드 사용률은 전용 VM·호스트 몫을 담아 공유 풀의 눈금이 아니다,
   * 리뷰 1). 노드 사용률은 `cpuUsePct`, 전용 할당은 `cpuDedicated`.
   */
  cpu: Measure;
  cpuOvercommit?: number;
  /** 지난 1분 노드 전체(모든 스레드) CPU 사용률 % */
  cpuUsePct?: number;
  /** 잡힌 전용 CPU / 전용 CPU(전용이 없으면 없음) */
  cpuDedicated?: { used: number; total: number };
  /** MiB */
  memory: Measure;
  memoryOvercommit?: number;
  /** 로컬 풀 합(바이트): 할당 = 프로비전 */
  storage: Measure;
  poolErrors: number;
  faults: number;
  /** 도는 VM이 NIC를 붙였는데 이 노드에 없는 네트워크 수 */
  unreachable: number;
  /** 물리 관계를 받지 못해 링크 상태를 모름(문제 거르기에는 넣지 않는다 — 모든 노드가 걸린다) */
  linkUnknown: boolean;
  problems: Problem[];
}

export type Problem = "offline" | "overcommit" | "hot" | "pool" | "link" | "unreachable";

export type SortKey = "node" | "cpu" | "memory" | "storage";

const pct = (part: number | undefined, total: number) =>
  part === undefined || total <= 0 ? undefined : (part / total) * 100;

/**
 * 행 하나. `physical`은 같은 노드의 물리 관계(없으면 장애 0), `linkKnown`이 거짓이면 물리 관계를 받지 못한 것(링크 "모름").
 */
export function capacityRow(
  n: NodeCapacity,
  physical?: NodePhysical,
  linkKnown = true,
): CapacityRow {
  const shared = Math.max(0, n.cpu.threads - n.cpu.reserved - n.cpu.dedicated_total);
  const cpu: Measure = { total: shared, allocated: n.cpu.vcpus_shared };
  const memory: Measure = {
    total: n.memory.total_mib,
    allocated: n.memory.allocated_mib,
    used: n.memory.used_mib,
  };
  const storage: Measure = {
    total: n.storage.reduce((s, p) => s + p.total_bytes, 0),
    allocated: n.storage.reduce((s, p) => s + p.provisioned_bytes, 0),
    used: n.storage.reduce((s, p) => s + p.used_bytes, 0),
  };
  const poolErrors = n.storage.filter((p) => !p.ready).length;
  const faults = physical?.impact?.length ?? 0;
  const unreachable = physical?.unreachable?.length ?? 0;
  const problems: Problem[] = [];
  if (!n.online) problems.push("offline");
  if ((n.cpu.overcommit ?? 0) > CPU_OVERCOMMIT_LIMIT || (n.memory.overcommit ?? 0) > 1) {
    problems.push("overcommit");
  }
  const hot = [
    n.cpu.use_pct,
    pct(memory.used, memory.total),
    ...n.storage.map((p) => pct(p.used_bytes, p.total_bytes)),
  ];
  if (hot.some((v) => v !== undefined && v > HOT_PERCENT)) problems.push("hot");
  if (poolErrors > 0) problems.push("pool");
  if (faults > 0) problems.push("link");
  if (unreachable > 0) problems.push("unreachable");
  return {
    id: n.id,
    node: n.node,
    online: n.online,
    cpu,
    cpuOvercommit: n.cpu.overcommit,
    cpuUsePct: n.cpu.use_pct,
    cpuDedicated:
      n.cpu.dedicated_total > 0
        ? { used: n.cpu.dedicated_used, total: n.cpu.dedicated_total }
        : undefined,
    memory,
    memoryOvercommit: n.memory.overcommit,
    storage,
    poolErrors,
    faults,
    unreachable,
    linkUnknown: !linkKnown,
    problems,
  };
}

/** 정렬 값: 이름, 또는 할당 ÷ 총(총이 0이면 할당이 있으면 맨 위) */
function load(m: Measure): number {
  const a = m.allocated ?? m.used ?? 0;
  if (m.total <= 0) {
    return a > 0 ? Number.POSITIVE_INFINITY : 0;
  }
  return a / m.total;
}

/** 표의 행: 거르고(문제만) 정렬한다. 수치 열이 같으면 이름 순이다. `physical`이 없으면 링크 "모름"이다. */
export function capacityRows(
  nodes: readonly NodeCapacity[],
  physical: readonly NodePhysical[] | undefined,
  {
    sort = "node",
    desc = false,
    problemsOnly = false,
  }: {
    sort?: SortKey;
    desc?: boolean;
    problemsOnly?: boolean;
  } = {},
): CapacityRow[] {
  const byId = new Map((physical ?? []).map((p) => [p.id, p]));
  const rows = nodes
    .map((n) => capacityRow(n, byId.get(n.id), physical !== undefined))
    .filter((r) => !problemsOnly || r.problems.length > 0);
  const sign = desc ? -1 : 1;
  const name = (a: CapacityRow, b: CapacityRow) => a.node.localeCompare(b.node);
  return rows.sort((a, b) =>
    sort === "node" ? sign * name(a, b) : sign * (load(a[sort]) - load(b[sort])) || name(a, b),
  );
}

/** 할당이 총을 넘는 몫(0~): 넘침 꼬리의 길이 */
export function overflow(m: Measure): number {
  return m.allocated === undefined ? 0 : Math.max(0, m.allocated - m.total);
}
