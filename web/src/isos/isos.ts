import type { StoragePool, Vm, Volume } from "@/api/types";
import { isShared } from "@/storage/pools";
import { nameFromFile } from "@/storage/upload";

/** ISO 이미지 볼륨(`content: iso`) */
export function isIso(volume: Volume): boolean {
  return volume.spec.content === "iso";
}

/** ISO 이미지 볼륨만 */
export function isoVolumes(volumes: readonly Volume[]): Volume[] {
  return volumes.filter(isIso);
}

/**
 * ISO `iso`를 CD-ROM 드라이브에 넣은 VM의 이름(서버의 `vm_uses_volume`처럼 스펙과, 아직 빼기를 마치지 못한 하드웨어
 * 기록의 매체).
 */
export function usedBy(vms: readonly Vm[], iso: string): string[] {
  return vms
    .filter((vm) =>
      [...(vm.spec.cdroms ?? []), ...(vm.status.hardware?.cdroms ?? [])].some((c) => c.iso === iso),
    )
    .map((vm) => vm.name);
}

/** ISO를 둘 수 있는 풀(서버의 `pool_holds_isos` — 디렉터리·NFS 풀) */
export function isoPools(pools: readonly StoragePool[]): StoragePool[] {
  return pools.filter((p) => p.spec.backend.kind === "dir" || p.spec.backend.kind === "nfs");
}

/**
 * CD-ROM에 넣을 수 있는 ISO: 준비된 ISO 볼륨. HA·복제 VM(`sharedOnly`)은 공유 풀의 ISO만(서버의 400 — 다른 노드에서 켤
 * 수 없다). 풀 목록을 모르면 공유 여부를 모르므로 거르지 않는다.
 */
export function choosableIsos(
  volumes: readonly Volume[],
  pools: readonly StoragePool[] | undefined,
  sharedOnly: boolean,
): Volume[] {
  return isoVolumes(volumes).filter((v) => {
    if (v.status.phase !== "ready") {
      return false;
    }
    const pool = pools?.find((p) => p.name === v.spec.pool);
    return !sharedOnly || pool === undefined || isShared(pool);
  });
}

/** URL 끝의 파일 이름으로 볼륨 이름을 제안한다: `https://x/ubuntu-24.04-live.iso?a=1` → `ubuntu-24-04-live` */
export function nameFromUrl(url: string): string {
  let path: string;
  try {
    path = new URL(url).pathname;
  } catch {
    // 아직 다 적지 않은 URL
    path = url.split(/[?#]/)[0] ?? "";
  }
  const file =
    path
      .split("/")
      .filter((s) => s !== "")
      .pop() ?? "";
  let decoded: string;
  try {
    decoded = decodeURIComponent(file);
  } catch {
    // 잘못된 퍼센트 인코딩(`%E0%A4%A` 등, 적는 중일 수도 있음)은 그대로 쓴다.
    decoded = file;
  }
  return nameFromFile(decoded);
}

/** 받기 URL의 입력 오류(번역 키). 서버처럼 http·https URL만 */
export function urlError(url: string): string | undefined {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" || parsed.protocol === "https:"
      ? undefined
      : "downloads.errors.url";
  } catch {
    return "downloads.errors.url";
  }
}

/** SHA-256 입력 오류(번역 키). 비면 확인하지 않는다. */
export function sha256Error(sha256: string): string | undefined {
  return sha256 === "" || /^[0-9a-fA-F]{64}$/.test(sha256) ? undefined : "downloads.errors.sha256";
}
