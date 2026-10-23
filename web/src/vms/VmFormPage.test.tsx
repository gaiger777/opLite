import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { List } from "@/api/live";
import type { Task } from "@/api/types";
import { NODE, task, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmCreatePage, VmEditPage } from "@/vms/VmFormPage";

it("creates a new volume first, then the VM", async () => {
  mockLists({ node: [NODE], "storage-pool": [{ id: "p1", name: "local" }] });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl("/volumes"), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t-vol", "volume-create", "v1"), { status: 202 });
    }),
    http.post(apiUrl("/vms"), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(vm("web", "stopped"), { status: 201 });
    }),
  );
  const { queryClient } = renderPage(<VmCreatePage />, { path: "/vms/new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  await user.click(screen.getByRole("button", { name: "New empty volume" }));
  const disk = screen.getByRole("group", { name: "Disk 1" });
  expect(within(disk).getByLabelText("New volume")).toHaveValue("web-disk0");
  await user.selectOptions(within(disk).getByLabelText("Storage pool"), "local");
  await user.click(screen.getByRole("button", { name: "Create" }));

  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]).toEqual({
    name: "web-disk0",
    spec: { pool: "local", size_bytes: 16 * 1024 ** 3, format: "qcow2" },
  });
  // watch가 Task 완료를 알려 줄 때까지 VM은 만들지 않는다.
  const done = {
    ...task("t-vol", "volume-create", "v1"),
    status: { state: "succeeded", error: null },
  };
  queryClient.setQueryData<List<Task>>(["list", "task"], { revision: 2, items: [done as Task] });
  expect(await screen.findByText("moved")).toBeInTheDocument();
  expect(bodies[1]).toMatchObject({
    name: "web",
    spec: {
      machine: "q35",
      cpu: { sockets: 1, cores: 2, model: "host" },
      disks: [{ volume: "web-disk0", bus: "virtio-blk", boot_order: 1 }],
      nics: [{ network: "user", model: "virtio", mac: null }],
      node: null,
      power: "running",
    },
  });
});

it("shows input errors without calling the API", async () => {
  mockLists({ node: [NODE] });
  renderPage(<VmCreatePage />, { path: "/vms/new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "Bad Name");
  await user.clear(screen.getByLabelText("Memory (MiB)"));
  await user.type(screen.getByLabelText("Memory (MiB)"), "32");
  await user.click(screen.getByRole("button", { name: "Create" }));
  expect(screen.getByText(/lowercase letters/)).toBeInTheDocument();
  expect(screen.getByText("At least 64 MiB.")).toBeInTheDocument();
});

it("saves with the revision it read and offers to reload on a conflict", async () => {
  const web = vm("web", "running");
  mockLists({ vm: [web], node: [NODE] });
  let ifMatch: string | null = null;
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), ({ request }) => {
      ifMatch = request.headers.get("if-match");
      return HttpResponse.json(
        { code: "revision_mismatch", message: "expected 1, found 2" },
        { status: 412 },
      );
    }),
  );
  const { queryClient } = renderPage(<VmEditPage />, {
    path: "/vms/:name/edit",
    url: "/vms/web/edit",
  });
  const user = userEvent.setup();
  // 실행 중이면 노드는 바꿀 수 없다.
  expect(await screen.findByLabelText("Node")).toBeDisabled();
  await user.clear(screen.getByLabelText("Memory (MiB)"));
  await user.type(screen.getByLabelText("Memory (MiB)"), "4096");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Someone else changed it first.");
  expect(ifMatch).toBe("1");

  const changed = { ...web, revision: 2, spec: { ...web.spec, memory_mib: 8192 } };
  queryClient.setQueryData(["list", "vm"], { revision: 2, items: [changed] });
  await user.click(screen.getByRole("button", { name: "Reload" }));
  expect(screen.getByLabelText("Memory (MiB)")).toHaveValue(8192);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});
