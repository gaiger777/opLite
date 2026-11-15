import type { QueryClient } from "@tanstack/react-query";

import { ApiError, api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Vm } from "@/api/types";
import { trackTask, waitForTask } from "@/tasks/tasks";
import { type DiskRow, gibToBytes, toSpec, type VmFormValues } from "@/vms/form";

/**
 * 새 빈 볼륨을 `format`(풀 종류의 기본 형식)으로 만들고 그 Task가 끝날 때까지 기다린다. `label`은
 * 진행 알림 문구다.
 */
export async function createVolume(
  queryClient: QueryClient,
  disk: DiskRow,
  format: components["schemas"]["ImageFormat"],
  label: string,
  signal: AbortSignal,
): Promise<void> {
  const task = await call(
    api.POST("/volumes", {
      body: {
        name: disk.volume,
        spec: {
          pool: disk.pool,
          size_bytes: gibToBytes(disk.sizeGib)!,
          format,
        },
      },
    }),
  );
  trackTask(task, label);
  await waitForTask(queryClient, task.id, signal);
}

/** VM을 만든다. `values.start`면 켜진 상태로 만든다. */
export async function createVm(values: VmFormValues): Promise<void> {
  await call(
    api.POST("/vms", {
      body: { name: values.name, spec: toSpec(values, values.start ? "running" : "stopped") },
    }),
  );
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
): Promise<string | null> {
  let response: Response | undefined;
  await call(
    api
      .PUT("/vms/{key}", {
        params: {
          path: { key: vm.id },
          header: { "If-Match": revision },
        },
        body: { labels: vm.labels, spec: toSpec(values, vm.spec.power ?? "stopped") },
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
