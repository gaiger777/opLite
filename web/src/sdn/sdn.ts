import { useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Node } from "@/api/types";

type Schemas = components["schemas"];
export type SdnView = Schemas["SdnView"];
export type SdnStray = Schemas["SdnStrayView"];
export type NodeSdn = Schemas["NodeSdn"];
export type Subnet = Schemas["Subnet"];
export type NetworkIps = Schemas["NetworkIps"];

/** `GET /sdn`의 캐시 키. `Sdn` 리소스는 watch로 나가지 않아 SDN 화면이 주기적으로 다시 읽는다. */
export const SDN_KEY = ["sdn"] as const;
export const SDN_REFRESH_MS = 10_000;
/** 서버의 `MAX_CENTRALS` */
export const MAX_CENTRALS = 5;
/** 서버의 `DEFAULT_SDN_MTU`: Geneve 머리 58바이트를 뺀 1500 */
export const DEFAULT_SDN_MTU = 1442;
/** 서버의 `MAX_DNS_SERVERS`·`MAX_POOLS` */
export const MAX_DNS_SERVERS = 4;
export const MAX_POOLS = 16;
/** macOS 노드의 하이퍼바이저(OVN chassis가 될 수 없다) */
const HVF = "qemu-hvf";

/** 첫 세우기 Task(`sdn-init`)의 진행 단계(서버 `tasks/sdn.rs`의 차례) */
export const INIT_STAGES = [
  "checking",
  "installing-packages",
  "writing-tls",
  "creating-cluster",
  "starting",
  "waiting-for-leader",
  "configuring-chassis",
] as const;

/** SDN 상태. 설정 전이면 `phase`가 없다. `poll`이면 SDN 화면처럼 주기적으로 다시 읽는다. */
export function useSdn(poll = false, enabled = true) {
  return useQuery({
    queryKey: SDN_KEY,
    queryFn: () => call(api.GET("/sdn")),
    refetchInterval: poll ? SDN_REFRESH_MS : false,
    enabled,
  });
}

/** SDN이 세워져 네트워크를 만들 수 있다(서버 `require_ready`). */
export function sdnReady(view: SdnView | undefined): boolean {
  return view?.phase === "ready" && view.centrals.length > 0;
}

/**
 * OVN을 돌릴 수 없는 까닭(번역 키, 서버 `ovn_node`·`central_node`와 같은 차례). `central`이면 유지보수 노드도 막는다.
 * 돌릴 수 있으면 null.
 */
export function nodeBlocker(node: Node, central: boolean): string | null {
  if (node.spec.witness) {
    return "sdn.blocked.witness";
  }
  if (node.status.hypervisor === HVF) {
    return "sdn.blocked.macos";
  }
  if (!node.status.underlay) {
    return "sdn.blocked.underlay";
  }
  if (central && node.spec.maintenance) {
    return "sdn.blocked.maintenance";
  }
  if (!node.status.online) {
    return "sdn.blocked.offline";
  }
  return null;
}

/**
 * 중앙으로 미리 고를 노드(설계 §7.1): 막히지 않은 투표 멤버 셋(이름 순). 투표 멤버를 모르면(목록을 읽지 못함) 막히지 않은
 * 첫 노드 하나다.
 */
export function suggestCentrals(
  nodes: readonly Node[],
  voters: ReadonlySet<string> | undefined,
): string[] {
  const usable = [...nodes]
    .filter((n) => nodeBlocker(n, true) === null)
    .sort((a, b) => a.name.localeCompare(b.name));
  const picked = voters ? usable.filter((n) => voters.has(n.id)).slice(0, 3) : [];
  return (picked.length > 0 ? picked : usable.slice(0, 1)).map((n) => n.id);
}

/** 경고 띠의 문장(번역 키와 값): 중앙이 짝수, 투표 멤버가 아닌 중앙, 오프라인 중앙 */
export function sdnWarnings(
  view: SdnView,
  nodes: readonly Node[] | undefined,
  voters: ReadonlySet<string> | undefined,
): { key: string; values?: Record<string, string | number> }[] {
  const out: { key: string; values?: Record<string, string | number> }[] = [];
  const count = view.centrals.length;
  if (count > 0 && count % 2 === 0) {
    out.push({ key: "sdn.warn.even", values: { count } });
  }
  if (voters) {
    const names = view.centrals.filter((c) => !voters.has(c.node)).map((c) => c.name);
    if (names.length > 0) {
      out.push({ key: "sdn.warn.notVoter", values: { names: names.join(", ") } });
    }
  }
  if (nodes) {
    const names = view.centrals
      .filter((c) => !nodes.find((n) => n.id === c.node)?.status.online)
      .map((c) => c.name);
    if (names.length > 0) {
      out.push({ key: "sdn.warn.offline", values: { names: names.join(", ") } });
    }
  }
  return out;
}

/** 응답 헤더 `orbivisor-warning`(ASCII, 없으면 null) */
export function warningOf(response: Response | undefined): string | null {
  return response?.headers.get("orbivisor-warning") ?? null;
}

/** openapi-fetch 요청의 데이터와 응답 헤더 `orbivisor-warning`. 오류 응답이면 `ApiError`를 던진다. */
export async function callWarned<T>(
  request: Promise<{ data?: T; error?: unknown; response: Response }>,
): Promise<{ data: T; warning: string | null }> {
  let response: Response | undefined;
  const data = await call(
    request.then((result) => {
      response = result.response;
      return result;
    }),
  );
  return { data, warning: warningOf(response) };
}

/** 남은 중앙 DB를 알린 노드 id(`GET /sdn`의 `strays` 가운데 아직 클러스터에 있는 것 — 노드 칸의 표시) */
export function strayNodes(view: SdnView | undefined): ReadonlySet<string> {
  return new Set((view?.strays ?? []).filter((s) => !s.gone).map((s) => s.node));
}

/** 노드 목록의 이름(모르면 id) */
export function nodeName(nodes: readonly Node[] | undefined, id: string): string {
  return nodes?.find((n) => n.id === id)?.name ?? id;
}

/** `GET /cluster`의 투표 멤버(노드 id). 읽지 못했으면 undefined */
export function useVoters(enabled = true): ReadonlySet<string> | undefined {
  const cluster = useQuery({
    queryKey: ["cluster"],
    queryFn: () => call(api.GET("/cluster")),
    enabled,
  });
  return cluster.data
    ? new Set(cluster.data.members.filter((m) => m.voter).map((m) => m.id))
    : undefined;
}
