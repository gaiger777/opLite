import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { BackupsPage } from "@/backups/BackupsPage";
import { resetTasks } from "@/tasks/tasks";
import { backup, backupJob, pool, remote, task, vm } from "@/test/fixtures";
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

it("shows where a received copy came from and the state of remote copies", async () => {
  const received = backup("web-copy", web, 3, {
    spec: {
      origin: {
        cluster: "prod",
        ca_fingerprint: `sha256:${"cd".repeat(32)}`,
        backup: "019a0f3c-0000-7000-8006-0000000000aa",
        job: "nightly",
      },
    },
  });
  const copied = backup("web-2", web, 4, {
    status: {
      replicas: [
        { remote: "dr", pool: "dr-bk", state: "ready", at: 1 },
        { remote: "dr2", pool: "bk", state: "copying", at: 1 },
        { remote: "dr3", pool: "bk", state: "failed", error: "remote is down", at: 1 },
      ],
    },
  });
  mockLists({ vm: [web], backup: [ofWeb, received, copied] });
  renderPage(<BackupsPage />);
  const table = await screen.findByRole("table", { name: "Backups" });
  const headers = within(table).getAllByRole("columnheader");
  const cell = async (name: string, header: string) =>
    within((await within(table).findByText(name)).closest("tr")!).getAllByRole("cell")[
      headers.findIndex((h) => h.textContent === header)
    ]!;
  const origin = within(await cell("web-copy", "Name")).getByText("From prod");
  expect(origin).toHaveAttribute(
    "title",
    "Copy of backup 019a0f3c-0000-7000-8006-0000000000aa (job nightly) on cluster prod",
  );
  // 툴팁 없이도(화면 읽기) 원본 백업 id와 작업이 읽힌다.
  expect(within(await cell("web-copy", "Name")).getByText(/^Copy of backup/)).toHaveClass(
    "sr-only",
  );
  expect(await cell("web-1", "Name")).not.toHaveTextContent("From");

  const copies = await cell("web-2", "Remote copies");
  expect(within(copies).getByText("dr").closest("[title]")).toHaveAttribute(
    "title",
    "dr / dr-bk: Ready",
  );
  expect(within(copies).getByText("dr2").closest("[title]")).toHaveAttribute(
    "title",
    "dr2 / bk: Copying",
  );
  expect(within(copies).getByText("dr3").closest("[title]")).toHaveAttribute(
    "title",
    "dr3 / bk: Failed. remote is down",
  );
  // 원격의 풀과 상태는 화면 읽기에도 읽히고, 실패한 사본의 오류는 글로 보인다.
  expect(within(copies).getByText("dr / dr-bk: Ready")).toHaveClass("sr-only");
  expect(within(copies).getByText("dr3 / bk: Failed")).toHaveClass("sr-only");
  expect(within(copies).getByText("dr3 / bk: remote is down")).toBeVisible();
  expect(within(copies).queryByText(/dr2 \/ bk: .*down/)).not.toBeInTheDocument();
  expect(await cell("web-1", "Remote copies")).toHaveTextContent("—");
});

it("copies a ready backup to a backup pool of a remote cluster", async () => {
  const notReady = backup("web-0", web, 2, { status: { phase: "pending" } });
  mockLists({ vm: [web], backup: [ofWeb, notReady] });
  let body: unknown;
  server.use(
    http.get(apiUrl("/remotes"), () =>
      HttpResponse.json({ revision: 1, items: [remote("dr"), remote("dr2")] }),
    ),
    // 원격의 풀은 중계로 읽고 백업 풀만 고른다.
    http.get(apiUrl("/remotes/dr/api/storage-pools"), () =>
      HttpResponse.json({
        revision: 1,
        items: [pool("local"), pool("dr-bk", { backups: true })],
      }),
    ),
    http.post(apiUrl(`/backups/${ofWeb.id}/actions/replicate`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "backup-replicate", ofWeb.id), { status: 202 });
    }),
  );
  renderPage(<BackupsPage />);
  expect(
    await screen.findByRole("button", { name: "Copy web-0 to a remote cluster" }),
  ).toBeDisabled();
  await userEvent.click(screen.getByRole("button", { name: "Copy web-1 to a remote cluster" }));
  const dialog = await screen.findByRole("dialog", { name: "Copy web-1 to a remote cluster" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Copy" }));
  expect(within(dialog).getByText("Choose a remote cluster.")).toBeVisible();
  await within(dialog).findByRole("option", { name: "dr2" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Remote cluster"), "dr");
  const pools = within(dialog).getByLabelText("Backup pool on the remote cluster");
  await within(dialog).findByRole("option", { name: "dr-bk" });
  expect(
    within(pools)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["Choose a pool", "dr-bk"]);
  await userEvent.click(within(dialog).getByRole("button", { name: "Copy" }));
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();
  await userEvent.selectOptions(pools, "dr-bk");
  await userEvent.click(within(dialog).getByRole("button", { name: "Copy" }));
  expect(await screen.findByText("Copy backup web-1 to dr")).toBeInTheDocument();
  expect(body).toEqual({ remote: "dr", pool: "dr-bk" });
});

it("explains a remote cluster whose pools cannot be read", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  server.use(
    http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items: [remote("dr")] })),
    http.get(apiUrl("/remotes/dr/api/storage-pools"), () =>
      HttpResponse.json(
        { code: "remote_unreachable", message: "connection refused" },
        { status: 502 },
      ),
    ),
  );
  renderPage(<BackupsPage />);
  await userEvent.click(
    await screen.findByRole("button", { name: "Copy web-1 to a remote cluster" }),
  );
  const dialog = await screen.findByRole("dialog", { name: "Copy web-1 to a remote cluster" });
  await within(dialog).findByRole("option", { name: "dr" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Remote cluster"), "dr");
  // 서버 오류라 두 번 더 읽어 본 뒤(1초·2초 뒤) 보인다.
  expect(await within(dialog).findByRole("alert", {}, { timeout: 6000 })).toHaveTextContent(
    "Cannot reach the remote cluster (connection refused).",
  );
}, 10_000);

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
    "A task on this backup (backing up, restoring, copying or deleting) is in progress.",
  );
});

it("lets operators restore and delete", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  renderPage(<BackupsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByRole("button", { name: "Restore web-1" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Delete backup web-1" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Copy web-1 to a remote cluster" })).toBeEnabled();
});

it("does not let viewers copy backups", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  renderPage(<BackupsPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("web-1")).toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Copy web-1 to a remote cluster" }),
  ).not.toBeInTheDocument();
});

it("shows the type, parent, reason for a full backup and chain size", async () => {
  const full = backup("web-full", web, 3, {
    spec: { incremental: true },
    status: { full_reason: 'no earlier backup of this VM in pool "bk"' },
  });
  const inc = backup("web-inc", web, 4, {
    spec: { incremental: true },
    status: {
      parent: full.id,
      disks: [
        { name: "disk0", file: "disk0.qcow2", virtual_bytes: 1, stored_bytes: 512 * 1024 ** 2 },
      ],
    },
  });
  mockLists({ vm: [web], backup: [ofWeb, full, inc] });
  renderPage(<BackupsPage />);
  const table = await screen.findByRole("table", { name: "Backups" });
  const headers = within(table).getAllByRole("columnheader");
  await within(table).findByText("web-inc");
  // 부모 링크에도 이름이 있어 행은 이름 열로 찾는다.
  const row = (name: string) =>
    within(table)
      .getAllByRole("row")
      .find((r) => r.firstChild?.textContent === name)!;
  const cell = (name: string, header: string) =>
    within(row(name)).getAllByRole("cell")[headers.findIndex((h) => h.textContent === header)]!;
  expect(cell("web-1", "Type")).toHaveTextContent(/^Full$/);
  // 증분을 청했는데 전체로 했으면 그 이유를 보인다.
  expect(cell("web-full", "Type")).toHaveTextContent(
    'FullWhy full: no earlier backup of this VM in pool "bk"',
  );
  const type = cell("web-inc", "Type");
  expect(type).toHaveTextContent("IncrementalParent: web-full");
  // 부모는 같은 표의 그 행으로 간다.
  const parent = within(type).getByRole("link", { name: "web-full" });
  expect(parent).toHaveAttribute("href", `#backup-${full.id}`);
  expect(row("web-full")).toHaveAttribute("id", `backup-${full.id}`);
  // 증분의 크기는 자기 파일이고, 복원에 드는 체인 전체 크기를 함께 보인다.
  expect(cell("web-inc", "Size")).toHaveTextContent("512 MiBChain 2 GiB");
  expect(cell("web-full", "Size")).toHaveTextContent(/^1\.5 GiB$/);
});

it("copies an incremental backup and shows how much of its chain was copied", async () => {
  const copy = (state: "ready" | "copying" | "failed", backup?: string) => ({
    remote: "dr",
    pool: "dr-bk",
    state,
    at: 1,
    ...(backup && { backup }),
  });
  const full = backup("web-full", web, 3, {
    status: { replicas: [copy("ready", "019a0f3c-0000-7000-8002-0000000000a1")] },
  });
  const inc = backup("web-inc", web, 4, {
    spec: { incremental: true },
    status: { parent: full.id, replicas: [copy("copying")] },
  });
  const next = backup("web-inc2", web, 5, {
    spec: { incremental: true },
    status: { parent: inc.id },
  });
  // 다 보낸 체인
  const sentFull = backup("web-sent", web, 6, {
    status: { replicas: [copy("ready", "019a0f3c-0000-7000-8002-0000000000b1")] },
  });
  const sentInc = backup("web-sent-inc", web, 7, {
    spec: { incremental: true },
    status: {
      parent: sentFull.id,
      replicas: [copy("ready", "019a0f3c-0000-7000-8002-0000000000b2")],
    },
  });
  // 이 백업의 실패 기록 하나뿐인 체인(복제 중이 아니어도 보인다)
  const failedFull = backup("web-failed", web, 8);
  const failedInc = backup("web-failed-inc", web, 9, {
    spec: { incremental: true },
    status: { parent: failedFull.id, replicas: [copy("failed")] },
  });
  mockLists({
    vm: [web],
    backup: [ofWeb, full, inc, next, sentFull, sentInc, failedFull, failedInc],
  });
  let body: unknown;
  server.use(
    http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items: [remote("dr")] })),
    http.get(apiUrl("/remotes/dr/api/storage-pools"), () =>
      HttpResponse.json({ revision: 1, items: [pool("dr-bk", { backups: true })] }),
    ),
    http.post(apiUrl(`/backups/${inc.id}/actions/replicate`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "backup-replicate", inc.id), { status: 202 });
    }),
  );
  renderPage(<BackupsPage />);
  const table = await screen.findByRole("table", { name: "Backups" });
  const headers = within(table).getAllByRole("columnheader");
  await within(table).findByText("web-inc2");
  const row = (name: string) =>
    within(table)
      .getAllByRole("row")
      .find((r) => r.firstChild?.textContent === name)!;
  const copies = (name: string) =>
    within(row(name)).getAllByRole("cell")[
      headers.findIndex((h) => h.textContent === "Remote copies")
    ]!;
  // 증분 백업은 원격 사본마다 체인의 몇 백업이 원격에 있는지 보인다(자기 사본 기록이 없어도).
  expect(
    within(copies("web-inc")).getByText("dr / dr-bk: 1 of 2 in the chain copied"),
  ).toBeVisible();
  expect(copies("web-inc2")).toHaveTextContent(/^dr \/ dr-bk: 1 of 3 in the chain copied$/);
  expect(
    within(copies("web-failed-inc")).getByText("dr / dr-bk: 0 of 2 in the chain copied"),
  ).toBeVisible();
  // 자기 사본 기록이 없으면 사본 배지 줄(빈 줄)을 그리지 않는다.
  expect(copies("web-inc2").querySelector(".flex")).toBeNull();
  // 전체 백업과 다 보낸 체인은 진행을 보이지 않는다.
  expect(copies("web-full")).not.toHaveTextContent("chain");
  expect(copies("web-sent-inc")).not.toHaveTextContent("chain");

  // 증분 백업도 복제한다. 설명은 체인의 앞 백업도 보낸다고 알린다.
  await userEvent.click(screen.getByRole("button", { name: "Copy web-inc to a remote cluster" }));
  const dialog = await screen.findByRole("dialog", { name: "Copy web-inc to a remote cluster" });
  expect(dialog).toHaveAccessibleDescription(
    /An incremental backup is sent with the earlier backups of its chain that the remote cluster does not have yet, starting from the full backup\./,
  );
  await within(dialog).findByRole("option", { name: "dr" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Remote cluster"), "dr");
  await within(dialog).findByRole("option", { name: "dr-bk" });
  await userEvent.selectOptions(
    within(dialog).getByLabelText("Backup pool on the remote cluster"),
    "dr-bk",
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Copy" }));
  expect(await screen.findByText("Copy backup web-inc to dr")).toBeInTheDocument();
  expect(body).toEqual({ remote: "dr", pool: "dr-bk" });
});

it.each([
  {
    // 복제의 backup_busy는 복제 대화상자 전용 문구다.
    code: "backup_busy",
    message:
      'backup "web-1" is being replicated to remote "dr"; try again when the task has finished',
    shown:
      'This backup is being deleted, or it or a backup of its chain is being copied to this remote: backup "web-1" is being replicated to remote "dr"; try again when the task has finished',
  },
  {
    // 그 밖의 오류는 전역 errors.<code> 문구다.
    code: "cluster_upgrading",
    message: "node n2 does not support incremental-replication yet",
    shown: "The cluster is being upgraded: node n2 does not support incremental-replication yet",
  },
])("explains a copy refused with $code", async ({ code, message, shown }) => {
  const inc = backup("web-inc", web, 4, {
    spec: { incremental: true },
    status: { parent: ofWeb.id },
  });
  mockLists({ vm: [web], backup: [ofWeb, inc] });
  server.use(
    http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items: [remote("dr")] })),
    http.get(apiUrl("/remotes/dr/api/storage-pools"), () =>
      HttpResponse.json({ revision: 1, items: [pool("dr-bk", { backups: true })] }),
    ),
    http.post(apiUrl(`/backups/${inc.id}/actions/replicate`), () =>
      HttpResponse.json({ code, message }, { status: 409 }),
    ),
  );
  renderPage(<BackupsPage />);
  await userEvent.click(
    await screen.findByRole("button", { name: "Copy web-inc to a remote cluster" }),
  );
  const dialog = await screen.findByRole("dialog", { name: "Copy web-inc to a remote cluster" });
  await within(dialog).findByRole("option", { name: "dr" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Remote cluster"), "dr");
  await within(dialog).findByRole("option", { name: "dr-bk" });
  await userEvent.selectOptions(
    within(dialog).getByLabelText("Backup pool on the remote cluster"),
    "dr-bk",
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Copy" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(shown);
});

it("explains why a parent backup cannot be deleted", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  server.use(
    http.delete(apiUrl(`/backups/${ofWeb.id}`), () =>
      HttpResponse.json(
        {
          code: "backup_in_use",
          message: 'backup "web-1" is the parent of 1 incremental backup(s); delete them first',
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<BackupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete backup web-1" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete backup web-1?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Incremental backups are based on this backup. Delete them first, starting from the newest.",
  );
});
