// DR 오케스트레이션(계획 76)의 질의와 오류 문구. 계획·실행은 watch 사건이 오면 다시 읽는다(`LiveProvider`의 `VIEW_KINDS` —
// 계획은 `["dr-plans"]`, 실행은 `["dr-runs"]`와 계획의 보고). 원격 VM 목록은 원격 중계(`/remotes/{key}/api/vms`)로 묻는다.
import { type QueryClient, useQuery } from "@tanstack/react-query";
import type { TFunction } from "i18next";

import { api, ApiError, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Node, Vm } from "@/api/types";
import { codeMessage } from "@/i18n/errors";
import { proxy } from "@/remotes/remotes";

type Schemas = components["schemas"];
export type RecoveryPlan = Schemas["Resource_RecoveryPlanSpec_RecoveryPlanStatus"];
export type RecoveryPlanSpec = Schemas["RecoveryPlanSpec"];
export type DrRun = Schemas["Resource_DrRunSpec_DrRunStatus"];
export type DrRunState = Schemas["DrRunState"];
export type DrRunKind = Schemas["DrRunKind"];
export type DrRunSummary = Schemas["DrRunSummary"];
export type DrStep = Schemas["DrStep"];
export type DrPhase = Schemas["DrPhase"];
export type DrReport = Schemas["DrReport"];
export type DrWait = Schemas["DrWait"];
export type DrGroup = Schemas["DrGroup"];
export type DrVm = Schemas["DrVm"];
export type DrIpRule = Schemas["DrIpRule"];
export type VmProtection = Schemas["VmProtection"];
export type FailoverMode = Schemas["FailoverMode"];
export type OnTimeout = Schemas["OnTimeout"];

export const PLANS_KEY = ["dr-plans"] as const;
export const RUNS_KEY = ["dr-runs"] as const;
export const planKey = (key: string) => [...PLANS_KEY, key] as const;
export const reportKey = (key: string) => [...PLANS_KEY, key, "report"] as const;
export const planRunsKey = (plan: string) => [...RUNS_KEY, "plan", plan] as const;
export const runKey = (id: string) => [...RUNS_KEY, id] as const;

/** 모든 노드(위트니스 포함)가 알아야 계획·실행을 바꾸는 기능(서버 `node::DR_PLANS`) */
export const DR_FEATURE = "dr-plans";

/** 기능 `dr-plans`를 적지 않은 노드 이름(서버 `missing_feature`와 같은 판단 — 있으면 쓰기가 409 `cluster_upgrading`) */
export function oldNodes(nodes: readonly Node[] | undefined): string[] {
  return (nodes ?? [])
    .filter((n) => !(n.status.features ?? []).includes(DR_FEATURE))
    .map((n) => n.name)
    .sort();
}

/** 복구 계획 목록(루트 `cluster.read`) */
export function usePlans(enabled = true) {
  return useQuery({
    queryKey: PLANS_KEY,
    queryFn: () => call(api.GET("/dr-plans")),
    select: (list) => [...list.items].sort((a, b) => a.name.localeCompare(b.name)),
    enabled,
  });
}

export function usePlan(key: string | undefined) {
  return useQuery({
    queryKey: planKey(key ?? ""),
    queryFn: () => call(api.GET("/dr-plans/{key}", { params: { path: { key: key! } } })),
    enabled: !!key,
    retry: false,
  });
}

/** 보고(보호 상태 나이는 지금 다시 셈) */
export function useReport(key: string | undefined) {
  return useQuery({
    queryKey: reportKey(key ?? ""),
    queryFn: () => call(api.GET("/dr-plans/{key}/report", { params: { path: { key: key! } } })),
    enabled: !!key,
    retry: false,
  });
}

/** 계획의 실행 기록(새것부터) */
export function usePlanRuns(plan: string | undefined) {
  return useQuery({
    queryKey: planRunsKey(plan ?? ""),
    queryFn: () => call(api.GET("/dr-plans/{key}/runs", { params: { path: { key: plan! } } })),
    select: (list) => [...list.items].sort((a, b) => b.status.started_at - a.status.started_at),
    enabled: !!plan,
  });
}

export function useRun(id: string | undefined) {
  return useQuery({
    queryKey: runKey(id ?? ""),
    queryFn: () => call(api.GET("/dr-runs/{key}", { params: { path: { key: id! } } })),
    enabled: !!id,
    retry: false,
  });
}

/** 원격(운영 클러스터)의 VM 이름. 닿지 않으면 오류(고르기는 DR 사본만으로도 된다). */
export function useRemoteVms(remote: string | undefined) {
  return useQuery({
    queryKey: ["dr-remote-vms", remote ?? ""],
    queryFn: ({ signal }) => proxy<{ items: Vm[] }>(remote!, "GET", "/vms", signal),
    select: (list) => list.items.map((vm) => vm.name),
    enabled: !!remote,
    retry: false,
    staleTime: 30_000,
  });
}

/** 바꾼 뒤 계획·실행을 다시 읽는다(사건이 오기 전에도 화면이 바뀌게). */
export async function refreshDr(queryClient: QueryClient) {
  await Promise.all(
    [PLANS_KEY, RUNS_KEY].map((queryKey) => queryClient.invalidateQueries({ queryKey })),
  );
}

/** DR API 오류를 문장으로(`dr.errors.<code>`가 먼저, 없으면 공통 문구나 서버 설명) */
export function drErrorText(t: TFunction, err: unknown): string {
  return err instanceof ApiError
    ? codeMessage(t, err.code, err.message, "dr.errors")
    : err instanceof Error
      ? err.message
      : String(err);
}

/**
 * 대화상자는 공통 문구로 오류를 보인다. DR 문구는 늦게 받는 묶음에 있으므로 던지기 전에 문장으로 바꾼다(코드는 머리를 붙여
 * 공통 문구에 없게 한다 — VMware 이전과 같은 방식).
 */
export function localized(t: TFunction, err: unknown): unknown {
  if (err instanceof ApiError) {
    return new ApiError(err.status, `dr:${err.code}`, drErrorText(t, err), err.details);
  }
  return err;
}
