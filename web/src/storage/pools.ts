import type { components } from "@/api/schema";
import type { Node, StoragePool } from "@/api/types";

export type PoolState = components["schemas"]["PoolState"];

/** 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`) */
export function poolLocation(pool: StoragePool): string {
  const b = pool.spec.backend;
  if (b.kind === "dir") {
    return b.path;
  }
  return b.server.includes(":") ? `[${b.server}]:${b.export}` : `${b.server}:${b.export}`;
}

/** 모든 노드가 같은 데이터를 보는 풀인지. NFS 풀은 늘 공유다(서버의 `is_shared`). */
export function isShared(pool: StoragePool): boolean {
  return pool.spec.shared === true || pool.spec.backend.kind === "nfs";
}

/**
 * NFS 서버 입력 오류(번역 키). 서버처럼 공백·`,`·`/`·`[`·`]`가 없고 `-`로 시작하지 않아야 한다.
 * `:`는 IPv6 주소에 쓰므로 여기서 막지 않는다(나머지는 서버가 본다).
 */
export function nfsServerError(server: string): string | undefined {
  return server === "" || server.startsWith("-") || /[\s,/[\]]/.test(server)
    ? "pools.errors.server"
    : undefined;
}

/** 서버의 `char::is_control`처럼 C0·C1 제어 문자와 DEL */
function hasControl(text: string): boolean {
  return [...text].some((c) => {
    const code = c.charCodeAt(0);
    return code < 0x20 || (code >= 0x7f && code <= 0x9f);
  });
}

/** NFS 내보내기 입력 오류(번역 키). 서버처럼 절대 경로이고 제어 문자가 없어야 한다. */
export function nfsExportError(path: string): string | undefined {
  return !path.startsWith("/") || hasControl(path) ? "pools.errors.export" : undefined;
}

/** 서버가 받지 않는 마운트 옵션(`=` 앞 이름, 대소문자 무시). 서버의 `denied_option`과 같다. */
const DENIED_OPTIONS = ["bind", "rbind", "remount", "move", "suid", "dev", "exec", "helper", "bg"];

/**
 * NFS 마운트 옵션 입력 오류(번역 키). 서버처럼 비어 있지 않은 쉼표 목록이고, 공백·제어 문자와
 * 마운트의 성격을 바꾸는 옵션(`bind`, `remount`, `exec`, `x-*` 등)이 없어야 한다.
 */
export function nfsOptionsError(options: string): string | undefined {
  const denied = options.split(",").some((option) => {
    const [name = ""] = option.toLowerCase().split("=");
    return DENIED_OPTIONS.includes(name) || name.startsWith("x-");
  });
  return options === "" || /\s/.test(options) || hasControl(options) || denied
    ? "pools.errors.options"
    : undefined;
}

/** 노드가 알린 이 풀의 상태. 같은 이름의 다른 풀(`pool_id`가 다름)의 상태는 없는 것으로 본다. */
function poolStateOn(pool: StoragePool, node: Node): PoolState | undefined {
  const state = node.status.pools[pool.name];
  return state?.pool_id === pool.id ? state : undefined;
}

/**
 * 이 노드에서 풀을 쓸 수 있는지. 서버의 `pool_usable_on`(노드 목록이 허용하고 준비됐다고 알림 —
 * 디렉터리 풀은 알리지 않았어도 됨)에 노드가 온라인일 것을 더한다.
 */
function usableOn(pool: StoragePool, node: Node): boolean {
  const listed = pool.spec.nodes ?? [];
  if (!node.status.online || (listed.length > 0 && !listed.includes(node.id))) {
    return false;
  }
  const state = poolStateOn(pool, node);
  return state === undefined ? pool.spec.backend.kind === "dir" : state.ready;
}

/** 노드 `nodeId`에서 쓸 수 있는 풀들. 노드를 아직 모르면(읽는 중) 없다. */
export function poolsUsableOn(
  pools: StoragePool[],
  nodes: Node[],
  nodeId: string | undefined,
): StoragePool[] {
  const node = nodes.find((n) => n.id === nodeId);
  return node === undefined ? [] : pools.filter((p) => usableOn(p, node));
}

/** 온라인 노드 가운데 하나에서라도 풀을 쓸 수 있는지(서버가 볼륨을 만들 노드를 고를 수 있는지) */
export function poolUsableSomewhere(pool: StoragePool, nodes: Node[]): boolean {
  return nodes.some((n) => usableOn(pool, n));
}

/**
 * 풀을 쓸 수 있는 노드와 그 노드가 알린 풀 상태. 노드 목록이 비었으면 모든 노드다. 꺼진 노드는
 * `offline`이다.
 */
export function poolNodes(
  pool: StoragePool,
  nodes: Node[],
): { id: string; name: string; state: PoolState | undefined; offline: boolean }[] {
  const listed = pool.spec.nodes ?? [];
  const ids = listed.length === 0 ? nodes.map((n) => n.id) : listed;
  return ids.map((id) => {
    const node = nodes.find((n) => n.id === id);
    return {
      id,
      // 목록에 없는 노드는 id 끝자리로 보인다.
      name: node?.name ?? id.slice(-8),
      state: node && poolStateOn(pool, node),
      offline: node !== undefined && !node.status.online,
    };
  });
}

/**
 * 노드가 알린 풀 용량. 공유 풀은 첫 준비된 온라인 노드의 값 하나(`node`는 null), 로컬 풀은 준비된
 * 온라인 노드마다.
 */
export function poolCapacity(
  pool: StoragePool,
  nodes: Node[],
): { node: string | null; free: number; total: number }[] {
  const ready = poolNodes(pool, nodes).flatMap(({ name, state, offline }) =>
    !offline && state?.ready
      ? [{ node: name, free: state.available_bytes, total: state.total_bytes }]
      : [],
  );
  return isShared(pool) ? ready.slice(0, 1).map((c) => ({ ...c, node: null })) : ready;
}
