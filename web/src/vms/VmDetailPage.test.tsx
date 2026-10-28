import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { NODE, snapshot, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
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
  expect(screen.getByRole("button", { name: "Start" })).toHaveAttribute("aria-disabled", "true");
  expect(screen.getByRole("button", { name: "Start" })).toHaveAccessibleDescription(
    "The VM is running.",
  );
  expect(screen.getByRole("button", { name: "Delete" })).toHaveAttribute("aria-disabled", "true");

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

it("stops deleting snapshots when the page goes away", async () => {
  const db = vm("db", "stopped");
  mockLists({ vm: [db], node: [NODE], snapshot: [snapshot("a", db, 1), snapshot("b", db, 2)] });
  const removed: string[] = [];
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/vms/${db.id}/snapshots/:name`), ({ params }) => {
      removed.push(String(params.name));
      return HttpResponse.json(task(`t-${removed.length}`, "snapshot-delete", db.id), {
        status: 202,
      });
    }),
    http.delete(apiUrl(`/vms/${db.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { queryClient, unmount } = renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: "/vms/db",
  });
  await userEvent.click(await screen.findByRole("button", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  await userEvent.click(
    within(dialog).getByRole("checkbox", { name: "Also delete all 2 of its snapshots" }),
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  await waitFor(() => expect(removed).toHaveLength(1));
  await waitFor(() => expect(queryClient.getQueryCache().hasListeners()).toBe(true));
  unmount();
  // 떠난 뒤 Task가 끝나도 다음 스냅샷과 VM을 지우지 않는다.
  const done = { ...task("t-1", "snapshot-delete", db.id), status: { state: "succeeded" } };
  queryClient.setQueryData(["list", "task"], { revision: 2, items: [done] });
  await new Promise((r) => setTimeout(r, 50));
  expect(removed).toHaveLength(1);
  expect(deleted).toBe(false);
});

it("hides the console from users who cannot operate VMs", async () => {
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: "/vms/web?tab=console",
    as: { ...ADMIN, role: "viewer" },
  });
  expect(await screen.findByRole("tab", { name: "Summary" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  expect(screen.queryByRole("tab", { name: "Console" })).not.toBeInTheDocument();
});

it("blocks force stop while a snapshot is being reverted and says why", async () => {
  const revert = {
    ...task("t-revert", "snapshot-revert", web.id),
    status: { state: "running" as const, error: null },
  };
  mockLists({ vm: [web], node: [NODE], task: [revert] });
  const { queryClient } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  const button = await screen.findByRole("button", { name: "Force stop" });
  await waitFor(() => expect(button).toHaveAttribute("aria-disabled", "true"));
  expect(button).toHaveAccessibleDescription("Not available while a snapshot is being reverted.");
  // 되돌리기가 끝나면 다시 켠다.
  queryClient.setQueryData(["list", "task"], {
    revision: 2,
    items: [{ ...revert, status: { state: "succeeded", error: null } }],
  });
  await waitFor(() => expect(button).not.toHaveAttribute("aria-disabled"));
  expect(button).not.toHaveAccessibleDescription();
  expect(
    screen.queryByText("Not available while a snapshot is being reverted."),
  ).not.toBeInTheDocument();
});

it("does not blame a revert for a VM that is already off", async () => {
  const db = vm("db", "stopped");
  const revert = {
    ...task("t-revert", "snapshot-revert", db.id),
    status: { state: "running" as const, error: null },
  };
  mockLists({ vm: [db], node: [NODE], task: [revert] });
  const { queryClient } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/db" });
  const button = await screen.findByRole("button", { name: "Force stop" });
  await waitFor(() => expect(queryClient.getQueryData(["list", "task"])).toBeDefined());
  expect(button).toHaveAttribute("aria-disabled", "true");
  expect(button).toHaveAccessibleDescription("The VM is stopped.");
});

it("keeps the same console link when the VM stops and blocks it with a reason", async () => {
  mockLists({ vm: [web], node: [NODE] });
  const { queryClient } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  const link = await screen.findByRole("link", { name: "Open console" });
  expect(link).not.toHaveAttribute("aria-disabled");
  link.focus();
  // 멈추면 같은 링크가 막히고 이유를 읽는다. 포커스는 그대로다.
  queryClient.setQueryData(["list", "vm"], {
    revision: 2,
    items: [
      {
        ...web,
        spec: { ...web.spec, power: "stopped" },
        status: { ...web.status, phase: "stopped" },
      },
    ],
  });
  await waitFor(() => expect(link).toHaveAttribute("aria-disabled", "true"));
  expect(link).toHaveAccessibleDescription("The VM is stopped.");
  expect(link).toHaveFocus();
});
