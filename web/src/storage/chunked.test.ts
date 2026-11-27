import { createHash } from "node:crypto";

import { http, HttpResponse } from "msw";

import type { Task } from "@/api/types";
import {
  cancelChunked,
  chunkedSupported,
  type Fingerprint,
  fingerprintOf,
  loadServerUploads,
  pauseChunked,
  remainingSeconds,
  resetChunked,
  resumeChunked,
  resumeSame,
  type ServerUpload,
  startChunked,
  uploadItems,
  uploadStage,
} from "@/storage/chunked";
import { FakeXhr } from "@/test/fakeXhr";
import { NODE, task } from "@/test/fixtures";
import { apiUrl, server } from "@/test/server";

beforeEach(() => vi.stubGlobal("XMLHttpRequest", FakeXhr));
afterEach(() => {
  vi.unstubAllGlobals();
  resetChunked();
});

const PARAMS = { name: "disk", pool: "local", format: "qcow2", sourceFormat: "qcow2" } as const;

function up(offset: number, extra: Partial<ServerUpload> = {}): ServerUpload {
  return {
    id: "u1",
    name: "disk",
    pool: "local",
    format: "qcow2",
    source_format: "qcow2",
    content: "disk",
    size: 10,
    offset,
    chunk_size: 4,
    direct: false,
    created_at: 0,
    updated_at: 0,
    expires_at: 0,
    node: "n1",
    ...extra,
  };
}

interface ServerState {
  offset: number;
  opened: unknown[];
  deleted: number;
  completed: number;
  sha256?: unknown;
}

const fresh = (offset = 0): ServerState => ({ offset, opened: [], deleted: 0, completed: 0 });

/** 서버: 업로드 열기·받은 데 묻기·끝내기·버리기. `state.offset`이 받은 데다. */
function serve(state: ServerState) {
  server.use(
    http.post(apiUrl("/uploads"), async ({ request }) => {
      state.opened.push(await request.json());
      return HttpResponse.json(up(0), { status: 201 });
    }),
    http.get(apiUrl("/uploads/u1"), () => HttpResponse.json(up(state.offset))),
    http.post(apiUrl("/uploads/u1/complete"), async ({ request }) => {
      state.completed += 1;
      state.sha256 = ((await request.json()) as { sha256?: unknown }).sha256;
      return HttpResponse.json(task("t1", "volume-import", "v1"), { status: 202 });
    }),
    http.delete(apiUrl("/uploads/u1"), () => {
      state.deleted += 1;
      return new HttpResponse(null, { status: 204 });
    }),
  );
}

/** 새 조각 요청(`FakeXhr.last`가 `prev`와 다를 때)을 기다린다. */
async function nextXhr(prev?: FakeXhr): Promise<FakeXhr> {
  let xhr: FakeXhr | undefined;
  await vi.waitFor(() => {
    xhr = FakeXhr.last;
    expect(xhr && xhr !== prev).toBe(true);
  });
  return xhr!;
}

const file = () => new File(["0123456789"], "disk.qcow2", { lastModified: 7 });
const sha = (text: string) => createHash("sha256").update(text).digest("hex");
const FP: Fingerprint = {
  file_name: "disk.qcow2",
  last_modified: 7,
  head_sha256: sha("0123456789"),
};

it("is used only when the serving node knows chunked uploads", () => {
  const knows = { ...NODE, status: { ...NODE.status, features: ["chunked-uploads"] } };
  expect(chunkedSupported([knows], "n1")).toBe(true);
  expect(chunkedSupported([NODE], "n1")).toBe(false);
  expect(chunkedSupported([knows], "n2")).toBe(false);
  expect(chunkedSupported(undefined, "n1")).toBe(false);
});

it("sends the chunks in order, follows the server offset and completes into a task", async () => {
  const state = fresh();
  serve(state);
  const tasks: Task[] = [];
  expect(await fingerprintOf(file())).toEqual(FP);
  startChunked(file(), PARAMS, (t) => tasks.push(t), FP);
  expect(uploadItems()[0]).toMatchObject({ phase: "starting", name: "disk", size: 10 });

  let xhr = await nextXhr();
  expect(state.opened).toEqual([
    {
      name: "disk",
      pool: "local",
      format: "qcow2",
      source_format: "qcow2",
      size: 10,
      fingerprint: FP,
    },
  ]);
  expect(xhr.method).toBe("PUT");
  expect(xhr.url).toMatch(/\/api\/v1\/uploads\/u1\?offset=0$/);
  expect((xhr.body as Blob).size).toBe(4);
  xhr.progress(2, 4);
  expect(uploadItems()[0]).toMatchObject({ id: "u1", phase: "uploading", sent: 2 });
  state.offset = 4;
  xhr.respond(200, up(4));

  // 서버가 다른 데를 받았다고 하면(같은 조각을 다시 보냄) 그 데부터
  xhr = await nextXhr(xhr);
  expect(xhr.url).toMatch(/offset=4$/);
  xhr.respond(409, {
    code: "upload_offset_mismatch",
    message: "upload has 8 bytes",
    details: { offset: 8 },
  });
  xhr = await nextXhr(xhr);
  expect(xhr.url).toMatch(/offset=8$/);
  expect((xhr.body as Blob).size).toBe(2);
  state.offset = 10;
  xhr.respond(200, up(10));

  await vi.waitFor(() => expect(tasks.map((t) => t.id)).toEqual(["t1"]));
  expect(state.completed).toBe(1);
  expect(uploadItems()[0]).toMatchObject({ phase: "processing", task: "t1", sent: 10 });
  // 올리며 센 파일 전체의 SHA-256을 보낸다(서버가 건너뛰라 한 4..8도 파일에서 읽어 셈).
  expect(state.sha256).toBe(sha("0123456789"));
});

it("stops on a refusal and keeps the server message", async () => {
  server.use(
    http.post(apiUrl("/uploads"), () =>
      HttpResponse.json(
        { code: "insufficient_storage", message: "not enough free space" },
        { status: 507 },
      ),
    ),
  );
  startChunked(file(), PARAMS, () => {}, FP);
  await vi.waitFor(() => expect(uploadItems()[0]?.phase).toBe("failed"));
  expect(String(uploadItems()[0]?.error)).toContain("not enough free space");
  // 열지 못했으니 서버에 버릴 것이 없다.
  expect(uploadItems()[0]?.kept).toBe(false);
});

it("retries a dropped chunk from what the server received", async () => {
  const state = fresh();
  serve(state);
  startChunked(file(), PARAMS, () => {}, FP);
  let xhr = await nextXhr();
  xhr.progress(3, 4);
  // 연결이 끊겼다 — 3바이트가 닿았다.
  state.offset = 3;
  xhr.fail();
  await vi.waitFor(() => expect(uploadItems()[0]?.error).toBeTruthy());
  xhr = await nextXhr(xhr);
  expect(xhr.url).toMatch(/offset=3$/);
  expect(uploadItems()[0]).toMatchObject({ phase: "uploading", sent: 3 });
});

it("pauses, resumes with the same file and cancels", async () => {
  const state = fresh();
  serve(state);
  const key = startChunked(file(), PARAMS, () => {}, FP);
  let xhr = await nextXhr();
  state.offset = 4;
  xhr.respond(200, up(4));
  xhr = await nextXhr(xhr);
  pauseChunked(key);
  expect(xhr.aborted).toBe(true);
  expect(uploadItems()[0]).toMatchObject({ phase: "paused", speed: null });

  expect(resumeSame(key, () => {})).toBe(true);
  xhr = await nextXhr(xhr);
  expect(xhr.url).toMatch(/offset=4$/);
  cancelChunked(key);
  expect(xhr.aborted).toBe(true);
  expect(uploadItems()[0]?.phase).toBe("discarding");
  await vi.waitFor(() => expect(uploadItems()[0]?.phase).toBe("cancelled"));
  expect(state.deleted).toBe(1);
});

it("lists the uploads left on the server and resumes only with the same file", async () => {
  const state = fresh(6);
  serve(state);
  server.use(
    http.get(apiUrl("/uploads"), () => HttpResponse.json({ items: [up(6, { fingerprint: FP })] })),
  );
  await loadServerUploads();
  expect(uploadItems()).toEqual([
    expect.objectContaining({ key: "u1", phase: "needsFile", sent: 6, fileName: "disk.qcow2" }),
  ]);
  // 크기·이름·고친 시각이 같아도 내용(앞부분 해시)이 다르면 잇지 않는다(다른 PC에서 같은 이름의 다른 이미지).
  const lookalike = new File(["abcdefghij"], "disk.qcow2", { lastModified: 7 });
  expect(await resumeChunked("u1", lookalike, () => {})).toBe(false);
  expect(await resumeChunked("u1", new File(["0123456789"], "disk.qcow2"), () => {})).toBe(false);
  // 같은 파일을 다시 고르면(새 업로드를 열지 않고) 받은 데부터 — 받은 앞부분의 해시는 파일에서 다시 센다.
  startChunked(file(), PARAMS, () => {}, FP);
  const xhr = await nextXhr();
  expect(xhr.url).toMatch(/offset=6$/);
  expect(state.opened).toEqual([]);
  state.offset = 10;
  xhr.respond(200, up(10));
  await vi.waitFor(() => expect(state.sha256).toBe(sha("0123456789")));
});

it("starts over when the name or the pool changed", async () => {
  const state = fresh(6);
  serve(state);
  server.use(
    http.get(apiUrl("/uploads"), () => HttpResponse.json({ items: [up(6, { fingerprint: FP })] })),
  );
  await loadServerUploads();
  startChunked(file(), { ...PARAMS, name: "disk-2" }, () => {}, FP);
  await vi.waitFor(() => expect(state.deleted).toBe(1));
  await vi.waitFor(() =>
    expect(state.opened).toEqual([expect.objectContaining({ name: "disk-2", size: 10 })]),
  );
  expect(uploadItems().map((x) => [x.name, x.phase])).toEqual([
    ["disk-2", "uploading"],
    ["disk", "cancelled"],
  ]);
});

it("sends the discard again while the server is busy and keeps a stuck one to retry", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  try {
    const state = fresh();
    serve(state);
    let refuse = 2;
    server.use(
      http.delete(apiUrl("/uploads/u1"), () => {
        if (refuse-- > 0) {
          return HttpResponse.json(
            { code: "upload_busy", message: "receiving another request" },
            { status: 409 },
          );
        }
        state.deleted += 1;
        return new HttpResponse(null, { status: 204 });
      }),
    );
    const key = startChunked(file(), PARAMS, () => {}, FP);
    const first = await nextXhr();
    cancelChunked(key);
    await vi.advanceTimersByTimeAsync(10_000);
    await vi.waitFor(() => expect(uploadItems()[0]?.phase).toBe("cancelled"));
    expect(state.deleted).toBe(1);

    // 끝내 버리지 못하면 "버리지 못함"으로 남아 다시 버린다.
    server.use(
      http.delete(apiUrl("/uploads/u1"), () =>
        HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
      ),
    );
    resetChunked();
    const again = startChunked(file(), PARAMS, () => {}, FP);
    await nextXhr(first);
    cancelChunked(again);
    await vi.waitFor(() => expect(uploadItems()[0]?.phase).toBe("discardFailed"));
    expect(uploadItems()[0]?.kept).toBe(true);
  } finally {
    vi.useRealTimers();
  }
});

it("lets a failed upload that the server keeps resume or be discarded", async () => {
  const state = fresh();
  serve(state);
  const key = startChunked(file(), PARAMS, () => {}, FP);
  let xhr = await nextXhr();
  xhr.respond(507, { code: "insufficient_storage", message: "not enough free space" });
  await vi.waitFor(() => expect(uploadItems()[0]?.phase).toBe("failed"));
  expect(uploadItems()[0]?.kept).toBe(true);
  // 공간을 비운 뒤 이어 올린다.
  expect(resumeSame(key, () => {})).toBe(true);
  xhr = await nextXhr(xhr);
  xhr.respond(507, { code: "insufficient_storage", message: "not enough free space" });
  await vi.waitFor(() => expect(uploadItems()[0]?.phase).toBe("failed"));
  cancelChunked(key);
  await vi.waitFor(() => expect(uploadItems()[0]?.phase).toBe("cancelled"));
  expect(state.deleted).toBe(1);
});

it("finds the import task when only the complete response was lost", async () => {
  const state = fresh();
  serve(state);
  let calls = 0;
  server.use(
    http.post(apiUrl("/uploads/u1/complete"), () => {
      calls += 1;
      // 처음 것은 서버가 끝냈지만 응답이 끊겼다 — 다시 보내면 기록이 없다.
      return calls === 1
        ? HttpResponse.error()
        : HttpResponse.json({ code: "not_found", message: "gone" }, { status: 404 });
    }),
    http.get(apiUrl("/volumes/disk"), () => HttpResponse.json({ id: "v1", name: "disk" })),
    http.get(apiUrl("/tasks"), () =>
      HttpResponse.json({ items: [task("t9", "volume-import", "v1")], revision: 1 }),
    ),
  );
  const tasks: Task[] = [];
  startChunked(file(), PARAMS, (t) => tasks.push(t), FP);
  let xhr = await nextXhr();
  xhr.respond(200, up(4));
  xhr = await nextXhr(xhr);
  xhr.respond(200, up(8));
  xhr = await nextXhr(xhr);
  xhr.respond(200, up(10));
  await vi.waitFor(() => expect(tasks.map((t) => t.id)).toEqual(["t9"]), { timeout: 5000 });
  expect(uploadItems()[0]).toMatchObject({ phase: "processing", task: "t9" });
});

it("works out the remaining time and the stage after the upload", () => {
  const item = { ...uploadItems()[0]!, size: 100, sent: 40, speed: 20 } as never;
  expect(remainingSeconds(item)).toBe(3);
  expect(remainingSeconds({ ...(item as object), speed: null } as never)).toBeNull();
  const t1 = task("t1", "volume-import", "v1");
  expect(uploadStage(undefined)).toEqual({ stage: "checking" });
  expect(
    uploadStage({
      ...t1,
      status: {
        state: "running",
        error: null,
        progress: { stage: "converting", done_bytes: 30, total_bytes: 100 },
      },
    }),
  ).toEqual({ stage: "converting", percent: 30 });
  expect(
    uploadStage({
      ...t1,
      status: {
        state: "running",
        error: null,
        progress: { stage: "finishing", done_bytes: 100, total_bytes: 100 },
      },
    }),
  ).toEqual({ stage: "finishing" });
  expect(uploadStage({ ...t1, status: { state: "succeeded", error: null } })).toEqual({
    stage: "done",
  });
  expect(uploadStage({ ...t1, status: { state: "failed", error: "x" } })).toEqual({
    stage: "failed",
  });
});
