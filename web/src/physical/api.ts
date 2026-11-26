// 운영자 대시보드(계획 75)의 서버 자료: 물리 관계(`/topology/physical`)와 용량(`/capacity`). 홈과 지도가 같은 열쇠로 나눠 쓴다.
// 둘 다 루트 `cluster.read`가 있어야 하고, 섞인 판 클러스터(기능 `operator-dashboard`가 모두에 없음)는 409 `cluster_upgrading`이다.
import { useQuery } from "@tanstack/react-query";

import { ApiError, api, call } from "@/api/client";
import type { components } from "@/api/schema";

export type PhysicalTopology = components["schemas"]["PhysicalTopology"];
export type NodePhysical = components["schemas"]["NodePhysical"];
export type PhysicalPort = components["schemas"]["Port"];
export type NetRail = components["schemas"]["NetRail"];
export type Impact = components["schemas"]["Impact"];
export type Capacity = components["schemas"]["Capacity"];
export type NodeCapacity = components["schemas"]["NodeCapacity"];

export const PHYSICAL_KEY = ["topology", "physical"] as const;
export const CAPACITY_KEY = ["capacity"] as const;
export const PHYSICAL_REFRESH_MS = 15_000;
export const CAPACITY_REFRESH_MS = 15_000;

/** 409 `cluster_upgrading`(옛 판 노드가 있음)이면 참 — 화면은 지금 홈(위젯만)과 안내를 보인다. */
export function isUpgrading(error: unknown): boolean {
  return error instanceof ApiError && error.code === "cluster_upgrading";
}

/** 403(루트 읽기 없음)·409(섞인 판)는 다시 묻지 않는다. 그 밖의 실패는 다음 주기에. */
function retry(count: number, error: unknown): boolean {
  if (error instanceof ApiError && (error.status === 403 || error.status === 409)) return false;
  return count < 1;
}

export function usePhysicalTopology(enabled = true) {
  return useQuery({
    queryKey: PHYSICAL_KEY,
    queryFn: () => call(api.GET("/topology/physical")),
    refetchInterval: PHYSICAL_REFRESH_MS,
    retry,
    enabled,
  });
}

export function useCapacity(enabled = true) {
  return useQuery({
    queryKey: CAPACITY_KEY,
    queryFn: () => call(api.GET("/capacity")),
    refetchInterval: CAPACITY_REFRESH_MS,
    retry,
    enabled,
  });
}
