import type {
  Node,
  ReplicaStatus,
  ReplicationState,
  ReplicationTarget,
  StoragePool,
  Vm,
  Volume,
} from "@/api/types";
import { isShared } from "@/storage/pools";
import { haEnabled } from "@/vms/ha";

/** 서버의 `MAX_REPLICATION_TARGETS` */
export const MAX_REPLICATION_TARGETS = 3;
/** 서버의 `MAX_INTERVAL_MINUTES`(하루) */
export const MAX_REPLICATION_MINUTES = 1440;
/** 서버의 기본 주기 `*\/15`의 분 */
export const DEFAULT_REPLICATION_MINUTES = 15;

/** 주기 `*\/N`의 N(분, 1~1440). 틀린 모양이면 null(서버의 `parse_interval`) */
export function intervalMinutes(schedule: string): number | null {
  const m = /^\*\/(\d+)$/.exec(schedule);
  const n = m ? Number(m[1]) : NaN;
  return Number.isInteger(n) && n >= 1 && n <= MAX_REPLICATION_MINUTES ? n : null;
}

/** 복제 카드·노드 표의 대상 한 줄 */
export interface ReplicaRow {
  /** 대상 노드 id */
  node: string;
  /** 노드 이름(모르면 id) */
  name: string;
  /** 상태 항목이 아직 없는 대상은 `pending`이다. */
  state: ReplicationState;
  /** 대상 풀. 없으면 디스크마다 같은 이름의 풀 */
  pool: string | null;
  /** 주기(분). 스펙에 없는 항목(HA 복구 뒤 남은 옛 원본)은 null */
  minutes: number | null;
  /** 마지막으로 끝난 회차의 시점(유닉스 초) */
  syncedAt: number | null;
  /** 그 시점에서 지난 초 */
  age: number | null;
  /** 마지막 회차가 보낸 바이트와 방식. 끝난 회차가 없으면 null */
  last: { bytes: number; full: boolean; secs: number } | null;
  /** 끝난 회차가 담은 디스크 크기의 합 */
  size: number;
  /** 다음 회차 예정 시각(유닉스 초). 꺼진 VM이나 스펙에 없는 항목은 null */
  nextRun: number | null;
  /** 마지막 실패 이유 */
  error: string | null;
}

/**
 * VM의 복제 대상마다 한 줄: 스펙의 대상 순서, 그 뒤에 스펙에 없는 상태 항목(옛 원본). 다음 회차는 서버 스케줄러처럼
 * 앞 시도·시점 가운데 늦은 것에서 주기 뒤이고, 아직 시도가 없으면 지금이다. 꺼진 VM은 복제하지 않는다.
 */
export function replicaRows(vm: Vm, nodes: readonly Node[], now: number): ReplicaRow[] {
  const entries = vm.status.replication ?? [];
  const targets = vm.spec.replication ?? [];
  const running = vm.status.phase === "running";
  const row = (
    node: string,
    target: ReplicationTarget | undefined,
    entry: ReplicaStatus | undefined,
  ): ReplicaRow => {
    const minutes = target ? intervalMinutes(target.schedule) : null;
    const syncedAt = entry?.synced_at ?? null;
    const done = entry?.generation != null;
    const since = Math.max(entry?.last_try ?? 0, syncedAt ?? 0);
    return {
      node,
      name: nodes.find((n) => n.id === node)?.name ?? node,
      state: entry?.state ?? "pending",
      pool: target?.pool ?? null,
      minutes,
      syncedAt,
      age: syncedAt === null ? null : Math.max(0, now - syncedAt),
      last: done
        ? {
            bytes: entry?.last_bytes ?? 0,
            full: entry?.last_full ?? false,
            secs: entry?.last_secs ?? 0,
          }
        : null,
      size: done ? (entry?.volumes ?? []).reduce((sum, v) => sum + v.size_bytes, 0) : 0,
      nextRun:
        running && minutes !== null
          ? since > 0
            ? Math.max(now, since + minutes * 60)
            : now
          : null,
      error: entry?.error ?? null,
    };
  };
  return [
    ...targets.map((t) =>
      row(
        t.node,
        t,
        entries.find((e) => e.node === t.node),
      ),
    ),
    ...entries
      .filter((e) => !targets.some((t) => t.node === e.node))
      .map((e) => row(e.node, undefined, e)),
  ];
}

/** 노드 로컬 풀(공유가 아닌 풀)에 있는 첫 디스크. `pool`이 있는 디스크(만들 새 볼륨)는 볼륨 대신 그 풀을 본다. */
export function firstLocalDisk(
  disks: readonly { volume: string; pool?: string }[],
  volumes: readonly Volume[],
  pools: readonly StoragePool[],
): { volume: string; pool: string } | null {
  for (const disk of disks) {
    const name = disk.pool || volumes.find((v) => v.name === disk.volume)?.spec.pool;
    const pool = pools.find((p) => p.name === name);
    if (pool && !isShared(pool)) {
      return { volume: disk.volume, pool: pool.name };
    }
  }
  return null;
}

/**
 * HA VM의 디스크가 노드 로컬 풀에 있는데 복제가 없으면 그 디스크(CLI의 경고와 같은 조건): 그 노드가 죽으면 HA가
 * 다른 노드에서 켤 수 없다. 아니면 null
 */
export function replicationWarning(
  vm: Vm,
  volumes: readonly Volume[],
  pools: readonly StoragePool[],
): { volume: string; pool: string } | null {
  if (!haEnabled(vm.spec) || (vm.spec.replication ?? []).length > 0) {
    return null;
  }
  return firstLocalDisk(vm.spec.disks ?? [], volumes, pools);
}

/**
 * 노드 `node`의 준비된 복제본을 HA·마이그레이션이 쓸 수 없게 한 디스크(서버 `replica_ready`와 같은 규칙): VM의 지금 노드
 * 로컬 풀 디스크 가운데 그 회차에 같은 크기(`spec.size_bytes`)로 담기지 않은 첫 디스크(더했거나 크기를 바꿨다). 쓸 수
 * 있거나 준비된 복제본이 아니면 null
 */
export function unusableDisk(
  vm: Vm,
  node: string,
  volumes: readonly Volume[],
  pools: readonly StoragePool[],
): string | null {
  const entry = vm.status.replication?.find((e) => e.node === node);
  if (entry?.state !== "ready") {
    return null;
  }
  for (const disk of vm.spec.disks ?? []) {
    const volume = volumes.find((v) => v.name === disk.volume);
    const pool = pools.find((p) => p.name === volume?.spec.pool);
    if (!volume || !pool || isShared(pool)) {
      continue;
    }
    const held = (entry.volumes ?? []).some(
      (v) => v.id === volume.id && v.size_bytes === volume.spec.size_bytes,
    );
    if (!held) {
      return disk.volume;
    }
  }
  return null;
}

/** VM들 가운데 노드 `node`가 복제 대상인 VM과 그 대상의 상태(항목이 없으면 `pending`) */
export function replicasOn(
  node: string,
  vms: readonly Vm[],
): { vm: Vm; state: ReplicationState }[] {
  return vms.flatMap((vm) =>
    (vm.spec.replication ?? []).some((t) => t.node === node)
      ? [{ vm, state: vm.status.replication?.find((e) => e.node === node)?.state ?? "pending" }]
      : [],
  );
}

/** 지난 초를 짧게: `45s`, `4m12s`, `3h5m`, `2d3h`(CLI의 나이와 같은 모양) */
export function formatAge(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) {
    return `${s}s`;
  }
  if (s < 3600) {
    return `${Math.floor(s / 60)}m${s % 60}s`;
  }
  if (s < 86400) {
    return `${Math.floor(s / 3600)}h${Math.floor((s % 3600) / 60)}m`;
  }
  return `${Math.floor(s / 86400)}d${Math.floor((s % 86400) / 3600)}h`;
}
