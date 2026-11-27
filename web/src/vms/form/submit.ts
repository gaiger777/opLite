import type { QueryClient } from "@tanstack/react-query";

import { ApiError, api, call } from "@/api/client";
import type { Vm } from "@/api/types";
import { trackTask, waitForTask } from "@/tasks/tasks";
import { toSpec, type VmFormValues, volumeRequest } from "@/vms/form";

/** `POST /volumes` 본문 그대로 볼륨을 만들고 Task를 기다린다(VM 빌더의 `requests()` 본문 — 폼도 이 길로 보낸다). */
export async function postVolume(
  queryClient: QueryClient,
  body: ReturnType<typeof volumeRequest>,
  label: string,
  signal: AbortSignal,
): Promise<void> {
  const task = await call(api.POST("/volumes", { body }));
  trackTask(task, label);
  await waitForTask(queryClient, task.id, signal);
}

/** 실행 중인 VM의 하드웨어 변경을 적용하는 핫플러그 Task의 id를 주는 응답 헤더(서버의 `revision::TASK`) */
export const TASK_HEADER = "orbivisor-task";

/**
 * VM spec 전체를 바꾼다. 읽은 `revision`을 If-Match로 보내므로 그 사이 바뀌었으면 실패한다. 실행 중인 VM의
 * 하드웨어가 바뀌어 서버가 핫플러그 Task를 만들었으면 그 id를, 아니면 null을 돌려준다.
 */
export async function updateVm(
  vm: Vm,
  revision: Vm["revision"] | undefined,
  values: VmFormValues,
  sdn: readonly boolean[] = [],
): Promise<string | null> {
  let response: Response | undefined;
  await call(
    api
      .PUT("/vms/{key}", {
        params: {
          path: { key: vm.id },
          header: { "If-Match": revision },
        },
        body: { labels: vm.labels, spec: toSpec(values, vm.spec.power ?? "stopped", sdn) },
      })
      .then((result) => {
        response = result.response;
        return result;
      }),
  );
  return response?.headers.get(TASK_HEADER) ?? null;
}

/**
 * CD-ROM 드라이브 `drive`의 매체만 바꾼다(`POST /vms/{key}/cdrom`, `iso`가 null이면 뺀다). 실행 중이면 서버가 매체를 바꾸는
 * 핫플러그 Task를 만들어 그 id를, 아니면 null을 돌려준다.
 */
export async function changeCdrom(
  vm: Vm,
  drive: number,
  iso: string | null,
): Promise<string | null> {
  let response: Response | undefined;
  await call(
    api
      .POST("/vms/{key}/cdrom", {
        params: { path: { key: vm.id } },
        body: { drive, iso },
      })
      .then((result) => {
        response = result.response;
        return result;
      }),
  );
  return response?.headers.get(TASK_HEADER) ?? null;
}

/** 핫플러그 Task를 읽어 알림으로 지켜본다. 읽지 못하면(이미 지워짐 등) 알리지 않는다. */
export async function trackHotplug(id: string, label: string): Promise<void> {
  try {
    const task = await call(api.GET("/tasks/{key}", { params: { path: { key: id } } }));
    trackTask(task, label);
  } catch {
    // 저장은 끝났다. 결과는 VM의 작업 탭에서 볼 수 있다.
  }
}

/** 다른 사람이 먼저 바꿔 If-Match가 맞지 않았다. */
export function isConflict(err: unknown): boolean {
  return err instanceof ApiError && err.code === "revision_mismatch";
}
