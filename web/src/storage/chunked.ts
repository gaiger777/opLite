// 조각 업로드(계획 77-8, 서버 `/uploads`): 큰 이미지를 조각(기본 16 MiB)으로 보내고 끊기면 받은 데부터 잇는다. 진행은 모듈 상태
// (`useUploads`)로 두어 업로드 대화상자와 작업·알림 센터가 같은 항목을 그린다 — 속도·남은 시간·올린 양, 일시정지·이어 올리기·
// 취소·버리기. 다 보내면 서버가 볼륨을 만들고 가져오기 Task(확인·변환·마무리)를 돌려주며, 그 뒤 단계는 Task 진행으로 그린다
// (`uploadStage`). 올리며 파일 전체의 SHA-256을 세어 끝낼 때 보내고(서버가 받은 것과 맞춰 봄), 이어 올리기는 열 때 서버에 둔 지문
// (이름·크기·고친 시각·앞 1 MiB의 SHA-256)이 같은 파일일 때만 한다(다른 브라우저·PC에서도). 접속한 노드가 기능
// `chunked-uploads`를 알리지 않으면 쓰지 않는다(옛 한 번 올리기).
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
import { headSha256, hashRange, Sha256 } from "@/storage/sha256";
import type { UploadParams } from "@/storage/upload";

type Schemas = components["schemas"];
export type ServerUpload = Schemas["Upload"];
export type Fingerprint = Schemas["UploadFingerprint"];

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
  /** 서버의 업로드를 버리는 중(취소·버리기) */
  | "discarding"
  /** 서버의 업로드를 버리지 못했다(다시 버리기) */
  | "discardFailed"
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
  /** 만들 볼륨 형식(고르지 않았으면 서버가 정할 때까지 null)과 원본 형식 — 이어 올릴 때 같은 대상인지 본다 */
  format: Schemas["ImageFormat"] | null;
  sourceFormat: Schemas["SourceFormat"];
  /** 원본 파일 이름(지문이 없으면 모름) */
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
  /** 열 때 서버에 둔 파일 지문 */
  fingerprint: Fingerprint | null;
  /** 업로드를 연(받는) 노드 id */
  node: string | null;
  /** 서버에 업로드(받은 것)가 남아 있는지 — 실패한 항목을 버리거나 이을 수 있는지 */
  kept: boolean;
}

/** 보내기를 몇 번 이어서 실패하면 멈추는가(그 뒤는 사용자가 "이어 올리기") */
export const MAX_RETRIES = 5;
/** 다시 보내기 전 기다림(ms, 실패 횟수마다 두 배, 많아야 `RETRY_MAX_MS`) */
export const RETRY_MS = 1000;
const RETRY_MAX_MS = 15_000;
/** 속도를 셀 창(ms) */
const SPEED_WINDOW_MS = 5000;
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
/** 이 탭이 센 받은 데까지의 SHA-256(`length` = 센 바이트) */
const hashes = new Map<string, Sha256>();
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

/** 파일 지문: 이름·고친 시각·앞 1 MiB의 SHA-256(크기는 업로드가 따로 안다) */
export async function fingerprintOf(file: File): Promise<Fingerprint> {
  return {
    file_name: file.name,
    last_modified: file.lastModified,
    head_sha256: await headSha256(file),
  };
}

/** 이 업로드의 파일인지: 크기와 열 때 둔 지문(이름·고친 시각·앞부분 해시)이 모두 같다. 지문이 없으면 맞춰 볼 수 없어 아니다. */
export function sameFile(item: UploadItem, size: number, fingerprint: Fingerprint): boolean {
  const f = item.fingerprint;
  return (
    f !== null &&
    item.size === size &&
    f.file_name === fingerprint.file_name &&
    f.last_modified === fingerprint.last_modified &&
    f.head_sha256 === fingerprint.head_sha256
  );
}

/** 이을 수 있는 항목(멈췄거나 파일을 기다리거나, 실패했지만 서버에 받은 것이 남음) */
function canResume(x: UploadItem): boolean {
  return (
    x.id !== null &&
    x.kept &&
    (x.phase === "paused" || x.phase === "needsFile" || x.phase === "failed")
  );
}

/** 이 파일로 이을 수 있는 업로드 */
export function resumableFor(file: File, fingerprint: Fingerprint): UploadItem | undefined {
  return items.find((x) => canResume(x) && sameFile(x, file.size, fingerprint));
}

/** 이어 올릴 업로드가 고른 대상(이름·풀·내용·형식)과 같은지 — 다르면 이전 업로드를 버리고 새로 연다(리뷰 I7). */
export function sameTarget(item: UploadItem, params: UploadParams): boolean {
  return (
    item.name === params.name &&
    item.pool === params.pool &&
    item.content === (params.content ?? "disk") &&
    item.sourceFormat === params.sourceFormat &&
    (item.format === null || item.format === params.format)
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

/** 이 탭이 센 해시를 `to`바이트까지 잇는다(서버가 받은 데가 센 데보다 앞이면 처음부터 다시 센다). */
async function catchUp(key: string, file: Blob, to: number, signal: AbortSignal): Promise<Sha256> {
  let hash = hashes.get(key);
  if (!hash || hash.length > to) {
    hash = new Sha256();
  }
  hashes.set(key, hash);
  return hash.length < to ? hashRange(hash, file, hash.length, to, signal) : hash;
}

/** 서버에 업로드가 남아 있지 않게 된 실패(찾지 못함·체크섬이 달라 서버가 버림) */
function gone(err: unknown): boolean {
  return err instanceof ApiError && (err.status === 404 || err.code === "upload_checksum_mismatch");
}

/** 다 보낼 때까지 조각을 보내고(보내는 동안 그 조각의 해시를 앞서 센다) 끝낸다. 멈추면(일시정지·취소) 조용히 돌아온다. */
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
    let hash = await catchUp(key, run.file, confirmed, signal);
    while (confirmed < item.size) {
      if (signal.aborted) {
        return;
      }
      const end = Math.min(confirmed + (files.get(key)?.chunk ?? DEFAULT_CHUNK), item.size);
      const from = confirmed;
      const ahead = hashRange(hash.clone(), run.file, from, end, signal);
      // 보내기가 먼저 실패하면 기다리지 않는다(거부는 버린다).
      ahead.catch(() => {});
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
        if (confirmed === end) {
          hash = await ahead;
          hashes.set(key, hash);
        } else {
          hash = await catchUp(key, run.file, confirmed, signal);
        }
        patch(key, { sent: confirmed, error: null });
      } catch (err) {
        if (signal.aborted) {
          return;
        }
        const at = offsetOf(err);
        if (at !== null) {
          confirmed = at;
          patch(key, { sent: confirmed });
          hash = await catchUp(key, run.file, confirmed, signal);
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
        hash = await catchUp(key, run.file, confirmed, signal);
        run.samples = [];
      }
    }
    hash = await catchUp(key, run.file, item.size, signal);
    if (signal.aborted) {
      return;
    }
    patch(key, { phase: "completing", sent: item.size, speed: null, error: null });
    const task = await completeWithRetry(item, hash.hex(), signal);
    runs.delete(key);
    hashes.delete(key);
    patch(key, { phase: "processing", task: task.id, kept: false });
    onTask(
      task,
      items.find((x) => x.key === key)!,
    );
  } catch (err) {
    if (signal.aborted) {
      return;
    }
    runs.delete(key);
    // 연결이 오래 끊겼으면 멈춤(사용자가 이어 올림), 서버가 거절했으면 실패(남았으면 이어 올리거나 버린다)
    patch(key, {
      phase: transient(err) ? "paused" : "failed",
      error: err,
      speed: null,
      ...(gone(err) && { kept: false }),
    });
  } finally {
    syncGuard();
  }
}

/** 끝내기에서 응답만 잃었을 때(다시 보내면 기록이 없어 404): 그 이름의 볼륨을 만든 가져오기 Task(리뷰 I8) */
async function findImportTask(name: string): Promise<Task | undefined> {
  try {
    const volume = await call(api.GET("/volumes/{key}", { params: { path: { key: name } } }));
    const { items: tasks } = await call(api.GET("/tasks"));
    return tasks
      .filter((t) => t.spec.op.kind === "volume-import" && t.spec.target === volume.id)
      .at(-1);
  } catch {
    return undefined;
  }
}

async function completeWithRetry(
  item: UploadItem,
  sha256: string,
  signal: AbortSignal,
): Promise<Task> {
  const id = item.id!;
  for (let tries = 0; ; tries++) {
    try {
      return await call(
        api.POST("/uploads/{id}/complete", {
          params: { path: { id } },
          body: { sha256 },
          signal,
        }),
      );
    } catch (err) {
      if (tries > 0 && err instanceof ApiError && err.status === 404) {
        const task = await findImportTask(item.name);
        if (task) {
          return task;
        }
      }
      if (!transient(err) || tries >= MAX_RETRIES || signal.aborted) {
        throw err;
      }
      await wait(RETRY_MS, signal);
    }
  }
}

/** 서버의 업로드를 버린다. 연결 오류·잠깐의 문제면 잠시 뒤 다시 보낸다. 이미 없으면(404) 된 것이다. */
async function discardOnServer(id: string): Promise<void> {
  for (let tries = 0; ; tries++) {
    try {
      await call(api.DELETE("/uploads/{id}", { params: { path: { id } } }));
      return;
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        return;
      }
      if (!transient(err) || tries >= MAX_RETRIES) {
        throw err;
      }
      await new Promise((r) => setTimeout(r, Math.min(RETRY_MS * 2 ** tries, RETRY_MAX_MS)));
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
 * 가져오기 Task를 넘긴다(알림·지켜보기). 같은 파일(`fingerprint` — [`fingerprintOf`])로 이을 업로드가 있으면 그것을 잇되,
 * 이름·풀·형식을 바꿨으면 그 업로드를 버리고 새로 연다.
 */
export function startChunked(
  file: File,
  params: UploadParams,
  onTask: (task: Task, item: UploadItem) => void,
  fingerprint: Fingerprint,
): string {
  const existing = resumableFor(file, fingerprint);
  if (existing) {
    if (sameTarget(existing, params)) {
      resumeWith(existing.key, file, onTask);
      return existing.key;
    }
    cancelChunked(existing.key);
  }
  const key = `local-${crypto.randomUUID()}`;
  items = [
    {
      key,
      id: null,
      name: params.name,
      pool: params.pool,
      content: params.content ?? "disk",
      format: params.format,
      sourceFormat: params.sourceFormat,
      fileName: file.name,
      size: file.size,
      sent: 0,
      phase: "starting",
      speed: null,
      direct: false,
      task: null,
      error: null,
      fingerprint,
      node: null,
      kept: false,
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
        fingerprint,
        ...(params.content === "iso" && { content: "iso" as const }),
      },
    }),
  ).then(
    (up) => {
      const phase = items.find((x) => x.key === key)?.phase;
      if (phase === "cancelled" || phase === "discarding") {
        void discardOnServer(up.id).catch(() => {});
        return;
      }
      files.set(key, { file, chunk: up.chunk_size > 0 ? up.chunk_size : DEFAULT_CHUNK });
      patch(key, {
        id: up.id,
        direct: up.direct,
        format: up.format,
        node: up.node ?? null,
        kept: true,
        phase: "uploading",
      });
      begin(key, file, onTask);
    },
    (err: unknown) => patch(key, { phase: "failed", error: err }),
  );
  return key;
}

function resumeWith(key: string, file: File, onTask: (task: Task, item: UploadItem) => void) {
  files.set(key, { file, chunk: files.get(key)?.chunk ?? DEFAULT_CHUNK });
  patch(key, { phase: "uploading", fileName: file.name, error: null });
  begin(key, file, onTask);
}

/** 멈춘 업로드를 잇는다(탭을 닫았다 왔으면 다시 고른 파일 — 지문이 같아야 한다). 다른 파일이면 거짓. */
export async function resumeChunked(
  key: string,
  file: File,
  onTask: (task: Task, item: UploadItem) => void,
): Promise<boolean> {
  const fingerprint = await fingerprintOf(file);
  const item = items.find((x) => x.key === key);
  if (!item || !canResume(item) || runs.has(key) || !sameFile(item, file.size, fingerprint)) {
    return false;
  }
  resumeWith(key, file, onTask);
  return true;
}

/** 멈춘 업로드를 이 탭에서 고른 파일로 잇는다. 파일이 없으면(탭을 닫았다 옴) 거짓 — 파일을 다시 골라야 한다. */
export function resumeSame(key: string, onTask: (task: Task, item: UploadItem) => void): boolean {
  const file = files.get(key)?.file;
  const item = items.find((x) => x.key === key);
  if (!file || !item || !canResume(item) || runs.has(key)) {
    return false;
  }
  resumeWith(key, file, onTask);
  return true;
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

/**
 * 취소·버리기: 보내는 조각을 끊고(서버는 그 조각을 마친 뒤 지운다) 서버의 업로드(받은 것)를 지운다. 연결이 안 되면 잠시 뒤
 * 다시 보내고, 끝내 못 버리면 "버리지 못함"(다시 버리기)으로 남는다(리뷰 I1·I2).
 */
export function cancelChunked(key: string) {
  const item = items.find((x) => x.key === key);
  const run = runs.get(key);
  runs.delete(key);
  files.delete(key);
  hashes.delete(key);
  run?.controller.abort();
  syncGuard();
  if (!item?.id || !item.kept) {
    patch(key, { phase: "cancelled", speed: null });
    return;
  }
  patch(key, { phase: "discarding", speed: null, error: null });
  discardOnServer(item.id).then(
    () => patch(key, { phase: "cancelled", kept: false }),
    (err: unknown) => patch(key, { phase: "discardFailed", error: err }),
  );
}

/** 끝났거나 취소한 항목을 목록에서 치운다. */
export function dismissUpload(key: string) {
  items = items.filter((x) => x.key !== key);
  files.delete(key);
  hashes.delete(key);
  emit();
}

/**
 * 서버에 남은 내 업로드(탭을 닫았거나 다른 탭·브라우저에서 연 것)를 "파일을 다시 골라 잇기" 항목으로 더한다. 이미 있는 것은
 * 둔다. 기능이 없거나 읽지 못하면 아무것도 하지 않는다.
 */
export async function loadServerUploads() {
  let list: ServerUpload[];
  try {
    list = (await call(api.GET("/uploads"))).items;
  } catch {
    return;
  }
  const known = new Set(items.map((x) => x.id));
  const added = list
    .filter((u) => !known.has(u.id))
    .map((u): UploadItem => ({
      key: u.id,
      id: u.id,
      name: u.name,
      pool: u.pool,
      content: u.content,
      format: u.format,
      sourceFormat: u.source_format,
      fileName: u.fingerprint?.file_name ?? null,
      size: u.size,
      sent: u.offset,
      phase: "needsFile",
      speed: null,
      direct: u.direct,
      task: null,
      error: null,
      fingerprint: u.fingerprint ?? null,
      node: u.node ?? null,
      kept: true,
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
  hashes.clear();
  items = [];
  emit();
  syncGuard();
}

/** 다 보낸 뒤의 단계(가져오기 Task — 서버 순서): 원본 확인, 변환 중(퍼센트), 마무리, 끝, 실패 */
export type AfterStage =
  | { stage: "checking" }
  | { stage: "converting"; percent: number | null }
  | { stage: "finishing" }
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
  if (p?.stage === "finishing") {
    return { stage: "finishing" };
  }
  return { stage: "checking" };
}
