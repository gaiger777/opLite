import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import {
  hardware,
  installVm,
  isoVolume,
  NODE,
  pool,
  replica,
  replicatedVm,
  snapshot,
  task,
  vm,
  volume,
} from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

const web = vm("web", "running", {
  spec: {
    ...vm("web", "running").spec,
    disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }],
    nics: [
      {
        network: "lan",
        model: "virtio",
        mac: "52:54:00:12:34:56",
        security_groups: ["web", "ssh"],
      },
    ],
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
  expect(screen.getByText("web, ssh")).toBeInTheDocument();

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
  // 영향 목록: 이미 받은 목록에서 센다.
  expect(within(dialog).getByRole("list", { name: "What this affects" })).toHaveTextContent(
    /Disk volumes \(kept\): \d+Snapshots: 2Backups \(kept\): 0/,
  );
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

it("marks an HA VM", async () => {
  mockLists({
    vm: [web, { ...web, id: "x", name: "app", spec: { ...web.spec, ha: true } }],
    node: [NODE],
  });
  const { unmount } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(await screen.findByRole("heading", { name: "web" })).toBeInTheDocument();
  expect(screen.queryByText("HA")).not.toBeInTheDocument();
  unmount();
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/app" });
  expect(await screen.findByRole("heading", { name: "app" })).toBeInTheDocument();
  expect(screen.getByText("HA")).toBeInTheDocument();
});

/** 요약의 값(이름표 `label` 아래) */
function summary(label: string) {
  return screen.getByText(label, { selector: "dt" }).closest("div")!;
}

it("shows the HA policy, broken placement rules and the HA error", async () => {
  const db = vm("db", "stopped", {
    spec: {
      ...vm("db", "stopped").spec,
      ha: { state: "stopped", group: "rack", max_restart: 2, max_relocate: 0, priority: 50 },
    },
    status: {
      ...vm("db", "stopped").status,
      rule_violations: ["spread", "ha-group:rack"],
      ha: { relocations: [], error: "HA gave up after 1 relocations" },
    },
  });
  mockLists({ vm: [db], node: [NODE] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/db" });
  expect(await screen.findByRole("heading", { name: "db" })).toBeInTheDocument();
  expect(summary("High availability")).toHaveTextContent(
    "Stopped (moves, stays off)Restarts 2, moves 0",
  );
  expect(summary("HA group")).toHaveTextContent("rack");
  expect(summary("HA recovery priority")).toHaveTextContent("50");
  expect(summary("Guest agent watch")).toHaveTextContent("Off");
  expect(summary("Broken placement rules")).toHaveTextContent("spread, ha-group:rack");
  expect(summary("HA error")).toHaveTextContent("HA gave up after 1 relocations");
});

it("shows the recovery priority and the guest watch", async () => {
  const db = vm("db", "running", {
    spec: {
      ...vm("db", "running").spec,
      ha: {
        state: "started",
        max_restart: 1,
        max_relocate: 1,
        priority: 90,
        guest_watch: { timeout_secs: 30 },
      },
    },
  });
  mockLists({ vm: [db], node: [NODE] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/db" });
  expect(await screen.findByRole("heading", { name: "db" })).toBeInTheDocument();
  expect(summary("HA recovery priority")).toHaveTextContent("90");
  expect(summary("Guest agent watch")).toHaveTextContent("Restart after 30 s without an answer");
});

it("shows a VM without HA plainly", async () => {
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(await screen.findByRole("heading", { name: "web" })).toBeInTheDocument();
  expect(summary("High availability")).toHaveTextContent("Off");
  expect(screen.queryByText("HA group", { selector: "dt" })).not.toBeInTheDocument();
  expect(screen.queryByText("Guest agent watch", { selector: "dt" })).not.toBeInTheDocument();
  expect(summary("Broken placement rules")).toHaveTextContent("—");
  expect(screen.queryByText("HA error", { selector: "dt" })).not.toBeInTheDocument();
});

it("says when the balancer does not move the VM", async () => {
  const pinned = {
    ...web,
    id: "p",
    name: "pinned",
    spec: { ...web.spec, placement: { pinned: true } },
  };
  mockLists({ vm: [web, pinned], node: [NODE] });
  const { unmount } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/pinned" });
  expect(await screen.findByRole("heading", { name: "pinned" })).toBeInTheDocument();
  expect(summary("Automatic moves")).toHaveTextContent("Excluded (pinned)");
  unmount();
  // 고정하지 않은 VM에는 그 줄이 없다.
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(await screen.findByRole("heading", { name: "web" })).toBeInTheDocument();
  expect(screen.queryByText("Automatic moves", { selector: "dt" })).not.toBeInTheDocument();
});

it("shows the running hardware, what was added while running and what waits for a restart", async () => {
  const disks = [
    { volume: "web-root", bus: "virtio-blk" as const, boot_order: 1 },
    { volume: "web-data", bus: "virtio-blk" as const, boot_order: null },
  ];
  const hot = vm("web", "running", {
    spec: {
      ...vm("web", "running").spec,
      cpu: { sockets: 1, cores: 4, model: "host", max_vcpus: 8 },
      memory_mib: 2048,
      max_memory_mib: 16384,
      firmware: "bios",
      disks,
    },
    status: {
      ...vm("web", "running").status,
      hardware: hardware({
        disks,
        layout: { vcpus: 4, plugged_mib: 2048, disk_ports: [0] },
        pending: [
          { field: "firmware", running: "uefi", wanted: "bios", reason: "restart" },
          {
            field: "cpu.max_vcpus",
            running: "8",
            wanted: "16",
            reason: "unsupported",
          },
        ],
      }),
    },
  });
  mockLists({ vm: [hot], node: [NODE] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(await screen.findByText("Restart needed")).toBeInTheDocument();
  expect(screen.getByText("4 (max 8)")).toBeInTheDocument();
  expect(screen.getByText("4 GiB (max 16 GiB)")).toBeInTheDocument();
  expect(screen.getByText("1/8 hotplug ports used")).toBeInTheDocument();
  const pending = screen.getByRole("list", { name: "Applies at the next restart" });
  expect(
    within(pending)
      .getAllByRole("listitem")
      .map((li) => li.textContent),
  ).toEqual([
    "Firmware: uefi → bios — set only when the VM starts",
    "Maximum vCPUs: 8 → 16 — not supported on this node",
  ]);
  // 실패한 변경이 없으면 다시 적용할 것이 없다.
  expect(screen.queryByRole("button", { name: "Apply again" })).not.toBeInTheDocument();

  await userEvent.click(screen.getByRole("tab", { name: "Hardware" }));
  const rows = within(screen.getByRole("table", { name: "Disks" })).getAllByRole("row");
  expect(rows[1]).not.toHaveTextContent("Added while running");
  expect(rows[2]).toHaveTextContent("web-dataAdded while running");
});

it("applies failed hardware changes again", async () => {
  const hot = vm("web", "running", {
    status: {
      ...vm("web", "running").status,
      hardware: hardware({
        pending: [
          {
            field: "memory_mib",
            running: "4096",
            wanted: "2048",
            reason: "failed",
            detail: "the guest released only 0 of 2048 MiB within 60s",
          },
        ],
      }),
    },
  });
  mockLists({ vm: [hot], node: [NODE] });
  let called = false;
  server.use(
    http.post(apiUrl(`/vms/${hot.id}/actions/hotplug`), () => {
      called = true;
      return HttpResponse.json(task("t-hot", "vm-hotplug", hot.id), { status: 202 });
    }),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(
    await screen.findByText("the guest released only 0 of 2048 MiB within 60s"),
  ).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Apply again" }));
  expect(await screen.findByText("Change hardware of web")).toBeInTheDocument();
  expect(called).toBe(true);
});

it("shows the server's reason when applying hardware changes again fails", async () => {
  const hot = vm("web", "running", {
    status: {
      ...vm("web", "running").status,
      hardware: hardware({
        pending: [{ field: "vcpus", running: "4", wanted: "2", reason: "failed", detail: "held" }],
      }),
    },
  });
  mockLists({ vm: [hot], node: [NODE] });
  server.use(
    http.post(apiUrl(`/vms/${hot.id}/actions/hotplug`), () =>
      HttpResponse.json(
        { code: "vm_busy", message: 'VM "web" is being migrated' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  await userEvent.click(await screen.findByRole("button", { name: "Apply again" }));
  expect(await screen.findByText(/VM "web" is being migrated/)).toBeInTheDocument();
});

it("says when a running VM was started without hotplug", async () => {
  const old = vm("web", "running", {
    status: { ...vm("web", "running").status, hardware: hardware({ layout: null }) },
  });
  mockLists({ vm: [old], node: [NODE] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(await screen.findByText("Not available (started without it)")).toBeInTheDocument();
  expect(screen.queryByText("Restart needed")).not.toBeInTheDocument();
});

const NODE_B = { ...NODE, id: "n2", name: "node-b" };
const NODE_C = { ...NODE, id: "n3", name: "node-c" };
/** 복제본의 시점(`replica`의 `synced_at`)에서 252초 뒤 */
const AFTER_SYNC = (1_800_000_000 + 252) * 1000;

describe("replication card", () => {
  afterEach(() => vi.restoreAllMocks());

  it("shows each target's state, age, size, next run and error", async () => {
    vi.spyOn(Date, "now").mockReturnValue(AFTER_SYNC);
    const web = replicatedVm("running");
    const spec = {
      ...web.spec,
      replication: [
        { node: "n2", schedule: "*/15" },
        { node: "n3", schedule: "*/5", pool: "b" },
      ],
    };
    const status = {
      ...web.status,
      replication: [
        replica("n2"),
        replica("n3", {
          state: "syncing" as const,
          generation: undefined,
          synced_at: undefined,
          volumes: [],
          last_try: undefined,
          error: 'node "node-c" is offline',
        }),
      ],
    };
    mockLists({ vm: [{ ...web, spec, status }], node: [NODE, NODE_B, NODE_C] });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    const list = await screen.findByRole("list", { name: "Replication" });
    const [b, c] = within(list).getAllByRole("listitem");
    expect(await within(b!).findByText("node-b")).toBeInTheDocument();
    expect(within(b!).getByText("Ready")).toBeInTheDocument();
    expect(within(b!).getByText("every 15 min")).toBeInTheDocument();
    expect(within(b!).getByText(/^Copy taken at .* \(4m12s ago\)$/)).toBeInTheDocument();
    expect(
      within(b!).getByText("Last run: changes only, sent 12 MiB in 4 s · disks 16 GiB"),
    ).toBeInTheDocument();
    expect(within(b!).getByText(/^Next run at /)).toBeInTheDocument();
    expect(within(c!).getByText("Copying")).toBeInTheDocument();
    expect(within(c!).getByText("every 5 min · pool b")).toBeInTheDocument();
    expect(within(c!).getByText("No complete copy yet.")).toBeInTheDocument();
    expect(within(c!).getByText('Last run failed: node "node-c" is offline')).toBeInTheDocument();
    // 아직 시도가 없으면 곧 한다.
    expect(within(c!).getByText("Next run soon")).toBeInTheDocument();
  });

  it("replicates now to the chosen target and follows the task", async () => {
    const web = replicatedVm("running");
    mockLists({ vm: [web], node: [NODE, NODE_B] });
    const bodies: unknown[] = [];
    server.use(
      http.post(apiUrl(`/vms/${web.id}/replicate`), async ({ request }) => {
        bodies.push(await request.json());
        return HttpResponse.json(task("t-repl", "vm-replicate", web.id), { status: 202 });
      }),
    );
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    await userEvent.click(await screen.findByRole("button", { name: "Replicate now to node-b" }));
    expect(await screen.findByText("Replicate web to node-b")).toBeInTheDocument();
    expect(bodies).toEqual([{ node: "n2" }]);
  });

  it("shows the server's reason when replicating now fails", async () => {
    const web = replicatedVm("running");
    mockLists({ vm: [web], node: [NODE, NODE_B] });
    server.use(
      http.post(apiUrl(`/vms/${web.id}/replicate`), () =>
        HttpResponse.json(
          { code: "vm_busy", message: 'VM "web" is already being replicated to node "node-b"' },
          { status: 409 },
        ),
      ),
    );
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    await userEvent.click(await screen.findByRole("button", { name: "Replicate now to node-b" }));
    expect(
      await screen.findByText(/VM "web" is already being replicated to node "node-b"/),
    ).toBeInTheDocument();
  });

  it("does not replicate a stopped VM and says why", async () => {
    mockLists({ vm: [replicatedVm("stopped")], node: [NODE, NODE_B] });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    const button = await screen.findByRole("button", { name: "Replicate now to node-b" });
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAccessibleDescription("Only a running VM is replicated.");
    expect(screen.queryByText(/^Next run/)).not.toBeInTheDocument();
  });

  it("hides replicating now from viewers", async () => {
    mockLists({ vm: [replicatedVm("running")], node: [NODE, NODE_B] });
    renderPage(<VmDetailPage />, {
      path: "/vms/:name",
      url: "/vms/web",
      as: { ...ADMIN, role: "viewer" },
    });
    expect(await screen.findByRole("list", { name: "Replication" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Replicate now/ })).not.toBeInTheDocument();
  });

  it("keeps showing the HA recovery from a replica after the VM is up", async () => {
    const web = replicatedVm("running");
    const recovered = {
      ...web,
      spec: { ...web.spec, node: "n2", replication: [{ node: "n1", schedule: "*/15" }] },
      status: {
        ...web.status,
        node: "n2",
        last_error: null,
        last_recovery: { from: "n1", node: "n2", synced_at: 1_800_000_000, at: 1_800_000_300 },
        replication: [replica("n1")],
      },
    };
    mockLists({ vm: [recovered], node: [NODE, NODE_B] });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    expect(
      await screen.findByText(
        /^Last recovery at .*: node-a failed; restarted on node-b from the replica taken at .* \(writes after that were lost\)\.$/,
      ),
    ).toBeInTheDocument();
  });

  it("says when HA cannot use a ready replica because a disk changed", async () => {
    const web = replicatedVm("running");
    const added = {
      ...web,
      spec: {
        ...web.spec,
        disks: [...web.spec.disks!, { volume: "web-data", bus: "virtio-blk" as const }],
      },
    };
    mockLists({
      vm: [added],
      node: [NODE, NODE_B],
      volume: [volume("web-root", "local"), volume("web-data", "local")],
      "storage-pool": [pool("local")],
    });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    expect(
      await screen.findByText(
        "HA cannot use this replica: disk web-data changed after the last run (it can after the next run).",
      ),
    ).toBeInTheDocument();
  });

  it("shows the HA recovery from a replica and the old source", async () => {
    const web = replicatedVm("running");
    const recovered = {
      ...web,
      spec: { ...web.spec, node: "n2", replication: [{ node: "n1", schedule: "*/15" }] },
      status: {
        ...web.status,
        node: "n2",
        last_error:
          "recovering: node n1 failed; restarting from the replica on node n2 taken at 2027-01-15 08:00:00 UTC (writes after that are lost)",
        replication: [
          replica("n1", { state: "stale" as const, generation: undefined, volumes: [] }),
        ],
      },
    };
    mockLists({ vm: [recovered], node: [NODE, NODE_B] });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    const list = await screen.findByRole("list", { name: "Replication" });
    expect(within(list).getByText("Old source")).toBeInTheDocument();
    // 요약의 마지막 오류와 복제 카드에 모두 보인다.
    expect(screen.getAllByText(/restarting from the replica on node n2/)).toHaveLength(2);
  });

  it("warns about an HA VM whose local disk has no replica", async () => {
    const web = replicatedVm("running");
    const ha = {
      ...web,
      spec: { ...web.spec, ha: true, replication: [] },
      status: { ...web.status, replication: [] },
    };
    mockLists({
      vm: [ha],
      node: [NODE],
      volume: [volume("web-root", "local")],
      "storage-pool": [pool("local")],
    });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    expect(
      await screen.findByText(
        "This VM cannot be restarted on another node: disk web-root is on node-local pool local and has no replica. Add replication.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Replication" })).not.toBeInTheDocument();
  });

  it("shows nothing for a VM without replication or warning", async () => {
    mockLists({
      vm: [vm("web", "running")],
      node: [NODE],
      volume: [],
      "storage-pool": [],
    });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    expect(await screen.findByRole("heading", { name: "web" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Replication" })).not.toBeInTheDocument();
  });
});

describe("install media", () => {
  const nas = pool("nas", { backend: { kind: "nfs", server: "nas.lan", export: "/srv/iso" } });
  const running = installVm("web", "running", {
    status: {
      ...vm("web", "running").status,
      hardware: hardware({
        cdroms: [{ iso: "alpine" }, {}],
        cloud_init: "iid-0123456789abcdef",
        pending: [
          {
            field: "cloud_init",
            running: "iid-0123456789abcdef",
            wanted: "iid-fedcba9876543210",
            reason: "restart",
          },
          { field: "cdroms[1].boot_order", running: "4", wanted: "1", reason: "restart" },
        ],
      }),
    },
  });

  it("summarizes the boot order and cloud-init", async () => {
    mockLists({ vm: [running], node: [NODE] });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
    expect(await screen.findByText("disk0, cdrom0, cdrom1")).toBeInTheDocument();
    expect(
      screen.getByText("user orbi, 1 SSH key, password set, nic0 10.0.0.5/24 gw 10.0.0.1"),
    ).toBeInTheDocument();
    expect(screen.getByText("instance iid-0123456789abcdef")).toBeInTheDocument();
    expect(screen.getByText("Changed settings apply at the next restart.")).toBeInTheDocument();
    // 보류 목록은 CD-ROM 드라이브 번호를 넣어 보인다.
    const pending = screen.getByRole("list", { name: "Applies at the next restart" });
    expect(pending).toHaveTextContent("Boot order of CD-ROM drive 1");
    expect(pending).toHaveTextContent("cloud-init");
  });

  it("inserts and ejects media while the VM runs and tracks the task", async () => {
    mockLists({
      vm: [running],
      node: [NODE],
      volume: [isoVolume("alpine", "local"), isoVolume("ubuntu", "nas")],
      "storage-pool": [pool("local"), nas],
    });
    const bodies: unknown[] = [];
    server.use(
      http.post(apiUrl(`/vms/${running.id}/cdrom`), async ({ request }) => {
        bodies.push(await request.json());
        return HttpResponse.json(running, { headers: { "orbivisor-task": "t-hot" } });
      }),
      http.get(apiUrl("/tasks/t-hot"), () =>
        HttpResponse.json(task("t-hot", "vm-hotplug", running.id)),
      ),
    );
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web?tab=hardware" });
    const user = userEvent.setup();
    const table = await screen.findByRole("table", { name: "CD/DVD" });
    const rows = within(table).getAllByRole("row");
    expect(rows[1]).toHaveTextContent("cdrom0");
    expect(rows[1]).toHaveTextContent("alpine");
    expect(rows[2]).toHaveTextContent("Empty");
    expect(within(rows[2]!).getByRole("button", { name: "Eject drive 1" })).toBeDisabled();
    const choose = within(rows[2]!).getByLabelText("ISO for drive 1");
    await within(choose).findByRole("option", { name: "ubuntu" });
    await user.selectOptions(choose, "ubuntu");
    await user.click(within(rows[2]!).getByRole("button", { name: "Insert" }));
    expect(await screen.findByText("Insert ubuntu into drive 1 of web")).toBeInTheDocument();
    await user.click(within(rows[1]!).getByRole("button", { name: "Eject drive 0" }));
    await waitFor(() => expect(bodies).toHaveLength(2));
    expect(bodies).toEqual([
      { drive: 1, iso: "ubuntu" },
      { drive: 0, iso: null },
    ]);
  });

  it("shows media that is still being changed", async () => {
    const changing = {
      ...running,
      spec: { ...running.spec, cdroms: [{}, {}] },
    };
    mockLists({ vm: [changing], node: [NODE] });
    renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web?tab=hardware" });
    const table = await screen.findByRole("table", { name: "CD/DVD" });
    expect(within(table).getAllByRole("row")[1]).toHaveTextContent("changing from alpine");
  });
});

it("shows summary cards under the VM header", async () => {
  mockLists({ vm: [vm("web", "running")], node: [NODE] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  const heading = await screen.findByRole("heading", { level: 1, name: "web" });
  const cards = heading.closest("div")!.parentElement!.parentElement!.querySelector("dl")!;
  expect(within(cards).getByText("Node").nextSibling).toHaveTextContent("node-a");
  expect(within(cards).getByText("vCPUs")).toBeInTheDocument();
  expect(within(cards).getByText("Memory")).toBeInTheDocument();
});
