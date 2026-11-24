// 옮길 수 있는 노드와 복사할 디스크(계획 71 설계 §5 — 옮기기 대화상자에서 옮김). 대화상자·일괄 옮기기·팔레트·지도
// 끌어 옮기기가 함께 쓰는 순수 함수다.
import type { components } from "@/api/schema";
import type { Node, StoragePool, Vm, Volume } from "@/api/types";
import { isShared } from "@/storage/pools";

type Schemas = components["schemas"];

/** 머신 타입의 아키텍처(서버의 `MachineType::arch`) */
const ARCH_OF: Record<Schemas["MachineType"], Schemas["Arch"]> = {
  q35: "x86_64",
  virt: "aarch64",
};

/**
 * 옮겨 갈 수 있어 보이는 노드: 위트니스가 아니고 온라인이고 유지보수 중이 아니며 VM 머신의 아키텍처인,
 * 지금 노드가 아닌 노드.
 * 풀·브리지·메모리는 서버가 본다(409로 알린다).
 */
export function migrationTargets(vm: Vm, nodes: readonly Node[]): Node[] {
  const here = vm.spec.node ?? vm.status.node;
  const arch = ARCH_OF[vm.spec.machine];
  return nodes.filter(
    (n) =>
      n.id !== here &&
      !n.spec.witness &&
      n.status.online &&
      !n.spec.maintenance &&
      n.status.arch === arch,
  );
}

/**
 * 마이그레이션이 대상 노드로 복사할 디스크: 노드 로컬 풀(공유가 아닌 풀)의 볼륨. 서버의 `disk_moves`처럼
 * 볼륨·풀 기록이 없는 디스크는 뺀다.
 */
export function localDisks(
  vm: Vm,
  volumes: readonly Volume[],
  pools: readonly StoragePool[],
): { volume: Volume; pool: StoragePool }[] {
  return (vm.spec.disks ?? []).flatMap((disk) => {
    const volume = volumes.find((v) => v.name === disk.volume);
    const pool = pools.find((p) => p.name === volume?.spec.pool);
    return volume && pool && !isShared(pool) ? [{ volume, pool }] : [];
  });
}
