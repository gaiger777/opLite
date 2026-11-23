import { useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { Resources, WatchKind } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";

/** 화면이 실시간으로 보는 리소스 종류. 사용자와 토큰은 감시할 수 없다. */
export const KINDS: WatchKind[] = [
  "vm",
  "volume",
  "storage-pool",
  "node",
  "task",
  "network",
  "snapshot",
  "security-group",
  "backup",
  "backup-job",
  "vm-import",
  "ha-group",
  "placement-rule",
  "folder",
];

/**
 * 서버가 상태를 덧붙인 보기 목록이라 watch로는 사건만 받고 그 화면의 질의를 다시 읽는 종류(계획 67): 라우터(게이트웨이
 * 순위)·유동 IP(상태)·부하 분산기(멤버 건강)·네트워크 옮기기. 모두 폴더를 따라 서버가 사건마다 거른다.
 */
export const VIEW_KINDS = [
  "router",
  "floating-ip",
  "load-balancer",
  "network-migration",
  // 백업 저장소(계획 68): 비밀을 뺀 보기라 사건이 오면 목록을 다시 읽는다.
  "backup-store",
] as const;
export type ViewKind = (typeof VIEW_KINDS)[number];

/**
 * 사건이 오면 다시 읽을 질의 키(앞부분). 라우터·유동 IP·부하 분산기는 서로의 상태와 네트워크 주소 표·토폴로지를 바꾼다.
 * 네트워크·VM 사건은 토폴로지·유동 IP 상태·옮기기 진행을 바꾼다.
 */
export const REFRESH_ON: Partial<Record<ViewKind | WatchKind, readonly (readonly string[])[]>> = {
  router: [
    ["routers"],
    ["floating-ips"],
    ["load-balancers"],
    ["network-ips"],
    ["network-topology"],
  ],
  "floating-ip": [["floating-ips"], ["network-ips"], ["network-topology"]],
  "load-balancer": [["load-balancers"], ["network-ips"], ["network-topology"]],
  "network-migration": [["network-migrations"], ["network-topology"]],
  "backup-store": [["backup-stores"]],
  network: [["network-topology"], ["load-balancers"]],
  vm: [["network-topology"], ["floating-ips"]],
};

/** 다시 읽기를 모으는 시간(사건이 몰려도 한 번 — 설계 §12.3 "1초 모아") */
export const REFRESH_BATCH_MS = 1000;

/**
 * 목록 없이 watch로만 받는 클러스터 설정(`Settings`, 늘 하나). 캐시 키는 `SETTINGS_KEY`이고, 연결할 때마다
 * `LiveProvider`가 다시 읽는다.
 */
export const SETTINGS_KIND = "settings";
export const SETTINGS_KEY = ["settings"] as const;
/** 구독 상태(`GET /subscription`). 키는 설정에 있으므로 설정이 바뀌거나 다시 연결하면 다시 읽는다. */
export const SUBSCRIPTION_KEY = ["subscription"] as const;
/** 구독 상태를 다시 읽는 주기(만료는 시각으로 정해진다) */
export const SUBSCRIPTION_REFRESH_MS = 60 * 60 * 1000;

/** 목록 응답. `revision`은 목록을 읽은 시점의 클러스터 revision이다. */
export interface List<T> {
  revision: number;
  items: T[];
}

interface Versioned {
  id: string;
  revision: number;
}

export type WatchEvent =
  | { type: "put"; kind: WatchKind | ViewKind | typeof SETTINGS_KIND; resource: Versioned }
  | {
      type: "deleted";
      revision: number;
      kind: WatchKind | ViewKind | typeof SETTINGS_KIND;
      resource: Versioned;
    }
  | { type: "reset"; revision: number }
  | { type: "error"; code: string; message?: string };

export function listKey(kind: WatchKind) {
  return ["list", kind] as const;
}

export async function fetchList<K extends WatchKind>(kind: K): Promise<List<Resources[K]>> {
  const list = await (() => {
    switch (kind) {
      case "vm":
        return call(api.GET("/vms"));
      case "volume":
        return call(api.GET("/volumes"));
      case "storage-pool":
        return call(api.GET("/storage-pools"));
      case "node":
        return call(api.GET("/nodes"));
      case "task":
        return call(api.GET("/tasks"));
      case "network":
        return call(api.GET("/networks"));
      case "snapshot":
        return call(api.GET("/snapshots"));
      case "security-group":
        return call(api.GET("/security-groups"));
      case "backup":
        return call(api.GET("/backups"));
      case "backup-job":
        return call(api.GET("/backup-jobs"));
      case "vm-import":
        return call(api.GET("/vm-imports"));
      case "ha-group":
        return call(api.GET("/ha-groups"));
      case "placement-rule":
        return call(api.GET("/placement-rules"));
      case "folder":
        return call(api.GET("/folders"));
    }
  })();
  return list as List<Resources[K]>;
}

/**
 * 목록에 이벤트 하나를 반영한다. 리소스의 revision이 목록에 있는 것보다 클 때만 적용하므로, 목록을 읽은
 * 시점보다 오래된 이벤트가 다시 와도 결과가 같다.
 */
export function applyEvent<T extends Versioned>(list: List<T>, event: WatchEvent): List<T> {
  if (event.type !== "put" && event.type !== "deleted") {
    return list;
  }
  const at = list.items.findIndex((item) => item.id === event.resource.id);
  const current = at >= 0 ? list.items[at] : undefined;
  if (event.type === "put") {
    if (current && current.revision >= event.resource.revision) {
      return list;
    }
    const items = [...list.items];
    if (at >= 0) {
      items[at] = event.resource as T;
    } else {
      items.push(event.resource as T);
    }
    return { ...list, items };
  }
  if (!current || current.revision >= event.revision) {
    return list;
  }
  return { ...list, items: list.items.filter((item) => item.id !== event.resource.id) };
}

/**
 * 리소스 목록. 처음에는 직접 읽고, 그 뒤로는 watch가 바꾼다(`LiveProvider`). 루트에서 읽어야 하는 종류(노드 등)를
 * 읽을 수 없으면(폴더 사용자) 묻지 않는다(`data`가 없다).
 */
export function useList<K extends WatchKind>(kind: K) {
  const readable = usePermissions().reads(kind);
  return useQuery({
    queryKey: listKey(kind),
    queryFn: () => fetchList(kind),
    staleTime: Infinity,
    select: (list) => list.items,
    enabled: readable,
  });
}

/**
 * 클러스터 설정. 아직 저장한 적이 없으면 서버가 기본값(revision 0)을 돌려준다. 루트 `cluster.read`가 없으면 묻지
 * 않는다(403이고, 권한을 잃으면 `LiveProvider`가 옛 값을 버린다).
 */
export function useSettings() {
  const readable = usePermissions().reads("settings");
  return useQuery({
    queryKey: SETTINGS_KEY,
    queryFn: () => call(api.GET("/settings")),
    staleTime: Infinity,
    enabled: readable,
  });
}

/** 구독 상태. 노드 수와 시각으로 서버가 계산한다. 시각에 따라 만료되므로 한 시간마다 다시 읽는다. */
export function useSubscription() {
  const readable = usePermissions().reads("settings");
  return useQuery({
    queryKey: SUBSCRIPTION_KEY,
    queryFn: () => call(api.GET("/subscription")),
    staleTime: Infinity,
    refetchInterval: SUBSCRIPTION_REFRESH_MS,
    enabled: readable,
  });
}
