import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Node, Principal, Vm } from "@/api/types";
import { resetTasks } from "@/tasks/tasks";
import { NODE, pool, poolState, snapshot, task, vm, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { migrationTargets } from "@/vms/VmMigrateDialog";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const OPERATOR: Principal = {
  user: "00000000-0000-0000-0000-000000000002",
  name: "ops",
  role: "operator",
};
const VIEWER: Principal = { ...OPERATOR, name: "viewer", role: "viewer" };

function node(id: string, name: string, extra: Partial<Node["status"]> = {}, maintenance = false) {
  return {
    ...NODE,
    id,
    name,
    spec: { ...NODE.spec, maintenance },
    status: { ...NODE.status, arch: "aarch64", ...extra },
  } satisfies Node;
}

// 기본 VM은 `virt`(aarch64) 머신으로 n1에서 돈다.
const HERE = node("n1", "node-a");
const NODES = [
  HERE,
  node("n2", "node-b"),
  node("n3", "node-c", { online: false }),
  node("n4", "node-d", {}, true),
  node("n5", "node-e", { arch: "x86_64" }),
  node("n6", "node-f", { arch: null }),
  node("n7", "node-g"),
];

/** 서버처럼 `migration`을 빼고 보낸 VM */
function served(v: Vm): Vm {
  const status: Partial<Vm["status"]> = { ...v.status };
  delete status.migration;
  return { ...v, status } as Vm;
}

const web = served(vm("web", "running"));

function openDetail(as = OPERATOR) {
  return renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web", as });
}

it("offers online nodes of the same architecture other than the current one", () => {
  expect(migrationTargets(web, NODES).map((n) => n.name)).toEqual(["node-b", "node-g"]);
  // x86_64 머신(q35)은 x86_64 노드로만
  const q35 = { ...web, spec: { ...web.spec, machine: "q35" as const } };
  expect(migrationTargets(q35, NODES).map((n) => n.name)).toEqual(["node-e"]);
  // 위트니스는 VM을 받지 않는다.
  const witness = { ...node("n8", "node-w"), spec: { ...NODE.spec, witness: true } };
  expect(migrationTargets(web, [...NODES, witness]).map((n) => n.name)).toEqual([
    "node-b",
    "node-g",
  ]);
});

it("migrates a running VM to the chosen node", async () => {
  mockLists({ vm: [web], node: NODES });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${web.id}/migrate`), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-migrate", web.id), { status: 202 });
    }),
  );
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  const target = within(dialog).getByLabelText("Target node");
  await waitFor(() =>
    expect(
      within(target)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["node-b", "node-g"]),
  );
  await userEvent.selectOptions(target, "n7");
  await userEvent.click(within(dialog).getByRole("button", { name: "Migrate" }));
  expect(await screen.findByText("Migrate web to node-g")).toBeInTheDocument();
  expect(bodies).toEqual([{ node: "n7" }]);
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("shows the server's refusal in the dialog", async () => {
  mockLists({ vm: [web], node: NODES });
  server.use(
    http.post(apiUrl(`/vms/${web.id}/migrate`), () =>
      HttpResponse.json(
        {
          code: "migration_unsupported",
          message: 'disk "web-root" is not on a shared storage pool',
        },
        { status: 409 },
      ),
    ),
  );
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await waitFor(() => expect(within(dialog).getByLabelText("Target node")).toHaveValue("n2"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Migrate" }));
  expect(
    await within(dialog).findByText(
      'This VM cannot be migrated: disk "web-root" is not on a shared storage pool',
    ),
  ).toBeInTheDocument();
});

it("says when no node can take the VM", async () => {
  mockLists({ vm: [web], node: [HERE, node("n3", "node-c", { online: false })] });
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  expect(
    await within(dialog).findByText(
      "No other online node with the VM's architecture is available.",
    ),
  ).toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Migrate" })).toBeDisabled();
});

it("blocks other actions and shows the target while migrating", async () => {
  const migrating = {
    ...web,
    status: { ...web.status, phase: "migrating" as const, migration: { node: "n2", task: "t1" } },
  };
  mockLists({ vm: [migrating], node: NODES });
  openDetail();
  expect(await screen.findByText("Migrating")).toBeInTheDocument();
  expect(await screen.findByText("node-a → node-b")).toBeInTheDocument();
  for (const name of ["Migrate", "Stop", "Force stop", "Reboot"]) {
    const button = screen.getByRole("button", { name });
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAccessibleDescription("Not available while the VM is being migrated.");
  }
  // 콘솔은 열 수 있다(원본 QEMU가 계속 돈다).
  expect(screen.getByRole("link", { name: "Open console" })).not.toHaveAttribute("aria-disabled");
});

it("shows the source from status.node while migrating", async () => {
  // 확정 쓰기 사이: spec.node는 벌써 대상이지만 QEMU는 아직 원본(n1)에서 돈다.
  const committing = {
    ...web,
    spec: { ...web.spec, node: "n2" },
    status: { ...web.status, phase: "migrating" as const, migration: { node: "n2", task: "t1" } },
  };
  mockLists({ vm: [committing], node: NODES });
  const { unmount } = openDetail();
  expect(await screen.findByText("node-a → node-b")).toBeInTheDocument();
  unmount();

  // 대상이 지금 노드면 화살표를 보이지 않는다.
  const moved = {
    ...committing,
    status: { ...committing.status, phase: "running" as const, node: "n2" },
  };
  mockLists({ vm: [moved], node: NODES });
  openDetail();
  expect(await screen.findByText("node-b")).toBeInTheDocument();
  expect(screen.queryByText(/→/)).not.toBeInTheDocument();
});

it("does not offer migration to a stopped VM or a viewer", async () => {
  const stopped = served(vm("web", "stopped"));
  mockLists({ vm: [stopped], node: NODES });
  const { unmount } = openDetail();
  const button = await screen.findByRole("button", { name: "Migrate" });
  expect(button).toHaveAttribute("aria-disabled", "true");
  unmount();

  mockLists({ vm: [web], node: NODES });
  openDetail(VIEWER);
  expect(await screen.findByRole("heading", { name: "web" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Migrate" })).not.toBeInTheDocument();
});

/** 로컬 디렉터리 풀 `local`의 디스크 `web-root`(16 GiB)로 도는 VM */
const local = served(
  vm("web", "running", {
    spec: {
      ...web.spec,
      disks: [{ volume: "web-root", bus: "virtio-blk" }],
    },
  }),
);
const POOLS = [pool("local"), pool("fast"), pool("nas", { shared: true })];

it("copies disks on node-local pools to the chosen pool", async () => {
  const nodes = [
    HERE,
    node("n2", "node-b"),
    node("n7", "node-g", { pools: { fast: poolState("fast", { ready: false }) } }),
  ];
  mockLists({
    vm: [local],
    node: nodes,
    volume: [volume("web-root", "local")],
    "storage-pool": POOLS,
  });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${local.id}/migrate`), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-migrate", local.id), { status: 202 });
    }),
  );
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  expect(await within(dialog).findByText("web-root on local, 16 GiB")).toBeInTheDocument();
  expect(
    within(dialog).getByText(
      "1 disk (16 GiB in total) will be copied. This can take a long time, and the VM cannot be stopped, snapshotted or backed up meanwhile.",
    ),
  ).toBeInTheDocument();
  const pools = within(dialog).getByLabelText("Target pool");
  const options = () =>
    within(pools)
      .getAllByRole("option")
      .map((o) => o.textContent);
  // 원본 풀과 같은 이름(`local`)은 기본과 같으므로 따로 두지 않는다.
  await waitFor(() => expect(options()).toEqual(["Pool with the same name", "fast", "nas"]));
  // 고른 노드에서 준비되지 않은 풀은 고를 수 없다.
  await userEvent.selectOptions(within(dialog).getByLabelText("Target node"), "n7");
  expect(options()).toEqual(["Pool with the same name", "nas"]);
  await userEvent.selectOptions(within(dialog).getByLabelText("Target node"), "n2");
  await userEvent.selectOptions(pools, "fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Migrate" }));
  expect(await screen.findByText("Migrate web to node-b")).toBeInTheDocument();
  expect(bodies).toEqual([{ node: "n2", pool: "fast" }]);
});

it("asks for a target pool when the chosen node has no pool of the same name", async () => {
  const nodes = [
    HERE,
    node("n2", "node-b", { pools: { local: poolState("local", { ready: false }) } }),
  ];
  mockLists({
    vm: [local],
    node: nodes,
    volume: [volume("web-root", "local")],
    "storage-pool": POOLS,
  });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${local.id}/migrate`), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-migrate", local.id), { status: 202 });
    }),
  );
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "node-b has no usable local pool. Choose a target pool.",
  );
  const submit = within(dialog).getByRole("button", { name: "Migrate" });
  expect(submit).toBeDisabled();
  await userEvent.selectOptions(within(dialog).getByLabelText("Target pool"), "fast");
  expect(within(dialog).queryByText(/has no usable/)).not.toBeInTheDocument();
  await userEvent.click(submit);
  expect(await screen.findByText("Migrate web to node-b")).toBeInTheDocument();
  expect(bodies).toEqual([{ node: "n2", pool: "fast" }]);
});

it("sends no pool for the same-name pool and offers none without local disks", async () => {
  mockLists({
    vm: [local],
    node: NODES,
    volume: [volume("web-root", "local")],
    "storage-pool": POOLS,
  });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${local.id}/migrate`), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-migrate", local.id), { status: 202 });
    }),
  );
  const { unmount } = openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  let dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await within(dialog).findByLabelText("Target pool");
  await userEvent.click(within(dialog).getByRole("button", { name: "Migrate" }));
  await waitFor(() => expect(bodies).toEqual([{ node: "n2" }]));
  unmount();

  // 공유 풀의 디스크는 복사하지 않는다.
  mockLists({
    vm: [local],
    node: NODES,
    volume: [volume("web-root", "nas")],
    "storage-pool": POOLS,
  });
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await waitFor(() => expect(within(dialog).getByLabelText("Target node")).toHaveValue("n2"));
  expect(within(dialog).queryByLabelText("Target pool")).not.toBeInTheDocument();
  expect(within(dialog).queryByText(/will be copied/)).not.toBeInTheDocument();
});

it("does not copy disks of a VM with snapshots", async () => {
  mockLists({
    vm: [local],
    node: NODES,
    volume: [volume("web-root", "local")],
    "storage-pool": POOLS,
    snapshot: [snapshot("s1", local, 1)],
  });
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  expect(
    await within(dialog).findByText(
      "VMs with snapshots cannot be migrated with disks on node-local pools. Delete the VM's snapshots first.",
    ),
  ).toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Migrate" })).toBeDisabled();
});

it("waits for the volume, pool and snapshot lists before migrating", async () => {
  mockLists({
    vm: [local],
    node: NODES,
    volume: [volume("web-root", "local")],
    "storage-pool": POOLS,
  });
  let release = () => {};
  const loaded = new Promise<void>((resolve) => (release = resolve));
  server.use(
    http.get(apiUrl("/snapshots"), async () => {
      await loaded;
      return HttpResponse.json({ revision: 1, items: [snapshot("s1", local, 1)] });
    }),
  );
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await within(dialog).findByText("web-root on local, 16 GiB");
  await waitFor(() => expect(within(dialog).getByLabelText("Target node")).toHaveValue("n2"));
  const submit = within(dialog).getByRole("button", { name: "Migrate" });
  expect(submit).toBeDisabled();
  release();
  expect(
    await within(dialog).findByText(/VMs with snapshots cannot be migrated/),
  ).toBeInTheDocument();
  expect(submit).toBeDisabled();
});

it("shows the progress of the migration task", async () => {
  const migrating = {
    ...local,
    status: { ...local.status, phase: "migrating" as const, migration: { node: "n2", task: "t1" } },
  };
  const copying = {
    ...task("t1", "vm-migrate", local.id),
    status: {
      state: "running" as const,
      error: null,
      progress: {
        stage: "copying-disks",
        done_bytes: Math.ceil(0.37 * 40 * 1024 ** 3),
        total_bytes: 40 * 1024 ** 3,
      },
    },
  };
  mockLists({ vm: [migrating], node: NODES, task: [copying] });
  const { unmount } = openDetail();
  expect(await screen.findByText("Copying disks 37% (14.8/40.0 GiB)")).toBeInTheDocument();
  unmount();

  const memory = {
    ...copying,
    status: {
      ...copying.status,
      progress: { ...copying.status.progress, stage: "migrating-memory" },
    },
  };
  mockLists({ vm: [migrating], node: NODES, task: [memory] });
  openDetail();
  expect(await screen.findByText("Migrating memory")).toBeInTheDocument();
});

it("lets an admin ignore placement rules after the server refuses", async () => {
  mockLists({ vm: [web], node: NODES });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${web.id}/migrate`), async ({ request }) => {
      const body = (await request.json()) as { force?: boolean };
      bodies.push(body);
      return body.force
        ? HttpResponse.json(task("t1", "vm-migrate", web.id), { status: 202 })
        : HttpResponse.json(
            {
              code: "placement_rule",
              message: 'node "node-b" breaks placement rules: spread',
              details: { rules: ["spread"] },
            },
            { status: 409 },
          );
    }),
  );
  openDetail(ADMIN);
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await waitFor(() => expect(within(dialog).getByLabelText("Target node")).toHaveValue("n2"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Migrate" }));
  expect(
    await within(dialog).findByText(
      'Placement rules leave no node for this VM: node "node-b" breaks placement rules: spread',
    ),
  ).toBeInTheDocument();
  await userEvent.click(within(dialog).getByLabelText("Ignore placement rules"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Migrate" }));
  expect(await screen.findByText("Migrate web to node-b")).toBeInTheDocument();
  expect(bodies).toEqual([{ node: "n2" }, { node: "n2", force: true }]);
});

it("does not offer ignoring placement rules to operators", async () => {
  mockLists({ vm: [web], node: NODES });
  openDetail(OPERATOR);
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await waitFor(() => expect(within(dialog).getByLabelText("Target node")).toHaveValue("n2"));
  expect(within(dialog).queryByLabelText("Ignore placement rules")).not.toBeInTheDocument();
});
