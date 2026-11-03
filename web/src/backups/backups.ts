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
