import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { VmExport } from "@/exports/exports";
import { ExportsPage } from "@/exports/ExportsPage";
import { startDownload } from "@/fileRestore/download";
import { resetTasks } from "@/tasks/tasks";
import { expectNoAxeViolations } from "@/test/axe";
import { NODE, pool, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

vi.mock("@/fileRestore/download", () => ({ startDownload: vi.fn() }));

afterEach(() => {
  toast.dismiss();
  resetTasks();
  vi.mocked(startDownload).mockClear();
});

const WEB = vm("web", "stopped");
const RUNNING = vm("app", "running");
const OPERATOR = { ...ADMIN, role: "operator" as const };

function exportOf(name: string, n: number, extra: Partial<VmExport["status"]> = {}): VmExport {
  return {
    id: `019a0f3c-0000-7000-8075-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: { vm: WEB.id, pool: "nfs1", node: "n1", hw_version: 14 },
    status: { phase: "ready", files: [], ova_bytes: 3 * 1024 ** 3, warnings: [], ...extra },
  };
}

const READY = exportOf("web-20261125-080000", 1, {
  warnings: ["vTPM is not exported", "CD-ROM drives are not exported"],
});
const WRITING = exportOf("web-20261125-090000", 2, { phase: "writing", ova_bytes: 0 });

describe("VM menu", () => {
  it("offers Export as OVA for a stopped VM and opens it in the exports view", async () => {
    const user = userEvent.setup();
    mockLists({ vm: [WEB], node: [NODE] });
    const { router } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    await user.click(await screen.findByRole("button", { name: "Actions for web" }));
    await user.click(screen.getByRole("menuitem", { name: "Export as OVA…" }));
    await waitFor(() =>
      expect(`${router.state.location.pathname}${router.state.location.search}`).toBe(
        "/vm-exports?export=web",
      ),
    );
  });

  it("starts an export task from the dialog the exports view opens", async () => {
    const user = userEvent.setup();
    mockLists({
      vm: [WEB],
      node: [NODE],
      "storage-pool": [
        pool("local"),
        pool("nfs1", { backend: { kind: "nfs", server: "nas", export: "/x" } as never }),
        pool("ssd", { backend: { kind: "lvm-thin", vg: "vg0", pool: "tp" } as never }),
      ],
    });
    let body: unknown;
    server.use(
      http.post(apiUrl("/vms/:key/export"), async ({ request }) => {
        body = await request.json();
        return HttpResponse.json(
          {
            export: exportOf("web-x", 9, { phase: "pending" }),
            task: task("t9", "vm-export", WEB.id),
          },
          { status: 202 },
        );
      }),
      http.get(apiUrl("/vm-exports"), () => HttpResponse.json({ revision: 1, items: [] })),
    );
    const { router } = renderPage(<ExportsPage />, {
      path: "/vm-exports",
      url: "/vm-exports?export=web",
    });
    const dialog = await screen.findByRole("dialog", { name: "Export web as OVA" });
    // 파일을 둘 수 있는 풀(dir·NFS)만
    const pools = within(dialog).getByLabelText("Pool");
    await within(pools).findByRole("option", { name: "nfs1 (nfs)" });
    expect(
      within(pools)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["local (dir)", "nfs1 (nfs)"]);
    await user.selectOptions(pools, "nfs1");
    await user.clear(within(dialog).getByLabelText("Hardware version"));
    await user.type(within(dialog).getByLabelText("Hardware version"), "5");
    await user.click(within(dialog).getByRole("button", { name: "Export" }));
    expect(within(dialog).getByText("Enter a number from 7 to 21.")).toBeInTheDocument();
    await user.clear(within(dialog).getByLabelText("Hardware version"));
    await user.type(within(dialog).getByLabelText("Hardware version"), "19");
    await user.click(within(dialog).getByLabelText("Keep MAC addresses"));
    await user.click(within(dialog).getByRole("button", { name: "Export" }));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
    expect(body).toEqual({ pool: "nfs1", hw_version: 19, keep_macs: true });
    expect(await screen.findByText("Exporting web-x.")).toBeInTheDocument();
    // 닫으면 주소의 `export`를 지운다(다시 열지 않게)
    expect(router.state.location.search).toBe("");
  });

  it("explains why a running VM cannot be exported", async () => {
    const user = userEvent.setup();
    mockLists({ vm: [RUNNING], node: [NODE] });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/app" });
    await user.click(await screen.findByRole("button", { name: "Actions for app" }));
    const item = screen.getByRole("menuitem", { name: "Export as OVA…" });
    expect(item).toHaveAttribute("aria-disabled", "true");
    expect(item).toHaveAccessibleDescription("The VM is running.");
  });

  it("shows the server's reason in words when the pool is too small", async () => {
    const user = userEvent.setup();
    mockLists({ vm: [WEB], node: [NODE], "storage-pool": [pool("local")] });
    server.use(
      http.get(apiUrl("/vm-exports"), () => HttpResponse.json({ revision: 1, items: [] })),
      http.post(apiUrl("/vms/:key/export"), () =>
        HttpResponse.json(
          { code: "insufficient_storage", message: "needs 9 GiB, 2 GiB free" },
          { status: 507 },
        ),
      ),
    );
    renderPage(<ExportsPage />, { path: "/vm-exports", url: "/vm-exports?export=web" });
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Export" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "The pool does not have room for the disks: needs 9 GiB, 2 GiB free",
    );
  });

  it("hides the export from users without vm.export (operators)", async () => {
    const user = userEvent.setup();
    mockLists({ vm: [WEB], node: [NODE] });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web", as: OPERATOR });
    await user.click(await screen.findByRole("button", { name: "Actions for web" }));
    expect(screen.getByRole("menuitem", { name: "Edit" })).toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: "Export as OVA…" })).not.toBeInTheDocument();
  });
});

describe("exports view", () => {
  function setup() {
    mockLists({ vm: [WEB] });
    const deleted: string[] = [];
    server.use(
      http.get(apiUrl("/vm-exports"), () =>
        HttpResponse.json({ revision: 1, items: [READY, WRITING] }),
      ),
      http.post(apiUrl("/vm-exports/:key/download-link"), ({ params }) =>
        HttpResponse.json({
          url: `/vm-exports/${String(params.key)}/download/tok`,
          expires_in: 600,
        }),
      ),
      http.delete(apiUrl("/vm-exports/:key"), ({ params }) => {
        deleted.push(String(params.key));
        return HttpResponse.json(task("t3", "vm-export-delete", String(params.key)), {
          status: 202,
        });
      }),
    );
    return deleted;
  }

  it("lists exports with size, state and warnings and passes axe", async () => {
    setup();
    const { container } = renderPage(<ExportsPage />, { path: "/vm-exports" });
    const table = await screen.findByRole("table", { name: "Exports" });
    const ready = within(table).getByRole("row", { name: /080000/ });
    expect(ready).toHaveTextContent("3 GiB");
    expect(ready).toHaveTextContent("Ready");
    expect(ready).toHaveTextContent("2 warnings");
    expect(within(ready).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
    const writing = within(table).getByRole("row", { name: /090000/ });
    expect(writing).toHaveTextContent("Writing");
    expect(within(writing).queryByRole("button", { name: /^Download/ })).not.toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it("downloads through a signed link the browser follows", async () => {
    const user = userEvent.setup();
    setup();
    renderPage(<ExportsPage />, { path: "/vm-exports" });
    await user.click(await screen.findByRole("button", { name: "Download web-20261125-080000" }));
    await waitFor(() =>
      expect(startDownload).toHaveBeenCalledWith(`/api/v1/vm-exports/${READY.id}/download/tok`),
    );
  });

  it("deletes an export through a task", async () => {
    const user = userEvent.setup();
    const deleted = setup();
    renderPage(<ExportsPage />, { path: "/vm-exports" });
    await user.click(await screen.findByRole("button", { name: "Delete web-20261125-090000" }));
    await user.click(
      within(screen.getByRole("alertdialog")).getByRole("button", { name: "Delete" }),
    );
    await waitFor(() => expect(deleted).toEqual([WRITING.id]));
  });

  it("shows no download or delete to users without vm.export", async () => {
    setup();
    renderPage(<ExportsPage />, { path: "/vm-exports", as: OPERATOR });
    await screen.findByRole("table", { name: "Exports" });
    expect(screen.queryByRole("button", { name: /^Download/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
  });
});
