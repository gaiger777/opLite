// 홈 지도·표·자세히 판이 함께 쓰는 "고른 것"(계획 75 §4.1) — 모양은 지도의 `clusterMap/selection.ts` 하나다. `node`·`vm`은 리소스 id,
// `port.node`는 노드 id와 인터페이스 이름, `rail.id`는 운반자(브리지·physnet 이름, VXLAN·오버레이는 `underlay`) — 네트워크 id가 아니다.
import type { MapSelection } from "@/clusterMap/selection";

export type { MapSelection };

/** 지도의 `focus`(설계 §4.2 `?focus=port:<node>/<port>`, 노드는 이름이나 id)로 바꾼다. 포트만 초점이 있다. */
export function focusOf(selection: MapSelection | null): string | undefined {
  return selection?.kind === "port" ? `port:${selection.node}/${selection.port}` : undefined;
}
