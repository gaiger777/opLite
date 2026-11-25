// 선언형 적용(계획 73 §8·§12)의 질의: 적용 묶음 목록·상세(드리프트는 묶음 status — 리더가 쓴다), 매니페스트 내보내기, 서버가
// 세운 계획(`PlanView`)을 그대로 적용하기(`POST /apply {plan}` — 되돌리기 대화상자가 미리 본 계획). 적용 묶음은 watch 종류이지만
// 화면은 목록을 열 때·적용·받아들이기 뒤에 다시 읽는다.
import { type QueryClient, useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Task } from "@/api/types";
import i18n from "@/i18n";

type Schemas = components["schemas"];
export type PlanView = Schemas["PlanView"];
export type StepView = Schemas["StepView"];
export type StepAction = Schemas["StepAction"];
export type Problem = Schemas["Problem"];
export type Manifest = Schemas["Manifest"];
export type ApplySet = Schemas["Resource_ApplySetSpec_ApplySetStatus"];
export type AppliedObject = Schemas["AppliedObject"];
export type DriftItem = Schemas["DriftItem"];
export type DriftState = Schemas["DriftState"];

export const APPLY_SETS_KEY = ["apply-sets"] as const;

/** 적용 묶음 목록(루트 `system.read`) */
export function useApplySets(enabled = true) {
  return useQuery({
    queryKey: APPLY_SETS_KEY,
    queryFn: () => call(api.GET("/apply-sets")),
    select: (list) => list.items,
    enabled,
  });
}

/** 적용 묶음 하나(이름 또는 id) */
export function useApplySet(key: string) {
  return useQuery({
    queryKey: [...APPLY_SETS_KEY, key],
    queryFn: () => call(api.GET("/apply-sets/{key}", { params: { path: { key } } })),
  });
}

export async function refreshApplySets(queryClient: QueryClient) {
  await queryClient.invalidateQueries({ queryKey: APPLY_SETS_KEY });
}

/** 막힌 단계가 있으면 적용할 수 없다(서버 409 `plan_blocked`). */
export function blocked(plan: PlanView): boolean {
  return plan.summary.blocked > 0;
}

/** 바꿀 것이 없는 계획(모두 그대로) */
export function noop(plan: PlanView): boolean {
  const s = plan.summary;
  return s.create + s.update + s.delete + s.blocked === 0;
}

type ApplyStarted = Schemas["ApplyStarted"];

/** 계획·적용 응답(200 계획 | 202 시작)에서 계획 */
export function planOf(reply: PlanView | ApplyStarted): PlanView {
  return "task" in reply ? reply.plan : reply;
}

/** 미리 본 계획을 그대로 적용한다(그사이 바뀐 것이 있으면 412 `plan_stale`). 시작한 Task를 돌려준다. */
export async function applyPlan(plan: PlanView): Promise<Task> {
  const started = await call(api.POST("/apply", { body: { plan: plan.plan } }));
  if (!("task" in started)) {
    throw new Error(i18n.t("errors.plan_not_started"));
  }
  return started.task;
}

/** 리소스 하나의 매니페스트(`GET /manifests/{kind}/{key}` — 비밀은 `secretRef`) */
export function fetchManifest(kind: string, key: string): Promise<Manifest> {
  return call(api.GET("/manifests/{kind}/{key}", { params: { path: { kind, key } } }));
}

/** 적용 묶음이 기억하는 리소스들의 매니페스트 */
export async function fetchSetManifests(set: string): Promise<Manifest[]> {
  const list = await call(api.GET("/manifests", { params: { query: { apply_set: set } } }));
  return list.documents;
}

/** Terraform이 관리하는 객체(레이블 `orbivisor.io/managed-by=terraform`) */
export function managedByTerraform(labels: Record<string, string> | undefined): boolean {
  return labels?.["orbivisor.io/managed-by"] === "terraform";
}

/** 내려받기 파일: 브라우저가 글을 파일로 저장한다(작은 YAML — 메모리에 담아도 된다). */
export function saveText(name: string, text: string, type = "application/yaml") {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.rel = "noopener";
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}
