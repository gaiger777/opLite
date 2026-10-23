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
