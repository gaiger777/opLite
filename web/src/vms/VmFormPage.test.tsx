import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { List } from "@/api/live";
import type { Task } from "@/api/types";
import { haGroup, NODE, pool, securityGroup, task, vm, volume } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmCreatePage, VmEditPage } from "@/vms/VmFormPage";

it("creates a new volume first, then the VM", async () => {
  mockLists({ node: [NODE], "storage-pool": [pool("local")] });
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
  const { queryClient } = renderPage(<VmCreatePage />, { path: "/vms/-new" });
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
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "Bad Name");
  await user.clear(screen.getByLabelText("Memory (MiB)"));
  await user.type(screen.getByLabelText("Memory (MiB)"), "32");
  await user.click(screen.getByRole("button", { name: "Create" }));
  expect(screen.getByText(/lowercase letters/)).toBeInTheDocument();
  expect(screen.getByText("At least 64 MiB.")).toBeInTheDocument();
});

it("follows the chosen node's architecture until a machine is picked", async () => {
  const arm = { ...NODE, status: { ...NODE.status, arch: "aarch64" as const } };
  const x86 = {
    ...NODE,
    id: "n2",
    name: "node-b",
    status: { ...NODE.status, arch: "x86_64" as const },
  };
  mockLists({ node: [arm, x86] });
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  const machine = await screen.findByLabelText("Machine type");
  // 아키텍처가 섞여 있으면 q35다.
  expect(machine).toHaveValue("q35");
  await user.selectOptions(screen.getByLabelText("Node"), "node-a");
  expect(machine).toHaveValue("virt");
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(machine).toHaveValue("q35");
  // 직접 고르면 노드를 바꿔도 그대로다.
  await user.selectOptions(machine, "virt");
  await user.selectOptions(screen.getByLabelText("Node"), "");
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(machine).toHaveValue("virt");
});

it("does not offer witness nodes", async () => {
  const arm = { ...NODE, status: { ...NODE.status, arch: "aarch64" as const } };
  const witness = {
    ...NODE,
    id: "n2",
    name: "node-w",
    spec: { ...NODE.spec, witness: true },
    status: { ...NODE.status, arch: "x86_64" as const },
  };
  mockLists({ node: [arm, witness] });
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  // 위트니스의 아키텍처는 머신 기본값에 섞이지 않는다.
  expect(await screen.findByLabelText("Machine type")).toHaveValue("virt");
  expect(screen.getByRole("option", { name: "node-a" })).toBeInTheDocument();
  expect(screen.queryByRole("option", { name: "node-w" })).not.toBeInTheDocument();
});

it("keeps the machine of an edited VM when its node changes", async () => {
  const arm = { ...NODE, status: { ...NODE.status, arch: "aarch64" as const } };
  const x86 = {
    ...NODE,
    id: "n2",
    name: "node-b",
    status: { ...NODE.status, arch: "x86_64" as const },
  };
  mockLists({ vm: [vm("web", "stopped")], node: [arm, x86] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const machine = await screen.findByLabelText("Machine type");
  expect(machine).toHaveValue("virt");
  await screen.findByRole("option", { name: "node-b" });
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(screen.getByLabelText("Node")).toHaveValue("n2");
  expect(machine).toHaveValue("virt");
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

it("locks the node of a VM that should run even if it has failed", async () => {
  const web = vm("web", "failed", { spec: { ...vm("web", "failed").spec, power: "running" } });
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  expect(await screen.findByLabelText("Node")).toBeDisabled();
  expect(screen.queryByRole("button", { name: "Attach a volume" })).not.toBeInTheDocument();
});

it("keeps the interfaces and MAC addresses of a running VM", async () => {
  const web = vm("web", "running", {
    spec: {
      ...vm("web", "running").spec,
      nics: [{ network: "user", model: "virtio", mac: "52:54:00:00:00:01" }],
    },
  });
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  expect(await screen.findByLabelText("MAC address")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Remove interface 1" })).toBeDisabled();
  expect(screen.queryByRole("button", { name: "Add an interface" })).not.toBeInTheDocument();
  expect(
    screen.getByText(
      "Interfaces cannot be added or removed, and their MAC addresses cannot change, while the VM is running.",
    ),
  ).toBeInTheDocument();
  // 네트워크와 모델은 바꿀 수 있다(다음 시작부터).
  expect(screen.getByLabelText("Network")).toBeEnabled();
});

it("keeps created volumes when the VM cannot be created, and does not recreate them", async () => {
  mockLists({ node: [NODE], "storage-pool": [pool("local")] });
  const volumes: string[] = [];
  let vmPosts = 0;
  server.use(
    http.post(apiUrl("/volumes"), async ({ request }) => {
      volumes.push(((await request.json()) as { name: string }).name);
      return HttpResponse.json(task("t-vol2", "volume-create", "v1"), { status: 202 });
    }),
    http.post(apiUrl("/vms"), () => {
      vmPosts += 1;
      return HttpResponse.json({ code: "conflict", message: "name is taken" }, { status: 409 });
    }),
  );
  const { queryClient } = renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  await user.click(screen.getByRole("button", { name: "New empty volume" }));
  await user.selectOptions(screen.getByLabelText("Storage pool"), "local");
  await user.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(volumes).toEqual(["web-disk0"]));
  // 보내는 동안에는 입력을 고칠 수 없다.
  expect(screen.getByLabelText("Name")).toBeDisabled();
  const done = { ...task("t-vol2", "volume-create", "v1"), status: { state: "succeeded" } };
  queryClient.setQueryData(["list", "task"], { revision: 2, items: [done] });
  // watch가 만든 볼륨도 알려 준다.
  queryClient.setQueryData(["list", "volume"], {
    revision: 3,
    items: [volume("web-disk0", "local")],
  });
  expect(await screen.findByRole("alert")).toHaveTextContent(
    /^Volumes web-disk0 were created; .*name is taken/,
  );

  // 다시 보내면 볼륨은 있는 것으로 붙이고 VM만 만든다.
  expect(
    within(screen.getByRole("group", { name: "Disk 1" })).getByLabelText("Volume"),
  ).toHaveValue("web-disk0");
  await user.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(vmPosts).toBe(2));
  expect(volumes).toEqual(["web-disk0"]);
});

it("stops waiting for the volume and does not create the VM after leaving", async () => {
  mockLists({ node: [NODE], "storage-pool": [pool("local")] });
  let posted = false;
  let vmPosted = false;
  server.use(
    http.post(apiUrl("/volumes"), () => {
      posted = true;
      return HttpResponse.json(task("t-vol3", "volume-create", "v1"), { status: 202 });
    }),
    http.post(apiUrl("/vms"), () => {
      vmPosted = true;
      return HttpResponse.json(vm("web", "stopped"), { status: 201 });
    }),
  );
  const { queryClient, unmount } = renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  await user.click(screen.getByRole("button", { name: "New empty volume" }));
  await user.selectOptions(screen.getByLabelText("Storage pool"), "local");
  await user.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(posted).toBe(true));
  await waitFor(() => expect(queryClient.getQueryCache().hasListeners()).toBe(true));
  unmount();
  // 떠난 뒤 Task가 끝나도 VM을 만들지 않는다.
  const done = { ...task("t-vol3", "volume-create", "v1"), status: { state: "succeeded" } };
  queryClient.setQueryData(["list", "task"], { revision: 2, items: [done] });
  await new Promise((r) => setTimeout(r, 50));
  expect(vmPosted).toBe(false);
});

it("ties errors to their inputs and clears them when rows change", async () => {
  mockLists({ node: [NODE] });
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  await user.click(screen.getByRole("button", { name: "Attach a volume" }));
  await user.clear(screen.getByLabelText("MAC address"));
  await user.type(screen.getByLabelText("MAC address"), "01:00:5e:00:00:01");
  await user.click(screen.getByRole("button", { name: "Create" }));

  const volume = within(screen.getByRole("group", { name: "Disk 1" })).getByLabelText("Volume");
  expect(volume).toHaveAttribute("aria-invalid", "true");
  expect(volume).toHaveAccessibleDescription("Choose a volume.");
  expect(screen.getByLabelText("MAC address")).toHaveAccessibleDescription(/multicast address/);
  expect(screen.getByLabelText("Name")).not.toHaveAttribute("aria-invalid");

  await user.click(screen.getByRole("button", { name: "Remove disk 1" }));
  expect(screen.getByLabelText("MAC address")).not.toHaveAttribute("aria-invalid");
});

it("allows at most 16 network interfaces", async () => {
  mockLists({ node: [NODE] });
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  const add = await screen.findByRole("button", { name: "Add an interface" });
  for (let i = 1; i < 16; i++) {
    await user.click(add);
  }
  expect(screen.getAllByRole("group", { name: /^Network interface/ })).toHaveLength(16);
  expect(screen.queryByRole("button", { name: "Add an interface" })).not.toBeInTheDocument();
});

it("changes the security groups of a running VM's interfaces", async () => {
  const web = vm("web", "running", {
    spec: {
      ...vm("web", "running").spec,
      nics: [
        { network: "lan", model: "virtio", mac: "52:54:00:00:00:01", security_groups: ["ssh"] },
        { network: "user", model: "virtio", mac: "52:54:00:00:00:02" },
      ],
    },
  });
  mockLists({
    vm: [web],
    node: [NODE],
    network: [{ id: "net", name: "lan", revision: 1, labels: {}, spec: { bridge: "br0" } }],
    "security-group": [securityGroup("web"), securityGroup("ssh")],
  });
  let body: { spec: { nics: unknown } } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const groups = await screen.findByRole("group", {
    name: "Security groups of interface 1",
  });
  expect(await within(groups).findByRole("checkbox", { name: "web" })).not.toBeChecked();
  expect(within(groups).getByRole("checkbox", { name: "ssh" })).toBeChecked();
  // user 네트워크(NAT)에는 그룹을 걸 수 없다.
  expect(
    screen.queryByRole("group", { name: "Security groups of interface 2" }),
  ).not.toBeInTheDocument();
  await user.click(within(groups).getByRole("checkbox", { name: "web" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(body).toBeDefined());
  expect(body!.spec.nics).toEqual([
    {
      network: "lan",
      model: "virtio",
      mac: "52:54:00:00:00:01",
      security_groups: ["ssh", "web"],
    },
    { network: "user", model: "virtio", mac: "52:54:00:00:00:02" },
  ]);
});

it("drops the security groups of an interface moved to the user network", async () => {
  mockLists({
    node: [NODE],
    network: [{ id: "net", name: "lan", revision: 1, labels: {}, spec: { bridge: "br0" } }],
    "security-group": [securityGroup("ssh")],
  });
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  const nic = await screen.findByRole("group", { name: "Network interface 1" });
  await within(nic).findByRole("option", { name: "lan" });
  await user.selectOptions(within(nic).getByLabelText("Network"), "lan");
  await user.click(await within(nic).findByRole("checkbox", { name: "ssh" }));
  await user.selectOptions(within(nic).getByLabelText("Network"), "user");
  await user.selectOptions(within(nic).getByLabelText("Network"), "lan");
  expect(within(nic).getByRole("checkbox", { name: "ssh" })).not.toBeChecked();
});

it("says why the create page cannot load", async () => {
  mockLists({});
  server.use(
    http.get(apiUrl("/nodes"), () =>
      HttpResponse.json({ code: "forbidden", message: "permission denied" }, { status: 403 }),
    ),
  );
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
});

it("sets a snapshot schedule and checks how many to keep", async () => {
  const web = vm("web", "running");
  mockLists({ vm: [web], node: [NODE] });
  const bodies: { spec: { snapshot_schedule: unknown } }[] = [];
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      bodies.push((await request.json()) as (typeof bodies)[number]);
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  // 실행 중인 VM도 예약은 바꿀 수 있다.
  const every = await screen.findByLabelText("Take snapshots");
  expect(every).toBeEnabled();
  expect(every).toHaveValue("off");
  // 끄면 보관 개수와 메모리는 묻지 않는다.
  expect(screen.queryByLabelText("Snapshots to keep")).not.toBeInTheDocument();
  await user.selectOptions(every, "daily");
  const keep = screen.getByLabelText("Snapshots to keep");
  expect(keep).toHaveValue(7);
  await user.clear(keep);
  await user.type(keep, "0");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(keep).toHaveAttribute("aria-invalid", "true");
  expect(keep).toHaveAccessibleDescription("From 1 to 100.");
  expect(bodies).toHaveLength(0);

  await user.clear(keep);
  await user.type(keep, "3");
  await user.click(screen.getByLabelText("Include memory"));
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]!.spec.snapshot_schedule).toEqual({ every: "daily", keep: 3, memory: true });
});

it("turns a snapshot schedule off", async () => {
  const web = vm("web", "stopped", {
    spec: {
      ...vm("web", "stopped").spec,
      snapshot_schedule: { every: "weekly", keep: 4, memory: false },
    },
  });
  mockLists({ vm: [web], node: [NODE] });
  const bodies: { spec: { snapshot_schedule: unknown } }[] = [];
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      bodies.push((await request.json()) as (typeof bodies)[number]);
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const every = await screen.findByLabelText("Take snapshots");
  expect(every).toHaveValue("weekly");
  expect(screen.getByLabelText("Snapshots to keep")).toHaveValue(4);
  await user.selectOptions(every, "off");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]!.spec.snapshot_schedule).toBeNull();
});

it("explains the schedule and warns about raw disks", async () => {
  const web = vm("web", "stopped", {
    spec: {
      ...vm("web", "stopped").spec,
      disks: [
        { volume: "web-root", bus: "virtio-blk", boot_order: 1 },
        { volume: "web-data", bus: "virtio-blk", boot_order: null },
      ],
    },
  });
  const raw = volume("web-data", "local");
  raw.spec.format = "raw";
  mockLists({ vm: [web], node: [NODE], volume: [volume("web-root", "local"), raw] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const every = await screen.findByLabelText("Take snapshots");
  expect(every).toHaveAccessibleDescription(
    "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools.",
  );
  // 끈 동안은 경고하지 않는다.
  expect(screen.queryByText(/scheduled snapshots will fail/)).not.toBeInTheDocument();
  await user.selectOptions(every, "hourly");
  expect(
    screen.getByText(
      "web-data is raw, so scheduled snapshots will fail. All disks must be qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools.",
    ),
  ).toBeVisible();
  // raw 디스크를 떼면 경고도 사라진다.
  await user.click(screen.getByRole("button", { name: "Remove disk 2" }));
  expect(screen.queryByText(/scheduled snapshots will fail/)).not.toBeInTheDocument();
});

const fast = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });

it("does not ask about memory for scheduled snapshots of disks in LVM-thin pools", async () => {
  const web = vm("web", "stopped", {
    spec: {
      ...vm("web", "stopped").spec,
      disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }],
      snapshot_schedule: { every: "daily", keep: 3, memory: true },
    },
  });
  const root = volume("web-root", "fast");
  root.spec.format = "raw";
  mockLists({ vm: [web], node: [NODE], volume: [root], "storage-pool": [fast] });
  const bodies: { spec: { snapshot_schedule: unknown } }[] = [];
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      bodies.push((await request.json()) as (typeof bodies)[number]);
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  // LV 스냅샷은 메모리를 담지 못하므로 묻지 않는다. 저장한 선택은 그대로 둔다(예약 작업이 이런 VM은
  // 메모리 없이 찍는다).
  expect(
    await screen.findByText(
      "Disks in LVM-thin, ZFS and Ceph RBD pools are saved without memory. A running VM pauses briefly while they are saved.",
    ),
  ).toBeVisible();
  expect(screen.queryByLabelText("Include memory")).not.toBeInTheDocument();
  expect(screen.queryByText(/scheduled snapshots will fail/)).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]!.spec.snapshot_schedule).toEqual({ every: "daily", keep: 3, memory: true });
});

it("warns that disks in shared LVM pools cannot be snapshotted on a schedule", async () => {
  const web = vm("web", "stopped", {
    spec: {
      ...vm("web", "stopped").spec,
      disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }],
    },
  });
  const root = volume("web-root", "san");
  root.spec.format = "raw";
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [root],
    "storage-pool": [pool("san", { backend: { kind: "lvm-shared", vg: "san" } })],
  });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  await user.selectOptions(await screen.findByLabelText("Take snapshots"), "daily");
  expect(
    screen.getByText("web-root is in a shared LVM pool, so scheduled snapshots will fail."),
  ).toBeVisible();
  // 스냅샷이 없으므로 메모리도 묻지 않는다.
  expect(screen.queryByLabelText("Include memory")).not.toBeInTheDocument();
  expect(screen.queryByText(/saved without memory/)).not.toBeInTheDocument();
});

it("makes new volumes in LVM-thin pools raw and warns about mixed snapshot methods", async () => {
  mockLists({
    node: [NODE],
    volume: [volume("data", "local")],
    "storage-pool": [pool("local"), fast],
  });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl("/volumes"), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t-vol", "volume-create", "v1"), { status: 202 });
    }),
  );
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  await user.click(screen.getByRole("button", { name: "New empty volume" }));
  await user.selectOptions(
    within(screen.getByRole("group", { name: "Disk 1" })).getByLabelText("Storage pool"),
    "fast",
  );
  await user.click(screen.getByRole("button", { name: "Attach a volume" }));
  await user.selectOptions(
    within(screen.getByRole("group", { name: "Disk 2" })).getByLabelText("Volume"),
    "data",
  );
  await user.selectOptions(screen.getByLabelText("Take snapshots"), "hourly");
  // 새 LVM 디스크(LV 스냅샷)와 qcow2 디스크(내부 스냅샷)는 함께 스냅샷하지 못한다.
  expect(
    screen.getByText(
      "Disks with different snapshot methods (qcow2, LVM-thin pools, ZFS pools, Ceph RBD pools) are mixed, so scheduled snapshots will fail.",
    ),
  ).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Remove disk 2" }));
  expect(screen.queryByText(/scheduled snapshots will fail/)).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]).toEqual({
    name: "web-disk0",
    spec: { pool: "fast", size_bytes: 16 * 1024 ** 3, format: "raw" },
  });
});

const FENCED = {
  ...NODE,
  status: { ...NODE.status, ha: { fencing: "watchdog" as const, recover_after_secs: 65 } },
};
const UNFENCED = { ...NODE, id: "n2", name: "node-b" };

it("turns HA on only for nodes with fencing on and says why not", async () => {
  mockLists({ node: [FENCED, UNFENCED] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/vms"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ code: "ha_unsupported", message: "node is off" }, { status: 409 });
    }),
  );
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  const ha = screen.getByRole("switch", { name: "High availability (HA)" });
  // 자동이면 HA 가능 노드가 하나라도 있으면 켤 수 있다.
  expect(ha).toBeEnabled();
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(ha).toBeDisabled();
  expect(screen.getByText("node-b has fencing off and cannot run HA VMs.")).toBeInTheDocument();
  await user.selectOptions(screen.getByLabelText("Node"), "node-a");
  expect(ha).toBeEnabled();
  expect(screen.queryByText(/has fencing off/)).not.toBeInTheDocument();
  await user.click(ha);
  expect(ha).toBeChecked();
  await user.click(screen.getByRole("button", { name: "Create" }));
  // 서버가 거부하면 번역한 이유를 보인다.
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The node has fencing off and cannot run HA VMs.",
  );
  expect(body).toMatchObject({ name: "web", spec: { node: "n1", ha: true } });
});

it("says when no node can run HA VMs but lets an HA VM turn HA off", async () => {
  const app = vm("app", "running", {
    spec: { ...vm("app", "running").spec, node: "n2", ha: true },
  });
  mockLists({ vm: [app], node: [UNFENCED] });
  let body: { spec: { ha: boolean } } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${app.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(app);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  const user = userEvent.setup();
  const ha = await screen.findByRole("switch", { name: "High availability (HA)" });
  // 실행 중이어도 켜진 HA는 끌 수 있다.
  expect(ha).toBeChecked();
  expect(ha).toBeEnabled();
  expect(
    await screen.findByText("node-b has fencing off and cannot run HA VMs."),
  ).toBeInTheDocument();
  await user.click(ha);
  expect(ha).toBeDisabled();
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(body?.spec.ha).toBe(false));
});

it("turns HA on only while the VM is stopped and says why not", async () => {
  mockLists({ vm: [vm("app", "running")], node: [FENCED] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  const ha = await screen.findByRole("switch", { name: "High availability (HA)" });
  expect(ha).not.toBeChecked();
  expect(ha).toBeDisabled();
  expect(screen.getByText("Turn on HA while the VM is stopped.")).toBeInTheDocument();
});

it("turns HA on for a stopped VM", async () => {
  mockLists({ vm: [vm("app", "stopped")], node: [FENCED] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  const ha = await screen.findByRole("switch", { name: "High availability (HA)" });
  expect(ha).toBeEnabled();
  expect(screen.queryByText(/while the VM is stopped/)).not.toBeInTheDocument();
});

it("says when no node has fencing on", async () => {
  mockLists({ node: [UNFENCED] });
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  expect(await screen.findByRole("switch", { name: "High availability (HA)" })).toBeDisabled();
  expect(screen.getByText("No node has fencing on, so HA VMs cannot run.")).toBeInTheDocument();
});

/** HA 가능 노드의 멈춘 VM app을 편집하는 화면. 보낸 본문을 모은다. */
function editStoppedApp() {
  mockLists({ vm: [vm("app", "stopped")], node: [FENCED], "ha-group": [haGroup("rack", 1)] });
  const bodies: { spec: { ha: unknown } }[] = [];
  server.use(
    http.put(apiUrl(`/vms/${vm("app", "stopped").id}`), async ({ request }) => {
      bodies.push((await request.json()) as (typeof bodies)[number]);
      return HttpResponse.json(vm("app", "stopped"));
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  return bodies;
}

it("sends the default HA policy as the old true", async () => {
  const bodies = editStoppedApp();
  const user = userEvent.setup();
  // 정책의 값은 HA를 켤 때만 보인다.
  expect(screen.queryByLabelText("HA state")).not.toBeInTheDocument();
  await user.click(await screen.findByRole("switch", { name: "High availability (HA)" }));
  expect(screen.getByLabelText("HA state")).toHaveValue("started");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  // 기본 정책은 옛 모양 그대로다(옛 노드가 남아도 저장된다).
  expect(bodies[0]?.spec.ha).toBe(true);
});

it("sets the HA state, group and counts", async () => {
  const bodies = editStoppedApp();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("switch", { name: "High availability (HA)" }));
  await user.selectOptions(screen.getByLabelText("HA state"), "stopped");
  await user.selectOptions(await screen.findByLabelText("HA group"), "rack");
  const restarts = screen.getByLabelText("Restarts on the same node");
  await user.clear(restarts);
  await user.type(restarts, "11");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(screen.getByText("From 0 to 10.")).toBeInTheDocument();
  await user.clear(restarts);
  await user.type(restarts, "3");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]?.spec.ha).toEqual({
    state: "stopped",
    group: "rack",
    max_restart: 3,
    max_relocate: 1,
    priority: 50,
  });
});

it("keeps the HA policy of a VM when other fields change", async () => {
  // 폼에 칸이 없는 우선순위·게스트 감시도 그대로 보낸다.
  const policy = {
    state: "stopped",
    group: "gone",
    max_restart: 0,
    max_relocate: 2,
    priority: 80,
    guest_watch: { timeout_secs: 30 },
  } as const;
  const app = vm("app", "stopped", { spec: { ...vm("app", "stopped").spec, ha: policy } });
  mockLists({ vm: [app], node: [FENCED] });
  let body: { spec: { ha: unknown; memory_mib: number } } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${app.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(app);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  const user = userEvent.setup();
  // 지운 그룹도 고른 값으로 보인다.
  expect(await screen.findByLabelText("HA group")).toHaveValue("gone");
  const memory = screen.getByLabelText("Memory (MiB)");
  await user.clear(memory);
  await user.type(memory, "4096");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(body?.spec.memory_mib).toBe(4096));
  expect(body?.spec.ha).toEqual(policy);
});

it("keeps a running VM with ignored HA from activating HA", async () => {
  const policy = { state: "ignored", max_restart: 1, max_relocate: 1, priority: 50 } as const;
  const app = vm("app", "running", { spec: { ...vm("app", "running").spec, ha: policy } });
  mockLists({ vm: [app], node: [FENCED] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  const state = await screen.findByLabelText("HA state");
  expect(state).toHaveValue("ignored");
  expect(within(state).getByRole("option", { name: "Started" })).toBeDisabled();
  expect(within(state).getByRole("option", { name: "Ignored (paused)" })).toBeEnabled();
  expect(screen.getByText("Turn on HA while the VM is stopped.")).toHaveClass(
    "text-muted-foreground",
  );
});
