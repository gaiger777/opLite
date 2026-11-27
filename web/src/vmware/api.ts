// VMware 이전(계획 72)의 질의와 오류 문구. 원본·계획·항목은 watch 사건이 오면 다시 읽는다(`LiveProvider`의 `VIEW_KINDS` —
// 원본은 비밀번호를 뺀 보기, 항목은 계획마다 따로 읽는다). 인벤토리는 노드가 60초 담아 두므로 화면도 그만큼 다시 묻지 않는다.
import { type QueryClient, useQuery } from "@tanstack/react-query";
import type { TFunction } from "i18next";

import { api, ApiError, call } from "@/api/client";
import type { components } from "@/api/schema";
import { codeMessage } from "@/i18n/errors";

type Schemas = components["schemas"];
export type VmwareSource = Schemas["Resource_VmwareSourceSpecView_VmwareSourceStatus"];
export type Migration = Schemas["Resource_VmwareMigrationSpec_VmwareMigrationStatus"];
export type MigrationItem = Schemas["Resource_VmwareVmSpec_VmwareVmStatus"];
export type ItemState = Schemas["VmwareVmState"];
export type MigrationPhase = Schemas["VmwareMigrationPhase"];
export type SourceInventory = Schemas["SourceInventory"];
export type Inventory = Schemas["Inventory"];
export type InvVm = Schemas["InvVm"];
export type PreflightVm = Schemas["PreflightVm"];
export type PreflightLevel = Schemas["PreflightLevel"];
export type CreateMigrationRequest = Schemas["CreateMigrationRequest"];
export type CreateSourceRequest = Schemas["CreateSourceRequest"];
export type SourcePatch = Schemas["SourcePatch"];
export type Rules = Schemas["VmwareRules"];
export type Limits = Schemas["VmwareLimits"];
export type SyncPolicy = Schemas["VmwareSyncPolicy"];
export type CutoverPolicy = Schemas["VmwareCutoverPolicy"];
export type VmCheckProblem = Schemas["VmCheckProblem"];

export const SOURCES_KEY = ["vmware-sources"] as const;
export const MIGRATIONS_KEY = ["vmware-migrations"] as const;
export const ITEMS_KEY = ["vmware-vms"] as const;
export const inventoryKey = (source: string) => ["vmware-inventory", source] as const;

/** 원본 연결 목록(루트 `cluster.read`). 읽을 수 없으면 묻지 않는다. */
export function useSources(enabled = true) {
  return useQuery({
    queryKey: SOURCES_KEY,
    queryFn: () => call(api.GET("/vmware-sources")),
    select: (list) => list.items,
    enabled,
  });
}

/** 이전 계획 목록(루트 `system.read`) */
export function useMigrations(enabled = true) {
  return useQuery({
    queryKey: MIGRATIONS_KEY,
    queryFn: () => call(api.GET("/vmware-migrations")),
    select: (list) => list.items,
    enabled,
  });
}

/** 계획 하나의 항목(`GET /vmware-vms?migration=`) */
export function useItems(migration: string | undefined) {
  return useQuery({
    queryKey: [...ITEMS_KEY, migration],
    queryFn: () => call(api.GET("/vmware-vms", { params: { query: { migration: migration! } } })),
    select: (list) => list.items,
    enabled: migration !== undefined,
  });
}

/** 원본의 인벤토리. `refresh`는 노드 캐시를 건너뛴다(다시 읽기 단추). */
export function fetchInventory(source: string, refresh = false) {
  return call(
    api.GET("/vmware-sources/{key}/inventory", {
      params: { path: { key: source }, query: refresh ? { refresh: true } : {} },
    }),
  );
}

export function useInventory(source: string | undefined) {
  return useQuery({
    queryKey: inventoryKey(source ?? ""),
    queryFn: () => fetchInventory(source!),
    enabled: !!source,
    // 노드 캐시가 60초다. 그 안에 다시 물어도 같은 답이다.
    staleTime: 60_000,
    retry: false,
  });
}

/** 바꾼 뒤 목록을 다시 읽는다(사건이 오기 전에도 화면이 바뀌게). */
export async function refreshVmware(queryClient: QueryClient) {
  await Promise.all(
    [SOURCES_KEY, MIGRATIONS_KEY, ITEMS_KEY].map((queryKey) =>
      queryClient.invalidateQueries({ queryKey }),
    ),
  );
}

/** 전환 요청이 원본을 새로 읽고 막은 까닭(409 `preflight_blocked`의 `details.problems`) */
function blockedProblems(err: ApiError): string[] {
  const problems = (err.details as { problems?: unknown } | undefined)?.problems;
  return Array.isArray(problems)
    ? (problems as Partial<VmCheckProblem>[])
        .filter((p) => typeof p.message === "string")
        .map((p) => (p.at ? `${p.at}: ${p.message}` : p.message!))
    : [];
}

/** API 오류 하나의 문장. 막힌 전환은 문제를 `; `로 이어 덧붙인다(대화상자의 오류 줄은 한 문단이다). */
function apiErrorText(t: TFunction, err: ApiError): string {
  const problems = err.code === "preflight_blocked" ? blockedProblems(err) : [];
  // 막힌 전환은 문제 목록이 서버 설명보다 자세하다.
  const text = codeMessage(t, err.code, err.message, "vmware.errors", problems.length === 0);
  return problems.length > 0 ? `${text} ${problems.join("; ")}` : text;
}

/** VMware API 오류를 문장으로(`vmware.errors.<code>`가 먼저, 없으면 공통 문구나 서버 설명) */
export function vmwareErrorText(t: TFunction, err: unknown): string {
  return err instanceof ApiError
    ? apiErrorText(t, err)
    : err instanceof Error
      ? err.message
      : String(err);
}

/**
 * 대화상자(`FormDialog`·`ConfirmDialog`)는 공통 문구로 오류를 보인다. VMware 문구는 늦게 받는 묶음에 있으므로 던지기 전에
 * 문장으로 바꾼다(코드를 보는 쪽을 위해 `ApiError`는 그대로 두고 설명만 바꾼다).
 */
export function localized(t: TFunction, err: unknown): unknown {
  if (err instanceof ApiError) {
    const text = apiErrorText(t, err);
    // 머리를 붙인 코드는 공통 문구(`errors.<code>`)에 없으므로 대화상자가 이 문장을 그대로 보인다.
    return new ApiError(err.status, `vmware:${err.code}`, text, err.details);
  }
  return err;
}

/** 오류의 코드(`localized`가 붙인 머리를 뗀다) */
export function errorCode(err: unknown): string | undefined {
  return err instanceof ApiError ? err.code.replace(/^vmware:/, "") : undefined;
}

/** 서버가 `untrusted_certificate`·`fingerprint_mismatch`의 details로 알려 준 지문 */
export function offeredCertificate(
  err: unknown,
): { fingerprint: string; thumbprint?: string } | undefined {
  const code = errorCode(err);
  if (code !== "untrusted_certificate" && code !== "fingerprint_mismatch") {
    return undefined;
  }
  const details = (err as ApiError).details as
    { fingerprint?: unknown; thumbprint_sha1?: unknown } | undefined;
  return typeof details?.fingerprint === "string"
    ? {
        fingerprint: details.fingerprint,
        thumbprint:
          typeof details.thumbprint_sha1 === "string" ? details.thumbprint_sha1 : undefined,
      }
    : undefined;
}
