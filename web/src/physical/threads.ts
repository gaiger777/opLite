// 자세히 판 CPU 칸(계획 75 §4.1): 물리 스레드 격자. NUMA 노드마다 묶고(모르면 한 묶음), 칸마다 예약·전용(고정된 VM 이름)·
// 공유. 칸 ↔ VM은 `GET /nodes/{key}/topology`의 `vms[].vcpus`(자리마다 pCPU)로 잇는다. 그리기와 떨어진 순수 함수다.
import type { NodeCapacity } from "@/physical/api";
import { type NodeTopologyInfo, parseCpuList } from "@/numa/numa";

export type ThreadKind = "reserved" | "dedicated" | "shared";

export interface ThreadCell {
  cpu: number;
  kind: ThreadKind;
  /** 이 스레드에 고정된 VM 이름(전용·고정) */
  vm?: string;
}

export interface ThreadGroup {
  /** NUMA 노드 id, 모르면 없음 */
  numa?: number;
  cells: ThreadCell[];
}

const list = (s: string | undefined) => new Set(parseCpuList(s ?? "") ?? []);

/**
 * 격자. `capacity.numa`가 있으면 NUMA 묶음과 집합(예약·전용)을 거기서, 없으면 토폴로지의 집합(`sets`)으로 한 묶음을, 그것도
 * 없으면 스레드 수만큼 모두 공유 칸이다(예약 수는 글로만 보인다).
 */
export function threadGroups(capacity: NodeCapacity, topology?: NodeTopologyInfo): ThreadGroup[] {
  const pinned = new Map<number, string>();
  for (const vm of topology?.vms ?? []) {
    for (const cpu of vm.vcpus) {
      pinned.set(cpu, vm.vm);
    }
  }
  const cell = (cpu: number, reserved: Set<number>, dedicated: Set<number>): ThreadCell => {
    const kind: ThreadKind = reserved.has(cpu)
      ? "reserved"
      : dedicated.has(cpu)
        ? "dedicated"
        : "shared";
    const vm = pinned.get(cpu);
    return vm === undefined ? { cpu, kind } : { cpu, kind, vm };
  };
  const numa = capacity.numa ?? [];
  if (numa.length > 0) {
    return numa.map((n) => {
      const reserved = list(n.reserved);
      const dedicated = list(n.dedicated);
      return {
        numa: n.id,
        cells: [...list(n.cpus)].sort((a, b) => a - b).map((c) => cell(c, reserved, dedicated)),
      };
    });
  }
  const reserved = list(topology?.sets?.reserved);
  const dedicated = list(topology?.sets?.dedicated);
  const cpus = Array.from({ length: capacity.cpu.threads }, (_, i) => i);
  return [{ cells: cpus.map((c) => cell(c, reserved, dedicated)) }];
}

/** VM 이름 하나가 잡은 스레드 — 고정이면 그 칸들, 공유면 빈 목록(공유 영역 전체를 강조한다) */
export function threadsOf(topology: NodeTopologyInfo | undefined, vm: string): number[] {
  return topology?.vms.find((v) => v.vm === vm)?.vcpus ?? [];
}
