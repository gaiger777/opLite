import { act, fireEvent, render, screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { ApiError } from "@/api/client";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { startOvaUpload, uploadOva } from "@/imports/ova";
import { ThemeProvider } from "@/lib/theme";
import { resetUploads } from "@/storage/upload";
import { FakeXhr } from "@/test/fakeXhr";
import { vmImport } from "@/test/fixtures";
import { apiUrl, server } from "@/test/server";

/** OVA 확인 요청의 쿼리. 기본으로 통과(204)시킨다. */
let checks: URLSearchParams[] = [];

beforeEach(() => {
  vi.stubGlobal("XMLHttpRequest", FakeXhr);
  checks = [];
  server.use(
    http.post(apiUrl("/vm-imports/ova/check"), ({ request }) => {
      checks.push(new URL(request.url).searchParams);
      return new HttpResponse(null, { status: 204 });
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  resetUploads();
});

it("posts the OVA with its file name and returns the import", async () => {
  const file = new File(["ova"], "Web 01.ova");
  const done = uploadOva(file, "web01", {});
  const xhr = FakeXhr.last!;
  expect(xhr.method).toBe("POST");
  expect(xhr.url).toBe(
    `${window.location.origin}/api/v1/vm-imports/ova?name=web01&file_name=Web+01.ova`,
  );
  expect(xhr.headers["Content-Type"]).toBe("application/octet-stream");
  expect(xhr.body).toBe(file);
  xhr.respond(201, vmImport("web01", 1));
  await expect(done).resolves.toMatchObject({ name: "web01" });

  // 해석하지 못한 OVA
  const bad = uploadOva(file, "web01", {});
  FakeXhr.last!.respond(400, { code: "invalid_ova", message: "no .ovf file" });
  await expect(bad).rejects.toEqual(new ApiError(400, "invalid_ova", "no .ovf file"));
});

it("checks first, shows progress and offers to open the import", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  const open = vi.fn();
  act(() => startOvaUpload(new File(["x".repeat(2048)], "web01.ova"), "web01", i18n.t, open));
  expect(await screen.findByText("Uploading web01: 0% (0 B / 2 KiB)")).toBeInTheDocument();
  const xhr = await vi.waitFor(() => {
    expect(FakeXhr.last).toBeDefined();
    return FakeXhr.last!;
  });
  expect(Object.fromEntries(checks[0]!)).toEqual({ name: "web01", size: "2048" });
  act(() => xhr.uploaded());
  expect(await screen.findByText("Processing web01…")).toBeInTheDocument();
  act(() => xhr.respond(201, vmImport("web01", 1)));
  expect(
    await screen.findByText("Uploaded web01. Review the proposed settings and run the import."),
  ).toBeInTheDocument();
  // sonner는 알림을 누르면 jsdom에 없는 포인터 캡처를 쓰므로 버튼만 누른다.
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  expect(open).toHaveBeenCalledWith("web01");
});

it("says why the server rejected the OVA", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  act(() => startOvaUpload(new File(["x"], "a.ova"), "a", i18n.t, () => {}));
  const xhr = await vi.waitFor(() => {
    expect(FakeXhr.last).toBeDefined();
    return FakeXhr.last!;
  });
  act(() => xhr.respond(400, { code: "invalid_ova", message: "no .ovf file" }));
  expect(
    await screen.findByText("Upload of a failed. The file is not a usable OVA: no .ovf file"),
  ).toBeInTheDocument();
});
