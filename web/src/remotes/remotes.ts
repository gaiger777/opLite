import { currentAuthEpoch, NetworkError, reportUnauthorized, toApiError } from "@/api/client";
import type { components } from "@/api/schema";
import type { Task, Vm } from "@/api/types";

export type RemoteView = components["schemas"]["RemoteView"];

/** 원격 클러스터 화면에서 하는 VM 동작. `stop`은 ACPI 종료, `force-stop`은 곧바로 끄기다. */
export type RemoteVmAction = "start" | "stop" | "force-stop";

export const REMOTE_ACTIONS: readonly RemoteVmAction[] = ["start", "stop", "force-stop"];

/** 원격 `remote`의 API `path`(`/vms` 따위, 쿼리 포함)로 가는 중계 주소 */
export function proxyUrl(remote: string, path: string): string {
  return `${window.location.origin}/api/v1/remotes/${encodeURIComponent(remote)}/api${path}`;
}

/**
 * 원격 클러스터의 API를 이 클러스터의 중계로 부른다. 중계는 API 문서에 없어 fetch로 부른다. 오류 응답은
 * `ApiError`, 닿지 못하면 `NetworkError`다. 401은 이 클러스터의 세션이 끝난 것이므로 다른 요청처럼
 * 알린다(원격이 저장된 토큰을 거절하면 중계는 502 `remote_unauthorized`를 준다).
 */
export async function proxy<T>(
  remote: string,
  method: "GET" | "POST",
  path: string,
  signal?: AbortSignal,
): Promise<T> {
  let response: Response;
  let body: unknown = null;
  const sentAt = currentAuthEpoch();
  try {
    response = await globalThis.fetch(proxyUrl(remote, path), {
      method,
      headers: { Accept: "application/json" },
      signal,
    });
    const text = await response.text();
    try {
      body = text === "" ? null : JSON.parse(text);
    } catch {
      // 프록시의 HTML 오류 페이지 따위
    }
  } catch (error) {
    if (signal?.aborted) {
      throw error;
    }
    throw new NetworkError(error);
  }
  if (response.status === 401) {
    reportUnauthorized(sentAt);
  }
  if (!response.ok) {
    throw toApiError(response.status, body);
  }
  return body as T;
}

/** 원격의 VM 목록 */
export async function remoteVms(remote: string, signal?: AbortSignal): Promise<Vm[]> {
  const list = await proxy<{ items: Vm[] }>(remote, "GET", "/vms", signal);
  return list.items;
}

/** 원격 VM의 동작을 요청한다. 원격이 만든 Task를 돌려준다(이 클러스터의 Task가 아니라 지켜보지 않는다). */
export function remoteVmAction(remote: string, vm: Vm, action: RemoteVmAction): Promise<Task> {
  const path =
    action === "force-stop"
      ? `/vms/${vm.id}/actions/stop?mode=force`
      : `/vms/${vm.id}/actions/${action}`;
  return proxy<Task>(remote, "POST", path);
}
