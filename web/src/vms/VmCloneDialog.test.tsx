import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Principal, Vm } from "@/api/types";
import { resetTasks } from "@/tasks/tasks";
import { NODE, pool, poolState, snapshot, task, vm, volume } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { cloneNode } from "@/vms/VmCloneDialog";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { VmListPage } from "@/vms/VmListPage";

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

function withDisk(v: Vm): Vm {
  return {
    ...v,
    spec: { ...v.spec, disks: [{ volume: `${v.name}-root`, bus: "virtio-blk", boot_order: 1 }] },
  };
}

const db = withDisk(vm("db", "stopped"));
const web = withDisk(vm("web", "running"));

function openDetail(v: Vm, as = OPERATOR) {
  return renderPage(<VmDetailPage />, { path: "/vms/:name", url: `/vms/${v.name}`, as });
}

/** 보낸 복제 요청의 본문을 모은다. */
function captureClone(v: Vm) {
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${v.id}/clone`), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-clone", v.id), { status: 202 });
    }),
  );
  return bodies;
}

it("clones a stopped VM's current state from the list menu", async () => {
  mockLists({
    vm: [db, web],
    node: [NODE],
    volume: [volume("db-root", "local")],
    // 복제 Task가 돌 노드(n1)에서 쓸 수 없는 풀은 고를 수 없다.
    "storage-pool": [
      pool("local"),
      pool("fast", { nodes: ["n1"] }),
      pool("far", { nodes: ["n2"] }),
    ],
  });
  const bodies = captureClone(db);
  renderPage(<VmListPage />, { as: OPERATOR });
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  expect(within(dialog).getByLabelText("Source")).toHaveValue("current");
  const pools = within(dialog).getByLabelText("Storage pool for the disks");
  await waitFor(() =>
    expect(
      within(pools)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Same pool as each source disk", "local", "fast"]),
  );

  // 이미 있는 VM 이름은 보내기 전에 알린다.
  const name = within(dialog).getByLabelText("New VM name");
  await userEvent.type(name, "web");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  expect(name).toHaveAccessibleDescription("A VM with this name already exists.");
  expect(name).toBeInvalid();
  expect(bodies).toEqual([]);

  await userEvent.clear(name);
  await userEvent.type(name, "db-copy");
  await userEvent.selectOptions(pools, "fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  expect(await screen.findByText("Clone db to db-copy")).toBeInTheDocument();
  expect(bodies).toEqual([{ name: "db-copy", snapshot: null, pool: "fast" }]);
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
});

it("clones a running VM from its newest ready snapshot", async () => {
  const base = snapshot("base", web, 1);
  const upgraded = snapshot("upgraded", web, 2, { spec: { parent: base.id } });
  const taking = snapshot("taking", web, 3, { status: { phase: "pending", created_at: null } });
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    "storage-pool": [pool("local")],
    snapshot: [base, upgraded, taking, snapshot("other", db, 4)],
  });
  const bodies = captureClone(web);
  openDetail(web);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  const source = within(dialog).getByLabelText("Source");
  // 만드는 중이거나 다른 VM의 스냅샷은 고를 수 없다.
  await waitFor(() =>
    expect(
      within(source)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Current state (stopped VM only)", "Snapshot upgraded", "Snapshot base"]),
  );
  expect(source).toHaveValue(upgraded.id);
  const submit = within(dialog).getByRole("button", { name: "Clone" });

  // 실행 중인 VM의 지금 상태는 복제할 수 없고, 그 까닭을 보인다.
  await userEvent.selectOptions(source, "current");
  expect(source).toHaveAccessibleDescription(
    "The VM is not stopped. Stop it first or choose a snapshot.",
  );
  expect(submit).toBeDisabled();

  await userEvent.selectOptions(source, base.id);
  expect(source).not.toHaveAccessibleDescription();
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "web-old");
  await userEvent.click(submit);
  expect(await screen.findByText("Clone web to web-old")).toBeInTheDocument();
  expect(bodies).toEqual([{ name: "web-old", snapshot: base.id, pool: null }]);
});

it("keeps a chosen snapshot that goes away and blocks cloning", async () => {
  const base = snapshot("base", web, 1);
  const upgraded = snapshot("upgraded", web, 2, { spec: { parent: base.id } });
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base, upgraded],
  });
  const bodies = captureClone(web);
  const { queryClient } = openDetail(web);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  const source = within(dialog).getByLabelText("Source");
  await waitFor(() => expect(source).toHaveValue(upgraded.id));
  await userEvent.selectOptions(source, base.id);
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "web-old");

  // 고른 스냅샷이 지워졌다. 다른 스냅샷으로 바꾸지 않고 까닭을 보인다.
  act(() => queryClient.setQueryData(["list", "snapshot"], { revision: 2, items: [upgraded] }));
  await waitFor(() =>
    expect(source).toHaveAccessibleDescription(
      "The chosen snapshot no longer exists or is not ready. Choose another source.",
    ),
  );
  expect(source).toHaveValue(base.id);
  expect(within(source).getByRole("option", { selected: true })).toHaveTextContent(
    "Chosen snapshot (no longer available)",
  );
  const submit = within(dialog).getByRole("button", { name: "Clone" });
  expect(submit).toBeDisabled();
  await userEvent.click(submit);
  expect(bodies).toEqual([]);

  // 다시 고르면 풀린다.
  await userEvent.selectOptions(source, upgraded.id);
  expect(source).not.toHaveAccessibleDescription();
  await userEvent.click(submit);
  expect(await screen.findByText("Clone web to web-old")).toBeInTheDocument();
  expect(bodies).toEqual([{ name: "web-old", snapshot: upgraded.id, pool: null }]);
});

it("explains why a running VM without snapshots cannot be cloned", async () => {
  mockLists({ vm: [web], node: [NODE], volume: [volume("web-root", "local")] });
  openDetail(web);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  expect(within(dialog).getByLabelText("Source")).toHaveAccessibleDescription(
    "The VM is not stopped. Stop it first or choose a snapshot.",
  );
  expect(within(dialog).getByRole("button", { name: "Clone" })).toBeDisabled();
});

it("blocks cloning while the VM is starting", async () => {
  const starting = withDisk(vm("web", "starting"));
  mockLists({
    vm: [starting],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [snapshot("base", starting, 1)],
  });
  openDetail(starting);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  expect(
    within(dialog).getByText(
      "The VM is starting, stopping or migrating. Try again when it has settled.",
    ),
  ).toBeVisible();
  expect(within(dialog).getByRole("button", { name: "Clone" })).toBeDisabled();
});

it("blocks cloning while a stop has yet to finish", async () => {
  // 정지를 요청했지만 게스트가 아직 꺼지지 않았다(서버의 check_not_busy).
  const halting = withDisk(vm("web", "running"));
  halting.spec.power = "stopped";
  mockLists({
    vm: [halting],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [snapshot("base", halting, 1)],
  });
  openDetail(halting);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  expect(
    within(dialog).getByText(
      "The VM is starting, stopping or migrating. Try again when it has settled.",
    ),
  ).toBeVisible();
  expect(within(dialog).getByRole("button", { name: "Clone" })).toBeDisabled();
});

it("shows the server's refusal in the dialog", async () => {
  mockLists({ vm: [db], node: [NODE], volume: [volume("db-root", "local")] });
  server.use(
    http.post(apiUrl(`/vms/${db.id}/clone`), () =>
      HttpResponse.json({ code: "vm_busy", message: "busy" }, { status: 409 }),
    ),
  );
  openDetail(db);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "db-copy");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Another operation on this VM (start, stop, snapshot, revert, clone or migration) is in progress. Try again when it has finished.",
  );
});

it("points at the pool when the VM's node cannot use it", async () => {
  mockLists({
    vm: [db],
    node: [NODE],
    volume: [volume("db-root", "local")],
    "storage-pool": [pool("local"), pool("fast")],
  });
  server.use(
    http.post(apiUrl(`/vms/${db.id}/clone`), () =>
      HttpResponse.json({ code: "pool_not_on_node", message: "elsewhere" }, { status: 409 }),
    ),
  );
  openDetail(db);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "db-copy");
  const pools = within(dialog).getByLabelText("Storage pool for the disks");
  await waitFor(() => expect(within(pools).getAllByRole("option")).toHaveLength(3));
  await userEvent.selectOptions(pools, "fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  // 업로드의 "접속한 노드" 문구가 아니라 VM이 있는 노드를 말한다.
  await waitFor(() =>
    expect(pools).toHaveAccessibleDescription(
      "That storage pool is not available on the node holding the VM.",
    ),
  );
  expect(pools).toBeInvalid();
  expect(screen.getByRole("dialog", { name: "Clone db" })).toBeInTheDocument();
});

it("offers only pools ready on the VM's node and points at one that is not", async () => {
  const nas = pool("nas", { backend: { kind: "nfs", server: "nas.lan", export: "/srv/vm" } });
  const fast = pool("fast", { backend: { kind: "nfs", server: "fast.lan", export: "/srv/vm" } });
  const here = {
    ...NODE,
    status: {
      ...NODE.status,
      pools: { nas: poolState("nas", { ready: false, error: "down" }), fast: poolState("fast") },
    },
  };
  mockLists({
    vm: [db],
    node: [here],
    volume: [volume("db-root", "local")],
    "storage-pool": [pool("local"), nas, fast],
  });
  // 대화상자를 연 사이 준비되지 않게 됐다.
  server.use(
    http.post(apiUrl(`/vms/${db.id}/clone`), () =>
      HttpResponse.json({ code: "pool_unavailable", message: "down" }, { status: 409 }),
    ),
  );
  openDetail(db);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "db-copy");
  const pools = within(dialog).getByLabelText("Storage pool for the disks");
  await waitFor(() =>
    expect(
      within(pools)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Same pool as each source disk", "local", "fast"]),
  );
  await userEvent.selectOptions(pools, "fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  await waitFor(() =>
    expect(pools).toHaveAccessibleDescription(
      "That storage pool is not ready on the node holding the VM.",
    ),
  );
  expect(pools).toBeInvalid();
});

it("picks the node like the server's vm_node", () => {
  const free = withDisk(vm("db", "stopped", { spec: { ...db.spec, node: null } }));
  const vols = [volume("db-root", "local", { node: "n3" })];
  // 고정한 노드, 놓인 노드
  expect(cloneNode(db, undefined, "n9")).toBe("n1");
  expect(cloneNode({ ...free, status: { ...free.status, node: "n2" } }, undefined, "n9")).toBe(
    "n2",
  );
  // 첫 디스크 볼륨의 노드. 볼륨 목록을 읽기 전에는 모른다.
  expect(cloneNode(free, vols, "n9")).toBe("n3");
  expect(cloneNode(free, undefined, "n9")).toBeUndefined();
  // 볼륨이 노드에 없거나 디스크가 없으면 접속한 노드(요청을 받는 노드)
  expect(cloneNode(free, [volume("db-root", "local", { node: null })], "n9")).toBe("n9");
  expect(cloneNode({ ...free, spec: { ...free.spec, disks: [] } }, undefined, "n9")).toBe("n9");
  expect(cloneNode(free, [], undefined)).toBeUndefined();
});

it("offers the pools of the connected node when the VM has no node", async () => {
  const free = withDisk(vm("db", "stopped", { spec: { ...db.spec, node: null } }));
  mockLists({
    vm: [free],
    node: [NODE, { ...NODE, id: "n2", name: "node-b" }],
    volume: [volume("db-root", "local", { node: null })],
    "storage-pool": [pool("all"), pool("here", { nodes: ["n2"] }), pool("far", { nodes: ["n1"] })],
  });
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n2",
        members: [],
      }),
    ),
  );
  openDetail(free);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  const pools = within(dialog).getByLabelText("Storage pool for the disks");
  await waitFor(() =>
    expect(
      within(pools)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Same pool as each source disk", "all", "here"]),
  );
});

it("offers no pools until it knows the node", async () => {
  const free = withDisk(vm("db", "stopped", { spec: { ...db.spec, node: null } }));
  mockLists({ vm: [free], node: [NODE], "storage-pool": [pool("all"), pool("far")] });
  // 볼륨 목록이 오지 않는다.
  server.use(http.get(apiUrl("/volumes"), () => new Promise<never>(() => {})));
  openDetail(free);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  await new Promise((r) => setTimeout(r, 50));
  expect(within(dialog).getByRole("status")).toHaveTextContent("Loading");
  expect(
    within(within(dialog).getByLabelText("Storage pool for the disks"))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["Same pool as each source disk"]);
});

it("hides cloning from viewers", async () => {
  mockLists({ vm: [db], node: [NODE] });
  openDetail(db, VIEWER);
  await screen.findByRole("heading", { name: "db" });
  expect(screen.queryByRole("button", { name: "Clone" })).not.toBeInTheDocument();
});
