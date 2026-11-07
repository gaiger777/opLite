import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { BackupsPage } from "@/backups/BackupsPage";
import { resetTasks } from "@/tasks/tasks";
import { backup, backupJob, pool, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(resetTasks);

const web = vm("web", "running");
const gone = vm("old", "stopped");
const ofWeb = backup("web-1", web, 1);
const ofGone = backup("old-1", gone, 2);

it("lists every backup and links only VMs that still exist", async () => {
  mockLists({ vm: [web], backup: [ofWeb, ofGone] });
  renderPage(<BackupsPage />);
  const table = await screen.findByRole("table", { name: "Backups" });
  const kept = (await within(table).findByText("web-1")).closest("tr")!;
  expect(within(kept).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  // VM을 지운 백업은 만든 때의 이름만 보인다.
  const orphan = within(table).getByText("old-1").closest("tr")!;
  expect(orphan).toHaveTextContent("old");
  expect(within(orphan).queryByRole("link")).not.toBeInTheDocument();
});

it("names the job that made each backup", async () => {
  const nightly = backupJob("nightly", 1);
  const byJob = backup("web-2", web, 3, { spec: { job: nightly.id } });
  // 작업을 지워도 백업은 작업 id를 그대로 가진다.
  const byGoneJob = backup("web-3", web, 4, { spec: { job: backupJob("gone", 2).id } });
  mockLists({ vm: [web], backup: [ofWeb, byJob, byGoneJob], "backup-job": [nightly] });
  renderPage(<BackupsPage />);
  const table = await screen.findByRole("table", { name: "Backups" });
  const headers = within(table).getAllByRole("columnheader");
  const job = async (name: string) =>
    within((await within(table).findByText(name)).closest("tr")!).getAllByRole("cell")[
      headers.findIndex((h) => h.textContent === "Job")
    ];
  expect(await job("web-2")).toHaveTextContent("nightly");
  expect(await job("web-3")).toHaveTextContent("Deleted job");
  expect(await job("web-1")).toHaveTextContent("—");
});

it("shows a backup still being received and does not restore it", async () => {
  const copy = backup("web-copy", web, 3, { status: { phase: "receiving" } });
  mockLists({ vm: [web], backup: [copy] });
  renderPage(<BackupsPage />);
  const row = (await screen.findByText("web-copy")).closest("tr")!;
  expect(row).toHaveTextContent("Receiving");
  expect(within(row).getByRole("button", { name: "Restore web-copy" })).toBeDisabled();
});

it("restores a backup as a new VM", async () => {
  mockLists({ vm: [web], backup: [ofGone], "storage-pool": [pool("local"), pool("fast")] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/backups/${ofGone.id}/restore`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "backup-restore", "new-vm"), { status: 202 });
    }),
  );
  renderPage(<BackupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Restore old-1" }));
  const dialog = await screen.findByRole("dialog", { name: "Restore old-1" });
  // 이름은 새 VM 이름이어야 하고 풀은 꼭 고른다.
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "web");
  await userEvent.click(within(dialog).getByRole("button", { name: "Restore" }));
  expect(within(dialog).getByText("A VM with this name already exists.")).toBeVisible();
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();
  await userEvent.clear(within(dialog).getByLabelText("New VM name"));
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "old2");
  await userEvent.selectOptions(
    within(dialog).getByLabelText("Storage pool for the disks"),
    "fast",
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Restore" }));
  expect(await screen.findByText("Restore old-1 as old2")).toBeInTheDocument();
  expect(body).toEqual({ name: "old2", pool: "fast" });
});

it("deletes a backup after confirming", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/backups/${ofWeb.id}`), () => {
      deleted = true;
      return HttpResponse.json(task("t1", "backup-delete", ofWeb.id), { status: 202 });
    }),
  );
  renderPage(<BackupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete backup web-1" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete backup web-1?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Delete backup web-1")).toBeInTheDocument();
  expect(deleted).toBe(true);
});

it("explains a backup that is busy", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  server.use(
    http.delete(apiUrl(`/backups/${ofWeb.id}`), () =>
      HttpResponse.json(
        { code: "backup_busy", message: 'backup "web-1" is being restored' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<BackupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete backup web-1" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete backup web-1?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "A task on this backup (backing up, restoring or deleting) is in progress.",
  );
});

it("lets operators restore and delete", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  renderPage(<BackupsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByRole("button", { name: "Restore web-1" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Delete backup web-1" })).toBeEnabled();
});
