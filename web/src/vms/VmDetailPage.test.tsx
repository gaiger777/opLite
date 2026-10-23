import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { NODE, task, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

const web = vm("web", "running", {
  spec: {
    ...vm("web", "running").spec,
    disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }],
    nics: [{ network: "lan", model: "virtio", mac: "52:54:00:12:34:56" }],
  },
});

it("shows a VM, its hardware and its tasks", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    task: [
      {
        ...task("019a0f3c-8a10-7000-8000-000000000009", "vm-start", web.id),
        status: { state: "succeeded", error: null },
      },
    ],
  });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(await screen.findByRole("heading", { name: "web" })).toBeInTheDocument();
  expect(await screen.findByText("node-a")).toBeInTheDocument();
  expect(screen.getByText("1 × 2 cores (host)")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Start" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Delete" })).toBeDisabled();

  await userEvent.click(screen.getByRole("tab", { name: "Hardware" }));
  expect(screen.getByText("web-root")).toBeInTheDocument();
  expect(screen.getByText("52:54:00:12:34:56")).toBeInTheDocument();

  await userEvent.click(screen.getByRole("tab", { name: "Tasks" }));
  expect(await screen.findByText("Start web")).toBeInTheDocument();
});

it("force stops after confirming", async () => {
  mockLists({ vm: [web], node: [NODE] });
  let mode: string | null = null;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/actions/stop`), ({ request }) => {
      mode = new URL(request.url).searchParams.get("mode");
      return HttpResponse.json(task("t1", "vm-force-stop", web.id), { status: 202 });
    }),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  await userEvent.click(await screen.findByRole("button", { name: "Force stop" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Force stop" }));
  expect(await screen.findByText("Force stop web")).toBeInTheDocument();
  expect(mode).toBe("force");
});

it("says when the VM does not exist", async () => {
  mockLists({ vm: [] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/gone" });
  expect(await screen.findByRole("heading", { name: "There is no VM gone" })).toBeInTheDocument();
});

it("names its hardware tables and says when they are empty", async () => {
  const bare = vm("bare", "stopped");
  mockLists({ vm: [bare], node: [NODE] });
  const { router } = renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: "/vms/bare?from=list",
  });
  await userEvent.click(await screen.findByRole("tab", { name: "Hardware" }));
  // 탭을 바꿔도 다른 쿼리 값은 남는다.
  expect(router.state.location.search).toBe("?from=list&tab=hardware");
  expect(within(screen.getByRole("table", { name: "Disks" })).getByText("No disks.")).toBeVisible();
  expect(
    within(screen.getByRole("table", { name: "Network interfaces" })).getByText(
      "No network interfaces.",
    ),
  ).toBeVisible();
  await userEvent.click(screen.getByRole("tab", { name: "Tasks" }));
  expect(screen.getByRole("table", { name: "Tasks" })).toBeInTheDocument();
});

it("shows the pinned node before the node it last ran on", async () => {
  const moved = vm("moved", "running", {
    spec: { ...vm("moved", "running").spec, node: "n2" },
  });
  mockLists({ vm: [moved], node: [NODE, { ...NODE, id: "n2", name: "node-b" }] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/moved" });
  expect(await screen.findByText("node-b")).toBeInTheDocument();
  expect(screen.queryByText("node-a")).not.toBeInTheDocument();
});

it("says why the VM could not be read", async () => {
  mockLists({ node: [NODE] });
  server.use(
    http.get(apiUrl("/vms"), () =>
      HttpResponse.json({ code: "forbidden", message: "permission denied" }, { status: 403 }),
    ),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
});

it("keeps the delete dialog open while the request runs", async () => {
  const db = vm("db", "stopped");
  mockLists({ vm: [db], node: [NODE] });
  let finish = () => {};
  server.use(
    http.delete(
      apiUrl(`/vms/${db.id}`),
      () =>
        new Promise<Response>((resolve) => {
          finish = () => resolve(new HttpResponse(null, { status: 204 }));
        }),
    ),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/db" });
  await userEvent.click(await screen.findByRole("button", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeDisabled();
  await userEvent.keyboard("{Escape}");
  expect(screen.getByRole("alertdialog")).toBeInTheDocument();
  finish();
  expect(await screen.findByText("moved")).toBeInTheDocument();
});
