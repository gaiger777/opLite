import type { components } from "@/api/schema";
import type { Node, Snapshot, StoragePool, Vm, Volume } from "@/api/types";
import type { NumaOverride } from "@/numa/numa";
import { isShared, usableOn } from "@/storage/pools";
import { isStopped } from "@/vms/actions";
import type { CloudInit } from "@/vms/cloudInit";
import { haEnabled } from "@/vms/ha";

type ImageFormat = components["schemas"]["ImageFormat"];
type CloneRequest = components["schemas"]["CloneRequest"];

/** 연결 복제 방식: 되는 디스크는 연결(서버 기본), 모두 연결(안 되면 거절), 전체 복사 */
export type DeployMode = "auto" | "linked" | "full";

/** 템플릿 배포의 덮어쓰기. 비운 값(빈 글·null·없음)은 템플릿 그대로다. */
export interface DeployOverrides {
  mode: DeployMode;
  pool: string;
  node: string;
  vcpus?: number;
  memoryMib?: number;
  /** NIC마다 바꿀 네트워크(빈 글은 그대로) */
  nics: readonly string[];
  /** NIC마다 SDN 주소(모두 비면 없음) */
  ips?: string[][];
  /** 디스크마다 새 크기(바이트, 늘리기만 — null은 그대로) */
  disks: readonly (number | null)[];
  cloudInit?: CloudInit;
  start: boolean;
  folder?: string;
  cpu: { cpu_model?: string; cpu_flags?: string[] };
  numa: NumaOverride;
}

/**
 * `POST /vms/{key}/clone` 본문(배포 대화상자와 VM 빌더의 템플릿 바탕이 함께 쓴다): 바꾼 칸만 넣는다(옛 서버도 받는다).
 */
export function cloneBody(name: string, o: DeployOverrides): CloneRequest {
  return {
    name,
    ...(o.mode !== "auto" && { linked: o.mode === "linked" }),
    ...(o.pool !== "" && { pool: o.pool }),
    ...(o.node !== "" && { node: o.node }),
    ...(o.vcpus !== undefined && { vcpus: o.vcpus }),
    ...(o.memoryMib !== undefined && { memory_mib: o.memoryMib }),
    ...(o.nics.some((n) => n !== "") && { nics: o.nics.map((n) => (n === "" ? null : n)) }),
    ...(o.ips && { ips: o.ips }),
    ...(o.disks.some((b) => b !== null) && { disks: [...o.disks] }),
    ...(o.cloudInit && Object.keys(o.cloudInit).length > 0 && { cloud_init: o.cloudInit }),
    ...(o.folder && { folder: o.folder }),
    ...(o.start && { start: true }),
    ...o.cpu,
    ...o.numa,
  };
}

/** 한 번에 배포할 수 있는 VM 수(CLI의 `MAX_COUNT`와 같다) */
export const MAX_COUNT = 100;

/** 템플릿 VM(`spec.template`) */
export function isTemplate(vm: Vm): boolean {
  return vm.spec.template === true;
}

/** VM 디스크의 볼륨(디스크 순서, 목록에 없는 볼륨은 undefined) */
export function diskVolumes(vm: Vm, volumes: readonly Volume[]): (Volume | undefined)[] {
  return (vm.spec.disks ?? []).map((d) => volumes.find((v) => v.name === d.volume));
}

/** 기반 볼륨들의 연결 복제(`spec.base`가 그 id인 볼륨) */
export function linkedClones(
  bases: readonly (Volume | undefined)[],
  volumes: readonly Volume[],
): Volume[] {
  const ids = new Set(bases.flatMap((b) => (b ? [b.id] : [])));
  return volumes.filter((v) => v.spec.base != null && ids.has(v.spec.base));
}

/** 템플릿 목록의 행: 디스크 볼륨 이름, 풀(겹치지 않게), 연결 복제 */
export interface TemplateRow {
  vm: Vm;
  disks: string[];
  pools: string[];
  clones: Volume[];
}

/** 템플릿 VM마다의 행(CLI `template list`의 NAME·DISKS·POOL·CLONES), 이름 순 */
export function templateRows(vms: readonly Vm[], volumes: readonly Volume[]): TemplateRow[] {
  return vms
    .filter(isTemplate)
    .map((vm) => {
      const bases = diskVolumes(vm, volumes);
      return {
        vm,
        disks: (vm.spec.disks ?? []).map((d) => d.volume),
        pools: [...new Set(bases.flatMap((b) => (b ? [b.spec.pool] : [])))],
        clones: linkedClones(bases, volumes),
      };
    })
    .sort((a, b) => a.vm.name.localeCompare(b.vm.name));
}

/** 연결 복제 볼륨을 디스크로 쓰는 VM 이름(쓰는 VM이 없는 복제는 볼륨 이름) */
export function cloneOwners(clones: readonly Volume[], vms: readonly Vm[]): string[] {
  const names = clones.map(
    (c) => vms.find((vm) => (vm.spec.disks ?? []).some((d) => d.volume === c.name))?.name ?? c.name,
  );
  return [...new Set(names)].sort();
}

/**
 * 서버의 `pool_supports_linked_clones`: 디렉터리·NFS 풀은 qcow2 겹층으로, LVM-thin·ZFS·RBD는 raw 볼륨의 네이티브 복제로
 * 연결 복제를 둔다. 공유 LVM은 못 한다.
 */
export function poolSupportsLinkedClones(pool: StoragePool, format: ImageFormat): boolean {
  switch (pool.spec.backend.kind) {
    case "dir":
    case "nfs":
      return true;
    case "lvm-thin":
    case "zfs":
    case "rbd":
      return format === "raw";
    default:
      return false;
  }
}

/**
 * 템플릿 디스크마다 연결 복제가 되는지(서버의 배포 검사): 디스크를 둘 풀(`pool`, 비면 기반의 풀)이 기반의 풀과 같고 그 풀이
 * 연결 복제를 둘 수 있어야 한다. 볼륨이나 풀을 모르면 false다.
 */
export function linkableDisks(
  bases: readonly (Volume | undefined)[],
  pools: readonly StoragePool[],
  pool: string,
): boolean[] {
  return bases.map((base) => {
    if (!base || (pool !== "" && pool !== base.spec.pool)) {
      return false;
    }
    const found = pools.find((p) => p.name === base.spec.pool);
    return found !== undefined && poolSupportsLinkedClones(found, base.spec.format);
  });
}

/**
 * 배포할 VM 이름들(CLI의 `deploy_names`). `count`가 2 이상이면 `pattern`에 `{n}`이 있어야 하고 `{n}`은 `first`부터의
 * 번호다. 잘못이면 번역 키를 돌려준다.
 */
export function deployNames(
  pattern: string,
  count: number,
  first: number,
): { names: string[] } | { error: string } {
  if (!Number.isInteger(count) || count < 1 || count > MAX_COUNT) {
    return { error: "templates.errors.count" };
  }
  if (!Number.isInteger(first) || first < 0 || first > 1_000_000) {
    return { error: "templates.errors.first" };
  }
  if (count > 1 && !pattern.includes("{n}")) {
    return { error: "templates.errors.pattern" };
  }
  return {
    names: Array.from({ length: count }, (_, i) => pattern.replaceAll("{n}", String(first + i))),
  };
}

/**
 * VM을 템플릿으로 바꿀 수 없는 이유(번역 키). 서버의 `POST /vms/{key}/template` 조건: 꺼짐, 스냅샷 없음, HA·복제 대상·
 * 예약 스냅샷 없음, 연결 복제 디스크 없음. 스냅샷·볼륨 목록을 모르면 그것으로는 막지 않는다(서버에 맡긴다).
 */
export function convertReason(
  vm: Vm,
  snapshots: readonly Snapshot[] | undefined,
  volumes: readonly Volume[] | undefined,
): string | null {
  if (!isStopped(vm)) {
    return "templates.reason.notStopped";
  }
  if (snapshots?.some((s) => s.spec.vm === vm.id)) {
    return "templates.reason.snapshots";
  }
  if (haEnabled(vm.spec)) {
    return "templates.reason.ha";
  }
  if ((vm.spec.replication ?? []).length > 0) {
    return "templates.reason.replication";
  }
  if (vm.spec.snapshot_schedule) {
    return "templates.reason.schedule";
  }
  if (volumes && diskVolumes(vm, volumes).some((v) => v?.spec.base != null)) {
    return "templates.reason.linkedDisk";
  }
  return null;
}

/**
 * 템플릿을 VM으로 되돌리거나 지울 수 없는 이유(번역 키): 연결 복제가 남았다(서버의 409 `template_in_use`). 볼륨 목록을
 * 모르면 복제가 있는지 모른다.
 */
export function revertReason(vm: Vm, volumes: readonly Volume[] | undefined): string | null {
  if (!volumes) {
    return "templates.reason.clonesUnknown";
  }
  return linkedClones(diskVolumes(vm, volumes), volumes).length > 0
    ? "templates.reason.inUse"
    : null;
}

/**
 * 서버의 `template_node`: 템플릿의 배포 Task가 돌 노드. 고정한 노드·놓인 노드 가운데 클러스터에 있고 온라인인 노드, 아니면
 * 첫 디스크로 정한다 — 공유 풀은 풀을 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드, 로컬 풀은 볼륨이 있는 노드(클러스터에
 * 있으면). 그도 아니면 요청을 받는 노드(접속한 노드 `here`)다. 목록이나 접속한 노드를 아직 몰라 정할 수 없으면 undefined.
 */
export function templateNode(
  vm: Vm,
  volumes: readonly Volume[] | undefined,
  pools: readonly StoragePool[] | undefined,
  nodes: readonly Node[] | undefined,
  here: string | undefined,
): string | undefined {
  if (nodes === undefined) {
    return undefined;
  }
  const live = (id: string | null | undefined) =>
    nodes.find((n) => n.id === id)?.status.online === true ? id : undefined;
  const placed = live(vm.spec.node) ?? live(vm.status.node);
  if (placed) {
    return placed;
  }
  const first = vm.spec.disks?.[0]?.volume;
  if (first !== undefined) {
    if (volumes === undefined || pools === undefined) {
      return undefined;
    }
    const volume = volumes.find((v) => v.name === first);
    const pool = volume && pools.find((p) => p.name === volume.spec.pool);
    if (volume && pool && isShared(pool)) {
      const usable = nodes.filter((n) => usableOn(pool, n)).map((n) => n.id);
      usable.sort();
      if (usable[0] !== undefined) {
        return usable[0];
      }
    } else if (volume?.status.node && nodes.some((n) => n.id === volume.status.node)) {
      return volume.status.node;
    }
  }
  return here;
}
