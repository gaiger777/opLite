import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { delay, http, HttpResponse } from "msw";

import { ApiError, NetworkError, setUnauthorizedHandler } from "@/api/client";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { ThemeProvider } from "@/lib/theme";
import {
  nameFromFile,
  NO_RESPONSE_MS,
  resetUploads,
  sourceFormatOf,
  startUpload,
  uploadImage,
} from "@/storage/upload";
import * as tasks from "@/tasks/tasks";
import { resetTasks } from "@/tasks/tasks";
import { FakeXhr } from "@/test/fakeXhr";
import { task } from "@/test/fixtures";
import { apiUrl, server } from "@/test/server";

function toasts() {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
}

const PARAMS = { name: "img", pool: "local", format: "qcow2", sourceFormat: "vmdk" } as const;

/** 확인을 통과한 뒤 만들어지는 XHR을 기다린다. */
async function sending(): Promise<FakeXhr> {
  return vi.waitFor(() => {
    expect(FakeXhr.last).toBeDefined();
    return FakeXhr.last!;
  });
}

/** 업로드 확인 요청의 쿼리. 기본으로 통과(204)시킨다. */
let checks: URLSearchParams[] = [];

beforeEach(() => {
  vi.stubGlobal("XMLHttpRequest", FakeXhr);
  checks = [];
  server.use(
    http.post(apiUrl("/volumes/import/check"), ({ request }) => {
      checks.push(new URL(request.url).searchParams);
      return new HttpResponse(null, { status: 204 });
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  resetUploads();
  resetTasks();
});

it("suggests a volume name and format from the file name", () => {
  expect(nameFromFile("Ubuntu 24.04.vmdk")).toBe("ubuntu-24-04");
  expect(nameFromFile("--disk--.qcow2")).toBe("disk");
  expect(nameFromFile(`${"a".repeat(62)}-b.img`)).toBe("a".repeat(62));
  expect(sourceFormatOf("disk.VMDK")).toBe("vmdk");
  expect(sourceFormatOf("disk.qcow2")).toBe("qcow2");
  expect(sourceFormatOf("disk.qcow")).toBe("raw");
  expect(sourceFormatOf("disk.img")).toBe("raw");
});

it("posts the file to the import API and reports progress", async () => {
  const progress = vi.fn();
  const sent = vi.fn();
  const file = new Blob(["x".repeat(1024)]);
  const done = uploadImage(file, PARAMS, { onProgress: progress, onSent: sent });
  const xhr = FakeXhr.last!;
  expect(xhr.method).toBe("POST");
  expect(xhr.url).toBe(
    `${window.location.origin}/api/v1/volumes/import?name=img&pool=local&format=qcow2&source_format=vmdk`,
  );
  expect(xhr.headers["Content-Type"]).toBe("application/octet-stream");
  expect(xhr.body).toBe(file);
  xhr.progress(512, 1024);
  expect(progress).toHaveBeenCalledWith(512, 1024);
  expect(sent).not.toHaveBeenCalled();
  xhr.uploaded();
  expect(sent).toHaveBeenCalledOnce();
  xhr.respond(202, task("t1", "volume-import", "v1"));
  await expect(done).resolves.toMatchObject({ id: "t1" });
});

it("turns failures into API, network and abort errors", async () => {
  const conflict = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.respond(409, { code: "pool_not_on_node", message: "elsewhere" });
  await expect(conflict).rejects.toEqual(new ApiError(409, "pool_not_on_node", "elsewhere"));

  const offline = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.fail();
  await expect(offline).rejects.toBeInstanceOf(NetworkError);

  const controller = new AbortController();
  const cancelled = uploadImage(new Blob(["x"]), PARAMS, { signal: controller.signal });
  controller.abort(new Error("stop"));
  await expect(cancelled).rejects.toThrow("stop");
  expect(FakeXhr.last!.aborted).toBe(true);

  // Task가 아닌 202 본문
  const odd = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.respond(202, { ok: true });
  await expect(odd).rejects.toBeInstanceOf(ApiError);
});

it("reports a 401 like other requests", async () => {
  const unauthorized = vi.fn();
  setUnauthorizedHandler(unauthorized, () => 3);
  onTestFinished(() =>
    setUnauthorizedHandler(
      () => {},
      () => 0,
    ),
  );
  const done = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.respond(401, { code: "unauthorized", message: "no session" });
  await expect(done).rejects.toBeInstanceOf(ApiError);
  expect(unauthorized).toHaveBeenCalledWith(3);
});

it("shows progress in a toast, guards the page and hands the task over", async () => {
  toasts();
  act(() => startUpload(new File(["x".repeat(2048)], "img.vmdk"), PARAMS, i18n.t));
  expect(await screen.findByText("Uploading img: 0% (0 B / 2 KiB)")).toBeInTheDocument();
  const xhr = await sending();
  // 먼저 파일 크기로 확인했다.
  expect(Object.fromEntries(checks[0]!)).toEqual({
    name: "img",
    pool: "local",
    format: "qcow2",
    source_format: "vmdk",
    size: "2048",
  });
  const leave = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(leave);
  expect(leave.defaultPrevented).toBe(true);

  act(() => xhr.progress(1024, 2048));
  expect(await screen.findByText("Uploading img: 50% (1 KiB / 2 KiB)")).toBeInTheDocument();
  // 5% 단위로 내림한다(56%가 아니라 55%). sonner는 나중에 바꾸므로 기다린다.
  act(() => xhr.progress(1150, 2048));
  expect(await screen.findByText("Uploading img: 55% (1.1 KiB / 2 KiB)")).toBeInTheDocument();
  // 다 보내면 서버가 처리하는 동안 취소할 수 없다.
  act(() => {
    xhr.progress(2048, 2048);
    xhr.uploaded();
  });
  expect(await screen.findByText("Processing img…")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
  act(() => xhr.respond(202, task("t9", "volume-import", "v1")));
  expect(await screen.findByText("Import volume img")).toBeInTheDocument();
  const after = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(after);
  expect(after.defaultPrevented).toBe(false);
});

it("cancels from the toast", async () => {
  toasts();
  act(() => startUpload(new File(["x"], "img.vmdk"), PARAMS, i18n.t));
  const xhr = await sending();
  await userEvent.click(await screen.findByRole("button", { name: "Cancel" }));
  expect(await screen.findByText("Cancelled the upload of img.")).toBeInTheDocument();
  expect(xhr.aborted).toBe(true);
  expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
});

it("cancels while the check is pending without sending the file", async () => {
  server.use(http.post(apiUrl("/volumes/import/check"), () => delay("infinite")));
  toasts();
  act(() => startUpload(new File(["x"], "img.vmdk"), PARAMS, i18n.t));
  await userEvent.click(await screen.findByRole("button", { name: "Cancel" }));
  expect(await screen.findByText("Cancelled the upload of img.")).toBeInTheDocument();
  expect(FakeXhr.last).toBeUndefined();
});

it("keeps guarding the page until the last upload ends", async () => {
  toasts();
  act(() => startUpload(new File(["x"], "a.raw"), { ...PARAMS, name: "a" }, i18n.t));
  const first = await sending();
  FakeXhr.last = undefined;
  act(() => startUpload(new File(["x"], "b.raw"), { ...PARAMS, name: "b" }, i18n.t));
  const second = await sending();
  const leaving = () => {
    const e = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(e);
    return e.defaultPrevented;
  };

  act(() => first.respond(202, task("ta", "volume-import", "va")));
  expect(await screen.findByText("Import volume a")).toBeInTheDocument();
  expect(leaving()).toBe(true);
  act(() => second.respond(202, task("tb", "volume-import", "vb")));
  expect(await screen.findByText("Import volume b")).toBeInTheDocument();
  expect(leaving()).toBe(false);
});

it("says the server may have dropped the connection", async () => {
  toasts();
  act(() => startUpload(new File(["x"], "img.raw"), PARAMS, i18n.t));
  const xhr = await sending();
  act(() => xhr.fail());
  expect(
    await screen.findByText(
      "Upload of img failed. The server rejected the upload or the connection was lost.",
    ),
  ).toBeInTheDocument();
});

it("fails instead of hanging when the answer is not a task", async () => {
  toasts();
  act(() => startUpload(new File(["x"], "img.raw"), PARAMS, i18n.t));
  const xhr = await sending();
  act(() => xhr.respond(202, "<html>"));
  expect(
    await screen.findByText("Upload of img failed. The server failed: HTTP 202"),
  ).toBeInTheDocument();
  expect(screen.queryByText(/^Uploading img/)).not.toBeInTheDocument();
});

// 넘기다 던져도 "올리는 중" 알림에 남지 않고 실패를 알린다.
it("says the upload failed when handing over the task throws", async () => {
  vi.spyOn(tasks, "trackTask").mockImplementation(() => {
    throw new Error("boom");
  });
  onTestFinished(() => {
    vi.restoreAllMocks();
  });
  toasts();
  act(() => startUpload(new File(["x"], "img.raw"), PARAMS, i18n.t));
  const xhr = await sending();
  act(() => xhr.respond(202, task("t9", "volume-import", "v1")));
  expect(await screen.findByText("Upload of img failed. boom")).toBeInTheDocument();
  expect(screen.queryByText(/^Uploading img/)).not.toBeInTheDocument();
});

it("does not send the file when the check rejects it", async () => {
  server.use(
    http.post(apiUrl("/volumes/import/check"), () =>
      HttpResponse.json({ code: "insufficient_storage", message: "needs 2 GiB" }, { status: 507 }),
    ),
  );
  toasts();
  act(() => startUpload(new File(["x"], "img.raw"), PARAMS, i18n.t));
  expect(
    await screen.findByText(
      "Upload of img failed. The node does not have enough free space for this upload.",
    ),
  ).toBeInTheDocument();
  expect(FakeXhr.last).toBeUndefined();
  expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
});

it("says when the check cannot reach the server", async () => {
  server.use(http.post(apiUrl("/volumes/import/check"), () => HttpResponse.error()));
  toasts();
  act(() => startUpload(new File(["x"], "img.raw"), PARAMS, i18n.t));
  expect(
    await screen.findByText("Upload of img failed. Cannot reach the server."),
  ).toBeInTheDocument();
  expect(FakeXhr.last).toBeUndefined();
});

it("stops waiting after two minutes without cancelling, and still reports a late answer", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  onTestFinished(() => {
    vi.useRealTimers();
  });
  toasts();
  // 빈 파일은 진행률 없이 다 보낸다.
  act(() => startUpload(new File([], "img.raw"), PARAMS, i18n.t));
  const xhr = await sending();
  act(() => xhr.uploaded());
  expect(await screen.findByText("Processing img…")).toBeInTheDocument();
  await act(() => vi.advanceTimersByTimeAsync(NO_RESPONSE_MS - 1000));
  expect(screen.getByText("Processing img…")).toBeInTheDocument();
  await act(() => vi.advanceTimersByTimeAsync(1000));
  const waiting = await screen.findByText(
    "img: no response from the server. Check the volume list for the result.",
  );
  expect(within(waiting.closest("li")!).getByRole("button", { name: "Close toast" })).toBeVisible();
  // 요청은 끊지 않는다(끊으면 서버가 받은 볼륨을 지운다).
  expect(xhr.aborted).toBe(false);
  act(() => xhr.respond(202, task("t9", "volume-import", "v1")));
  expect(await screen.findByText("Import volume img")).toBeInTheDocument();
});
