import type { components } from "@/api/schema";
import type { Snapshot, StoragePool, Volume } from "@/api/types";
import { poolFormats } from "@/storage/pools";
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
 * 디스크들의 스냅샷 가능 여부. `ok`면 `lvm`이 LV 스냅샷(메모리 없음)인지, `format`은 풀 종류가
 * 담지 못하는 형식의 디스크, `mixed`는 qcow2 내부 스냅샷과 LV 스냅샷이 섞인 것이다.
 */
export type SnapshotSupport =
  { kind: "ok"; lvm: boolean } | { kind: "format"; disk: SnapshotDisk } | { kind: "mixed" };

/** 볼륨을 스냅샷 방식을 따질 디스크로 바꾼다. */
export function volumeDisk(volume: Volume, pools: readonly StoragePool[]): SnapshotDisk {
  return {
    volume: volume.name,
    format: volume.spec.format,
    pool: pools.find((p) => p.name === volume.spec.pool),
  };
}

/**
 * 서버의 `check_disks`처럼 판단한다: 파일 풀의 디스크는 qcow2, LVM-thin 풀의 디스크는 raw여야 하고,
 * 모든 디스크가 같은 방식이어야 한다. 서버와 다른 점: 호출자는 목록에 없는 볼륨을 넘기지 않으므로
 * (서버는 400) 그런 디스크는 건너뛴 셈이고, 풀을 모르는 디스크는 파일 풀처럼 본다(서버는 400).
 */
export function snapshotSupport(disks: readonly SnapshotDisk[]): SnapshotSupport {
  let lvm: boolean | undefined;
  for (const disk of disks) {
    const lv = disk.pool?.spec.backend.kind === "lvm-thin";
    if (disk.format !== poolFormats(disk.pool)[0]) {
      return { kind: "format", disk };
    }
    if (lvm !== undefined && lvm !== lv) {
      return { kind: "mixed" };
    }
    lvm = lv;
  }
  return { kind: "ok", lvm: lvm === true };
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
