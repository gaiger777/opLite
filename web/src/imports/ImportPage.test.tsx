import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Network, VmImport } from "@/api/types";
import { ImportPage } from "@/imports/ImportPage";
import { NODE, pool, task, vm, vmImport, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const lan = { id: "019a0f3c-0000-7000-8004-000000000001", name: "lan" } as Network;

/** 원본 NIC가 둘이고 그중 "Backup"은 이름이 같은 opLite 네트워크가 없는 가져오기 */
const web01 = vmImport("web01", 1, {
  source_vm: {
    ...vmImport("web01", 1).status.source_vm!,
    nics: [
      { network: "VM Network", adapter: "vmxnet3", mac: "00:50:56:01:02:03" },
      { network: "Backup", adapter: "e1000", mac: null },
    ],
  },
  proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
  error: "checksum mismatch for web01-disk1.vmdk",
});

function open(imp: VmImport, as = ADMIN) {
  return renderPage(<ImportPage />, { path: "/imports/:name", url: `/imports/${imp.name}`, as });
}

it("shows the source VM, its warnings and the last error", async () => {
  mockLists({ "vm-import": [web01], node: [NODE] });
  open(web01);
  expect(await screen.findByRole("heading", { name: "web01" })).toBeInTheDocument();
  expect(screen.getByText("Ready")).toBeInTheDocument();
  // 페이지에 남는 오류라 알림(alert)이 아니다.
  expect(
    screen.getByText("The last run failed: checksum mismatch for web01-disk1.vmdk"),
  ).toHaveAttribute("role", "status");
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(await screen.findByText("web01.ova (3 GiB), node-a")).toBeInTheDocument();
  expect(screen.getByText("Web 01")).toBeInTheDocument();
  expect(screen.getByText("2 vCPU, 2 per socket")).toBeInTheDocument();
  const disks = screen.getByRole("table", { name: "Disks" });
  expect(within(disks).getAllByRole("row")[1]).toHaveTextContent("disk0web01-disk1.vmdk16 GiBscsi");
  const nics = screen.getByRole("table", { name: "Network interfaces" });
  expect(within(nics).getAllByRole("row")[1]).toHaveTextContent(
    "VM Networkvmxnet300:50:56:01:02:03",
  );
  expect(within(nics).getAllByRole("row")[2]).toHaveTextContent("Backupe1000—");
  expect(screen.getByRole("region", { name: "Warnings" })).toHaveTextContent(
    "UEFI boot entries (NVRAM) are not imported",
  );
});

it("runs the import with the reviewed settings after every network is mapped", async () => {
  mockLists({
    "vm-import": [web01],
    node: [NODE],
    network: [lan],
    "storage-pool": [pool("local"), pool("far", { nodes: ["n2"] })],
    vm: [vm("web01", "stopped")],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl("/vm-imports/:key/run"), async ({ request, params }) => {
      expect(params.key).toBe(web01.id);
      body = await request.json();
      return HttpResponse.json(task("t1", "vm-import", "new-vm"), { status: 202 });
    }),
  );
  open(web01);
  // 제안으로 채운다. VM 이름은 가져오기 이름이다.
  expect(await screen.findByLabelText("New VM name")).toHaveValue("web01");
  expect(screen.getByLabelText("Sockets")).toHaveValue("1");
  expect(screen.getByLabelText("Cores per socket")).toHaveValue("2");
  expect(screen.getByLabelText("Memory (MiB)")).toHaveValue("1024");
  expect(screen.getByLabelText("Firmware")).toHaveValue("uefi");
  expect(screen.getByLabelText("Disk bus")).toHaveValue("virtio-blk");
  expect(screen.getByLabelText("NIC model")).toHaveValue("virtio");
  // 네트워크 목록을 읽어야 고른 네트워크가 보인다.
  await screen.findAllByRole("option", { name: "lan" });
  expect(screen.getByLabelText("VM Network")).toHaveValue("lan");
  expect(screen.getByLabelText("Backup")).toHaveValue("");
  expect(screen.getByLabelText("Keep the source MAC addresses")).toBeChecked();
  // OVA가 있는 노드(n1)에서 쓸 수 있는 풀만 고른다.
  await screen.findByRole("option", { name: "local" });
  expect(
    within(screen.getByLabelText("Storage pool for the disks"))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["—", "local"]);

  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(screen.getByText("A VM with this name already exists.")).toBeVisible();
  expect(screen.getByText("Choose a storage pool.")).toBeVisible();
  expect(screen.getByText("Choose a network.")).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.clear(screen.getByLabelText("New VM name"));
  await userEvent.type(screen.getByLabelText("New VM name"), "web");
  await userEvent.selectOptions(screen.getByLabelText("Storage pool for the disks"), "local");
  await userEvent.selectOptions(screen.getByLabelText("Backup"), "user");
  await userEvent.clear(screen.getByLabelText("Memory (MiB)"));
  await userEvent.type(screen.getByLabelText("Memory (MiB)"), "2048");
  await userEvent.selectOptions(screen.getByLabelText("Disk bus"), "sata");
  await userEvent.click(screen.getByLabelText("Keep the source MAC addresses"));
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(await screen.findByText("Import VM web")).toBeInTheDocument();
  expect(body).toEqual({
    name: "web",
    pool: "local",
    networks: { "VM Network": "lan", Backup: "user" },
    disk_bus: "sata",
    nic_model: "virtio",
    firmware: "uefi",
    sockets: 1,
    cores: 2,
    memory_mib: 2048,
    keep_macs: false,
  });
});

it("checks the volume names the import will create", async () => {
  const imp = vmImport("web01", 1, {
    proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
  });
  mockLists({
    "vm-import": [imp],
    node: [NODE],
    network: [lan],
    "storage-pool": [pool("local")],
    volume: [volume("web01-disk0", "local")],
  });
  open(imp);
  await userEvent.selectOptions(
    await screen.findByLabelText("Storage pool for the disks"),
    await screen.findByRole("option", { name: "local" }),
  );
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(
    screen.getByText("A volume this import would create (NAME-disk0, …) already exists."),
  ).toBeVisible();
});

it("explains server errors in terms of the node holding the OVA", async () => {
  const imp = vmImport("web01", 1, {
    proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
  });
  mockLists({ "vm-import": [imp], node: [NODE], network: [lan], "storage-pool": [pool("local")] });
  let code = "pool_not_on_node";
  server.use(
    http.post(apiUrl("/vm-imports/:key/run"), () =>
      HttpResponse.json({ code, message: "nope" }, { status: 409 }),
    ),
  );
  open(imp);
  await userEvent.selectOptions(
    await screen.findByLabelText("Storage pool for the disks"),
    await screen.findByRole("option", { name: "local" }),
  );
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(
    await screen.findByText("This storage pool is not available on the node holding the OVA."),
  ).toBeVisible();
  code = "node_offline";
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The node holding the OVA is offline or has left the cluster.",
  );
});

it("keeps the form after a failed run and does not send it twice", async () => {
  const imp = vmImport("web01", 1, {
    proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
  });
  mockLists({ "vm-import": [imp], node: [NODE], network: [lan], "storage-pool": [pool("local")] });
  let sent = 0;
  server.use(
    http.post(apiUrl("/vm-imports/:key/run"), () => {
      sent += 1;
      return HttpResponse.json(task("t1", "vm-import", "new-vm"), { status: 202 });
    }),
  );
  const { queryClient } = open(imp);
  await userEvent.selectOptions(
    await screen.findByLabelText("Storage pool for the disks"),
    await screen.findByRole("option", { name: "local" }),
  );
  await userEvent.clear(screen.getByLabelText("New VM name"));
  await userEvent.type(screen.getByLabelText("New VM name"), "web");
  await userEvent.clear(screen.getByLabelText("Memory (MiB)"));
  await userEvent.type(screen.getByLabelText("Memory (MiB)"), "2048");
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(await screen.findByText("Import VM web")).toBeInTheDocument();
  // 가져오기가 바뀔 때까지 다시 보내거나 지우지 못한다.
  expect(screen.getByRole("button", { name: "Import" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Delete import web01" })).toBeDisabled();
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(sent).toBe(1);

  const update = (revision: number, status: Partial<VmImport["status"]>) =>
    queryClient.setQueryData(["list", "vm-import"], {
      revision,
      items: [{ ...imp, revision, status: { ...imp.status, ...status } }],
    });
  update(2, { phase: "importing" });
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Import" })).not.toBeInTheDocument(),
  );
  // 실패해 `ready`로 돌아오면 고친 값이 그대로 있고 다시 보낼 수 있다.
  update(3, { phase: "ready", error: "cannot create VM" });
  expect(await screen.findByLabelText("New VM name")).toHaveValue("web");
  expect(screen.getByLabelText("Memory (MiB)")).toHaveValue("2048");
  expect(screen.getByLabelText("Storage pool for the disks")).toHaveValue("local");
  expect(screen.getByRole("button", { name: "Import" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Delete import web01" })).toBeEnabled();
});

it("shows taken names on the name field and explains MAC conflicts", async () => {
  const imp = vmImport("web01", 1, {
    proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
  });
  mockLists({ "vm-import": [imp], node: [NODE], network: [lan], "storage-pool": [pool("local")] });
  let reply = { code: "name_taken", message: 'volume "web01-disk0" already exists' };
  server.use(
    http.post(apiUrl("/vm-imports/:key/run"), () => HttpResponse.json(reply, { status: 409 })),
  );
  open(imp);
  await userEvent.selectOptions(
    await screen.findByLabelText("Storage pool for the disks"),
    await screen.findByRole("option", { name: "local" }),
  );
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  // 서버 메시지는 번역 키가 아니라 정해진 문장에 끼운다.
  expect(await screen.findByText('Name in use: volume "web01-disk0" already exists')).toBeVisible();
  expect(screen.getByLabelText("New VM name")).toHaveAccessibleDescription(
    'Name in use: volume "web01-disk0" already exists',
  );
  // 실패하면 다시 보낼 수 있다.
  expect(screen.getByRole("button", { name: "Import" })).toBeEnabled();
  reply = { code: "mac_in_use", message: "MAC address 00:50:56:01:02:03 is already in use" };
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    'Another VM uses a source MAC address. Turn off "Keep the source MAC addresses" to give the interfaces new MAC addresses.',
  );
});

it("links the imported VM and offers no form once done or while importing", async () => {
  const web = vm("web", "stopped");
  const done = vmImport("web01", 1, { phase: "done", vm: web.id, warnings: [] });
  const busy = vmImport("db01", 2, { phase: "importing" });
  mockLists({ "vm-import": [done, busy], vm: [web] });
  open(done);
  expect(await screen.findByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(screen.getByText("Done")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Import" })).not.toBeInTheDocument();
  expect(screen.queryByRole("region", { name: "Warnings" })).not.toBeInTheDocument();
});

it("cannot run while importing and lets the server decide on deleting", async () => {
  const busy = vmImport("db01", 2, { phase: "importing" });
  mockLists({ "vm-import": [busy, web01], node: [NODE] });
  server.use(
    http.delete(apiUrl("/vm-imports/:key"), () =>
      HttpResponse.json({ code: "import_busy", message: "busy" }, { status: 409 }),
    ),
  );
  open(busy);
  expect(await screen.findByText("Importing. The progress is on the Tasks page.")).toBeVisible();
  expect(screen.queryByRole("button", { name: "Import" })).not.toBeInTheDocument();
  // 실행 Task가 이미 끝났으면(원본 노드가 없어졌다) `importing`이어도 지울 수 있다.
  await userEvent.click(screen.getByRole("button", { name: "Delete import db01" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete import db01?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(
    await within(dialog).findByText(
      "The import is running, or another task on it has not finished.",
    ),
  ).toBeVisible();
});

it("is read-only for viewers", async () => {
  mockLists({ "vm-import": [web01] });
  open(web01, { ...ADMIN, role: "viewer" });
  expect(await screen.findByRole("heading", { name: "web01" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Import" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete import web01" })).not.toBeInTheDocument();
});

it("deletes the import and goes back to the list", async () => {
  mockLists({ "vm-import": [web01], node: [NODE] });
  let offline = true;
  server.use(
    http.delete(apiUrl("/vm-imports/:key"), () =>
      offline
        ? HttpResponse.json({ code: "node_offline", message: "offline" }, { status: 409 })
        : HttpResponse.json(task("t2", "vm-import-delete", web01.id), { status: 202 }),
    ),
  );
  open(web01);
  await userEvent.click(await screen.findByRole("button", { name: "Delete import web01" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete import web01?" });
  expect(dialog).toHaveTextContent("Deletes the uploaded OVA from node-a.");
  // 원본 노드가 없으면 웹에서는 지울 수 없다. 리소스만 지우는 명령을 알린다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(
    await within(dialog).findByText(
      "The node holding the OVA is offline or has left the cluster. To forget the import and leave its files on that node, run oplctl import delete web01 --force.",
    ),
  ).toBeVisible();
  offline = false;
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("moved")).toBeInTheDocument();
  expect(await screen.findByText("Delete import web01")).toBeInTheDocument();
});

it("says when the import does not exist", async () => {
  mockLists({ "vm-import": [] });
  open(vmImport("nope", 9));
  expect(await screen.findByText("Import nope not found")).toBeInTheDocument();
});
