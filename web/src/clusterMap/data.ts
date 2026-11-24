// 클러스터 지도의 자료(계획 70 §5·§9.1): 이미 있는 목록·watch와 질의를 합친다 — 서버 변경 없음. 첫 그림은 노드·VM
// 목록만 있으면 그리고 `/placement`·`/cluster`·`/alerts`·Task 진행률은 오는 대로 얹는다. watch 사건은 1초 모아 다시 만든다.
import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";

import { useAlerts } from "@/alerts/alerts";
import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import { usePermissions } from "@/auth/usePermissions";
import type { MapInput, TaskProgress } from "@/clusterMap/model";
import { PLACEMENT_KEY } from "@/placement/balance";

/** 최적 배치 화면과 같은 주기(같은 질의 열쇠라 캐시를 함께 쓴다) */
export const PLACEMENT_REFRESH_MS = 15_000;
export const CLUSTER_REFRESH_MS = 30_000;
/** watch 사건을 모으는 시간(토폴로지 `REFRESH_BATCH_MS`와 같은 뜻) */
export const COALESCE_MS = 1000;

/**
 * 값이 바뀌어도 `ms`에 한 번만 내보낸다(마지막 값). `live`가 거짓인 동안(첫 자료를 기다리는 동안)은 그대로 내보낸다 —
 * 첫 그림을 늦추지 않는다.
 */
export function useCoalesced<T>(value: T, ms: number, live: boolean): T {
  const [state, setState] = useState({ value, at: 0 });
  // 기다리는 동안과 첫 값은 바로(그림을 늦추지 않음), 그 뒤로는 모아서
  if ((!live || state.value == null) && !Object.is(state.value, value)) {
    setState({ value, at: 0 });
  }
  useEffect(() => {
    if (!live || Object.is(state.value, value)) {
      return;
    }
    const wait = Math.max(0, state.at + ms - Date.now());
    const id = setTimeout(() => setState({ value, at: Date.now() }), wait);
    return () => clearTimeout(id);
  }, [live, value, state, ms]);
  return live && state.value != null ? state.value : value;
}

/** 지도가 읽는 것 모두. `input`은 노드·VM 목록이 오기 전에는 `null`. */
export function useClusterMapData() {
  const perms = usePermissions();
  const nodes = useList("node");
  const vms = useList("vm");
  const pools = useList("storage-pool");
  const placement = useQuery({
    queryKey: PLACEMENT_KEY,
    queryFn: () => call(api.GET("/placement")),
    refetchInterval: PLACEMENT_REFRESH_MS,
    // 리더에 묻지 못한 503(`placement_unavailable`)은 다음 주기에 다시 묻는다 — 그 사이 그림은 "부하 모름".
    retry: false,
    enabled: perms.reads("node"),
  });
  const cluster = useQuery({
    queryKey: ["cluster"],
    queryFn: () => call(api.GET("/cluster")),
    refetchInterval: CLUSTER_REFRESH_MS,
    // 403(클러스터 읽기 없음)이면 리더 꼬리표 없이 그린다.
    retry: false,
  });
  const alerts = useAlerts();
  const tasks = useTaskIndex();

  const progress = useMemo(() => {
    const out = new Map<string, TaskProgress>();
    for (const list of tasks.values()) {
      for (const task of list) {
        if (task.status.progress) {
          out.set(task.id, task.status.progress);
        }
      }
    }
    return out;
  }, [tasks]);

  const ready = nodes.data !== undefined && vms.data !== undefined;
  const raw = useMemo<MapInput | null>(
    () =>
      ready
        ? {
            nodes: nodes.data,
            vms: vms.data,
            pools: pools.data,
            // 다시 읽기가 실패하면(503) 옛 값을 부하처럼 보이지 않는다.
            placement: placement.isError ? null : (placement.data ?? null),
            cluster: cluster.isError || !cluster.data ? null : cluster.data,
            alerts: alerts.data?.alerts ?? [],
            progress,
          }
        : null,
    [
      ready,
      nodes.data,
      vms.data,
      pools.data,
      placement.isError,
      placement.data,
      cluster.isError,
      cluster.data,
      alerts.data,
      progress,
    ],
  );
  const input = useCoalesced(raw, COALESCE_MS, ready);
  return {
    input,
    /** 노드 목록 질의(빈 상태·오류 글) */
    nodes,
    vms,
    placement,
    error: nodes.error ?? vms.error,
  };
}

export type ClusterMapData = ReturnType<typeof useClusterMapData>;
