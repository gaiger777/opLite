import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Vm } from "@/api/types";
import { formatTime } from "@/lib/format";
import { resetTasks } from "@/tasks/tasks";
import { NODE, pool, snapshot, task, vm, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

afterEach(resetTasks);

function withDisk(v: Vm): Vm {
  return {
    ...v,
    spec: { ...v.spec, disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }] },
  };
}

const web = withDisk(vm("web", "running"));
const base = snapshot("base", web, 1, {
  spec: { description: "fresh install", memory: true },
  status: { vmstate_bytes: 1536 * 1024 ** 2 },
});
const upgraded = snapshot("upgraded", web, 2, { spec: { parent: base.id, memory: true } });
const taking = snapshot("taking", web, 3, {
  spec: { parent: upgraded.id },
  status: { phase: "pending", created_at: null },
});
// 다른 VM의 스냅샷은 보이지 않는다.
const other = snapshot("other", vm("db", "stopped"), 4);

function open(v: Vm, as = ADMIN) {
  return renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: `/vms/${v.name}?tab=snapshots`,
    as,
  });
}

it("shows the VM's snapshots as a tree with the current position", async () => {
  const at = { ...web, status: { ...web.status, snapshot: upgraded.id } };
  mockLists({
    vm: [at],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [taking, other, upgraded, base],
  });
  open(at);
  const table = await screen.findByRole("table", { name: "Snapshots" });
  await within(table).findByText("taking");
  const [, first, second, third] = within(table).getAllByRole("row") as [
    HTMLElement,
    HTMLElement,
    HTMLElement,
    HTMLElement,
  ];
  const rows = [first, second, third];
  // 깊이는 스크린 리더에 부모 이름으로 알리고, 이름과 "현재" 표시는 붙지 않는다.
  expect(rows.map((r) => within(r).getAllByRole("cell")[0]!.textContent)).toEqual([
    "base",
    "upgraded (after base) Current",
    "taking (after upgraded)",
  ]);
  expect(first).toHaveTextContent(formatTime(new Date(1_792_000_000 * 1000), "en"));
  expect(first).toHaveTextContent("Included (1.5 GiB)");
  expect(first).toHaveTextContent("fresh install");
  expect(second).toHaveTextContent("Included");
  expect(third).toHaveTextContent("No");
  // 만드는 중인 스냅샷은 되돌리거나 지울 수 없다.
  expect(third).toHaveTextContent("Creating");
  expect(within(third).getByRole("button", { name: "Revert to taking" })).toBeDisabled();
  expect(within(third).getByRole("button", { name: "Delete snapshot taking" })).toBeDisabled();
  expect(within(first).getByRole("button", { name: "Revert to base" })).toBeEnabled();
});

it("takes a memory snapshot of a running VM and follows its task", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/snapshots`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "snapshot-create", web.id), { status: 202 });
    }),
  );
  open(web);
  await userEvent.click(await screen.findByRole("button", { name: "Take snapshot" }));
  const dialog = await screen.findByRole("dialog", { name: "Take a snapshot of web" });
  expect(within(dialog).getByLabelText("Include memory")).toBeChecked();
  await userEvent.type(within(dialog).getByLabelText("Name"), "base");
  await userEvent.click(within(dialog).getByRole("button", { name: "Take snapshot" }));
  expect(within(dialog).getByText("This VM already has a snapshot with this name.")).toBeVisible();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "before-upgrade");
  await userEvent.type(within(dialog).getByLabelText("Description"), "kernel 6.12");
  await userEvent.click(within(dialog).getByRole("button", { name: "Take snapshot" }));
  expect(await screen.findByText("Take snapshot before-upgrade of web")).toBeInTheDocument();
  expect(body).toEqual({ name: "before-upgrade", description: "kernel 6.12", memory: true });
});

it("offers only a disk snapshot of a stopped VM", async () => {
  const off = withDisk(vm("web", "stopped"));
  mockLists({ vm: [off], node: [NODE], volume: [volume("web-root", "local")] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${off.id}/snapshots`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "snapshot-create", off.id), { status: 202 });
    }),
  );
  open(off);
  expect(await screen.findByText("No snapshots.")).toBeInTheDocument();
  await userEvent.click(await screen.findByRole("button", { name: "Take snapshot" }));
  const dialog = await screen.findByRole("dialog", { name: "Take a snapshot of web" });
  expect(within(dialog).queryByLabelText("Include memory")).not.toBeInTheDocument();
  await userEvent.type(within(dialog).getByLabelText("Name"), "base");
  await userEvent.click(within(dialog).getByRole("button", { name: "Take snapshot" }));
  expect(await screen.findByText("Take snapshot base of web")).toBeInTheDocument();
  expect(body).toEqual({ name: "base", description: "", memory: false });
  await waitFor(() => expect(screen.getByRole("button", { name: "Take snapshot" })).toHaveFocus());
});

it("takes only a disk snapshot of a running VM without disks", async () => {
  const diskless = vm("web", "running");
  mockLists({ vm: [diskless], node: [NODE], volume: [] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${diskless.id}/snapshots`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "snapshot-create", diskless.id), { status: 202 });
    }),
  );
  open(diskless);
  await userEvent.click(await screen.findByRole("button", { name: "Take snapshot" }));
  const dialog = await screen.findByRole("dialog", { name: "Take a snapshot of web" });
  expect(within(dialog).queryByLabelText("Include memory")).not.toBeInTheDocument();
  await userEvent.type(within(dialog).getByLabelText("Name"), "base");
  await userEvent.click(within(dialog).getByRole("button", { name: "Take snapshot" }));
  expect(await screen.findByText("Take snapshot base of web")).toBeInTheDocument();
  expect(body).toEqual({ name: "base", description: "", memory: false });
});

it("says why a snapshot cannot be taken before the volumes are read", async () => {
  mockLists({ vm: [web], node: [NODE], snapshot: [base] });
  server.use(
    http.get(apiUrl("/volumes"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  open(web);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
});

it("says why a VM with a raw disk cannot take snapshots", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [
      { ...volume("web-root", "local"), spec: { pool: "local", size_bytes: 1, format: "raw" } },
    ],
  });
  open(web);
  expect(
    await screen.findByText(
      "Only VMs whose disks are all qcow2, or all in LVM-thin pools, can take snapshots. web-root is raw.",
    ),
  ).toBeVisible();
  expect(screen.queryByRole("button", { name: "Take snapshot" })).not.toBeInTheDocument();
});

const fast = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });
const lv = (name: string) => {
  const v = volume(name, "fast");
  return { ...v, spec: { ...v.spec, format: "raw" as const } };
};

it("takes a running VM's snapshot of disks in LVM-thin pools without memory", async () => {
  mockLists({ vm: [web], node: [NODE], volume: [lv("web-root")], "storage-pool": [fast] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/snapshots`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "snapshot-create", web.id), { status: 202 });
    }),
  );
  open(web);
  await userEvent.click(await screen.findByRole("button", { name: "Take snapshot" }));
  const dialog = await screen.findByRole("dialog", { name: "Take a snapshot of web" });
  // LV 스냅샷은 메모리를 담지 못한다(서버가 409).
  expect(within(dialog).queryByLabelText("Include memory")).not.toBeInTheDocument();
  expect(
    within(dialog).getByText(
      "Disks in LVM-thin pools are saved without memory. A running VM pauses briefly while they are saved.",
    ),
  ).toBeVisible();
  await userEvent.type(within(dialog).getByLabelText("Name"), "base");
  await userEvent.click(within(dialog).getByRole("button", { name: "Take snapshot" }));
  expect(await screen.findByText("Take snapshot base of web")).toBeInTheDocument();
  expect(body).toEqual({ name: "base", description: "", memory: false });
});

it("does not promise memory in the schedule of a VM with disks in LVM-thin pools", async () => {
  const scheduled = {
    ...web,
    spec: { ...web.spec, snapshot_schedule: { every: "daily" as const, keep: 7, memory: true } },
  };
  mockLists({ vm: [scheduled], node: [NODE], volume: [lv("web-root")], "storage-pool": [fast] });
  open(scheduled);
  // 예약 작업이 이런 VM은 메모리 없이 찍는다.
  expect(await screen.findByText("Snapshot schedule: Daily, keep 7")).toBeInTheDocument();
  expect(screen.queryByText(/with memory/)).not.toBeInTheDocument();
});

it("says why a VM mixing snapshot methods cannot take snapshots", async () => {
  const both: Vm = {
    ...web,
    spec: {
      ...web.spec,
      disks: [
        { volume: "web-root", bus: "virtio-blk", boot_order: 1 },
        { volume: "web-data", bus: "virtio-blk", boot_order: null },
      ],
    },
  };
  mockLists({
    vm: [both],
    node: [NODE],
    volume: [volume("web-root", "local"), lv("web-data")],
    "storage-pool": [pool("local"), fast],
  });
  open(both);
  expect(
    await screen.findByText(
      "Only VMs whose disks are all qcow2, or all in LVM-thin pools, can take snapshots. This VM has both.",
    ),
  ).toBeVisible();
  expect(screen.queryByRole("button", { name: "Take snapshot" })).not.toBeInTheDocument();
});

it("says why snapshots wait while the VM is starting", async () => {
  const starting = withDisk(vm("web", "starting"));
  mockLists({
    vm: [starting],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [snapshot("base", starting, 1)],
  });
  open(starting);
  expect(
    await screen.findByText(
      "The VM is starting or stopping. Snapshots can be taken, reverted and deleted once it settles.",
    ),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
  expect(await screen.findByRole("button", { name: "Revert to base" })).toBeDisabled();
});

it("waits like the server while the VM has yet to start", async () => {
  // 켜려는데 아직 멈춰 있다(노드가 막 받았거나 내려갔다). 서버는 vm_busy로 거절한다.
  const pending = withDisk(vm("web", "stopped"));
  pending.spec.power = "running";
  mockLists({
    vm: [pending],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [snapshot("base", pending, 1)],
  });
  open(pending);
  expect(
    await screen.findByText(
      "The VM is starting or stopping. Snapshots can be taken, reverted and deleted once it settles.",
    ),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
  expect(await screen.findByRole("button", { name: "Revert to base" })).toBeDisabled();
});

it("reverts after warning that the current state is lost", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base, upgraded],
  });
  let reverted = false;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/snapshots/${upgraded.id}/revert`), () => {
      reverted = true;
      return HttpResponse.json(task("t1", "snapshot-revert", web.id), { status: 202 });
    }),
  );
  open(web);
  await userEvent.click(await screen.findByRole("button", { name: "Revert to upgraded" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Revert web to upgraded?" });
  expect(dialog).toHaveTextContent("The current state of web is lost.");
  expect(dialog).toHaveTextContent("resumes running from the saved memory");
  await userEvent.click(within(dialog).getByRole("button", { name: "Revert" }));
  expect(await screen.findByText("Revert web to upgraded")).toBeInTheDocument();
  expect(reverted).toBe(true);
  // 연 버튼으로 포커스가 돌아온다.
  const trigger = screen.getByRole("button", { name: "Revert to upgraded" });
  await waitFor(() => expect(trigger).toHaveFocus());
});

it("deletes a snapshot after confirming and shows why it could not", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base],
  });
  server.use(
    http.delete(apiUrl(`/vms/${web.id}/snapshots/${base.id}`), () =>
      HttpResponse.json({ code: "vm_busy", message: "busy" }, { status: 409 }),
    ),
  );
  open(web);
  await userEvent.click(await screen.findByRole("button", { name: "Delete snapshot base" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete snapshot base?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Another operation on this VM (start, stop, snapshot, revert or clone) is in progress. Try again when it has finished.",
  );

  server.use(
    http.delete(apiUrl(`/vms/${web.id}/snapshots/${base.id}`), () =>
      HttpResponse.json(task("t1", "snapshot-delete", web.id), { status: 202 }),
    ),
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Delete snapshot base of web")).toBeInTheDocument();
});

it("shows snapshots without actions to viewers", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base],
  });
  open(web, { ...ADMIN, role: "viewer" });
  expect(await screen.findByText("base")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Take snapshot" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Revert to base" })).not.toBeInTheDocument();
});

it("says why the snapshots could not be read", async () => {
  mockLists({ vm: [web], node: [NODE], volume: [volume("web-root", "local")] });
  server.use(
    http.get(apiUrl("/snapshots"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  open(web);
  const table = await screen.findByRole("table", { name: "Snapshots" });
  expect(
    await within(table).findByText("You do not have permission to do this."),
  ).toBeInTheDocument();
  // 목록을 모르면 이름이 겹치는지 모르므로 만들 수 없다.
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
});

it("shows the schedule, its last failure and the snapshots it took", async () => {
  const failure =
    'VM "web" is being snapshotted, reverted or cloned; try again when the task has finished';
  const scheduled = {
    ...web,
    spec: { ...web.spec, snapshot_schedule: { every: "daily" as const, keep: 7, memory: true } },
    status: { ...web.status, schedule_error: { code: "vm_busy", message: failure } },
  };
  const auto = snapshot("auto-20261027-0300", web, 5, { spec: { scheduled: true } });
  mockLists({
    vm: [scheduled],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base, auto],
  });
  open(scheduled);
  expect(
    await screen.findByText("Snapshot schedule: Daily, keep 7, with memory"),
  ).toBeInTheDocument();
  // 서버가 남긴 code를 예약 전용 문구로 번역해, 무엇의 실패인지와 함께 보인다.
  const shown = screen.getByText("Last scheduled snapshot failed:").parentElement;
  expect(shown).toHaveTextContent(
    "Last scheduled snapshot failed: Skipped because the VM was busy; it will try again later.",
  );
  expect(shown).not.toHaveTextContent(failure);
  expect(screen.getByRole("link", { name: "Change schedule" })).toHaveAttribute(
    "href",
    "/vms/web/edit",
  );
  const table = screen.getByRole("table", { name: "Snapshots" });
  await within(table).findByText("base");
  const cells = within(table)
    .getAllByRole("row")
    .slice(1)
    .map((r) => within(r).getAllByRole("cell")[0]!.textContent);
  // 예약으로 만든 스냅샷만 표시한다.
  expect(cells).toEqual(["base", "auto-20261027-0300 Scheduled"]);
});

it("says there is no schedule and lets only writers change it", async () => {
  mockLists({ vm: [web], node: [NODE], volume: [volume("web-root", "local")], snapshot: [] });
  open(web, { ...ADMIN, role: "viewer" });
  expect(await screen.findByText("No snapshot schedule.")).toBeInTheDocument();
  expect(screen.queryByText("Last scheduled snapshot failed:")).not.toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "Change schedule" })).not.toBeInTheDocument();
});

it.each([
  // 실패한 Task는 번역 뒤에 Task의 오류를 덧붙인다.
  ["task_failed", "no space left on device", "Task error: no space left on device"],
  // 예약 전용 문구에 서버의 설명(노드 이름)을 넣는다.
  [
    "node_offline",
    'node "node-a" holding VM "web" is offline',
    'Skipped because the VM\'s node is offline (node "node-a" holding VM "web" is offline).',
  ],
  // 예약 전용 문구가 없으면 오류 번역을 쓴다.
  [
    "snapshot_unsupported",
    "raw disks cannot be snapshotted",
    "Only VMs whose disks are all qcow2, or all in LVM-thin pools, can take snapshots. Snapshots of disks in LVM-thin pools cannot include memory.",
  ],
  // 번역이 없는 code(옛 문자열을 읽은 unknown 포함)는 서버의 설명을 그대로 보인다.
  ["unknown", "VM busy", "VM busy"],
  ["something_new", "a new failure", "a new failure"],
])("shows the schedule failure %s in the user's language", async (code, message, text) => {
  const failed = {
    ...web,
    spec: { ...web.spec, snapshot_schedule: { every: "hourly" as const, keep: 3, memory: false } },
    status: { ...web.status, schedule_error: { code, message } },
  };
  mockLists({ vm: [failed], node: [NODE], volume: [volume("web-root", "local")] });
  open(failed);
  const shown = (await screen.findByText("Last scheduled snapshot failed:")).parentElement;
  expect(shown).toHaveTextContent(`Last scheduled snapshot failed: ${text}`);
});
