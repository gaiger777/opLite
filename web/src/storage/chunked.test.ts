import { http, HttpResponse } from "msw";

import type { Task } from "@/api/types";
import {
  cancelChunked,
  chunkedSupported,
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
  localStorage.clear();
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
    ...extra,
  };
}

/** 서버: 업로드 열기·받은 데 묻기·끝내기·버리기. `state.offset`이 받은 데다. */
function serve(state: { offset: number; opened: unknown[]; deleted: number; completed: number }) {
  server.use(
    http.post(apiUrl("/uploads"), async ({ request }) => {
      state.opened.push(await request.json());
      return HttpResponse.json(up(0), { status: 201 });
    }),
    http.get(apiUrl("/uploads/u1"), () => HttpResponse.json(up(state.offset))),
    http.post(apiUrl("/uploads/u1/complete"), () => {
      state.completed += 1;
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

it("is used only when the serving node knows chunked uploads", () => {
  const knows = { ...NODE, status: { ...NODE.status, features: ["chunked-uploads"] } };
  expect(chunkedSupported([knows], "n1")).toBe(true);
  expect(chunkedSupported([NODE], "n1")).toBe(false);
  expect(chunkedSupported([knows], "n2")).toBe(false);
  expect(chunkedSupported(undefined, "n1")).toBe(false);
});

it("sends the chunks in order, follows the server offset and completes into a task", async () => {
  const state = { offset: 0, opened: [] as unknown[], deleted: 0, completed: 0 };
  serve(state);
  const tasks: Task[] = [];
  startChunked(file(), PARAMS, (t) => tasks.push(t));
  expect(uploadItems()[0]).toMatchObject({ phase: "starting", name: "disk", size: 10 });

  let xhr = await nextXhr();
  expect(state.opened).toEqual([
    { name: "disk", pool: "local", format: "qcow2", source_format: "qcow2", size: 10 },
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
  // 다 올렸으면 이 브라우저의 파일 표시를 지운다.
  expect(localStorage.getItem("orbivisor.uploads")).toBe("{}");
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
  startChunked(file(), PARAMS, () => {});
  await vi.waitFor(() => expect(uploadItems()[0]?.phase).toBe("failed"));
  expect(String(uploadItems()[0]?.error)).toContain("not enough free space");
});

it("retries a dropped chunk from what the server received", async () => {
  const state = { offset: 0, opened: [] as unknown[], deleted: 0, completed: 0 };
  serve(state);
  startChunked(file(), PARAMS, () => {});
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
  const state = { offset: 0, opened: [] as unknown[], deleted: 0, completed: 0 };
  serve(state);
  const key = startChunked(file(), PARAMS, () => {});
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
  expect(uploadItems()[0]?.phase).toBe("cancelled");
  await vi.waitFor(() => expect(state.deleted).toBe(1));
});

it("lists the uploads left on the server and resumes only with the same file", async () => {
  const state = { offset: 6, opened: [] as unknown[], deleted: 0, completed: 0 };
  serve(state);
  localStorage.setItem(
    "orbivisor.uploads",
    JSON.stringify({ u1: { fileName: "disk.qcow2", size: 10, lastModified: 7 } }),
  );
  server.use(http.get(apiUrl("/uploads"), () => HttpResponse.json({ items: [up(6)] })));
  await loadServerUploads();
  expect(uploadItems()).toEqual([
    expect.objectContaining({ key: "u1", phase: "needsFile", sent: 6, fileName: "disk.qcow2" }),
  ]);
  // 크기가 같아도 다른 파일(이름·고친 시각)이면 잇지 않는다.
  expect(resumeChunked("u1", new File(["abcdefghij"], "other.qcow2"), () => {})).toBe(false);
  // 같은 파일을 다시 고르면(새 업로드를 열지 않고) 받은 데부터
  startChunked(file(), PARAMS, () => {});
  const xhr = await nextXhr();
  expect(xhr.url).toMatch(/offset=6$/);
  expect(state.opened).toEqual([]);
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
  expect(uploadStage({ ...t1, status: { state: "succeeded", error: null } })).toEqual({
    stage: "done",
  });
  expect(uploadStage({ ...t1, status: { state: "failed", error: "x" } })).toEqual({
    stage: "failed",
  });
});
