import { useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Unit } from "@/charts/format";

type Schemas = components["schemas"];
export type HistoryReply = Schemas["HistoryReply"];
export type ClusterHistory = Schemas["ClusterHistoryResponse"];
export type Cf = Schemas["Cf"];
export type Stat = Schemas["Stat"];
export type Pct = Schemas["Pct"];

/** 범위 고르기의 값. API의 `range`와 같다(1개월 30일, 1년 365일, 모두 지금에서 끝난다). */
export const RANGES = ["hour", "day", "week", "month", "year"] as const;
export type Range = (typeof RANGES)[number];

/** 1시간은 5초 점을 따라가고, 더 긴 범위는 1분마다 다시 읽는다(설계 §12). */
export function refetchMs(range: Range): number {
  return range === "hour" ? 5_000 : 60_000;
}

export { formatValue, type Unit } from "@/charts/format";

/** 차트 하나: 제목 키(`metrics.chart.<id>`), 단위, 그릴 칸(API 열 이름, 칸 이름 키 `metrics.field.<field>`) */
export interface ChartSpec {
  id: "cpu" | "memory" | "disk" | "network";
  unit: Unit;
  fields: string[];
}

/** 노드와 클러스터의 차트 넷 */
export const NODE_CHARTS: ChartSpec[] = [
  { id: "cpu", unit: "percent", fields: ["cpu"] },
  { id: "memory", unit: "mib", fields: ["mem_used", "mem_total"] },
  { id: "disk", unit: "bytesRate", fields: ["disk_read", "disk_write"] },
  { id: "network", unit: "bytesRate", fields: ["net_rx", "net_tx"] },
];

/** VM의 차트 넷 */
export const VM_CHARTS: ChartSpec[] = [
  { id: "cpu", unit: "percent", fields: ["cpu"] },
  { id: "memory", unit: "mib", fields: ["mem_rss"] },
  { id: "disk", unit: "bytesRate", fields: ["disk_read", "disk_write"] },
  { id: "network", unit: "bytesRate", fields: ["net_rx", "net_tx"] },
];

/** 차트가 그리는 응답의 부분. 노드·VM 기록과 클러스터 모으기가 같은 모양이다. */
export interface Series {
  start: number;
  step_ms: number;
  columns: Record<string, Cf>;
  stats?: Record<string, Stat>;
  pct?: Record<string, Pct>;
  covered_from?: number;
}

/** 점마다의 시각(유닉스 ms, 칸의 시작). 가장 긴 열의 길이만큼 */
export function pointTimes(series: Series, fields: string[]): number[] {
  const n = Math.max(0, ...fields.map((f) => series.columns[f]?.avg.length ?? 0));
  return Array.from({ length: n }, (_, i) => series.start + i * series.step_ms);
}

/**
 * 칸 하나의 범위 요약. 서버가 셈한 `stats`가 있으면 그것을, 없으면(클러스터 모으기는 AVG 열만) AVG 열에서 셈한다:
 * 평균은 아는 값의 평균, 최대·최소는 그 열의 최대·최소다.
 */
export function fieldStats(series: Series, field: string): Stat {
  const given = series.stats?.[field];
  if (given) {
    return given;
  }
  const known = (series.columns[field]?.avg ?? []).filter((v): v is number => v !== null);
  if (known.length === 0) {
    return { avg: null, max: null, min: null };
  }
  return {
    avg: known.reduce((a, b) => a + b, 0) / known.length,
    max: Math.max(...known),
    min: Math.min(...known),
  };
}

/** 노드 지표 기록 */
export function useNodeHistory(key: string, range: Range, enabled = true) {
  return useQuery({
    queryKey: ["history", "node", key, range],
    queryFn: () =>
      call(
        api.GET("/nodes/{key}/metrics/history", { params: { path: { key }, query: { range } } }),
      ),
    refetchInterval: refetchMs(range),
    enabled,
  });
}

/** VM 지표 기록(VM 주인 노드가 답한다) */
export function useVmHistory(key: string, range: Range) {
  return useQuery({
    queryKey: ["history", "vm", key, range],
    queryFn: () =>
      call(api.GET("/vms/{key}/metrics/history", { params: { path: { key }, query: { range } } })),
    refetchInterval: refetchMs(range),
  });
}

/** 클러스터 모으기(클러스터 열·노드 줄·상위 VM·풀). 대시보드 위젯들이 같은 캐시를 나눠 쓴다. */
export function useClusterHistory(range: Range) {
  return useQuery({
    queryKey: ["history", "cluster", range],
    queryFn: () => call(api.GET("/metrics/cluster", { params: { query: { range } } })),
    refetchInterval: refetchMs(range),
  });
}
