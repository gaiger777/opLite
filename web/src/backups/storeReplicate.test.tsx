// 리뷰 1 웹 I1: 저장소 백업과 저장소 작업의 원격 복제는 원격의 백업 풀이 아니라 원격의 백업 저장소(준비됐고 쓸 수 있는
// 것)를 고르고, 서버에 `{remote, pool: <원격 저장소>}`로 보낸다.
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { BackupsPage } from "@/backups/BackupsPage";
import { resetTasks } from "@/tasks/tasks";
import { backupStore, pool, remote, storeBackup, task, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { rowAction } from "@/test/rowMenu";

afterEach(resetTasks);

const web = vm("web", "running");
const ST = backupStore("st", 1);
const inStore = storeBackup("web-s1", web, 2, "st");

/** 원격 `dr`: 백업 풀 하나와 저장소 넷(쓸 수 있는 것은 `dr-st`·`dr-s3`뿐, 모두 평문) */
function remoteDr(local = [ST]) {
  server.use(
    http.get(apiUrl("/backup-stores"), () => HttpResponse.json({ revision: 1, items: local })),
    http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items: [remote("dr")] })),
    http.get(apiUrl("/remotes/dr/api/storage-pools"), () =>
      HttpResponse.json({ revision: 1, items: [pool("dr-bk", { backups: true })] }),
    ),
    http.get(apiUrl("/remotes/dr/api/backup-stores"), () =>
      HttpResponse.json({
        revision: 1,
        items: [
          backupStore("dr-st", 1),
          backupStore("dr-ro", 2, { spec: { read_only: true } }),
          backupStore("dr-new", 3, { status: { phase: "initializing" } }),
          backupStore("dr-s3", 4),
        ],
      }),
    ),
  );
}

it("copies a store backup to a backup store of the remote cluster", async () => {
  const user = userEvent.setup();
  mockLists({ vm: [web], backup: [inStore] });
  remoteDr();
  let body: unknown;
  server.use(
    http.post(apiUrl(`/backups/${inStore.id}/actions/replicate`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "backup-replicate", inStore.id), { status: 202 });
    }),
  );
  renderPage(<BackupsPage />);
  await user.click(await rowAction("Copy web-s1 to a remote cluster"));
  const dialog = await screen.findByRole("dialog", { name: "Copy web-s1 to a remote cluster" });
  await within(dialog).findByRole("option", { name: "dr" });
  await user.selectOptions(within(dialog).getByLabelText("Remote cluster"), "dr");
  const stores = within(dialog).getByLabelText("Backup store on the remote cluster");
  await within(dialog).findByRole("option", { name: "dr-st" });
  expect(
    within(stores)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["Choose a store", "dr-s3", "dr-st"]);
  expect(within(dialog).queryByLabelText("Backup pool on the remote cluster")).toBeNull();
  await user.click(within(dialog).getByRole("button", { name: "Copy" }));
  expect(within(dialog).getByText("Choose a backup store.")).toBeVisible();
  await user.selectOptions(stores, "dr-st");
  await user.click(within(dialog).getByRole("button", { name: "Copy" }));
  await waitFor(() => expect(body).toEqual({ remote: "dr", pool: "dr-st" }));
});

it("a store job copies to a remote store and a changed target clears the remote choice", async () => {
  const user = userEvent.setup();
  mockLists({ "storage-pool": [pool("bk", { backups: true })] });
  remoteDr();
  let body: unknown;
  server.use(
    http.post(apiUrl("/backup-jobs"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({}, { status: 201 });
    }),
  );
  renderPage(<BackupJobsPage />);
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create backup schedule" });
  await user.type(within(dialog).getByLabelText("Name"), "offsite");
  await user.type(within(dialog).getByLabelText("Time (UTC)"), "03:00");
  // 풀 작업은 원격의 백업 풀을 고른다.
  await within(dialog).findByRole("option", { name: "dr" });
  await user.selectOptions(within(dialog).getByLabelText("Copy to a remote cluster"), "dr");
  await within(dialog).findByRole("option", { name: "dr-bk" });
  await user.selectOptions(
    within(dialog).getByLabelText("Backup pool on the remote cluster"),
    "dr-bk",
  );
  // 저장소 작업으로 바꾸면 원격 저장소를 다시 고른다.
  await user.selectOptions(within(dialog).getByLabelText("Target"), "store:st");
  const stores = within(dialog).getByLabelText("Backup store on the remote cluster");
  await within(dialog).findByRole("option", { name: "dr-st" });
  expect(stores).toHaveValue("");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Choose a backup store.")).toBeVisible();
  await user.selectOptions(stores, "dr-s3");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  await waitFor(() =>
    expect(body).toMatchObject({
      name: "offsite",
      spec: { store: "st", replicate: { remote: "dr", pool: "dr-s3" } },
    }),
  );
});

// 리뷰 2(N2): 받는 쪽은 같은 키(지문)이거나 둘 다 평문이어야 받는다 — 키가 다른 원격 저장소는 고를 수 없게 보인다.
it("offers only remote stores with the same key as the backup's store", async () => {
  const user = userEvent.setup();
  const ENC = backupStore("enc", 5, { spec: { encryption: { fingerprint: "ab:cd" } } });
  const fromEnc = storeBackup("web-e1", web, 3, "enc");
  mockLists({ vm: [web], backup: [fromEnc] });
  remoteDr([ST, ENC]);
  server.use(
    http.get(apiUrl("/remotes/dr/api/backup-stores"), () =>
      HttpResponse.json({
        revision: 1,
        items: [
          backupStore("dr-plain", 1),
          backupStore("dr-same", 2, { spec: { encryption: { fingerprint: "ab:cd" } } }),
          backupStore("dr-new", 3, { spec: { encryption: { fingerprint: "ef:01" } } }),
        ],
      }),
    ),
  );
  renderPage(<BackupsPage />);
  await user.click(await rowAction("Copy web-e1 to a remote cluster"));
  const dialog = await screen.findByRole("dialog", { name: "Copy web-e1 to a remote cluster" });
  await within(dialog).findByRole("option", { name: "dr" });
  await user.selectOptions(within(dialog).getByLabelText("Remote cluster"), "dr");
  await within(dialog).findByRole("option", { name: "dr-same" });
  expect(within(dialog).getByRole("option", { name: "dr-new (different key)" })).toBeDisabled();
  expect(within(dialog).getByRole("option", { name: "dr-plain (different key)" })).toBeDisabled();
  expect(within(dialog).getByRole("option", { name: "dr-same" })).toBeEnabled();
});

it("says when no remote store has the same key", async () => {
  const user = userEvent.setup();
  mockLists({ vm: [web], backup: [inStore] });
  remoteDr();
  server.use(
    http.get(apiUrl("/remotes/dr/api/backup-stores"), () =>
      HttpResponse.json({
        revision: 1,
        items: [backupStore("dr-enc", 1, { spec: { encryption: { fingerprint: "ab:cd" } } })],
      }),
    ),
  );
  renderPage(<BackupsPage />);
  await user.click(await rowAction("Copy web-s1 to a remote cluster"));
  const dialog = await screen.findByRole("dialog", { name: "Copy web-s1 to a remote cluster" });
  await within(dialog).findByRole("option", { name: "dr" });
  await user.selectOptions(within(dialog).getByLabelText("Remote cluster"), "dr");
  expect(
    await within(dialog).findByText(
      "No writable backup store on the remote cluster uses the same key. Create one there with this store's recovery key.",
    ),
  ).toBeVisible();
});

// 리뷰 2(N3): 목록이 오기 전에 고른 원격 대상은 그 뒤 대상 종류가 바뀌면(늦게 온 저장소 목록) 다시 고른다 — 원격 풀을
// 원격 저장소 이름으로 보내지 않는다.
it("asks again for the remote target when a late list changes a new job's target kind", async () => {
  const user = userEvent.setup();
  mockLists({ "storage-pool": [] });
  remoteDr();
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
  const dialog = await screen.findByRole("dialog", { name: "Create backup schedule" });
  await user.type(within(dialog).getByLabelText("Name"), "offsite");
  await user.type(within(dialog).getByLabelText("Time (UTC)"), "03:00");
  await within(dialog).findByRole("option", { name: "dr" });
  await user.selectOptions(within(dialog).getByLabelText("Copy to a remote cluster"), "dr");
  await within(dialog).findByRole("option", { name: "dr-bk" });
  await user.selectOptions(
    within(dialog).getByLabelText("Backup pool on the remote cluster"),
    "dr-bk",
  );
  release();
  expect(await within(dialog).findByLabelText("Target")).toHaveValue("store:st");
  const stores = await within(dialog).findByLabelText("Backup store on the remote cluster");
  await within(dialog).findByRole("option", { name: "dr-st" });
  expect(stores).toHaveValue("");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Choose a backup store.")).toBeVisible();
  await user.selectOptions(stores, "dr-st");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  await waitFor(() =>
    expect(body).toMatchObject({
      name: "offsite",
      spec: { store: "st", replicate: { remote: "dr", pool: "dr-st" } },
    }),
  );
});
