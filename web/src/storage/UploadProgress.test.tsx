import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { ApiError } from "@/api/client";
import type { Task } from "@/api/types";
import { fingerprintOf, resetChunked, type UploadItem, uploadItems } from "@/storage/chunked";
import { UploadDialog } from "@/storage/UploadDialog";
import { createVmHref, durationText, UploadProgress } from "@/storage/UploadProgress";
import { expectNoAxeViolations } from "@/test/axe";
import { FakeXhr } from "@/test/fakeXhr";
import { NODE, pool, task } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() => vi.stubGlobal("XMLHttpRequest", FakeXhr));
afterEach(() => {
  vi.unstubAllGlobals();
  resetChunked();
});

function item(extra: Partial<UploadItem> = {}): UploadItem {
  return {
    key: "u1",
    id: "u1",
    name: "ubuntu",
    pool: "local",
    content: "disk",
    format: "qcow2",
    sourceFormat: "qcow2",
    fileName: "ubuntu.qcow2",
    size: 4 * 1024 * 1024 * 1024,
    sent: 1024 * 1024 * 1024,
    phase: "uploading",
    speed: 50 * 1024 * 1024,
    direct: false,
    task: null,
    error: null,
    fingerprint: null,
    node: "n1",
    kept: true,
    ...extra,
  };
}

it("shows the steps, the amount, the speed and the time left with pause and cancel", async () => {
  mockLists({});
  const { container } = renderPage(<UploadProgress item={item()} onTask={() => {}} />);
  const steps = await screen.findByRole("list", { name: "Upload steps" });
  expect(
    within(steps)
      .getAllByRole("listitem")
      .map((li) => li.textContent),
  ).toEqual(["Uploading", "→Check", "→Convert", "→Finish", "→Done"]);
  expect(within(steps).getByText("Uploading").closest("li")).toHaveAttribute(
    "aria-current",
    "step",
  );
  expect(screen.getByRole("progressbar", { name: "Image ubuntu" })).toHaveAttribute(
    "aria-valuenow",
    "25",
  );
  // 3 GiB 남음 ÷ 50 MiB/s = 62초
  expect(
    screen.getByText(
      (_, el) => el?.textContent === "25% · 1 GiB / 4 GiB · 50 MiB/s · 1 min 2 s left",
    ),
  ).toBeInTheDocument();
  // 센터에 여럿이어도 단추 이름으로 어느 업로드인지 안다.
  expect(screen.getByRole("button", { name: "Pause ubuntu" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Cancel upload ubuntu" })).toBeEnabled();
  await expectNoAxeViolations(container, { disable: ["region"] });
});

it("follows the import task: converting, then done with a link to create a VM", async () => {
  const converting: Task = {
    ...task("t1", "volume-import", "v1"),
    status: {
      state: "running",
      error: null,
      progress: { stage: "converting", done_bytes: 30, total_bytes: 100 },
    },
  };
  mockLists({ task: [converting] });
  const view = renderPage(
    <UploadProgress item={item({ phase: "processing", task: "t1" })} onTask={() => {}} />,
  );
  const steps = await screen.findByRole("list", { name: "Upload steps" });
  await vi.waitFor(() =>
    expect(within(steps).getByText("Convert").closest("li")).toHaveAttribute(
      "aria-current",
      "step",
    ),
  );
  expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "30");
  // 확인은 이미 지났다(서버 순서: 확인 → 변환 → 마무리).
  expect(within(steps).getByText("Check").closest("li")).toHaveClass("text-success-fg");
  view.unmount();

  mockLists({
    task: [
      {
        ...converting,
        status: {
          state: "running",
          error: null,
          progress: { stage: "finishing", done_bytes: 100, total_bytes: 100 },
        },
      },
    ],
  });
  const finishing = renderPage(
    <UploadProgress item={item({ phase: "processing", task: "t1" })} onTask={() => {}} />,
  );
  const later = await screen.findByRole("list", { name: "Upload steps" });
  await vi.waitFor(() =>
    expect(within(later).getByText("Finish").closest("li")).toHaveAttribute("aria-current", "step"),
  );
  finishing.unmount();

  mockLists({ task: [{ ...converting, status: { state: "succeeded", error: null } }] });
  renderPage(<UploadProgress item={item({ phase: "processing", task: "t1" })} onTask={() => {}} />);
  const link = await screen.findByRole("link", { name: "Create a VM with this image" });
  expect(link).toHaveAttribute("href", "/vms/-new?purpose=image&disk=ubuntu");
});

it("asks for the same file when the tab was closed and refuses another file", async () => {
  mockLists({});
  renderPage(<UploadProgress item={item({ phase: "needsFile", speed: null })} onTask={() => {}} />);
  expect(await screen.findByText("Choose the file again to resume")).toBeInTheDocument();
  await userEvent.upload(
    screen.getByLabelText("Choose the same file to resume"),
    new File(["x"], "other.iso"),
  );
  expect(await screen.findByRole("alert")).toHaveTextContent(/not the file of this upload/);
});

it("offers to resume or discard a failed upload the server keeps, and explains a lost one", async () => {
  mockLists({ node: [NODE] });
  const failed = renderPage(
    <UploadProgress
      item={item({ phase: "failed", speed: null, error: new Error("no space") })}
      onTask={() => {}}
    />,
  );
  expect(await screen.findByRole("button", { name: "Discard upload ubuntu" })).toBeEnabled();
  expect(screen.getByLabelText("Choose the same file to resume ubuntu")).toBeInTheDocument();
  // 버리기 전에는 닫기(서버에 남김)를 보이지 않는다.
  expect(screen.queryByRole("button", { name: /Close the upload item/ })).toBeNull();
  failed.unmount();

  renderPage(
    <UploadProgress
      item={item({
        phase: "failed",
        kept: false,
        speed: null,
        error: new ApiError(404, "not_found", "upload u1 not found"),
      })}
      onTask={() => {}}
    />,
  );
  await vi.waitFor(() =>
    expect(screen.getByText(/connect to that node again/)).toHaveTextContent(`(${NODE.name})`),
  );
  expect(screen.queryByRole("button", { name: /Discard/ })).toBeNull();
  expect(screen.getByRole("button", { name: "Close the upload item ubuntu" })).toBeEnabled();
});

it("links ISOs to the install CD and writes short durations", () => {
  expect(createVmHref({ name: "alma 9", content: "iso" })).toBe(
    "/vms/-new?purpose=iso&iso=alma%209",
  );
  const t = ((k: string, o: Record<string, number>) => `${k}:${JSON.stringify(o)}`) as never;
  expect(durationText(40, t)).toBe('uploads.time.s:{"s":40}');
  expect(durationText(3700, t)).toBe('uploads.time.hm:{"h":1,"m":1}');
});

it("uploads in chunks from the dialog when the serving node knows them", async () => {
  const knows = { ...NODE, status: { ...NODE.status, features: ["chunked-uploads"] } };
  mockLists({ "storage-pool": [pool("local")], node: [knows] });
  const opened: unknown[] = [];
  server.use(
    http.get(apiUrl("/uploads"), () => HttpResponse.json({ items: [] })),
    http.post(apiUrl("/uploads"), async ({ request }) => {
      opened.push(await request.json());
      return HttpResponse.json(
        {
          id: "u1",
          name: "ubuntu",
          pool: "local",
          format: "qcow2",
          source_format: "qcow2",
          content: "disk",
          size: 4,
          offset: 0,
          chunk_size: 16,
          direct: false,
          created_at: 0,
          updated_at: 0,
          expires_at: 0,
        },
        { status: 201 },
      );
    }),
    http.get(apiUrl("/uploads/u1"), () =>
      HttpResponse.json({ id: "u1", size: 4, offset: 0, direct: false }),
    ),
  );
  renderPage(<UploadDialog onClose={() => {}} />);
  const dialog = await screen.findByRole("dialog", { name: "Upload a disk image" });
  await userEvent.upload(
    within(dialog).getByLabelText("Image file"),
    new File(["disk"], "Ubuntu.qcow2"),
  );
  await userEvent.selectOptions(within(dialog).getByLabelText("Storage pool"), "local");
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  // 같은 창이 진행을 보인다(닫아도 계속).
  expect(
    await screen.findByText(/The upload goes on if you close this window/),
  ).toBeInTheDocument();
  await vi.waitFor(() => expect(FakeXhr.last?.url).toMatch(/\/uploads\/u1\?offset=0$/));
  expect(opened).toEqual([
    {
      name: "ubuntu",
      pool: "local",
      format: "qcow2",
      source_format: "qcow2",
      size: 4,
      fingerprint: expect.objectContaining({ file_name: "Ubuntu.qcow2" }),
    },
  ]);
  expect(screen.getByRole("button", { name: "Pause ubuntu" })).toBeInTheDocument();
});

it("fills the dialog from an upload of the same file and starts over when the name changes", async () => {
  const knows = { ...NODE, status: { ...NODE.status, features: ["chunked-uploads"] } };
  mockLists({ "storage-pool": [pool("local")], node: [knows] });
  const chosen = new File(["disk"], "Ubuntu.qcow2", { lastModified: 9 });
  const fingerprint = await fingerprintOf(chosen);
  server.use(
    http.get(apiUrl("/uploads"), () =>
      HttpResponse.json({
        items: [
          {
            id: "u7",
            name: "old-ubuntu",
            pool: "local",
            format: "qcow2",
            source_format: "qcow2",
            content: "disk",
            size: 4,
            offset: 2,
            chunk_size: 16,
            direct: false,
            node: "n1",
            fingerprint,
            created_at: 0,
            updated_at: 0,
            expires_at: 0,
          },
        ],
      }),
    ),
  );
  renderPage(<UploadDialog onClose={() => {}} />);
  const dialog = await screen.findByRole("dialog", { name: "Upload a disk image" });
  await vi.waitFor(() => expect(uploadItems()).toHaveLength(1));
  await userEvent.upload(within(dialog).getByLabelText("Image file"), chosen);
  await vi.waitFor(() => expect(within(dialog).getByLabelText("Name")).toHaveValue("old-ubuntu"));
  expect(within(dialog).getByText(/The upload resumes from there/)).toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Resume upload" })).toBeInTheDocument();
  await userEvent.type(within(dialog).getByLabelText("Name"), "-2");
  expect(within(dialog).getByText(/You changed the name, pool or format/)).toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Upload" })).toBeInTheDocument();
});
