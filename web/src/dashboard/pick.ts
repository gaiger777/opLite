// 장애 → 고른 것(계획 75 리뷰 2): 배지와 자세히 판의 장애 목록이 같이 쓴다.
import type { MapSelection } from "@/dashboard/selection";
import type { FaultEntry } from "@/physical/chain";

/** 장애 하나를 고른다: 지도 포트 줄에 있는 포트(본딩이면 끊긴 구성 포트), 없으면(브리지·없는 네트워크) 노드 */
export const pickFault = (f: FaultEntry): MapSelection =>
  f.focus !== undefined ? { kind: "port", node: f.id, port: f.focus } : { kind: "node", id: f.id };
