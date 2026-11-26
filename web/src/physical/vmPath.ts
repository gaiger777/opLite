// "이 VM의 경로"(계획 75 §4.1 자세히 판 VM): vCPU → 스레드, 메모리 → NUMA, 디스크 → 풀, NIC → 네트워크 → 브리지 → 포트(상태).
// 노드의 토폴로지(고정 pCPU·NUMA), 용량(풀 상태), 물리 사슬(`buildChain`)을 모아 한 줄씩 낸다. 그리기와 떨어진 순수 함수다.
import type { Volume, Vm } from "@/api/types";
import type { NetRail, NodeCapacity, NodePhysical } from "@/physical/api";
import { buildChain, carrierOf, type ChainItem, lowerPorts } from "@/physical/chain";
import type { NodeTopologyInfo } from "@/numa/numa";
import { vcpus } from "@/vms/VmListPage";

export interface VmPath {
  vcpus: number;
  /** 고정된 pCPU(자리 순). 비면 공유 스레드 위에서 돈다. */
  pinned: number[];
  memoryMib: number;
  /** 묶인 호스트 NUMA 노드. 비면 묶지 않음 */
  numa: number[];
  hugepages?: string;
  disks: { volume: string; pool?: string; ready?: boolean }[];
  nics: {
    network: string;
    /** 이 노드에서 레일 상태(레일이 없으면 없음) */
    state?: string;
    carrier?: ChainItem;
    ports: ChainItem[];
  }[];
}

/** VM의 경로. 놓인 노드의 자료가 없으면(꺼짐·옛 노드) 그 줄은 빈다. */
export function vmPath(
  vm: Vm,
  {
    topology,
    capacity,
    physical,
    rails,
    volumes,
    sharedPools = [],
  }: {
    topology?: NodeTopologyInfo;
    capacity?: NodeCapacity;
    physical?: NodePhysical;
    rails: readonly NetRail[];
    volumes: readonly Volume[];
    sharedPools?: readonly { pool: string; nodes_ready: number }[];
  },
): VmPath {
  const placed = topology?.vms.find((v) => v.vm === vm.name);
  const chain = physical ? buildChain(physical, rails) : undefined;
  const pools = new Map<string, boolean>([
    ...(capacity?.storage ?? []).map((p) => [p.pool, p.ready] as [string, boolean]),
    ...sharedPools.map((p) => [p.pool, p.nodes_ready > 0] as [string, boolean]),
  ]);
  return {
    vcpus: vcpus(vm),
    pinned: placed?.vcpus ?? [],
    memoryMib: vm.spec.memory_mib,
    numa: placed?.host_nodes ?? [],
    hugepages: vm.spec.hugepages ?? undefined,
    disks: (vm.spec.disks ?? []).map((d) => {
      const pool = volumes.find((v) => v.name === d.volume)?.spec.pool;
      return { volume: d.volume, pool, ready: pool === undefined ? undefined : pools.get(pool) };
    }),
    nics: (vm.spec.nics ?? []).map((nic) => {
      const rail = rails.find((r) => r.name === nic.network);
      const carrier = rail && chain ? carrierOf(chain, rail.id) : undefined;
      return {
        network: nic.network,
        state: rail?.nodes.find((n) => n.node === physical?.node)?.state,
        carrier,
        ports: carrier && chain ? lowerPorts(chain, carrier.id) : [],
      };
    }),
  };
}
