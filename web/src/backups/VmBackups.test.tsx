import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Vm } from "@/api/types";
import { formatTime } from "@/lib/format";
import { resetTasks } from "@/tasks/tasks";
import { backup, NODE, pool, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

afterEach(resetTasks);

const web = vm("web", "running");
const first = backup("web-20261104-030000", web, 1);
const taking = backup("web-20261105-030000", web, 2, {
  status: { phase: "pending", created_at: null, disks: [] },
});
const broken = backup("web-broken", web, 3, {
  status: { phase: "failed", disks: [], error: "qemu-img convert failed" },
});
// 다른 VM의 백업은 보이지 않는다.
const other = backup("db-20261104-030000", vm("db", "stopped"), 4);

function open(v: Vm, as = ADMIN) {
  return renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: `/vms/${v.name}?tab=backups`,
    as,
  });
}

it("lists the VM's backups, newest first", async () => {
  mockLists({ vm: [web], node: [NODE], backup: [first, other, taking, broken] });
  open(web);
  const table = await screen.findByRole("table", { name: "Backups" });
  await within(table).findByText("web-broken");
  const rows = within(table).getAllByRole("row").slice(1);
  expect(rows.map((r) => within(r).getAllByRole("cell")[0]!.textContent)).toEqual([
    "web-broken",
    "web-20261105-030000",
    "web-20261104-030000",
  ]);
  const [failed, pending, ready] = rows as [HTMLElement, HTMLElement, HTMLElement];
  expect(ready).toHaveTextContent("bk");
  expect(ready).toHaveTextContent("Ready");
  expect(ready).toHaveTextContent(formatTime(new Date(1_793_761_200 * 1000), "en"));
  expect(ready).toHaveTextContent("1.5 GiB");
  expect(within(ready).getByRole("button", { name: "Restore web-20261104-030000" })).toBeEnabled();
  // 만드는 중이거나 실패한 백업은 복원할 수 없지만 지울 수는 있다.
  expect(pending).toHaveTextContent("Creating");
  expect(
    within(pending).getByRole("button", { name: "Restore web-20261105-030000" }),
  ).toBeDisabled();
  expect(failed).toHaveTextContent("Failed");
  expect(failed).toHaveTextContent("qemu-img convert failed");
  expect(within(failed).getByRole("button", { name: "Delete backup web-broken" })).toBeEnabled();
});

it("backs up a running VM to the chosen backup pool and follows its task", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    "storage-pool": [pool("local"), pool("bk", { backups: true }), pool("nas", { backups: true })],
    backup: [first],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/backups`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "vm-backup", web.id), { status: 202 });
    }),
  );
  open(web);
  await userEvent.click(await screen.findByRole("button", { name: "Back up now" }));
  const dialog = await screen.findByRole("dialog", { name: "Back up web" });
  // 실행 중인 VM은 멈추지 않고 한 시점의 디스크를 복사한다고 알린다.
  expect(dialog).toHaveTextContent("The VM keeps running");
  const select = within(dialog).getByLabelText("Backup pool");
  expect(
    within(select)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["bk", "nas"]);
  await userEvent.selectOptions(select, "nas");
  // 이름은 클러스터에서 고유하다.
  await userEvent.type(within(dialog).getByLabelText("Name (optional)"), "web-20261104-030000");
  await userEvent.click(within(dialog).getByRole("button", { name: "Back up now" }));
  expect(within(dialog).getByText("A backup with this name already exists.")).toBeVisible();
  await userEvent.clear(within(dialog).getByLabelText("Name (optional)"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Back up now" }));
  expect(await screen.findByText("Back up web")).toBeInTheDocument();
  // 이름을 비우면 보내지 않는다(서버가 정한다).
  expect(body).toEqual({ pool: "nas" });
  await waitFor(() => expect(screen.getByRole("button", { name: "Back up now" })).toHaveFocus());
});

it("says when no pool stores backups", async () => {
  const off = vm("web", "stopped");
  mockLists({ vm: [off], node: [NODE], "storage-pool": [pool("local")] });
  open(off);
  await userEvent.click(await screen.findByRole("button", { name: "Back up now" }));
  const dialog = await screen.findByRole("dialog", { name: "Back up web" });
  expect(dialog).toHaveTextContent("No storage pool stores backups.");
  expect(within(dialog).getByRole("button", { name: "Back up now" })).toBeDisabled();
});

it("does not back up a VM that is starting", async () => {
  const starting = vm("web", "starting");
  mockLists({ vm: [starting], node: [NODE], "storage-pool": [pool("bk", { backups: true })] });
  open(starting);
  expect(await screen.findByRole("button", { name: "Back up now" })).toBeDisabled();
  expect(screen.getByText(/Back it up when it has settled/)).toBeInTheDocument();
});

it("hides backup changes from viewers", async () => {
  mockLists({ vm: [web], node: [NODE], backup: [first] });
  open(web, { ...ADMIN, role: "viewer" });
  const table = await screen.findByRole("table", { name: "Backups" });
  await within(table).findByText("web-20261104-030000");
  expect(screen.queryByRole("button", { name: "Back up now" })).not.toBeInTheDocument();
  expect(within(table).queryByRole("button")).not.toBeInTheDocument();
});

it("asks for an incremental backup when chosen", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    "storage-pool": [pool("bk", { backups: true })],
    backup: [first],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/backups`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "vm-backup", web.id), { status: 202 });
    }),
  );
  open(web);
  await userEvent.click(await screen.findByRole("button", { name: "Back up now" }));
  const dialog = await screen.findByRole("dialog", { name: "Back up web" });
  const incremental = within(dialog).getByLabelText("Incremental if possible");
  expect(incremental).not.toBeChecked();
  // 할 수 없으면 서버가 전체로 한다고 알린다.
  expect(dialog).toHaveTextContent("A full backup is taken when that is not possible");
  await userEvent.click(incremental);
  await userEvent.click(within(dialog).getByRole("button", { name: "Back up now" }));
  expect(await screen.findByText("Back up web")).toBeInTheDocument();
  expect(body).toEqual({ pool: "bk", incremental: true });
});
