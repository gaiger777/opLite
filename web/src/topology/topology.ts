// 네트워크 토폴로지(계획 67 §12.3)의 질의. 그림은 `GET /network-topology` 응답 하나로 그리고, 네트워크·VM·라우터·
// 유동 IP·부하 분산기 watch 사건이 오면 다시 읽는다(`LiveProvider`가 이 키를 버린다). 게이트웨이 주인·멤버 건강 같은 살아
// 있는 상태(`live=true`)만 30초마다 읽는다.
import { useQuery } from "@tanstack/react-query";

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
/** 살아 있는 상태(NB 게이트웨이 주인·SB 건강 검사)를 다시 읽는 주기 */
export const TOPOLOGY_LIVE_MS = 30_000;

/** 토폴로지(폴더로 거를 수 있다). 살아 있는 상태도 읽고 30초마다 다시 읽는다. */
export function useTopology(folder: string | null) {
  return useQuery({
    queryKey: [...TOPOLOGY_KEY, folder ?? ""],
    queryFn: () =>
      call(
        api.GET("/network-topology", {
          params: { query: { live: true, ...(folder && { folder }) } },
        }),
      ),
    refetchInterval: TOPOLOGY_LIVE_MS,
  });
}
