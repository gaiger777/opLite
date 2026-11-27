import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import {
  hardware,
  haGroup,
  installVm,
  isoVolume,
  NODE,
  pool,
  poolState,
  replicatedVm,
  securityGroup,
  task,
  vm,
  volume,
} from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmEditPage } from "@/vms/VmFormPage";

/**
 * 만들기는 그림 한 화면(계획 77-5, `vmBuilder/BuilderPage`)이 되었다. 폼 섹션(만들기와 고치기가 함께 쓰던 칸)은 노드 없는
 * 멈춘 새 VM(user NIC 하나, 디스크 없음)을 고치는 화면으로 본다.
 */
const FRESH = vm("web", "stopped", {
  spec: {
    ...vm("web", "stopped").spec,
    machine: "q35",
    node: null,
    nics: [{ network: "user", model: "virtio", mac: "52:54:00:00:00:01" }],
  },
});

function editFresh() {
  return renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
}

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
  const machine = await screen.findByLabelText("Virtual motherboard (advanced)");
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

it("ties errors to their inputs and clears them when rows change", async () => {
  mockLists({
    vm: [FRESH],
    node: [NODE],
  });
  editFresh();
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Edit web" });
  await user.click(screen.getByRole("button", { name: "Attach a volume" }));
  await user.clear(screen.getByLabelText("MAC address"));
  await user.type(screen.getByLabelText("MAC address"), "01:00:5e:00:00:01");
  await user.click(screen.getByRole("button", { name: "Save" }));

  const volume = within(screen.getByRole("group", { name: "Disk 1" })).getByLabelText("Volume");
  expect(volume).toHaveAttribute("aria-invalid", "true");
  expect(volume).toHaveAccessibleDescription("Choose a volume.");
  expect(screen.getByLabelText("MAC address")).toHaveAccessibleDescription(/multicast address/);

  await user.click(screen.getByRole("button", { name: "Remove disk 1" }));
  expect(screen.getByLabelText("MAC address")).not.toHaveAttribute("aria-invalid");
});

it("allows at most 16 network interfaces", async () => {
  mockLists({
    vm: [FRESH],
    node: [NODE],
  });
  editFresh();
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
    vm: [FRESH],
    node: [NODE],
    network: [{ id: "net", name: "lan", revision: 1, labels: {}, spec: { bridge: "br0" } }],
    "security-group": [securityGroup("ssh")],
  });
  editFresh();
  const user = userEvent.setup();
  const nic = await screen.findByRole("group", { name: "Network interface 1" });
  await within(nic).findByRole("option", { name: "lan" });
  await user.selectOptions(within(nic).getByLabelText("Network"), "lan");
  await user.click(await within(nic).findByRole("checkbox", { name: "ssh" }));
  await user.selectOptions(within(nic).getByLabelText("Network"), "user");
  await user.selectOptions(within(nic).getByLabelText("Network"), "lan");
  expect(within(nic).getByRole("checkbox", { name: "ssh" })).not.toBeChecked();
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
  expect(keep).toHaveAccessibleDescription(expect.stringContaining("From 1 to 100."));
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
    "Takes snapshots automatically and keeps only the newest scheduled ones (manual snapshots stay). All disks must be qcow2 files or all on one pool type (LVM-thin, ZFS, Ceph RBD).",
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

const FENCED = {
  ...NODE,
  status: { ...NODE.status, ha: { fencing: "watchdog" as const, recover_after_secs: 65 } },
};
const UNFENCED = { ...NODE, id: "n2", name: "node-b" };

it("turns HA on only for nodes with fencing on and says why not", async () => {
  mockLists({
    vm: [FRESH],
    node: [FENCED, UNFENCED],
  });
  let body: unknown;
  server.use(
    http.put(apiUrl(`/vms/${FRESH.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ code: "ha_unsupported", message: "node is off" }, { status: 409 });
    }),
  );
  editFresh();
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Edit web" });
  const ha = screen.getByRole("switch", { name: "High availability (HA)" });
  await screen.findByRole("option", { name: "node-b" });
  // 자동이면 HA 가능 노드가 하나라도 있으면 켤 수 있다.
  expect(ha).toBeEnabled();
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(ha).toBeDisabled();
  expect(
    screen.getByText("node-b has failed-node isolation (fencing) off, so it can't run HA VMs."),
  ).toBeInTheDocument();
  await user.selectOptions(screen.getByLabelText("Node"), "node-a");
  expect(ha).toBeEnabled();
  expect(screen.queryByText(/has failed-node isolation \(fencing\) off/)).not.toBeInTheDocument();
  await user.click(ha);
  expect(ha).toBeChecked();
  await user.click(screen.getByRole("button", { name: "Save" }));
  // 서버가 거부하면 번역한 이유를 보인다.
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The node has node fencing (forced cut-off) turned off and cannot run HA VMs.",
  );
  expect(body).toMatchObject({ spec: { node: "n1", ha: true } });
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
    await screen.findByText(
      "node-b has failed-node isolation (fencing) off, so it can't run HA VMs.",
    ),
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
  mockLists({
    vm: [FRESH],
    node: [UNFENCED],
  });
  editFresh();
  const ha = await screen.findByRole("switch", { name: "High availability (HA)" });
  await waitFor(() => expect(ha).toBeDisabled());
  expect(
    screen.getByText("No node has failed-node isolation (fencing) on, so HA VMs can't run."),
  ).toBeInTheDocument();
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

it("sets the recovery priority and the guest watch", async () => {
  const bodies = editStoppedApp();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("switch", { name: "High availability (HA)" }));
  const priority = screen.getByLabelText("Recovery priority");
  expect(priority).toHaveValue(50);
  await user.clear(priority);
  await user.type(priority, "101");
  // 초 칸은 감시를 켤 때만 보인다(기본 60초).
  expect(screen.queryByLabelText("Seconds without an answer")).not.toBeInTheDocument();
  await user.click(screen.getByRole("switch", { name: "Restart when the guest hangs" }));
  const secs = screen.getByLabelText("Seconds without an answer");
  expect(secs).toHaveValue(60);
  await user.clear(secs);
  await user.type(secs, "5");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(screen.getByText("From 0 to 100.")).toBeInTheDocument();
  expect(screen.getByText("From 30 to 3600 seconds.")).toBeInTheDocument();
  expect(bodies).toHaveLength(0);
  await user.clear(priority);
  await user.type(priority, "90");
  await user.clear(secs);
  await user.type(secs, "30");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]?.spec.ha).toEqual({
    state: "started",
    max_restart: 1,
    max_relocate: 1,
    priority: 90,
    guest_watch: { timeout_secs: 30 },
  });
});

it("keeps the HA policy of a VM when other fields change", async () => {
  // 고치지 않은 우선순위·게스트 감시도 그대로 보낸다.
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

/** VM `app`을 편집하는 화면. 보낸 본문을 모은다. */
function editApp(app: ReturnType<typeof vm>) {
  mockLists({ vm: [app], node: [FENCED] });
  const bodies: { spec: { placement?: unknown } }[] = [];
  server.use(
    http.put(apiUrl(`/vms/${app.id}`), async ({ request }) => {
      bodies.push((await request.json()) as (typeof bodies)[number]);
      return HttpResponse.json(app);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  return bodies;
}

it("pins a running VM so the balancer does not move it", async () => {
  const bodies = editApp(vm("app", "running"));
  const user = userEvent.setup();
  // 실행 중에도 바꿀 수 있다(장치 변화 없음).
  const pinned = await screen.findByRole("switch", { name: "Not moved by the balancer" });
  expect(pinned).toBeEnabled();
  expect(pinned).not.toBeChecked();
  await user.click(pinned);
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]?.spec.placement).toEqual({ pinned: true });
});

it("unpins a VM without sending placement", async () => {
  const app = vm("app", "running");
  const bodies = editApp({ ...app, spec: { ...app.spec, placement: { pinned: true } } });
  const user = userEvent.setup();
  const pinned = await screen.findByRole("switch", { name: "Not moved by the balancer" });
  expect(pinned).toBeChecked();
  await user.click(pinned);
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  // 끄면 `placement`를 보내지 않는다(옛 VM과 같은 모양).
  expect(bodies[0]?.spec).not.toHaveProperty("placement");
});

/** 디스크 `disks`·NIC 하나로 도는 VM. 핫플러그 배치(`layout`)는 디스크 하나로 켠 것이다. */
function hotVm(
  disks: string[],
  layout: NonNullable<Parameters<typeof hardware>[0]>["layout"] = {},
) {
  const base = vm("web", "running");
  const spec = {
    ...base.spec,
    disks: disks.map((volume, i) => ({
      volume,
      bus: "virtio-blk" as const,
      boot_order: i === 0 ? 1 : null,
    })),
    nics: [{ network: "user", model: "virtio" as const, mac: "52:54:00:00:00:01" }],
  };
  return vm("web", "running", {
    spec,
    status: {
      ...base.status,
      hardware: hardware({ disks: spec.disks, nics: spec.nics, layout }),
    },
  });
}

it("attaches a disk to a running VM and tracks the hotplug task", async () => {
  const web = hotVm(["web-root"]);
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local", { attached_to: web.id }), volume("web-data", "local")],
  });
  let body: { spec: { disks: unknown } } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(web, { headers: { "orbivisor-task": "t-hot" } });
    }),
    http.get(apiUrl("/tasks/t-hot"), () => HttpResponse.json(task("t-hot", "vm-hotplug", web.id))),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const root = await screen.findByRole("group", { name: "Disk 1" });
  // 켤 때 단 디스크는 바꾸지도 떼지도 못한다.
  expect(within(root).getByLabelText("Volume")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Remove disk 1" })).toBeDisabled();
  expect(screen.getByText(/you can add disks at the end/)).toBeInTheDocument();
  expect(screen.getByText(/within the maximums apply right away/)).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Attach a volume" }));
  const added = screen.getByRole("group", { name: "Disk 2" });
  await user.selectOptions(within(added).getByLabelText("Volume"), "web-data");
  // 실행 중에는 virtio-blk만 붙인다.
  expect(
    within(added).getByRole("option", { name: "SATA — compatible (before Windows drivers)" }),
  ).toBeDisabled();
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Change hardware of web")).toBeInTheDocument();
  expect(body!.spec.disks).toEqual([
    { volume: "web-root", bus: "virtio-blk", boot_order: 1 },
    { volume: "web-data", bus: "virtio-blk", boot_order: null },
  ]);
});

it("shows the server's reason when a running VM's hardware cannot change", async () => {
  const web = hotVm(["web-root"]);
  mockLists({ vm: [web], node: [NODE] });
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), () =>
      HttpResponse.json(
        {
          code: "vm_running",
          message: 'cannot remove disk "web-root" of VM "web" while it is running',
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Save" }));
  expect(
    await screen.findByText('cannot remove disk "web-root" of VM "web" while it is running'),
  ).toBeInTheDocument();
  expect(screen.queryByText("Stop the VM first.")).not.toBeInTheDocument();
});

it("removes only the last device added while a VM runs", async () => {
  const web = hotVm(["web-root", "web-a", "web-b"], { disk_ports: [0, 1] });
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  expect(await screen.findByRole("button", { name: "Remove disk 3" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Remove disk 2" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Remove disk 1" })).toBeDisabled();
  // 실행 중 붙인 디스크도 고치지는 못한다.
  const last = screen.getByRole("group", { name: "Disk 3" });
  expect(within(last).getByLabelText("Bus")).toBeDisabled();
  await user.click(screen.getByRole("button", { name: "Remove disk 3" }));
  // 이제 실행 중 붙인 첫 디스크가 마지막이다. 켤 때 단 것은 여전히 못 뗀다.
  expect(screen.getByRole("button", { name: "Remove disk 2" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Remove disk 1" })).toBeDisabled();
});

it("adds interfaces to a running VM but keeps the running ones", async () => {
  const web = hotVm(["web-root"]);
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const first = await screen.findByRole("group", { name: "Network interface 1" });
  expect(within(first).getByLabelText("Network")).toBeDisabled();
  expect(within(first).getByLabelText("Model")).toBeDisabled();
  expect(within(first).getByLabelText("MAC address")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Remove interface 1" })).toBeDisabled();
  await user.click(screen.getByRole("button", { name: "Add an interface" }));
  const second = screen.getByRole("group", { name: "Network interface 2" });
  expect(within(second).getByLabelText("Network")).toBeEnabled();
  expect(within(second).getByLabelText("MAC address")).toBeEnabled();
  expect(screen.getByRole("button", { name: "Remove interface 2" })).toBeEnabled();
});

it("asks to restart a VM started without hotplug before changing its devices", async () => {
  const web = hotVm(["web-root"], null);
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  expect(
    await screen.findByText(
      "This VM was started without room for adding devices. Restart it once to attach or detach disks and interfaces while running.",
    ),
  ).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Attach a volume" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Add an interface" })).not.toBeInTheDocument();
  expect(screen.getByLabelText("Network")).toBeDisabled();
  // vCPU 코어와 메모리는 그대로 고칠 수 있다.
  expect(screen.getByLabelText("vCPU cores")).toBeEnabled();
  expect(screen.getByLabelText("Maximum vCPUs")).toBeEnabled();
});

it("sets the hotplug maximums and checks them like the server", async () => {
  mockLists({
    vm: [FRESH],
    node: [NODE],
  });
  let body: { spec: { cpu: unknown; max_memory_mib?: number } } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${FRESH.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(FRESH);
    }),
  );
  editFresh();
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Edit web" });
  await user.type(screen.getByLabelText("Maximum vCPUs"), "1");
  await user.type(screen.getByLabelText("Maximum memory (MiB)"), "4000");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(screen.getByText("At least sockets × cores.")).toBeInTheDocument();
  expect(
    screen.getByText("With a maximum, memory and maximum memory must be multiples of 128 MiB."),
  ).toBeInTheDocument();
  expect(body).toBeUndefined();
  await user.clear(screen.getByLabelText("Maximum vCPUs"));
  await user.type(screen.getByLabelText("Maximum vCPUs"), "8");
  await user.clear(screen.getByLabelText("Maximum memory (MiB)"));
  await user.type(screen.getByLabelText("Maximum memory (MiB)"), "16384");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(body).toBeDefined());
  expect(body!.spec.cpu).toEqual({ sockets: 1, cores: 2, model: "host", max_vcpus: 8 });
  expect(body!.spec.max_memory_mib).toBe(16384);
});

describe("replication", () => {
  const NODE_B = {
    ...NODE,
    id: "n2",
    name: "node-b",
    status: { ...NODE.status, pools: { b: poolState("b") } },
  };
  const WITNESS = { ...NODE, id: "n9", name: "node-w", spec: { ...NODE.spec, witness: true } };

  it("adds replication targets to a new VM, but not to its own node or a witness", async () => {
    mockLists({
      vm: [FRESH],
      node: [NODE, NODE_B, WITNESS],
      "storage-pool": [pool("local"), pool("b")],
    });
    let body: unknown;
    server.use(
      http.put(apiUrl(`/vms/${FRESH.id}`), async ({ request }) => {
        body = await request.json();
        return HttpResponse.json(FRESH);
      }),
    );
    editFresh();
    const user = userEvent.setup();
    await screen.findByRole("heading", { name: "Edit web" });
    await screen.findByRole("option", { name: "node-a" });
    await user.selectOptions(screen.getByLabelText("Node"), "node-a");
    expect(
      screen.getByText(/Copies disks on node-local pools to other nodes on a schedule/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Add replication target" }));
    const row = screen.getByRole("group", { name: "Replication target 1" });
    const node = within(row).getByLabelText("Node");
    expect(node).toHaveValue("n2");
    expect(
      within(node)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["node-b"]);
    expect(within(row).getByLabelText("Every (minutes)")).toHaveValue(15);
    await user.clear(within(row).getByLabelText("Every (minutes)"));
    await user.type(within(row).getByLabelText("Every (minutes)"), "5");
    await user.selectOptions(within(row).getByLabelText("Target pool"), "b");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("moved")).toBeInTheDocument();
    expect(body).toMatchObject({
      spec: { node: "n1", replication: [{ node: "n2", schedule: "*/5", pool: "b" }] },
    });
  });

  it("allows at most three targets and checks them before sending", async () => {
    const nodes = [
      NODE,
      NODE_B,
      ...["n3", "n4"].map((id) => ({ ...NODE, id, name: `node-${id}` })),
    ];
    mockLists({
      vm: [FRESH],
      node: nodes,
    });
    let called = false;
    server.use(
      http.put(apiUrl(`/vms/${FRESH.id}`), () => {
        called = true;
        return HttpResponse.json(FRESH);
      }),
    );
    editFresh();
    const user = userEvent.setup();
    await screen.findByRole("heading", { name: "Edit web" });
    const add = screen.getByRole("button", { name: "Add replication target" });
    await user.click(add);
    await user.click(add);
    await user.click(add);
    expect(add).toBeDisabled();
    // 같은 노드를 두 번, 범위 밖의 주기
    const second = screen.getByRole("group", { name: "Replication target 2" });
    // 노드를 스케줄러에 맡겼으므로 node-a도 고를 수 있고, 줄마다 아직 대상이 아닌 노드가 먼저 골라져 있다.
    expect(within(second).getByLabelText("Node")).toHaveValue("n2");
    await user.selectOptions(within(second).getByLabelText("Node"), "node-a");
    await user.clear(within(second).getByLabelText("Every (minutes)"));
    await user.type(within(second).getByLabelText("Every (minutes)"), "1441");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(within(second).getByText("This node is already a target.")).toBeInTheDocument();
    expect(within(second).getByText("Enter whole minutes from 1 to 1440.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Remove replication target 2" }));
    expect(add).toBeEnabled();
    expect(called).toBe(false);
  });

  it("does not combine replication with scheduled snapshots", async () => {
    mockLists({
      vm: [FRESH],
      node: [NODE, NODE_B],
    });
    let called = false;
    server.use(
      http.put(apiUrl(`/vms/${FRESH.id}`), () => {
        called = true;
        return HttpResponse.json(FRESH);
      }),
    );
    editFresh();
    const user = userEvent.setup();
    await screen.findByRole("heading", { name: "Edit web" });
    await user.selectOptions(screen.getByLabelText("Take snapshots"), "daily");
    await user.click(screen.getByRole("button", { name: "Add replication target" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(
      screen.getByText(
        "Replication cannot be used together with scheduled snapshots. Turn the snapshot schedule off.",
      ),
    ).toBeInTheDocument();
    expect(called).toBe(false);
  });

  it("warns that an HA VM with a local disk needs a replica", async () => {
    const fenced = { ...NODE_B, status: { ...NODE_B.status, ha: FENCED.status.ha } };
    const web = vm("web", "stopped", {
      spec: {
        ...vm("web", "stopped").spec,
        disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }],
        ha: true,
      },
    });
    mockLists({
      vm: [web],
      node: [FENCED, fenced],
      volume: [volume("web-root", "local")],
      "storage-pool": [pool("local"), pool("nas", { shared: true })],
    });
    let body: { spec: Record<string, unknown> } | undefined;
    server.use(
      http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
        body = (await request.json()) as typeof body;
        return HttpResponse.json(web);
      }),
    );
    renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
    const user = userEvent.setup();
    const warning =
      "This VM cannot be restarted on another node: disk web-root is on node-local pool local and has no replica. Add replication.";
    expect(await screen.findByText(warning)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Add replication target" }));
    expect(screen.queryByText(warning)).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(body?.spec.replication).toEqual([{ node: "n2", schedule: "*/15" }]));
  });

  it("removes all targets of a replicated VM", async () => {
    const web = replicatedVm("running");
    mockLists({ vm: [web], node: [NODE, NODE_B] });
    let body: { spec: Record<string, unknown> } | undefined;
    server.use(
      http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
        body = (await request.json()) as typeof body;
        return HttpResponse.json(web);
      }),
    );
    renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
    const user = userEvent.setup();
    const row = await screen.findByRole("group", { name: "Replication target 1" });
    expect(within(row).getByLabelText("Node")).toHaveValue("n2");
    await user.click(screen.getByRole("button", { name: "Remove replication target 1" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(body).toBeDefined());
    expect(body!.spec).not.toHaveProperty("replication");
  });
});

describe("install media", () => {
  const nas = pool("nas", { backend: { kind: "nfs", server: "nas.lan", export: "/srv/iso" } });
  const media = [
    volume("web-root", "local"),
    isoVolume("alpine", "local"),
    isoVolume("ubuntu", "nas"),
  ];

  it("checks cloud-init input and warns when user-data replaces the user", async () => {
    mockLists({
      vm: [FRESH],
      node: [NODE],
      "storage-pool": [pool("local")],
      volume: media,
    });
    editFresh();
    const user = userEvent.setup();
    await screen.findByRole("heading", { name: "Edit web" });
    await user.click(screen.getByLabelText("Set up the guest on first boot (cloud-init)"));
    await user.type(screen.getByLabelText("User"), "root");
    await user.type(screen.getByLabelText("SSH public keys"), "not a key");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(screen.getByText("Use SSH keys with the default user instead of root.")).toBeVisible();
    expect(screen.getByText("Line 1 is not an SSH public key.")).toBeVisible();
    expect(screen.queryByText(/replaces the generated user-data/)).not.toBeInTheDocument();
    await user.click(screen.getByText("Advanced: user-data and vendor-data"));
    await user.type(screen.getByLabelText("user-data"), "#cloud-config");
    expect(screen.getByRole("status")).toHaveTextContent(
      "The user-data replaces the generated user-data; user, password and SSH keys are not used.",
    );
  });

  it("keeps the stored password when other cloud-init fields change", async () => {
    const web = installVm("web", "stopped");
    mockLists({ vm: [web], node: [NODE], "storage-pool": [pool("local")], volume: media });
    let body: { spec: Record<string, unknown> } | undefined;
    server.use(
      http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
        body = (await request.json()) as typeof body;
        return HttpResponse.json(web);
      }),
    );
    renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
    const user = userEvent.setup();
    expect(await screen.findByText("A password is set.")).toBeVisible();
    expect(screen.queryByLabelText("Password")).not.toBeInTheDocument();
    await user.type(screen.getByLabelText("Host name"), "web.lab");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(body).toBeDefined());
    expect(body!.spec.cloud_init).toEqual({ ...web.spec.cloud_init, hostname: "web.lab" });
    expect(body!.spec.cdroms).toEqual([{ iso: "alpine" }, {}]);
  });

  it("changes the media of a running VM but not its drives or boot order", async () => {
    const web = installVm("web", "running", {
      status: {
        ...vm("web", "running").status,
        hardware: hardware({ cdroms: [{ iso: "alpine" }, {}] }),
      },
    });
    mockLists({ vm: [web], node: [NODE], "storage-pool": [pool("local"), nas], volume: media });
    let body: { spec: { cdroms: unknown } } | undefined;
    server.use(
      http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
        body = (await request.json()) as typeof body;
        return HttpResponse.json(web, { headers: { "orbivisor-task": "t-hot" } });
      }),
      http.get(apiUrl("/tasks/t-hot"), () =>
        HttpResponse.json(task("t-hot", "vm-hotplug", web.id)),
      ),
    );
    renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
    const user = userEvent.setup();
    const cd = await screen.findByRole("group", { name: "CD-ROM drive 1" });
    expect(screen.queryByRole("button", { name: "Add a CD/DVD drive" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove CD-ROM drive 0" })).toBeDisabled();
    expect(screen.getByText("Stop the VM to change the boot order.")).toBeVisible();
    expect(screen.getByRole("button", { name: "Move cdrom1 up" })).toBeDisabled();
    expect(screen.getByText(/apply at the next start/)).toBeVisible();
    // 볼륨 목록을 읽으면 ISO를 고를 수 있다.
    await within(cd).findByRole("option", { name: "ubuntu" });
    await user.selectOptions(within(cd).getByLabelText("Media of cdrom1"), "ubuntu");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("Change hardware of web")).toBeInTheDocument();
    expect(body!.spec.cdroms).toEqual([{ iso: "alpine" }, { iso: "ubuntu" }]);
  });

  it("offers only ISOs from shared pools to HA VMs", async () => {
    const web = installVm("web", "stopped", {
      spec: { ...installVm("web", "stopped").spec, ha: true },
    });
    mockLists({ vm: [web], node: [NODE], "storage-pool": [pool("local"), nas], volume: media });
    renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
    const cd = await screen.findByRole("group", { name: "CD-ROM drive 1" });
    await within(cd).findByRole("option", { name: "ubuntu" });
    const options = within(within(cd).getByLabelText("Media of cdrom1")).getAllByRole("option");
    expect(options.map((o) => o.textContent)).toEqual(["Empty", "ubuntu"]);
  });
});

/** TPM·Secure Boot를 돌릴 수 있는 노드 */
const CAPABLE = {
  ...NODE,
  status: { ...NODE.status, firmware_caps: { tpm: true, secure_boot: true } },
};

it("says why TPM and Secure Boot cannot be turned on and needs UEFI", async () => {
  // 옛 노드는 능력을 알리지 않는다.
  mockLists({
    vm: [FRESH],
    node: [NODE],
  });
  editFresh();
  const tpm = await screen.findByRole("switch", { name: "TPM 2.0" });
  await waitFor(() => expect(tpm).toBeDisabled());
  expect(
    screen.getByText("No node can run a TPM. Install swtpm on the nodes and restart orbivisor."),
  ).toBeInTheDocument();
  expect(screen.getByRole("switch", { name: "Secure Boot" })).toBeDisabled();
});

it("asks for UEFI before sending TPM", async () => {
  mockLists({
    vm: [FRESH],
    node: [CAPABLE],
  });
  editFresh();
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Edit web" });
  await user.click(screen.getByRole("switch", { name: "TPM 2.0" }));
  await user.selectOptions(screen.getByLabelText("Firmware"), "bios");
  await user.click(screen.getByRole("button", { name: "Save" }));
  // 이유와 입력 오류
  expect(screen.getAllByText("Needs UEFI firmware.").length).toBeGreaterThan(0);
  expect(screen.getByRole("switch", { name: "Secure Boot" })).toBeDisabled();
});

it("changes Secure Boot only through the firmware reset once the VM has UEFI variables", async () => {
  const win = vm("win", "stopped", {
    status: { ...vm("win", "stopped").status, firmware_node: "n1" },
  });
  mockLists({ vm: [win], node: [CAPABLE] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/win/edit" });
  const sb = await screen.findByRole("switch", { name: "Secure Boot" });
  expect(sb).toBeDisabled();
  expect(screen.getByText(/Change Secure Boot with "Reset firmware" on the VM page/)).toBeVisible();
  // TPM은 바꿀 수 있다.
  expect(screen.getByRole("switch", { name: "TPM 2.0" })).toBeEnabled();
});

it("changes the guest agent of a running VM and keeps Secure Boot locked", async () => {
  const app = vm("app", "running", {
    status: { ...vm("app", "running").status, hardware: hardware() },
  });
  mockLists({ vm: [app], node: [CAPABLE] });
  let body: { spec: Record<string, unknown> } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${app.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(app);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  const user = userEvent.setup();
  expect(await screen.findByRole("switch", { name: "Secure Boot" })).toBeDisabled();
  expect(screen.getByText("Stop the VM to change Secure Boot.")).toBeInTheDocument();
  await user.click(screen.getByRole("switch", { name: "QEMU guest agent" }));
  await user.click(
    screen.getByRole("switch", { name: "Freeze file systems for snapshots and backups" }),
  );
  await user.selectOptions(screen.getByLabelText("Shutdown"), "acpi");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(body).toBeDefined());
  expect(body!.spec.guest_agent).toEqual({ freeze_fs: false, shutdown: "acpi" });
  expect(body!.spec).not.toHaveProperty("secure_boot");
});

it("shows the server's message when Secure Boot needs a firmware reset", async () => {
  const win = vm("win", "stopped");
  mockLists({ vm: [win], node: [CAPABLE] });
  server.use(
    http.put(apiUrl(`/vms/${win.id}`), () =>
      HttpResponse.json(
        { code: "firmware_reset_required", message: 'VM "win" has UEFI variables …' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/win/edit" });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("switch", { name: "Secure Boot" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    'Change Secure Boot with "Reset firmware" on the VM page',
  );
});

it("moves focus to the first wrong input of the first step with errors", async () => {
  mockLists({
    vm: [FRESH],
    node: [NODE],
  });
  editFresh();
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Edit web" });
  const nic = screen.getByRole("group", { name: "Network interface 1" });
  const mac = within(nic).getByLabelText("MAC address");
  await user.type(mac, "zz");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(mac).toHaveFocus();
  expect(mac).toHaveAccessibleDescription("Use the form 52:54:00:12:34:56.");
});

it("warns when a simple-network rate limit can't run on any node (review 2 m2)", async () => {
  // NODE는 `qos`를 알리지 않는다(tc 없음 — 서버가 409 `qos_unsupported`로 막는다).
  mockLists({
    vm: [FRESH],
    node: [NODE],
    network: [{ id: "net", name: "lan", revision: 1, labels: {}, spec: { bridge: "br0" } }],
  });
  editFresh();
  const user = userEvent.setup();
  const nic = await screen.findByRole("group", { name: "Network interface 1" });
  await within(nic).findByRole("option", { name: "lan" });
  await user.selectOptions(within(nic).getByLabelText("Network"), "lan");
  await user.click(within(nic).getByText("QoS (rate limit, DSCP)"));
  expect(within(nic).queryByText(/can't rate-limit/)).not.toBeInTheDocument();
  await user.type(within(nic).getByLabelText("Send limit (Mbit/s)"), "100");
  expect(
    within(nic).getByText(
      "No node can rate-limit simple-network interfaces (no tc). Remove the rate limit or use an SDN network.",
    ),
  ).toBeVisible();
});
