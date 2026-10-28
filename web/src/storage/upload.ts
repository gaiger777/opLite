import type { TFunction } from "i18next";
import { toast } from "sonner";

import {
  api,
  call,
  currentAuthEpoch,
  NetworkError,
  reportUnauthorized,
  toApiError,
} from "@/api/client";
import type { components } from "@/api/schema";
import type { Task } from "@/api/types";
import { errorMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

type Schemas = components["schemas"];

export interface UploadParams {
  name: string;
  pool: string;
  format: Schemas["ImageFormat"];
  sourceFormat: Schemas["SourceFormat"];
}

/** 파일 이름으로 볼륨 이름을 제안한다: `Ubuntu 24.04.vmdk` → `ubuntu-24-04` */
export function nameFromFile(file: string): string {
  return file
    .replace(/\.[^.]*$/, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+/, "")
    .slice(0, 63)
    .replace(/-+$/, "");
}

/** 확장자로 원본 형식을 짐작한다. 모르면 raw다(`.img`, `.raw`, `.iso`). */
export function sourceFormatOf(file: string): Schemas["SourceFormat"] {
  const ext = file.toLowerCase().split(".").pop();
  return ext === "vmdk" ? "vmdk" : ext === "qcow2" ? "qcow2" : "raw";
}

/** 다 보낸 뒤 이만큼 응답이 없으면 알림을 바꾼다. 요청은 끊지 않는다(끊으면 서버가 받은 볼륨을 지운다). */
export const NO_RESPONSE_MS = 2 * 60 * 1000;

/**
 * 본문을 보내기 전에 서버가 받을 수 있는지 묻는다(`/volumes/import/check`, 204). 권한·이름·풀·공간 따위로
 * 거절하면 `ApiError`를 던진다.
 */
export async function checkUpload(size: number, params: UploadParams, signal?: AbortSignal) {
  await call(
    api.POST("/volumes/import/check", {
      params: {
        query: {
          name: params.name,
          pool: params.pool,
          format: params.format,
          source_format: params.sourceFormat,
          size,
        },
      },
      signal,
    }),
  );
}

/**
 * 이미지를 `/volumes/import`로 올린다. fetch는 올리는 진행률을 알려 주지 않아 `XMLHttpRequest`를 쓴다.
 * 본문을 다 보내면 `onSent`를 부른다(빈 파일이어도). 받은 Task(202)로 끝나고, 오류 응답은 `ApiError`, 연결 실패는 `NetworkError`, 취소는 `signal`의 이유로
 * 거부한다. 중간에 끊기면 서버가 만들던 볼륨과 받은 파일을 지운다.
 */
export function uploadImage(
  file: Blob,
  params: UploadParams,
  {
    onProgress,
    onSent,
    signal,
  }: {
    onProgress?(loaded: number, total: number): void;
    onSent?(): void;
    signal?: AbortSignal;
  },
): Promise<Task> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(signal.reason);
      return;
    }
    const query = new URLSearchParams({
      name: params.name,
      pool: params.pool,
      format: params.format,
      source_format: params.sourceFormat,
    });
    const xhr = new XMLHttpRequest();
    const sentAt = currentAuthEpoch();
    const onAbort = () => xhr.abort();
    signal?.addEventListener("abort", onAbort);
    const done = () => signal?.removeEventListener("abort", onAbort);
    xhr.open("POST", `${window.location.origin}/api/v1/volumes/import?${query}`);
    xhr.setRequestHeader("Content-Type", "application/octet-stream");
    xhr.upload.onprogress = (e) => onProgress?.(e.loaded, e.total || file.size);
    xhr.upload.onload = () => onSent?.();
    xhr.onload = () => {
      done();
      let body: unknown = null;
      try {
        body = JSON.parse(xhr.responseText);
      } catch {
        // 프록시의 HTML 오류 페이지 따위
      }
      if (xhr.status === 202) {
        // Task가 아닌 본문(프록시 따위)은 실패로 본다.
        if (typeof (body as Partial<Task> | null)?.id === "string") {
          resolve(body as Task);
        } else {
          reject(toApiError(xhr.status, body));
        }
        return;
      }
      if (xhr.status === 401) {
        reportUnauthorized(sentAt);
      }
      reject(toApiError(xhr.status, body));
    };
    xhr.onerror = () => {
      done();
      reject(new NetworkError(new Error("upload failed")));
    };
    xhr.onabort = () => {
      done();
      reject(signal?.reason ?? new DOMException("upload aborted", "AbortError"));
    };
    xhr.send(file);
  });
}

/** 진행 중인 업로드: 알림 id → 취소. `silent`면 취소를 알리지 않는다. */
const active = new Map<string, { controller: AbortController; silent: boolean }>();

/** 올리는 중에 페이지를 떠나려 하면 브라우저가 확인한다. */
function guard(e: BeforeUnloadEvent) {
  e.preventDefault();
  // 옛 브라우저
  e.returnValue = "";
}

/** 업로드를 모두 취소한다. 로그아웃하거나 세션이 끝나면 부른다(다음 사용자에게 이어지지 않게). */
export function resetUploads() {
  for (const upload of active.values()) {
    upload.silent = true;
    upload.controller.abort();
  }
}

/**
 * 업로드를 시작하고 진행률과 취소 버튼을 알림으로 보여 준다. 화면을 옮겨도 계속된다. 다 올리면 볼륨을
 * 채우는 Task를 `trackTask`로 넘긴다.
 */
export function startUpload(file: File, params: UploadParams, t: TFunction) {
  const id = `upload-${crypto.randomUUID()}`;
  const controller = new AbortController();
  const cancel = { label: t("common.cancel"), onClick: () => controller.abort() };
  if (active.size === 0) {
    window.addEventListener("beforeunload", guard);
  }
  const upload = { controller, silent: false };
  active.set(id, upload);
  let shown = -1;
  let processing = false;
  let waiting: ReturnType<typeof setTimeout> | undefined;
  // 다 보냈다. 서버가 파일을 쓰고 Task를 만드는 동안은 취소하지 않는다.
  const sentAll = () => {
    processing = true;
    toast.loading(t("uploads.processing", { name: params.name }), {
      id,
      action: undefined,
      duration: Infinity,
    });
    // 오래 답이 없으면 닫을 수 있는 알림으로 바꾼다. 늦게 오는 응답은 평소처럼 알린다.
    waiting = setTimeout(() => {
      toast.warning(t("uploads.noResponse", { name: params.name }), {
        id,
        duration: Infinity,
        closeButton: true,
      });
    }, NO_RESPONSE_MS);
  };
  const show = (loaded: number, total: number) => {
    if (processing) {
      return;
    }
    // 화면 낭독기가 덜 읽도록 5% 단위로만 바꾼다.
    const percent = total > 0 ? Math.floor((loaded / total) * 20) * 5 : 0;
    if (percent === shown) {
      return;
    }
    shown = percent;
    toast.loading(
      t("uploads.progress", {
        name: params.name,
        percent,
        loaded: formatBytes(loaded),
        total: formatBytes(total),
      }),
      { id, action: cancel, duration: Infinity },
    );
  };
  show(0, file.size);
  // 확인에서 거절되면 본문을 보내지 않는다.
  let sent = false;
  void checkUpload(file.size, params, controller.signal)
    .then(() => {
      sent = true;
      return uploadImage(file, params, {
        onProgress: show,
        onSent: sentAll,
        signal: controller.signal,
      });
    })
    .then((task) => {
      toast.dismiss(id);
      trackTask(task, t("tasks.op.volume-import", { target: params.name }));
    })
    // 넘기다 던져도 알림이 "올리는 중"에 남지 않게 여기서 함께 받는다.
    // `trackTask`에서 던졌다면 서버의 가져오기 Task는 이미 돌고 있을 수 있다.
    .catch((err: unknown) => {
      // 같은 id로 바꾸는 알림은 이전 옵션을 물려받으므로 취소 버튼을 지운다.
      if (upload.silent) {
        toast.dismiss(id);
      } else if (controller.signal.aborted) {
        toast.info(t("uploads.cancelled", { name: params.name }), {
          id,
          action: undefined,
          duration: 4000,
        });
      } else {
        // 서버가 본문을 다 받기 전에 거절해도(401·409·507 따위) 브라우저는 연결 끊김으로만 알린다.
        const error =
          sent && err instanceof NetworkError ? t("uploads.dropped") : errorMessage(t, err);
        toast.error(t("uploads.failed", { name: params.name, error }), {
          id,
          action: undefined,
          duration: Infinity,
          closeButton: true,
        });
      }
    })
    .finally(() => {
      clearTimeout(waiting);
      active.delete(id);
      if (active.size === 0) {
        window.removeEventListener("beforeunload", guard);
      }
    });
}
