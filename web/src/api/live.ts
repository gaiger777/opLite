import { useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { Resources, WatchKind } from "@/api/types";

/** 화면이 실시간으로 보는 리소스 종류. 사용자와 토큰은 감시할 수 없다. */
export const KINDS: WatchKind[] = [
  "vm",
  "volume",
  "storage-pool",
  "node",
  "task",
  "network",
  "snapshot",
];

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
  | { type: "put"; kind: WatchKind; resource: Versioned }
  | { type: "deleted"; revision: number; kind: WatchKind; resource: Versioned }
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

/** 리소스 목록. 처음에는 직접 읽고, 그 뒤로는 watch가 바꾼다(`LiveProvider`). */
export function useList<K extends WatchKind>(kind: K) {
  return useQuery({
    queryKey: listKey(kind),
    queryFn: () => fetchList(kind),
    staleTime: Infinity,
    select: (list) => list.items,
  });
}
