import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ApiError, NetworkError, setUnauthorizedHandler } from "@/api/client";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { ThemeProvider } from "@/lib/theme";
import {
  nameFromFile,
  resetUploads,
  sourceFormatOf,
  startUpload,
  uploadImage,
} from "@/storage/upload";
import { resetTasks } from "@/tasks/tasks";
import { FakeXhr } from "@/test/fakeXhr";
import { task } from "@/test/fixtures";

function toasts() {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
}

const PARAMS = { name: "img", pool: "local", format: "qcow2", sourceFormat: "vmdk" } as const;

beforeEach(() => vi.stubGlobal("XMLHttpRequest", FakeXhr));
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
  expect(sourceFormatOf("disk.img")).toBe("raw");
});

it("posts the file to the import API and reports progress", async () => {
  const progress = vi.fn();
  const file = new Blob(["x".repeat(1024)]);
  const done = uploadImage(file, PARAMS, { onProgress: progress });
  const xhr = FakeXhr.last!;
  expect(xhr.method).toBe("POST");
  expect(xhr.url).toBe(
    `${window.location.origin}/api/v1/volumes/import?name=img&pool=local&format=qcow2&source_format=vmdk`,
  );
  expect(xhr.headers["Content-Type"]).toBe("application/octet-stream");
  expect(xhr.body).toBe(file);
  xhr.progress(512, 1024);
  expect(progress).toHaveBeenCalledWith(512, 1024);
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
});

it("reports a 401 like other requests", async () => {
  const unauthorized = vi.fn();
  setUnauthorizedHandler(unauthorized, () => 3);
  const done = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.respond(401, { code: "unauthorized", message: "no session" });
  await expect(done).rejects.toBeInstanceOf(ApiError);
  expect(unauthorized).toHaveBeenCalledWith(3);
  setUnauthorizedHandler(
    () => {},
    () => 0,
  );
});

it("shows progress in a toast, guards the page and hands the task over", async () => {
  toasts();
  act(() => startUpload(new File(["x".repeat(2048)], "img.vmdk"), PARAMS, i18n.t));
  expect(await screen.findByText("Uploading img: 0% (0 B / 2 KiB)")).toBeInTheDocument();
  const leave = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(leave);
  expect(leave.defaultPrevented).toBe(true);

  act(() => FakeXhr.last!.progress(1024, 2048));
  expect(await screen.findByText("Uploading img: 50% (1 KiB / 2 KiB)")).toBeInTheDocument();
  act(() => FakeXhr.last!.respond(202, task("t9", "volume-import", "v1")));
  expect(await screen.findByText("Import volume img")).toBeInTheDocument();
  const after = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(after);
  expect(after.defaultPrevented).toBe(false);
});

it("cancels from the toast", async () => {
  toasts();
  act(() => startUpload(new File(["x"], "img.vmdk"), PARAMS, i18n.t));
  await userEvent.click(await screen.findByRole("button", { name: "Cancel" }));
  expect(await screen.findByText("Cancelled the upload of img.")).toBeInTheDocument();
  expect(FakeXhr.last!.aborted).toBe(true);
  expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
});
