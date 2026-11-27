// 조각 업로드(계획 77-8, 서버 `/uploads`): 큰 이미지를 조각(기본 16 MiB)으로 보내고 끊기면 받은 데부터 잇는다. 진행은 모듈 상태
// (`useUploads`)로 두어 업로드 대화상자와 작업·알림 센터가 같은 항목을 그린다 — 속도·남은 시간·올린 양, 일시정지·이어 올리기·
// 취소. 다 보내면 서버가 볼륨을 만들고 가져오기 Task(변환·확인)를 돌려주며, 그 뒤 단계는 Task 진행으로 그린다
// (`uploadStage`). 탭을 닫았다 와도 서버의 업로드 목록(`GET /uploads`)과 이 브라우저에 적어 둔 파일 표시(이름·크기·
// 고친 시각)로 같은 파일을 고르면 잇는다. 접속한 노드가 기능 `chunked-uploads`를 알리지 않으면 쓰지 않는다(옛 한 번 올리기).
import { useSyncExternalStore } from "react";

import {
  ApiError,
  api,
  call,
  currentAuthEpoch,
  NetworkError,
  reportUnauthorized,
  toApiError,
} from "@/api/client";
import type { components } from "@/api/schema";
import type { Node, Task } from "@/api/types";
import type { UploadParams } from "@/storage/upload";

type Schemas = components["schemas"];
export type ServerUpload = Schemas["Upload"];

/** 서버 기능(접속한 노드가 알려야 쓴다 — 서버 `node::CHUNKED_UPLOADS`) */
export const CHUNKED_FEATURE = "chunked-uploads";

/** 접속한 노드(`serving`)가 조각 업로드를 아는지 */
export function chunkedSupported(nodes: readonly Node[] | undefined, serving?: string): boolean {
  const node = nodes?.find((n) => n.id === serving);
  return (node?.status.features ?? []).includes(CHUNKED_FEATURE);
}

/** 보내는 쪽 단계. 다 보낸 뒤(`processing`)의 단계는 가져오기 Task가 정한다. */
export type UploadPhase =
  | "starting"
  | "uploading"
  | "paused"
  | "needsFile"
  | "completing"
  | "processing"
  | "failed"
  | "cancelled";

export interface UploadItem {
  /** 항목 열쇠(이 탭에서 연 것은 임시 열쇠, 서버 목록에서 온 것은 서버 id) */
  key: string;
  /** 서버 업로드 id(열기 전에는 없음) */
  id: string | null;
  /** 만들 볼륨 이름 */
  name: string;
  pool: string;
  content: Schemas["VolumeContent"];
  /** 원본 파일 이름(탭을 닫은 뒤 서버 목록에서 온 항목은 모름) */
  fileName: string | null;
  size: number;
  /** 서버가 받았다고 한 바이트 + 보내는 조각의 올라간 양 */
  sent: number;
  phase: UploadPhase;
  /** 초당 바이트(최근 몇 초의 평균 — 모르면 null) */
  speed: number | null;
  /** 풀에 바로 쓰는지(raw·ISO — 변환 단계 없음) */
  direct: boolean;
  /** 가져오기 Task id(다 보낸 뒤) */
  task: string | null;
  /** 실패·멈춘 까닭(번역한 글이 아니라 오류 — 그리는 쪽이 `errorMessage`로) */
  error: unknown;
}

/** 보내기를 몇 번 이어서 실패하면 멈추는가(그 뒤는 사용자가 "이어 올리기") */
export const MAX_RETRIES = 5;
/** 다시 보내기 전 기다림(ms, 실패 횟수마다 두 배, 많아야 `RETRY_MAX_MS`) */
export const RETRY_MS = 1000;
const RETRY_MAX_MS = 15_000;
/** 속도를 셀 창(ms) */
const SPEED_WINDOW_MS = 5000;
/** 이 브라우저가 적어 둔 파일 표시(탭을 닫았다 와서 같은 파일인지 볼 때) */
const MARKS_KEY = "orbivisor.uploads";

interface Mark {
  fileName: string;
  size: number;
  lastModified: number;
}

/** 돌고 있는 업로드의 파일·취소·속도 표본 */
interface Run {
  file: File;
  controller: AbortController;
  samples: { at: number; bytes: number }[];
}

/** 조각 크기를 서버가 알리지 않았을 때 */
const DEFAULT_CHUNK = 16 * 1024 * 1024;

let items: UploadItem[] = [];
const runs = new Map<string, Run>();
/** 이 탭에서 고른 파일과 서버가 권한 조각 크기(일시정지·끊김 뒤 다시 고르지 않고 잇게) */
const files = new Map<string, { file: File; chunk: number }>();
const listeners = new Set<() => void>();

function emit() {
  items = [...items];
  for (const l of listeners) {
    l();
  }
}

function patch(key: string, change: Partial<UploadItem>) {
  const i = items.findIndex((x) => x.key === key);
  if (i < 0) {
    return;
  }
  items[i] = { ...items[i]!, ...change };
  emit();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** 지금 업로드 항목(새것이 앞) */
export function useUploads(): UploadItem[] {
  return useSyncExternalStore(subscribe, () => items);
}

export function uploadItems(): UploadItem[] {
  return items;
}

function readMarks(): Record<string, Mark> {
  try {
    const v = JSON.parse(localStorage.getItem(MARKS_KEY) ?? "{}") as unknown;
    return v && typeof v === "object" ? (v as Record<string, Mark>) : {};
  } catch {
    return {};
  }
}

function writeMarks(marks: Record<string, Mark>) {
  try {
    localStorage.setItem(MARKS_KEY, JSON.stringify(marks));
  } catch {
    // 저장소를 쓸 수 없으면 이어 올리기 때 이름·크기만 본다.
  }
}

function mark(id: string, file: File) {
  writeMarks({
    ...readMarks(),
    [id]: { fileName: file.name, size: file.size, lastModified: file.lastModified },
  });
}

function unmark(id: string | null) {
  if (id) {
    const marks = readMarks();
    delete marks[id];
    writeMarks(marks);
  }
}

/** 고른 파일이 이 업로드의 파일인지: 크기가 같고, 적어 둔 표시가 있으면 이름·고친 시각도 같다. */
export function sameFile(item: UploadItem, file: File): boolean {
  if (file.size !== item.size) {
    return false;
  }
  const m = item.id ? readMarks()[item.id] : undefined;
  if (m) {
    return m.fileName === file.name && m.lastModified === file.lastModified;
  }
  return item.fileName === null || item.fileName === file.name;
}

/** 이 파일로 이을 수 있는 업로드(멈췄거나 파일을 기다림) */
export function resumableFor(file: File): UploadItem | undefined {
  return items.find(
    (x) => (x.phase === "paused" || x.phase === "needsFile") && x.id !== null && sameFile(x, file),
  );
}

/** 올리는 동안 페이지를 떠나려 하면 브라우저가 확인한다. */
function guard(e: BeforeUnloadEvent) {
  e.preventDefault();
  e.returnValue = "";
}

function syncGuard() {
  const busy = items.some((x) => runs.has(x.key));
  if (busy) {
    window.addEventListener("beforeunload", guard);
  } else {
    window.removeEventListener("beforeunload", guard);
  }
}

/** 조각 하나를 보낸다(진행은 `XMLHttpRequest` — fetch는 올리는 진행을 알리지 않는다). */
function putChunk(
  id: string,
  offset: number,
  body: Blob,
  onProgress: (loaded: number) => void,
  signal: AbortSignal,
): Promise<ServerUpload> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(signal.reason);
      return;
    }
    const xhr = new XMLHttpRequest();
    const sentAt = currentAuthEpoch();
    const onAbort = () => xhr.abort();
    signal.addEventListener("abort", onAbort);
    const done = () => signal.removeEventListener("abort", onAbort);
    xhr.open(
      "PUT",
      `${window.location.origin}/api/v1/uploads/${encodeURIComponent(id)}?offset=${offset}`,
    );
    xhr.setRequestHeader("Content-Type", "application/octet-stream");
    xhr.upload.onprogress = (e) => onProgress(e.loaded);
    xhr.onload = () => {
      done();
      let parsed: unknown = null;
      try {
        parsed = JSON.parse(xhr.responseText);
      } catch {
        // 프록시의 HTML 오류 페이지 따위
      }
      if (xhr.status === 200 && typeof (parsed as ServerUpload | null)?.offset === "number") {
        resolve(parsed as ServerUpload);
        return;
      }
      if (xhr.status === 401) {
        reportUnauthorized(sentAt);
      }
      reject(toApiError(xhr.status, parsed));
    };
    xhr.onerror = () => {
      done();
      reject(new NetworkError(new Error("upload failed")));
    };
    xhr.onabort = () => {
      done();
      reject(signal.reason ?? new DOMException("upload aborted", "AbortError"));
    };
    xhr.send(body);
  });
}

/** 다시 보내면 될 수도 있는 실패(연결·서버 잠깐의 문제·같은 업로드의 앞 요청이 아직 돎) */
function transient(err: unknown): boolean {
  if (err instanceof NetworkError) {
    return true;
  }
  return (
    err instanceof ApiError &&
    (err.status >= 500 || err.status === 408 || err.status === 429 || err.code === "upload_busy") &&
    err.code !== "insufficient_storage"
  );
}

function offsetOf(err: unknown): number | null {
  if (err instanceof ApiError && err.code === "upload_offset_mismatch") {
    const offset = (err.details as { offset?: unknown } | undefined)?.offset;
    return typeof offset === "number" ? offset : null;
  }
  return null;
}

const wait = (ms: number, signal: AbortSignal) =>
  new Promise<void>((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener("abort", () => {
      clearTimeout(timer);
      resolve();
    });
  });

/** 속도(최근 `SPEED_WINDOW_MS`): 표본을 더하고 창 밖의 것을 버린다. */
function sampleSpeed(run: Run, bytes: number): number | null {
  const now = Date.now();
  run.samples.push({ at: now, bytes });
  while (run.samples.length > 2 && now - run.samples[0]!.at > SPEED_WINDOW_MS) {
    run.samples.shift();
  }
  const first = run.samples[0]!;
  const dt = now - first.at;
  return dt >= 500 ? ((bytes - first.bytes) * 1000) / dt : null;
}

/** 남은 시간(초): 속도를 모르면 null */
export function remainingSeconds(item: UploadItem): number | null {
  return item.speed && item.speed > 0 ? Math.ceil((item.size - item.sent) / item.speed) : null;
}

/** 다 보낼 때까지 조각을 보내고 끝낸다. 멈추면(일시정지·취소) 조용히 돌아온다. */
async function pump(key: string, onTask: (task: Task, item: UploadItem) => void) {
  const run = runs.get(key);
  const item = items.find((x) => x.key === key);
  if (!run || !item?.id) {
    return;
  }
  const { signal } = run.controller;
  const id = item.id;
  let failures = 0;
  try {
    let confirmed = (await call(api.GET("/uploads/{id}", { params: { path: { id } }, signal })))
      .offset;
    patch(key, { phase: "uploading", sent: confirmed, error: null });
    run.samples = [];
    while (confirmed < item.size) {
      const end = Math.min(confirmed + (files.get(key)?.chunk ?? DEFAULT_CHUNK), item.size);
      const from = confirmed;
      try {
        const up = await putChunk(
          id,
          from,
          run.file.slice(from, end),
          (loaded) => {
            const sent = from + loaded;
            patch(key, { sent, speed: sampleSpeed(run, sent) });
          },
          signal,
        );
        confirmed = up.offset;
        failures = 0;
        patch(key, { sent: confirmed, error: null });
      } catch (err) {
        if (signal.aborted) {
          return;
        }
        const at = offsetOf(err);
        if (at !== null) {
          confirmed = at;
          patch(key, { sent: confirmed });
          continue;
        }
        if (!transient(err) || ++failures > MAX_RETRIES) {
          throw err;
        }
        patch(key, { error: err, speed: null });
        await wait(Math.min(RETRY_MS * 2 ** (failures - 1), RETRY_MAX_MS), signal);
        if (signal.aborted) {
          return;
        }
        // 끊긴 조각이 어디까지 닿았는지 묻는다(못 물으면 다음 조각 실패가 알린다).
        try {
          confirmed = (await call(api.GET("/uploads/{id}", { params: { path: { id } }, signal })))
            .offset;
        } catch {
          // 다음 시도에서
        }
        run.samples = [];
      }
    }
    patch(key, { phase: "completing", sent: item.size, speed: null, error: null });
    const task = await completeWithRetry(id, signal);
    unmark(id);
    runs.delete(key);
    patch(key, { phase: "processing", task: task.id });
    onTask(
      task,
      items.find((x) => x.key === key)!,
    );
  } catch (err) {
    if (signal.aborted) {
      return;
    }
    runs.delete(key);
    // 연결이 오래 끊겼으면 멈춤(사용자가 이어 올림), 서버가 거절했으면 실패
    patch(key, { phase: transient(err) ? "paused" : "failed", error: err, speed: null });
  } finally {
    syncGuard();
  }
}

async function completeWithRetry(id: string, signal: AbortSignal): Promise<Task> {
  for (let tries = 0; ; tries++) {
    try {
      return await call(api.POST("/uploads/{id}/complete", { params: { path: { id } }, signal }));
    } catch (err) {
      if (!transient(err) || tries >= MAX_RETRIES || signal.aborted) {
        throw err;
      }
      await wait(RETRY_MS, signal);
    }
  }
}

function begin(key: string, file: File, onTask: (task: Task, item: UploadItem) => void) {
  files.set(key, { file, chunk: files.get(key)?.chunk ?? DEFAULT_CHUNK });
  runs.set(key, { file, controller: new AbortController(), samples: [] });
  syncGuard();
  void pump(key, onTask);
}

/**
 * 조각 업로드를 시작한다: 서버에 업로드를 열고(확인 — 거절하면 항목이 실패로) 조각을 보낸다. `onTask`는 다 보낸 뒤 받은
 * 가져오기 Task를 넘긴다(알림·지켜보기). 같은 파일로 이을 업로드가 있으면 그것을 잇는다.
 */
export function startChunked(
  file: File,
  params: UploadParams,
  onTask: (task: Task, item: UploadItem) => void,
): string {
  const existing = resumableFor(file);
  if (existing) {
    resumeChunked(existing.key, file, onTask);
    return existing.key;
  }
  const key = `local-${crypto.randomUUID()}`;
  items = [
    {
      key,
      id: null,
      name: params.name,
      pool: params.pool,
      content: params.content ?? "disk",
      fileName: file.name,
      size: file.size,
      sent: 0,
      phase: "starting",
      speed: null,
      direct: false,
      task: null,
      error: null,
    },
    ...items,
  ];
  emit();
  void call(
    api.POST("/uploads", {
      body: {
        name: params.name,
        pool: params.pool,
        format: params.format,
        source_format: params.sourceFormat,
        size: file.size,
        ...(params.content === "iso" && { content: "iso" as const }),
      },
    }),
  ).then(
    (up) => {
      if (items.find((x) => x.key === key)?.phase === "cancelled") {
        void api.DELETE("/uploads/{id}", { params: { path: { id: up.id } } });
        return;
      }
      files.set(key, { file, chunk: up.chunk_size > 0 ? up.chunk_size : DEFAULT_CHUNK });
      mark(up.id, file);
      patch(key, { id: up.id, direct: up.direct, phase: "uploading" });
      begin(key, file, onTask);
    },
    (err: unknown) => patch(key, { phase: "failed", error: err }),
  );
  return key;
}

/** 멈춘 업로드를 잇는다(같은 파일 — 탭을 닫았다 왔으면 다시 고른 파일). 다른 파일이면 거짓. */
export function resumeChunked(
  key: string,
  file: File,
  onTask: (task: Task, item: UploadItem) => void,
): boolean {
  const item = items.find((x) => x.key === key);
  if (!item?.id || runs.has(key) || !sameFile(item, file)) {
    return false;
  }
  files.set(key, { file, chunk: files.get(key)?.chunk ?? DEFAULT_CHUNK });
  mark(item.id, file);
  patch(key, { phase: "uploading", fileName: file.name, error: null });
  begin(key, file, onTask);
  return true;
}

/** 멈춘 업로드를 이 탭에서 고른 파일로 잇는다. 파일이 없으면(탭을 닫았다 옴) 거짓 — 파일을 다시 골라야 한다. */
export function resumeSame(key: string, onTask: (task: Task, item: UploadItem) => void): boolean {
  const file = files.get(key)?.file;
  return file ? resumeChunked(key, file, onTask) : false;
}

/** 이 탭에 파일이 있어 다시 고르지 않고 이을 수 있는지 */
export function hasFile(key: string): boolean {
  return files.has(key);
}

/** 일시정지: 보내는 조각을 끊는다(받은 데까지는 서버에 남는다). */
export function pauseChunked(key: string) {
  const run = runs.get(key);
  if (!run) {
    return;
  }
  runs.delete(key);
  run.controller.abort();
  patch(key, { phase: "paused", speed: null, error: null });
  syncGuard();
}

/** 취소: 끊고 서버의 업로드(받은 것)를 지운다. */
export function cancelChunked(key: string) {
  const item = items.find((x) => x.key === key);
  const run = runs.get(key);
  runs.delete(key);
  files.delete(key);
  run?.controller.abort();
  if (item?.id) {
    unmark(item.id);
    void api.DELETE("/uploads/{id}", { params: { path: { id: item.id } } });
  }
  patch(key, { phase: "cancelled", speed: null });
  syncGuard();
}

/** 끝났거나 취소한 항목을 목록에서 치운다. */
export function dismissUpload(key: string) {
  items = items.filter((x) => x.key !== key);
  files.delete(key);
  emit();
}

/**
 * 서버에 남은 내 업로드(탭을 닫았거나 다른 탭에서 연 것)를 "파일을 다시 골라 잇기" 항목으로 더한다. 이미 있는 것은 둔다.
 * 기능이 없거나 읽지 못하면 아무것도 하지 않는다.
 */
export async function loadServerUploads() {
  let list: ServerUpload[];
  try {
    list = (await call(api.GET("/uploads"))).items;
  } catch {
    return;
  }
  const marks = readMarks();
  const known = new Set(items.map((x) => x.id));
  const added = list
    .filter((u) => !known.has(u.id))
    .map((u): UploadItem => ({
      key: u.id,
      id: u.id,
      name: u.name,
      pool: u.pool,
      content: u.content,
      fileName: marks[u.id]?.fileName ?? null,
      size: u.size,
      sent: u.offset,
      phase: "needsFile",
      speed: null,
      direct: u.direct,
      task: null,
      error: null,
    }));
  if (added.length > 0) {
    items = [...items, ...added];
    emit();
  }
}

/** 모두 멈추고 잊는다(로그아웃·세션 끝 — 다음 사용자에게 이어지지 않게). 서버의 업로드는 남는다(같은 사람이 다시 이음). */
export function resetChunked() {
  for (const run of runs.values()) {
    run.controller.abort();
  }
  runs.clear();
  files.clear();
  items = [];
  emit();
  syncGuard();
}

/** 다 보낸 뒤의 단계(가져오기 Task): 변환 중(퍼센트), 확인 중, 끝, 실패 */
export type AfterStage =
  | { stage: "converting"; percent: number | null }
  | { stage: "checking" }
  | { stage: "done" }
  | { stage: "failed" };

export function uploadStage(task: Task | undefined): AfterStage {
  if (!task) {
    return { stage: "checking" };
  }
  if (task.status.state === "succeeded") {
    return { stage: "done" };
  }
  if (task.status.state === "failed") {
    return { stage: "failed" };
  }
  const p = task.status.progress;
  if (p?.stage === "converting") {
    return {
      stage: "converting",
      percent:
        p.total_bytes > 0
          ? Math.floor((Math.min(p.done_bytes, p.total_bytes) * 100) / p.total_bytes)
          : null,
    };
  }
  return { stage: "checking" };
}
