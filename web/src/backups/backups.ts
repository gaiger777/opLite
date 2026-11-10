import type { Backup, StoragePool } from "@/api/types";

/** 백업 파일 크기의 합. 다 만들지 않은 백업은 undefined */
export function storedBytes(backup: Backup): number | undefined {
  if (backup.status.phase !== "ready") {
    return undefined;
  }
  return backup.status.disks.reduce((sum, disk) => sum + disk.stored_bytes, 0);
}

/** 백업을 담는 풀(`backups`를 켠 디렉터리·NFS 풀) */
export function backupPools(pools: readonly StoragePool[]): StoragePool[] {
  return pools.filter((pool) => pool.spec.backups === true);
}

/** 최근 백업이 위에 오게 정렬한다(id가 UUIDv7이라 문자열 순서가 만든 차례다). */
export function newestFirst(backups: readonly Backup[]): Backup[] {
  return [...backups].sort((a, b) => (a.id < b.id ? 1 : -1));
}

/** 전체 백업부터 이 백업까지의 체인. 목록에 없는 조상이 있으면 undefined */
export function chainOf(backup: Backup, all: readonly Backup[]): Backup[] | undefined {
  const byId = new Map(all.map((b) => [b.id, b]));
  const chain = [backup];
  for (let parent = backup.status.parent; parent; parent = chain[0]!.status.parent) {
    const found = byId.get(parent);
    // 체인이 고리를 이루는 일은 없지만 목록이 잘못돼도 멈춘다.
    if (!found || chain.includes(found)) {
      return undefined;
    }
    chain.unshift(found);
  }
  return chain;
}

/** 복원에 드는 체인 파일 크기의 합. 체인을 모르거나 다 만들지 않은 백업이 있으면 undefined */
export function chainBytes(backup: Backup, all: readonly Backup[]): number | undefined {
  const chain = chainOf(backup, all);
  let sum = 0;
  for (const b of chain ?? []) {
    const size = storedBytes(b);
    if (size === undefined) {
      return undefined;
    }
    sum += size;
  }
  return chain && sum;
}

/** 원격 사본 대상 하나에 사본이 있는 체인 백업 수(`sent`)와 체인 길이(`total`) */
export interface ChainReplica {
  remote: string;
  pool: string;
  sent: number;
  total: number;
}

/**
 * 증분 백업의 체인 복제 진행: 체인 백업들의 원격 사본 (원격, 풀)마다(전체 백업부터 처음 나온 차례) 체인
 * 가운데 사본이 ready로 적힌(사본 id가 있는) 백업의 수. 전체 백업이거나 체인을 모르면 빈 목록이다.
 */
export function chainReplicas(backup: Backup, all: readonly Backup[]): ChainReplica[] {
  const chain = backup.status.parent ? chainOf(backup, all) : undefined;
  if (!chain) {
    return [];
  }
  const targets: ChainReplica[] = [];
  for (const b of chain) {
    for (const r of b.status.replicas ?? []) {
      let target = targets.find((t) => t.remote === r.remote && t.pool === r.pool);
      if (!target) {
        target = { remote: r.remote, pool: r.pool, sent: 0, total: chain.length };
        targets.push(target);
      }
      if (r.state === "ready" && r.backup) {
        target.sent += 1;
      }
    }
  }
  return targets;
}
