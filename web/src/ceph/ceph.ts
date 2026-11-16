import { useQuery } from "@tanstack/react-query";

import { ApiError, api, call } from "@/api/client";
import type {
  CephReport,
  CephStatusView,
  CephView,
  NodeDisk,
  Node,
  OsdReport,
  PgReport,
  StoragePool,
} from "@/api/types";

/** `GET /ceph`의 캐시 키. Ceph 리소스는 watch로 나가지 않아(키가 있음) 이 화면들이 주기적으로 다시 읽는다. */
export const CEPH_KEY = ["ceph"] as const;
export const CEPH_STATUS_KEY = ["ceph", "status"] as const;
/** 상태를 다시 읽는 간격. 서버가 5초 캐시를 둔다. */
export const CEPH_REFRESH_MS = 10_000;
/** 서버의 `MAX_MONS` */
export const MAX_MONS = 5;

/** 부트스트랩 Task(`ceph-init`)의 진행 단계(서버의 진행률 stage 순서) */
export const INIT_STAGES = [
  "checking",
  "installing-packages",
  "writing-config",
  "creating-monitor",
  "starting-monitor",
  "waiting-for-quorum",
  "creating-manager",
] as const;

/** 관리 Ceph. 없으면(404) null이다. `poll`이면 Ceph 화면처럼 주기적으로 다시 읽는다. */
export function useCeph(poll = false) {
  return useQuery({
    queryKey: CEPH_KEY,
    queryFn: async (): Promise<CephView | null> => {
      try {
        return await call(api.GET("/ceph"));
      } catch (err) {
        if (err instanceof ApiError && err.status === 404) {
          return null;
        }
        throw err;
      }
    },
    refetchInterval: poll ? CEPH_REFRESH_MS : false,
  });
}

/** Ceph 상태(mon 노드에서 읽음). 503 `ceph_unreachable`을 곧바로 보이도록 다시 시도하지 않는다. */
export function useCephStatus(enabled: boolean) {
  return useQuery({
    queryKey: CEPH_STATUS_KEY,
    // openapi-fetch는 PG 상태의 짝(`[string, number]`)을 배열로 넓히므로 스키마 타입으로 되돌린다.
    queryFn: async () => (await call(api.GET("/ceph/status"))) as CephStatusView,
    refetchInterval: CEPH_REFRESH_MS,
    retry: false,
    enabled,
  });
}

/** health 배지의 모양 */
export type HealthTone = "ok" | "warn" | "err" | "unknown";

export function healthTone(status: string): HealthTone {
  switch (status) {
    case "HEALTH_OK":
      return "ok";
    case "HEALTH_WARN":
      return "warn";
    case "HEALTH_ERR":
      return "err";
    default:
      return "unknown";
  }
}

/** 호스트 하나의 OSD들 */
export interface OsdHost {
  host: string;
  osds: OsdReport[];
  up: number;
}

/** OSD를 호스트별로(호스트 이름 순, 그 안은 id 순) */
export function osdHosts(report: CephReport): OsdHost[] {
  const byHost = new Map<string, OsdReport[]>();
  for (const osd of report.osds) {
    byHost.set(osd.host, [...(byHost.get(osd.host) ?? []), osd]);
  }
  return [...byHost.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([host, osds]) => ({
      host,
      osds: [...osds].sort((a, b) => a.id - b.id),
      up: osds.filter((o) => o.up).length,
    }));
}

/** PG 상태 하나와 전체에서의 비율(%) */
export interface PgPart {
  state: string;
  count: number;
  percent: number;
  /** `active+clean`이면 참 */
  clean: boolean;
}

/** PG 상태 요약: 많은 상태부터. 전체가 0이면 빈 목록 */
export function pgSummary(pgs: PgReport): PgPart[] {
  if (pgs.total === 0) {
    return [];
  }
  return [...pgs.states]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([state, count]) => ({
      state,
      count,
      percent: (count * 100) / pgs.total,
      clean: state === "active+clean",
    }));
}

/** 새 풀의 기본 복제 수: min(3, OSD가 up인 호스트 수). 호스트가 없으면 1(서버가 거절한다) */
export function defaultSize(hosts: number): number {
  return Math.max(1, Math.min(3, hosts));
}

/** 복제 수의 경고(번역 키): 1은 사본 하나, 2는 하나가 꺼지면 잃을 수 있음 */
export function sizeWarning(size: number): string | null {
  return size === 1 ? "ceph.pool.sizeOne" : size === 2 ? "ceph.pool.sizeTwo" : null;
}

/** mon 수의 경고(번역 키): 짝수면 홀수(3·5)가 정족수를 가장 잘 지킨다. */
export function monWarning(count: number): string | null {
  return count > 0 && count % 2 === 0 ? "ceph.mon.even" : null;
}

/** 디스크를 OSD로 고를 수 있는지: 그대로, 지우고, 못 씀(이유) */
export type DiskChoice =
  { kind: "available" } | { kind: "wipe"; reasons: string[] } | { kind: "blocked"; reason: string };

export function diskChoice(disk: NodeDisk): DiskChoice {
  switch (disk.state) {
    case "available":
      return { kind: "available" };
    case "wipeable":
      return { kind: "wipe", reasons: disk.reasons };
    case "in-use":
      return { kind: "blocked", reason: disk.reason };
  }
}

/** 지우기 확인의 입력이 그 디스크인지(CLI처럼 `/dev/sdb`·`sdb`·by-id 경로) */
export function confirmWipe(disk: NodeDisk, input: string): boolean {
  const text = input.trim();
  return (
    text !== "" &&
    (text === disk.path || disk.path.replace(/^\/dev\//, "") === text || disk.by_id === text)
  );
}

/** IPv4 주소를 32비트 수로. 아니면 null */
function ipv4(text: string): number | null {
  const parts = text.split(".");
  if (parts.length !== 4 || parts.some((p) => !/^\d{1,3}$/.test(p) || Number(p) > 255)) {
    return null;
  }
  return parts.reduce((n, p) => n * 256 + Number(p), 0);
}

/**
 * 서버의 `default_public_network`처럼: 노드 주소(`host:port`나 `host`)의 IP를 가진 인터페이스 주소의 망(CIDR).
 * 마법사가 미리 채우는 값이라 IPv4만 본다(그 밖에는 빈 문자열 — 비워 두면 서버가 같은 규칙으로 정한다).
 */
export function publicNetworkFor(node: Node): string {
  const parts = node.spec.address.split(":");
  const ip = ipv4(parts.length <= 2 ? (parts[0] ?? "") : "");
  if (ip === null) {
    return "";
  }
  for (const iface of node.status.interfaces ?? []) {
    for (const cidr of iface.addresses ?? []) {
      const [addr = "", prefixText = ""] = cidr.split("/");
      const prefix = Number(prefixText);
      if (ipv4(addr) !== ip || !/^\d+$/.test(prefixText) || prefix > 32) {
        continue;
      }
      const mask = prefix === 0 ? 0 : (0xffffffff << (32 - prefix)) >>> 0;
      const net = (ip & mask) >>> 0;
      return `${[24, 16, 8, 0].map((s) => (net >>> s) & 255).join(".")}/${prefix}`;
    }
  }
  return "";
}

/** Ceph 역할이 있는 노드인지와 그 역할들(`mon`·`mgr`·`osd`) */
export function cephRoles(ceph: CephView | null | undefined, node: string): string[] {
  if (!ceph) {
    return [];
  }
  return [
    ceph.mons.some((m) => m.node === node) && "mon",
    ceph.mgrs.includes(node) && "mgr",
    ceph.osd_nodes.includes(node) && "osd",
  ].filter((r): r is string => r !== false);
}

/** Orbivisor가 관리하는 Ceph의 RBD 풀인지(관리 클라이언트 conf를 쓰는 풀) */
export function isManagedPool(pool: StoragePool, ceph: CephView | null | undefined): boolean {
  const b = pool.spec.backend;
  return !!ceph && b.kind === "rbd" && b.conf === ceph.managed_conf;
}

/** 사용률(0~100, 소수 한 자리). 전체가 0이면 0 */
export function percentOf(used: number, total: number): number {
  return total > 0 ? Math.round((Math.min(used, total) * 1000) / total) / 10 : 0;
}
