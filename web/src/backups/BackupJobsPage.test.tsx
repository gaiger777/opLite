import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { backupJob, pool, remote } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const nightly = backupJob("nightly", 1, {
  spec: {
    vms: { labels: { tier: "prod" } },
    schedule: { at: "03:00", days: ["mon", "wed"] },
    keep: { last: 2, daily: 0, weekly: 4, monthly: 0 },
  },
  status: {
    last_run: { started_at: 1_793_674_800, finished_at: 1_793_675_100, ok: 3, failed: 1 },
    errors: [{ vm: "db", error: "a backup of this VM is already running" }],
  },
});
const running = backupJob("hourly", 2, {
  spec: { vms: { names: ["web", "db"] }, keep: { last: 0, daily: 0, weekly: 0, monthly: 0 } },
  status: { run: { started_at: 1, queue: ["x", "y"], backups: [] } },
});
const off = backupJob("weekly", 3, { spec: { enabled: false } });
const copying = backupJob("offsite", 4, { spec: { replicate: { remote: "dr", pool: "dr-bk" } } });

/** 원격 dr과 그 백업 풀 dr-bk·dr-bk2(중계) */
function mockRemotes() {
  server.use(
    http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items: [remote("dr")] })),
    http.get(apiUrl("/remotes/dr/api/storage-pools"), () =>
      HttpResponse.json({
        revision: 1,
        items: [pool("dr-bk", { backups: true }), pool("dr-bk2", { backups: true })],
      }),
    ),
  );
}

function cells(table: HTMLElement, name: string) {
  const headers = within(table).getAllByRole("columnheader");
  const row = within(table).getByText(name).closest("tr")!;
  return (header: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === header)];
}

it("lists jobs with their schedule, retention and runs in UTC", async () => {
  mockLists({ "backup-job": [off, running, nightly] });
  renderPage(<BackupJobsPage />);
  const table = await screen.findByRole("table", { name: "Backup jobs" });
  await within(table).findByText("nightly");
  const job = cells(table, "nightly");
  expect(job("VMs")).toHaveTextContent("tier=prod");
  expect(job("Schedule (UTC)")).toHaveTextContent("Mon, Wed 03:00");
  expect(job("Keep")).toHaveTextContent("last 2, 4 weekly");
  expect(job("State")).toHaveTextContent("On");
  expect(job("Next run")).toHaveTextContent("2026-11-04 03:00 UTC");
  expect(job("Last run")).toHaveTextContent("2026-11-03 03:05 UTC3 ok, 1 failed");
  expect(job("Last run")).toHaveTextContent("db: a backup of this VM is already running");

  const hourly = cells(table, "hourly");
  expect(hourly("VMs")).toHaveTextContent("web, db");
  expect(hourly("Schedule (UTC)")).toHaveTextContent("Every day 03:00");
  expect(hourly("Keep")).toHaveTextContent("All backups");
  expect(hourly("State")).toHaveTextContent("Running2 VMs waiting");
  expect(hourly("Last run")).toHaveTextContent("—");
  // 진행 중이거나 꺼 둔 작업은 지금 실행할 수 없다.
  expect(screen.getByRole("button", { name: "Run hourly now" })).toBeDisabled();

  const weekly = cells(table, "weekly");
  expect(weekly("VMs")).toHaveTextContent("All VMs");
  expect(weekly("State")).toHaveTextContent("Off");
  expect(weekly("Next run")).toHaveTextContent("—");
  expect(weekly("Remote copy")).toHaveTextContent("—");
  expect(screen.getByRole("button", { name: "Run weekly now" })).toBeDisabled();
  // 이름 순
  expect(
    within(table)
      .getAllByRole("row")
      .map((r) => r.firstChild?.textContent),
  ).toEqual(["Name", "hourly", "nightly", "weekly"]);
});

it("shows where a job copies its backups", async () => {
  mockLists({ "backup-job": [copying] });
  renderPage(<BackupJobsPage />);
  const table = await screen.findByRole("table", { name: "Backup jobs" });
  await within(table).findByText("offsite");
  expect(cells(table, "offsite")("Remote copy")).toHaveTextContent("dr / dr-bk");
});

it("creates a job that copies its backups to a remote cluster", async () => {
  mockLists({ "storage-pool": [pool("bk", { backups: true })] });
  mockRemotes();
  let body: unknown;
  server.use(
    http.post(apiUrl("/backup-jobs"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(copying, { status: 201 });
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create backup job" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "offsite");
  await userEvent.type(within(dialog).getByLabelText("Time (UTC)"), "03:00");
  await within(dialog).findByRole("option", { name: "dr" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Copy to a remote cluster"), "dr");
  await within(dialog).findByRole("option", { name: "dr-bk2" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();
  await userEvent.selectOptions(
    within(dialog).getByLabelText("Backup pool on the remote cluster"),
    "dr-bk2",
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Backup job offsite created.")).toBeInTheDocument();
  expect(body).toMatchObject({
    name: "offsite",
    spec: { pool: "bk", replicate: { remote: "dr", pool: "dr-bk2" } },
  });
});

it("turns off copying to a remote cluster", async () => {
  mockLists({ "backup-job": [copying], "storage-pool": [pool("bk", { backups: true })] });
  mockRemotes();
  let body: unknown;
  server.use(
    http.patch(apiUrl(`/backup-jobs/${copying.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(copying);
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Edit offsite" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit offsite" });
  expect(within(dialog).getByLabelText("Copy to a remote cluster")).toHaveValue("dr");
  expect(within(dialog).getByLabelText("Backup pool on the remote cluster")).toHaveValue("dr-bk");
  await userEvent.selectOptions(
    within(dialog).getByLabelText("Copy to a remote cluster"),
    "Do not copy",
  );
  expect(
    within(dialog).queryByLabelText("Backup pool on the remote cluster"),
  ).not.toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Backup job offsite saved.")).toBeInTheDocument();
  expect(body).toEqual({ replicate: null });
});

it("creates a job after checking the form", async () => {
  mockLists({
    "backup-job": [nightly],
    "storage-pool": [pool("local"), pool("bk", { backups: true }), pool("bk2", { backups: true })],
  });
  mockRemotes();
  let body: unknown;
  server.use(
    http.post(apiUrl("/backup-jobs"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(backupJob("prod", 9), { status: 201 });
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create backup job" });
  // 백업 풀만 고를 수 있다.
  expect(
    within(within(dialog).getByLabelText("Backup pool"))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["bk", "bk2"]);
  await userEvent.type(within(dialog).getByLabelText("Name"), "nightly");
  await userEvent.selectOptions(within(dialog).getByLabelText("VMs to back up"), "labels");
  await userEvent.type(within(dialog).getByLabelText("Time (UTC)"), "25:00");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("A backup job with this name already exists.")).toBeVisible();
  expect(within(dialog).getByText("Enter at least one KEY=VALUE label.")).toBeVisible();
  expect(within(dialog).getByText("Enter a time from 00:00 to 23:59.")).toBeVisible();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "prod");
  await userEvent.selectOptions(within(dialog).getByLabelText("Backup pool"), "bk2");
  await userEvent.type(within(dialog).getByLabelText("Labels"), "tier=prod");
  await userEvent.clear(within(dialog).getByLabelText("Time (UTC)"));
  await userEvent.type(within(dialog).getByLabelText("Time (UTC)"), "01:30");
  await userEvent.click(within(dialog).getByLabelText("Sat"));
  await userEvent.type(within(dialog).getByLabelText("Last"), "2");
  await userEvent.type(within(dialog).getByLabelText("Monthly"), "6");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Backup job prod created.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "prod",
    spec: {
      vms: { labels: { tier: "prod" } },
      pool: "bk2",
      schedule: { at: "01:30", days: ["sat"] },
      keep: { last: 2, daily: 0, weekly: 0, monthly: 6 },
      enabled: true,
    },
  });
});

it("edits a job and sends only what changed", async () => {
  mockLists({ "backup-job": [nightly], "storage-pool": [pool("bk", { backups: true })] });
  mockRemotes();
  let body: unknown;
  server.use(
    http.patch(apiUrl(`/backup-jobs/${nightly.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(nightly);
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Edit nightly" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit nightly" });
  expect(within(dialog).getByLabelText("Labels")).toHaveValue("tier=prod");
  expect(within(dialog).getByLabelText("Mon")).toBeChecked();
  expect(within(dialog).getByLabelText("Weekly")).toHaveValue("4");
  // 바꾼 것이 없으면 저장하지 않는다.
  expect(within(dialog).getByRole("button", { name: "Save" })).toBeDisabled();
  await userEvent.click(within(dialog).getByLabelText("Mon"));
  await userEvent.click(within(dialog).getByLabelText("Run on schedule"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Backup job nightly saved.")).toBeInTheDocument();
  expect(body).toEqual({ schedule: { at: "03:00", days: ["wed"] }, enabled: false });
});

it("runs a job now and explains a refusal", async () => {
  mockLists({ "backup-job": [nightly] });
  let calls = 0;
  server.use(
    http.post(apiUrl(`/backup-jobs/${nightly.id}/run`), () => {
      calls += 1;
      return calls === 1
        ? HttpResponse.json(nightly)
        : HttpResponse.json(
            { code: "job_running", message: 'backup job "nightly" is already running' },
            { status: 409 },
          );
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Run nightly now" }));
  expect(
    await screen.findByText("nightly starts at the scheduler's next check."),
  ).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Run nightly now" }));
  expect(
    await screen.findByText("The backup job is already running. Wait for the run to finish."),
  ).toBeInTheDocument();
});

it("deletes a job after confirming and keeps its backups", async () => {
  mockLists({ "backup-job": [nightly] });
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/backup-jobs/${nightly.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete backup job nightly" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete backup job nightly?" });
  expect(dialog).toHaveTextContent("The backups it made are kept.");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Backup job nightly deleted.")).toBeInTheDocument();
  expect(deleted).toBe(true);
});

it("lets operators change jobs and viewers only read them", async () => {
  mockLists({ "backup-job": [nightly] });
  const { unmount } = renderPage(<BackupJobsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByRole("button", { name: "Run nightly now" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Create" })).toBeInTheDocument();
  unmount();
  renderPage(<BackupJobsPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("nightly")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Run nightly now" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
});

it("makes incremental backups between full ones", async () => {
  const incremental = backupJob("daily", 5, { spec: { full_every: 6 } });
  mockLists({ "backup-job": [incremental, off], "storage-pool": [pool("bk", { backups: true })] });
  mockRemotes();
  let body: unknown;
  server.use(
    http.patch(apiUrl(`/backup-jobs/${off.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(off);
    }),
  );
  renderPage(<BackupJobsPage />);
  const table = await screen.findByRole("table", { name: "Backup jobs" });
  await within(table).findByText("daily");
  expect(cells(table, "daily")("Schedule (UTC)")).toHaveTextContent(
    "Incremental, full after 6 incremental backups",
  );
  expect(cells(table, "weekly")("Schedule (UTC)")).not.toHaveTextContent("Incremental");

  await userEvent.click(screen.getByRole("button", { name: "Edit weekly" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit weekly" });
  const every = within(dialog).getByLabelText("Incremental backups between full backups");
  expect(every).toHaveValue("0");
  await userEvent.clear(every);
  await userEvent.type(every, "x");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText("Enter a whole number of 0 or more.")).toBeVisible();
  // 증분 백업은 아직 원격으로 복제하지 못한다.
  await userEvent.clear(every);
  await userEvent.type(every, "6");
  await within(dialog).findByRole("option", { name: "dr" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Copy to a remote cluster"), "dr");
  await within(dialog).findByRole("option", { name: "dr-bk" });
  await userEvent.selectOptions(
    within(dialog).getByLabelText("Backup pool on the remote cluster"),
    "dr-bk",
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    within(dialog).getByText(/Incremental backups cannot be copied to a remote cluster yet/),
  ).toBeVisible();
  await userEvent.selectOptions(
    within(dialog).getByLabelText("Copy to a remote cluster"),
    "Do not copy",
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Backup job weekly saved.")).toBeInTheDocument();
  expect(body).toEqual({ full_every: 6 });
});
