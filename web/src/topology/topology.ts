// 네트워크 토폴로지(계획 67 §12.3)의 질의. 그림은 `GET /network-topology?live=false` 응답 하나로 그리고, 네트워크·VM·라우터·
// 유동 IP·부하 분산기 watch 사건이 오면 다시 읽는다(`LiveProvider`가 이 키를 버린다 — OVN을 읽지 않는 가벼운 읽기). 게이트웨이
// 주인·멤버 건강 같은 살아 있는 상태(`live=true`, NB·SB 읽기)는 다른 키로 30초마다만 읽어 그림에 얹는다(리뷰 1 I3).
import { useQuery } from "@tanstack/react-query";
import { useMemo } from "react";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";

type Schemas = components["schemas"];
export type Topology = Schemas["NetworkTopology"];
export type TopoNetwork = Schemas["TopoNetwork"];
export type TopoRouter = Schemas["TopoRouter"];
export type TopoLb = Schemas["TopoLb"];
export type TopoFip = Schemas["TopoFip"];
export type TopoVm = Schemas["TopoVm"];

export const TOPOLOGY_KEY = ["network-topology"] as const;
/** 살아 있는 상태의 키 — watch 사건이 버리지 않는다(`REFRESH_ON`에 없음) */
export const TOPOLOGY_LIVE_KEY = ["network-topology-live"] as const;
/** 살아 있는 상태(NB 게이트웨이 주인·SB 건강 검사)를 다시 읽는 주기 */
export const TOPOLOGY_LIVE_MS = 30_000;

function read(folder: string | null, live: boolean) {
  return call(
    api.GET("/network-topology", {
      params: { query: { live, ...(folder && { folder }) } },
    }),
  );
}

/**
 * 그림(`live=false`)에 살아 있는 응답의 라우터 게이트웨이와 LB 멤버 건강을 얹는다(같은 id·같은 차례의 멤버만 — 그사이
 * 바뀐 멤버는 그림의 값 그대로).
 */
export function withLive(graph: Topology, live: Topology | undefined): Topology {
  if (!live) {
    return graph;
  }
  const routers = new Map(live.routers.map((r) => [r.id, r]));
  const lbs = new Map(live.load_balancers.map((l) => [l.id, l]));
  return {
    ...graph,
    routers: graph.routers.map((r) => {
      const fresh = routers.get(r.id);
      return fresh ? { ...r, gateway: fresh.gateway } : r;
    }),
    load_balancers: graph.load_balancers.map((l) => {
      const fresh = lbs.get(l.id);
      if (!fresh) {
        return l;
      }
      return {
        ...l,
        members: l.members.map((m, i) => {
          const f = fresh.members[i];
          return f && f.vm === m.vm && f.ip === m.ip ? { ...m, health: f.health } : m;
        }),
      };
    }),
  };
}

/** 토폴로지(폴더로 거를 수 있다): 그림은 watch로, 살아 있는 상태는 30초마다 */
export function useTopology(folder: string | null) {
  const graph = useQuery({
    queryKey: [...TOPOLOGY_KEY, folder ?? ""],
    queryFn: () => read(folder, false),
  });
  const live = useQuery({
    queryKey: [...TOPOLOGY_LIVE_KEY, folder ?? ""],
    queryFn: () => read(folder, true),
    refetchInterval: TOPOLOGY_LIVE_MS,
  });
  const data = useMemo(
    () => (graph.data ? withLive(graph.data, live.data) : undefined),
    [graph.data, live.data],
  );
  return { data, error: graph.error };
}
