// 계획 68 웹: 백업 표의 저장소 대상·검증 배지·새 데이터, 옮기기·복사·검증, 라이브 복원 스위치, VM 백업 대상 고르기,
// VM 머리의 라이브 복원 띠와 막힌 동작, 작업 폼의 저장소·복사본.
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Vm } from "@/api/types";
import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { BackupsPage } from "@/backups/BackupsPage";
import type { BackupStore } from "@/backupStores/stores";
import { resetTasks } from "@/tasks/tasks";
import { expectNoAxeViolations } from "@/test/axe";
import { backup, backupJob, backupStore, NODE, pool, storeBackup, task, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

afterEach(resetTasks);

const web = vm("web", "running");
const ST = backupStore("st", 1);
const S3 = backupStore("s3", 2, { spec: { encryption: { fingerprint: "ab:cd" } } });
const BUSY = backupStore("busy", 3, { status: { phase: "initializing" } });
const inPool = backup("web-1", web, 1);
const base = storeBackup("web-s1", web, 2, "st", {
  status: { verify: { at: 1_793_761_200, state: "ok" } },
});
const inStore = storeBackup("web-s2", web, 3, "st", {
  status: {
    base: base.id,
    full_reason: "a chunk of the base backup failed verification",
    verify: { at: 1_793_761_200, state: "failed", bad_chunks: 2 },
  },
});
const unverified = storeBackup("web-s3", web, 4, "s3");

function stores(items: BackupStore[]) {
  server.use(http.get(apiUrl("/backup-stores"), () => HttpResponse.json({ revision: 1, items })));
}

function row(name: string) {
  return screen
    .getAllByRole("row")
    .find((r) => within(r).queryAllByRole("cell")[0]?.textContent?.startsWith(name))!;
}

it("shows the store, verify badge, new data and base of store backups", async () => {
  mockLists({ vm: [web], backup: [inPool, base, inStore, unverified] });
  stores([ST, S3]);
  const { container } = renderPage(<BackupsPage />);
  const table = await screen.findByRole("table", { name: "Backups" });
  await within(table).findByText("web-s2");
  const headers = within(table).getAllByRole("columnheader");
  const cell = (name: string, header: string) =>
    within(row(name)).getAllByRole("cell")[headers.findIndex((h) => h.textContent === header)]!;
  expect(cell("web-1", "Target")).toHaveTextContent(/^bk$/);
  expect(cell("web-1", "Verified")).toHaveTextContent(/^—$/);
  expect(cell("web-s2", "Target")).toHaveTextContent("stbackup store");
  expect(cell("web-s2", "Verified")).toHaveTextContent("2 bad chunks");
  expect(cell("web-s1", "Verified")).toHaveTextContent("Verified");
  expect(cell("web-s3", "Verified")).toHaveTextContent("Not verified");
  expect(cell("web-s2", "Size")).toHaveTextContent("New data 300 MiBLogical 16 GiB");
  expect(cell("web-s2", "Type")).toHaveTextContent(
    "ChunkedBase: web-s1Read in full: a chunk of the base backup failed verification",
  );
  expect(within(cell("web-s2", "Type")).getByRole("link", { name: "web-s1" })).toHaveAttribute(
    "href",
    `#backup-${base.id}`,
  );
  await expectNoAxeViolations(container);
});

it("verifies a store backup and moves a pool backup into a store", async () => {
  const user = userEvent.setup();
  mockLists({ vm: [web], backup: [inPool, base] });
  // 준비되지 않은 저장소는 고를 수 없다.
  stores([ST, S3, BUSY]);
  let verified = false;
  let convert: unknown;
  let copy: unknown;
  server.use(
    http.post(apiUrl(`/backups/${base.id}/actions/verify`), () => {
      verified = true;
      return HttpResponse.json(task("t1", "backup-store-verify", ST.id), { status: 202 });
    }),
    http.post(apiUrl(`/backups/${inPool.id}/actions/convert`), async ({ request }) => {
      convert = await request.json();
      return HttpResponse.json(task("t2", "backup-convert", inPool.id), { status: 202 });
    }),
    http.post(apiUrl(`/backups/${base.id}/actions/copy`), async ({ request }) => {
      copy = await request.json();
      return HttpResponse.json(task("t3", "backup-copy", base.id), { status: 202 });
    }),
  );
  renderPage(<BackupsPage />);
  await user.click(await screen.findByRole("button", { name: "Verify web-s1 now" }));
  await waitFor(() => expect(verified).toBe(true));
  // 풀 백업은 검증하지 않고(서버 400) 저장소로 옮긴다.
  expect(screen.queryByRole("button", { name: "Verify web-1 now" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Move web-1 to a backup store" }));
  let dialog = await screen.findByRole("dialog", { name: "Move web-1 to a backup store" });
  const select = within(dialog).getByLabelText("Backup store");
  expect(
    within(select)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["st", "s3"]);
  await user.selectOptions(select, "s3");
  await user.click(within(dialog).getByRole("button", { name: "Move to store" }));
  await waitFor(() => expect(convert).toEqual({ store: "s3" }));
  // 복사는 다른 저장소로만
  await user.click(screen.getByRole("button", { name: "Copy web-s1 to another backup store" }));
  dialog = await screen.findByRole("dialog", { name: "Copy web-s1 to another backup store" });
  expect(
    within(within(dialog).getByLabelText("Backup store"))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["s3"]);
  await user.click(within(dialog).getByRole("button", { name: "Copy to store" }));
  await waitFor(() => expect(copy).toEqual({ store: "s3" }));
});

it("restores live when the switch is on", async () => {
  const user = userEvent.setup();
  mockLists({ vm: [web], backup: [base], "storage-pool": [pool("local")] });
  stores([ST]);
  let body: unknown;
  server.use(
    http.post(apiUrl(`/backups/${base.id}/restore`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "backup-live-restore", "x"), { status: 202 });
    }),
  );
  renderPage(<BackupsPage />);
  await user.click(await screen.findByRole("button", { name: "Restore web-s1" }));
  const dialog = await screen.findByRole("dialog", { name: "Restore web-s1" });
  await user.type(within(dialog).getByLabelText("New VM name"), "web2");
  await user.selectOptions(within(dialog).getByLabelText("Storage pool for the disks"), "local");
  const live = within(dialog).getByRole("switch", { name: "Live restore (start right away)" });
  await user.click(live);
  expect(dialog).toHaveTextContent("starts it right away");
  expect(dialog).toHaveTextContent("HA protects it only afterwards");
  await user.click(within(dialog).getByRole("button", { name: "Restore" }));
  expect(await screen.findByText("Live restore web-s1 as web2")).toBeInTheDocument();
  expect(body).toEqual({ name: "web2", pool: "local", live: true });
});

it("backs up a VM to a backup store", async () => {
  const user = userEvent.setup();
  mockLists({
    vm: [web],
    node: [NODE],
    "storage-pool": [pool("bk", { backups: true })],
    backup: [],
  });
  stores([ST, S3, BUSY]);
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/backups`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "vm-backup", web.id), { status: 202 });
    }),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web?tab=backups" });
  await user.click(await screen.findByRole("button", { name: "Back up now" }));
  const dialog = await screen.findByRole("dialog", { name: "Back up web" });
  const target = await within(dialog).findByLabelText("Target");
  expect(
    within(target)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["bk", "st", "s3 (encrypted)"]);
  expect(target).toHaveValue("pool:bk");
  await user.selectOptions(target, "store:st");
  expect(dialog).toHaveTextContent("Every backup still has a complete index");
  await user.click(within(dialog).getByLabelText("Incremental if possible"));
  await user.click(within(dialog).getByRole("button", { name: "Back up now" }));
  await waitFor(() => expect(body).toEqual({ store: "st", incremental: true }));
});

const restoring: Vm = {
  ...web,
  status: {
    ...web.status,
    live_restore: {
      backup: base.id,
      task: "019a0f3c-0000-7000-8001-000000000099",
      phase: "paused",
      done_bytes: 8 * 1024 ** 3,
      total_bytes: 16 * 1024 ** 3,
      started_at: 1_793_761_200,
      error: "backup store unreachable",
    },
  },
};

it("shows the live restore strip, blocks actions and cancels with a confirmation", async () => {
  const user = userEvent.setup();
  mockLists({ vm: [restoring], node: [NODE], backup: [base] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/live-restore/cancel`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "backup-live-restore", web.id), { status: 202 });
    }),
  );
  const { container } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  const strip = await screen.findByRole("region", { name: "Live restore" });
  expect(strip).toHaveTextContent("Paused");
  expect(await within(strip).findByText("from web-s1")).toBeVisible();
  expect(strip).toHaveTextContent("50% (8 GiB of 16 GiB)");
  expect(strip).toHaveTextContent("Paused: backup store unreachable");
  expect(within(strip).getByRole("progressbar", { name: "Live restore progress" })).toHaveAttribute(
    "aria-valuenow",
    "50",
  );
  // 정지·옮기기는 까닭과 함께 막힌다(강제 정지는 받는다).
  expect(screen.getByRole("button", { name: "Stop" })).toHaveAttribute("aria-disabled", "true");
  expect(
    screen.getAllByText("Not available during a live restore. Wait for it or cancel it.")[0],
  ).toBeInTheDocument();
  await expectNoAxeViolations(container);
  await user.click(within(strip).getByRole("button", { name: "Cancel live restore" }));
  const confirm = await screen.findByRole("alertdialog", {
    name: "Cancel the live restore of web?",
  });
  expect(confirm).toHaveTextContent("Everything the guest wrote since it started is discarded.");
  await user.type(within(confirm).getByLabelText("Type web to confirm"), "web");
  await user.click(within(confirm).getByRole("button", { name: "Discard and delete" }));
  await waitFor(() => expect(body).toEqual({ discard: true }));
});

it("blocks backing up a VM during a live restore", async () => {
  mockLists({
    vm: [restoring],
    node: [NODE],
    "storage-pool": [pool("bk", { backups: true })],
    backup: [base],
  });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web?tab=backups" });
  expect(await screen.findByRole("button", { name: "Back up now" })).toBeDisabled();
  expect(
    screen.getByText("The VM is being live-restored. Back it up when the restore has finished."),
  ).toBeVisible();
});

it("makes a job that backs up to a store and copies to another store", async () => {
  const user = userEvent.setup();
  mockLists({ "storage-pool": [pool("bk", { backups: true })] });
  stores([ST, S3]);
  let body: unknown;
  server.use(
    http.post(apiUrl("/backup-jobs"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({}, { status: 201 });
    }),
  );
  renderPage(<BackupJobsPage />);
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create backup job" });
  await user.type(within(dialog).getByLabelText("Name"), "tiered");
  await user.type(within(dialog).getByLabelText("Time (UTC)"), "03:00");
  // 풀 작업에는 복사본이 없다.
  expect(within(dialog).queryByLabelText("Copy store")).toBeNull();
  await user.selectOptions(within(dialog).getByLabelText("Target"), "store:st");
  await user.selectOptions(within(dialog).getByLabelText("Copy store"), "s3");
  await user.type(within(dialog).getByLabelText("Daily (copies)"), "90");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  await waitFor(() =>
    expect(body).toMatchObject({
      name: "tiered",
      spec: {
        store: "st",
        copy_to: { store: "s3", keep: { last: 0, daily: 90, weekly: 0, monthly: 0 } },
      },
    }),
  );
  expect((body as { spec: object }).spec).not.toHaveProperty("pool");
});

// 리뷰 1(M1·M10·M12): 풀과 같은 이름의 저장소도 옮길 곳이고, 크기 정렬은 저장소 백업의 새 데이터로, 행에는 링크가 가는 id가 있다.
it("moves into a store named like the pool, sorts store backups by new data and anchors rows", async () => {
  const user = userEvent.setup();
  const big = storeBackup("web-s9", web, 9, "st", {
    status: { store_stats: { ...inStore.status.store_stats!, new_bytes: 2 * 1024 ** 3 } },
  });
  mockLists({ vm: [web], backup: [big, inPool] });
  // 풀 `bk`와 저장소 `bk`는 이름 공간이 다르다.
  stores([backupStore("bk", 5)]);
  renderPage(<BackupsPage />, { url: "/?sort=size" });
  const table = await screen.findByRole("table", { name: "Backups" });
  await within(table).findByText("web-s9");
  // 풀 백업 1.5 GiB < 저장소 백업의 새 데이터 2 GiB(예전에는 저장소 백업이 0으로 맨 앞이었다)
  expect(
    within(table)
      .getAllByRole("row")
      .slice(1)
      .map((r) => within(r).getAllByRole("cell")[0]!.textContent),
  ).toEqual(["web-1", "web-s9"]);
  expect(row("web-1")).toHaveAttribute("id", `backup-${inPool.id}`);
  await user.click(screen.getByRole("button", { name: "Move web-1 to a backup store" }));
  const dialog = await screen.findByRole("dialog", { name: "Move web-1 to a backup store" });
  expect(
    within(within(dialog).getByLabelText("Backup store"))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["bk"]);
});

// 리뷰 1(M9): 백업 풀이 없고 저장소 목록이 대화상자를 연 뒤에 와도 새 작업은 첫 저장소로 간다.
it("defaults a new job to the first store when the stores arrive after the dialog opens", async () => {
  const user = userEvent.setup();
  mockLists({ "storage-pool": [] });
  let release!: () => void;
  const gate = new Promise<void>((resolve) => (release = resolve));
  server.use(
    http.get(apiUrl("/backup-stores"), async () => {
      await gate;
      return HttpResponse.json({ revision: 1, items: [ST] });
    }),
  );
  let body: unknown;
  server.use(
    http.post(apiUrl("/backup-jobs"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({}, { status: 201 });
    }),
  );
  renderPage(<BackupJobsPage />);
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create backup job" });
  release();
  expect(await within(dialog).findByLabelText("Target")).toHaveValue("store:st");
  await user.type(within(dialog).getByLabelText("Name"), "late");
  await user.type(within(dialog).getByLabelText("Time (UTC)"), "03:00");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  await waitFor(() => expect(body).toMatchObject({ name: "late", spec: { store: "st" } }));
});

// 리뷰 1(M8): 복사 대상이 이제 받을 수 없는 저장소여도 "복사 안 함"이 아니라 그 저장소로 보인다.
it("keeps showing a job's copy target that can no longer take backups", async () => {
  const RO = backupStore("ro", 6, { spec: { read_only: true } });
  const job = backupJob("tiered", 1, {
    spec: {
      pool: undefined,
      store: "st",
      copy_to: { store: "ro", keep: { last: 0, daily: 90, weekly: 0, monthly: 0 } },
    },
  });
  mockLists({ "storage-pool": [pool("bk", { backups: true })], "backup-job": [job] });
  stores([ST, RO]);
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Edit tiered" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit tiered" });
  await waitFor(() => expect(within(dialog).getByLabelText("Copy store")).toHaveValue("ro"));
});

// 리뷰 2(N1): `backup_in_use`는 부모·만드는 중인 증분의 base·라이브 복원의 세 뜻이라 서버의 설명을 보인다. 라이브 복원이
// 쓰는 백업은 지우기 단추부터 막는다.
it("blocks deleting a backup that a live restore uses and shows why the server refused", async () => {
  mockLists({ vm: [restoring], node: [NODE], backup: [base, inStore] });
  stores([ST]);
  renderPage(<BackupsPage />);
  const del = await screen.findByRole("button", { name: "Delete backup web-s1" });
  expect(del).toBeDisabled();
  expect(del).toHaveAttribute(
    "title",
    "VM web is being live-restored from this backup. Finish or cancel the live restore first.",
  );
  expect(screen.getByRole("button", { name: "Delete backup web-s2" })).toBeEnabled();
});

it("shows the server's reason when a backup is still in use", async () => {
  const user = userEvent.setup();
  mockLists({ vm: [web], node: [NODE], backup: [base] });
  stores([ST]);
  const reason =
    'backup "web-s1" is being live-restored into VM "web2"; finish or cancel that first';
  server.use(
    http.delete(apiUrl(`/backups/${base.id}`), () =>
      HttpResponse.json({ code: "backup_in_use", message: reason }, { status: 409 }),
    ),
  );
  renderPage(<BackupsPage />);
  await user.click(await screen.findByRole("button", { name: "Delete backup web-s1" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete backup web-s1?" });
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    `This backup is still needed: ${reason}`,
  );
  expect(within(dialog).getByRole("alert")).not.toHaveTextContent("Incremental backups");
});
