import type { components } from "@/api/schema";
import type { Snapshot, StoragePool, Volume } from "@/api/types";
import { blockSnapshotKind, poolFormats } from "@/storage/pools";
import { validName, type VmFormValues } from "@/vms/form";

/** 서버의 `MAX_DESCRIPTION_CHARS`(문자 수) */
const MAX_DESCRIPTION_CHARS = 1024;

/** 나무 목록의 한 줄: 스냅샷과 그 깊이(뿌리가 0) */
export interface TreeRow {
  snapshot: Snapshot;
  depth: number;
}

/**
 * 한 VM의 스냅샷을 나무 차례로 편다. 형제는 id(UUIDv7) 차례, 곧 만든 차례다. 목록에 없는 부모를
 * 가리키는 스냅샷은 뿌리로 둔다.
 */
export function snapshotTree(snapshots: readonly Snapshot[]): TreeRow[] {
  const ids = new Set(snapshots.map((s) => s.id));
  const sorted = [...snapshots].sort((a, b) => (a.id < b.id ? -1 : 1));
  const children = new Map<string | null, Snapshot[]>();
  for (const s of sorted) {
    const parent = s.spec.parent && ids.has(s.spec.parent) ? s.spec.parent : null;
    children.set(parent, [...(children.get(parent) ?? []), s]);
  }
  const rows: TreeRow[] = [];
  const seen = new Set<string>();
  const visit = (parent: string | null, depth: number) => {
    for (const s of children.get(parent) ?? []) {
      if (!seen.has(s.id)) {
        seen.add(s.id);
        rows.push({ snapshot: s, depth });
        visit(s.id, depth + 1);
      }
    }
  };
  visit(null, 0);
  // 부모가 돌고 도는(망가진) 스냅샷은 뿌리에서 닿지 않으므로 남은 것을 뿌리로 더한다.
  for (const s of sorted) {
    if (!seen.has(s.id)) {
      seen.add(s.id);
      rows.push({ snapshot: s, depth: 0 });
      visit(s.id, 1);
    }
  }
  return rows;
}

/** 새 스냅샷 이름의 입력 오류(번역 키). `taken`은 이 VM에 이미 있는 스냅샷 이름이다. */
export function snapshotNameError(name: string, taken: readonly string[]): string | undefined {
  if (!validName(name)) {
    return "forms.name";
  }
  if (taken.includes(name)) {
    return "snapshots.errors.exists";
  }
  return undefined;
}

/** 설명의 입력 오류(번역 키). 서버처럼 바이트가 아니라 문자 수로 센다. */
export function descriptionError(description: string): string | undefined {
  return [...description].length > MAX_DESCRIPTION_CHARS
    ? "snapshots.errors.description"
    : undefined;
}

/** 스냅샷 방식을 따질 디스크: 볼륨 이름, 형식, 볼륨의 풀(모르면 `undefined`) */
export interface SnapshotDisk {
  volume: string;
  format: components["schemas"]["ImageFormat"];
  pool: StoragePool | undefined;
}

/**
 * 디스크들의 스냅샷 가능 여부. `ok`면 `block`이 블록 스냅샷(LVM-thin·ZFS, 메모리 없음)인지,
 * `format`은 풀 종류가 담지 못하는 형식의 디스크, `mixed`는 방식(qcow2 내부 스냅샷, 블록 백엔드
 * 종류마다의 블록 스냅샷)이 섞인 것이다.
 */
export type SnapshotSupport =
  { kind: "ok"; block: boolean } | { kind: "format"; disk: SnapshotDisk } | { kind: "mixed" };

/** 볼륨을 스냅샷 방식을 따질 디스크로 바꾼다. */
export function volumeDisk(volume: Volume, pools: readonly StoragePool[]): SnapshotDisk {
  return {
    volume: volume.name,
    format: volume.spec.format,
    pool: pools.find((p) => p.name === volume.spec.pool),
  };
}

/**
 * 서버의 `check_disks`처럼 판단한다: 파일 풀의 디스크는 qcow2, 블록 풀(LVM-thin, ZFS)의 디스크는
 * raw여야 하고, 모든 디스크가 같은 방식이어야 한다. 서버와 다른 점: 호출자는 목록에 없는 볼륨을 넘기지 않으므로
 * (서버는 400) 그런 디스크는 건너뛴 셈이고, 풀을 모르는 디스크는 파일 풀처럼 본다(서버는 400).
 */
export function snapshotSupport(disks: readonly SnapshotDisk[]): SnapshotSupport {
  // 디스크마다의 방식: 블록 백엔드 종류, qcow2 내부 스냅샷이면 `null`. 아직 디스크가 없으면 `undefined`.
  let method: string | null | undefined;
  for (const disk of disks) {
    const kind = blockSnapshotKind(disk.pool) ?? null;
    if (disk.format !== poolFormats(disk.pool)[0]) {
      return { kind: "format", disk };
    }
    if (method !== undefined && method !== kind) {
      return { kind: "mixed" };
    }
    method = kind;
  }
  return { kind: "ok", block: typeof method === "string" };
}

/**
 * ZFS 디스크라 되돌릴 수 없는 스냅샷인지. 서버의 `check_zfs_latest`처럼 스냅샷이 찍은 디스크
 * (`vm_spec`)의 ZFS 풀 볼륨 가운데 하나를 담은 더 나중의 스냅샷이 `snapshots`(같은 VM의 스냅샷)에
 * 있으면 막힌다. 순서는 만든 시각, 같으면 id(UUIDv7)이고 만드는 중인 스냅샷은 가장 나중이다. 목록에
 * 없는 볼륨·풀은 건너뛴다.
 */
export function zfsNotLatest(
  snapshot: Snapshot,
  snapshots: readonly Snapshot[],
  volumes: readonly Volume[],
  pools: readonly StoragePool[],
): boolean {
  // 스냅샷이 찍은 디스크의 볼륨(목록에 있는 것)
  const diskVolumes = (s: Snapshot) =>
    (s.spec.vm_spec.disks ?? []).flatMap((d) => {
      const volume = volumes.find((v) => v.name === d.volume);
      return volume ? [volume] : [];
    });
  const zfs = new Set(
    diskVolumes(snapshot)
      .filter((v) => pools.find((p) => p.name === v.spec.pool)?.spec.backend.kind === "zfs")
      .map((v) => v.id),
  );
  const newer = (a: Snapshot, b: Snapshot) => {
    const at = (s: Snapshot) => s.status.created_at ?? Infinity;
    return at(a) !== at(b) ? at(a) > at(b) : a.id > b.id;
  };
  return snapshots.some((s) => newer(s, snapshot) && diskVolumes(s).some((v) => zfs.has(v.id)));
}

/**
 * VM 폼의 디스크로 판단한다. 새 디스크는 풀의 기본 형식으로 만든다. 아직 고르지 않은 볼륨·풀은
 * 따지지 않는다. 볼륨·풀 목록을 다 읽기 전에는 모른다(`undefined`).
 */
export function formSnapshotSupport(
  values: VmFormValues,
  volumes: readonly Volume[] | undefined,
  pools: readonly StoragePool[] | undefined,
): SnapshotSupport | undefined {
  if (volumes === undefined || pools === undefined) {
    return undefined;
  }
  const disks = values.disks.flatMap((d): SnapshotDisk[] => {
    if (d.kind === "existing") {
      const volume = volumes.find((v) => v.name === d.volume);
      return volume ? [volumeDisk(volume, pools)] : [];
    }
    const pool = pools.find((p) => p.name === d.pool);
    return pool ? [{ volume: d.volume, format: poolFormats(pool)[0], pool }] : [];
  });
  return snapshotSupport(disks);
}
