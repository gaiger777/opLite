import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Node } from "@/api/types";
import { BackupStoresPage } from "@/backupStores/BackupStoresPage";
import type { BackupStore } from "@/backupStores/stores";
import { resetTasks } from "@/tasks/tasks";
import { expectNoAxeViolations } from "@/test/axe";
import { backupStore, NODE, pool, task } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const FP = "ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89";
const KEY = "OPK1-ABCD-EFGH-IJKL-MNOP-QRST-UVWX-YZ23-4567-ABCD-EFGH-IJKL-MNOP-QRST-UV";

const LOCAL = backupStore("local", 1, {
  status: {
    phase: "ready",
    usage: {
      snapshots: 12,
      chunks: 900,
      stored_bytes: 4 * 1024 ** 3,
      logical_bytes: 13 * 1024 ** 3,
      measured_at: 1_793_761_200,
    },
    last_gc: {
      started_at: 1_793_761_200,
      finished_at: 1_793_761_260,
      removed_chunks: 3,
      removed_bytes: 12 * 1024 ** 2,
      kept_young: 0,
    },
    last_verify: { started_at: 1, finished_at: 1_793_761_200, snapshots: 12, chunks: 900, bad: 0 },
    next_gc: 1_793_840_400,
  },
});
const DR = backupStore("s3-dr", 2, {
  spec: {
    backend: {
      s3: {
        endpoint: "https://s3.example.com",
        region: "us-east-1",
        bucket: "bk",
        prefix: "orbi",
        access_key_id: "AK",
      },
    },
    encryption: { fingerprint: FP },
    read_only: true,
  },
  status: {
    phase: "failed",
    error: "store.json is unreadable",
    owner: { cluster: "prod", ca_fingerprint: "sha256:00" },
    gc: { task: "t9", started_at: 1, marker_at: 1, phase: "sweep" },
    last_verify: { started_at: 1, finished_at: 1_793_761_200, snapshots: 2, chunks: 10, bad: 2 },
  },
});

function stores(items: BackupStore[]) {
  server.use(http.get(apiUrl("/backup-stores"), () => HttpResponse.json({ revision: 1, items })));
}

it("shows the empty state", async () => {
  mockLists({ node: [NODE], "storage-pool": [pool("bk", { backups: true })] });
  const { container } = renderPage(<BackupStoresPage />);
  expect(await screen.findByText("No backup stores")).toBeVisible();
  expect(screen.getByRole("button", { name: "Create a backup store" })).toBeEnabled();
  await expectNoAxeViolations(container);
});

it("lists stores with backend, encryption, usage, GC and verify state", async () => {
  mockLists({ node: [NODE], "storage-pool": [pool("bk", { backups: true })] });
  stores([LOCAL, DR]);
  const { container } = renderPage(<BackupStoresPage />);
  const table = await screen.findByRole("table", { name: "Backup stores" });
  const local = within(table).getByRole("button", { name: "Open local" }).closest("tr")!;
  expect(local).toHaveTextContent("Backup pool");
  expect(local).toHaveTextContent("Not encrypted");
  expect(local).toHaveTextContent("12");
  expect(local).toHaveTextContent("4 GiB");
  expect(local).toHaveTextContent("3.3×");
  expect(local).toHaveTextContent("Removed 3 chunks (12 MiB)");
  expect(local).toHaveTextContent("Verified");
  expect(local).toHaveTextContent("Ready");
  const dr = within(table).getByRole("button", { name: "Open s3-dr" }).closest("tr")!;
  expect(dr).toHaveTextContent("Read-only");
  expect(dr).toHaveTextContent("https://s3.example.com/bk/orbi");
  expect(dr).toHaveTextContent("Encrypted");
  expect(dr).toHaveTextContent("ab:cd:ef:01…");
  expect(dr).toHaveTextContent("GC: sweeping");
  expect(dr).toHaveTextContent("2 bad chunks");
  expect(dr).toHaveTextContent("Failed");
  expect(dr).toHaveTextContent("store.json is unreadable");
  await expectNoAxeViolations(container);
});

it("creates an encrypted store and does not close until the recovery key is stored", async () => {
  const user = userEvent.setup();
  mockLists({ node: [NODE], "storage-pool": [pool("bk", { backups: true })] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/backup-stores"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(
        {
          store: backupStore("st", 3, {
            spec: { encryption: { fingerprint: FP } },
            status: { phase: "initializing" },
          }),
          recovery_key: KEY,
          task: task("t1", "backup-store-init", "st"),
        },
        { status: 201 },
      );
    }),
  );
  const { container } = renderPage(<BackupStoresPage />);
  await user.click(await screen.findByRole("button", { name: "Create a backup store" }));
  const wizard = await screen.findByRole("dialog", { name: "Create a backup store" });
  expect(within(wizard).getByRole("listitem", { current: "step" })).toHaveTextContent("1. Backend");
  await expectNoAxeViolations(container, { disable: ["region"] });
  // ① 이름이 없으면 넘어가지 않는다.
  await user.click(within(wizard).getByRole("button", { name: "Next" }));
  expect(within(wizard).getByText(/^Use 1–63 lowercase letters/)).toBeVisible();
  await user.type(within(wizard).getByLabelText("Name"), "st");
  expect(within(wizard).getByLabelText("Backup pool")).toHaveValue("bk");
  await user.click(within(wizard).getByRole("button", { name: "Next" }));
  // ② 새 키
  await user.click(within(wizard).getByRole("radio", { name: /Encrypt with a new key/ }));
  expect(within(wizard).getByRole("listitem", { current: "step" })).toHaveTextContent(
    "2. Encryption",
  );
  // 새 키면 ④ 복구 키 단계가 있다.
  expect(within(wizard).getByText("4. Recovery key")).toBeVisible();
  await user.click(within(wizard).getByRole("button", { name: "Next" }));
  // ③ 일정의 기본: GC 매일 05:00, 검증 일요일 06:00
  expect(within(wizard).getByLabelText("GC time (UTC)")).toHaveValue("05:00");
  expect(within(wizard).getByLabelText("Verify time (UTC)")).toHaveValue("06:00");
  await user.click(within(wizard).getByRole("button", { name: "Create" }));
  // ④ 복구 키
  expect(await within(wizard).findByLabelText("Recovery key")).toHaveTextContent(KEY);
  expect(within(wizard).getByText(`Key fingerprint: ${FP}`)).toBeVisible();
  expect(
    within(wizard).getByRole("img", { name: "Recovery key of st as a QR code" }),
  ).toBeVisible();
  expect(body).toEqual({
    name: "st",
    spec: {
      backend: { pool: "bk" },
      gc: { at: "05:00", days: [] },
      verify: { schedule: { at: "06:00", days: ["sun"] }, reverify_days: 30, mode: "full" },
    },
    encryption: { generate: true },
  });
  const done = within(wizard).getByRole("button", { name: "Done" });
  expect(done).toBeDisabled();
  // 확인 없이는 닫히지 않는다(Esc).
  await user.keyboard("{Escape}");
  expect(screen.getByRole("dialog", { name: "Create a backup store" })).toBeVisible();
  expect(within(wizard).getByRole("alert")).toHaveTextContent(
    "Confirm that you have stored the recovery key before closing.",
  );
  await user.click(within(wizard).getByLabelText("I have stored the recovery key"));
  await user.click(done);
  await waitFor(() =>
    expect(screen.queryByRole("dialog", { name: "Create a backup store" })).not.toBeInTheDocument(),
  );
});

it("asks for the S3 endpoint, bucket and credentials and imports a recovery key", async () => {
  const user = userEvent.setup();
  mockLists({ node: [NODE], "storage-pool": [pool("bk", { backups: true })] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/backup-stores"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(
        { store: backupStore("dr", 4), task: task("t2", "backup-store-init", "dr") },
        { status: 201 },
      );
    }),
  );
  renderPage(<BackupStoresPage />);
  await user.click(await screen.findByRole("button", { name: "Create a backup store" }));
  const wizard = await screen.findByRole("dialog", { name: "Create a backup store" });
  await user.type(within(wizard).getByLabelText("Name"), "dr");
  await user.selectOptions(within(wizard).getByLabelText("Backend"), "s3");
  await user.click(within(wizard).getByRole("button", { name: "Next" }));
  expect(within(wizard).getByText("Enter an https:// endpoint.")).toBeVisible();
  expect(within(wizard).getByText("Enter the bucket.")).toBeVisible();
  await user.type(within(wizard).getByLabelText("Endpoint"), "https://s3.example.com");
  await user.type(within(wizard).getByLabelText("Bucket"), "bk");
  await user.type(within(wizard).getByLabelText("Access key ID"), "AK");
  await user.type(within(wizard).getByLabelText("Secret access key"), "SK");
  await user.click(within(wizard).getByLabelText("Attach read-only"));
  await user.click(within(wizard).getByRole("button", { name: "Next" }));
  // 읽기 전용이면 새 키를 고를 수 없다.
  expect(within(wizard).queryByRole("radio", { name: /Encrypt with a new key/ })).toBeNull();
  await user.click(within(wizard).getByRole("radio", { name: /Use a recovery key/ }));
  await user.type(within(wizard).getByLabelText("Recovery key"), "nope");
  await user.click(within(wizard).getByRole("button", { name: "Next" }));
  expect(within(wizard).getByText("Enter a recovery key that starts with OPK1-.")).toBeVisible();
  await user.clear(within(wizard).getByLabelText("Recovery key"));
  await user.type(within(wizard).getByLabelText("Recovery key"), KEY);
  await user.click(within(wizard).getByRole("button", { name: "Next" }));
  await user.click(within(wizard).getByLabelText("Verify on a schedule"));
  await user.click(within(wizard).getByRole("button", { name: "Create" }));
  // 새 키가 아니면 복구 키 화면 없이 닫는다.
  await waitFor(() =>
    expect(screen.queryByRole("dialog", { name: "Create a backup store" })).not.toBeInTheDocument(),
  );
  expect(body).toEqual({
    name: "dr",
    spec: {
      backend: {
        s3: {
          endpoint: "https://s3.example.com",
          region: "us-east-1",
          bucket: "bk",
          access_key_id: "AK",
          secret_access_key: "SK",
        },
      },
      read_only: true,
      gc: { at: "05:00", days: [] },
    },
    encryption: { recovery_key: KEY },
  });
});

it("opens the drawer with schedules, node reachability and the key card", async () => {
  const user = userEvent.setup();
  const node: Node = {
    ...NODE,
    status: {
      ...NODE.status,
      backup_stores: [{ store: DR.id, ok: false, error: "connection refused", checked_at: 1 }],
    },
  };
  mockLists({ node: [node], "storage-pool": [pool("bk", { backups: true })] });
  stores([LOCAL, DR]);
  let exported = false;
  server.use(
    http.post(apiUrl(`/backup-stores/${DR.id}/key/export`), () => {
      exported = true;
      return HttpResponse.json({ recovery_key: KEY });
    }),
  );
  renderPage(<BackupStoresPage />);
  await user.click(await screen.findByRole("button", { name: "Open s3-dr" }));
  const sheet = await screen.findByRole("dialog", { name: "Backup store s3-dr" });
  expect(sheet).toHaveTextContent("Owner clusterprod");
  expect(sheet).toHaveTextContent("Sun 06:00 UTC");
  expect(sheet).toHaveTextContent("Verify again after 30 days, Full (read every chunk)");
  const reach = within(sheet).getByRole("table", { name: "Reachable from nodes" });
  expect(reach).toHaveTextContent("node-a");
  expect(reach).toHaveTextContent("Unreachable");
  expect(reach).toHaveTextContent("connection refused");
  // 읽기 전용: GC는 없고 스캔·넘겨받기가 있다. escrow는 쓰지 않는다.
  expect(within(sheet).queryByRole("button", { name: "Run GC now" })).toBeNull();
  expect(within(sheet).getByRole("button", { name: "Take over" })).toBeVisible();
  expect(within(sheet).queryByRole("button", { name: "Escrow in the store" })).toBeNull();
  expect(within(sheet).getByText(FP)).toBeVisible();
  // 복구 키 다시 보기는 확인 뒤에(감사에 남음)
  await user.click(within(sheet).getByRole("button", { name: "Show the recovery key" }));
  const confirm = await screen.findByRole("alertdialog");
  expect(confirm).toHaveTextContent("recorded in the audit log");
  expect(exported).toBe(false);
  await user.click(within(confirm).getByRole("button", { name: "Show" }));
  const shown = await screen.findByRole("dialog", { name: "Recovery key of s3-dr" });
  expect(within(shown).getByLabelText("Recovery key")).toHaveTextContent(KEY);
  expect(exported).toBe(true);
});

it("runs GC and verification now and takes over a read-only store", async () => {
  const user = userEvent.setup();
  mockLists({ node: [NODE], "storage-pool": [pool("bk", { backups: true })] });
  const dr = { ...DR, status: { ...DR.status, phase: "ready" as const, gc: undefined } };
  stores([LOCAL, dr]);
  const calls: string[] = [];
  let takeOver: unknown;
  server.use(
    http.post(apiUrl(`/backup-stores/${LOCAL.id}/actions/gc`), () => {
      calls.push("gc");
      return HttpResponse.json(task("t3", "backup-store-gc", LOCAL.id), { status: 202 });
    }),
    http.post(apiUrl(`/backup-stores/${LOCAL.id}/actions/verify`), () => {
      calls.push("verify");
      return HttpResponse.json(task("t4", "backup-store-verify", LOCAL.id), { status: 202 });
    }),
    http.post(apiUrl(`/backup-stores/${dr.id}/actions/take-over`), async ({ request }) => {
      takeOver = await request.json();
      return HttpResponse.json(dr);
    }),
  );
  renderPage(<BackupStoresPage />);
  await user.click(await screen.findByRole("button", { name: "Open local" }));
  let sheet = await screen.findByRole("dialog", { name: "Backup store local" });
  expect(within(sheet).getByText("This store is not encrypted.")).toBeVisible();
  await user.click(within(sheet).getByRole("button", { name: "Run GC now" }));
  await user.click(within(sheet).getByRole("button", { name: "Verify now" }));
  await waitFor(() => expect(calls).toEqual(["gc", "verify"]));
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());

  await user.click(screen.getByRole("button", { name: "Open s3-dr" }));
  sheet = await screen.findByRole("dialog", { name: "Backup store s3-dr" });
  await user.click(within(sheet).getByRole("button", { name: "Take over" }));
  const confirm = await screen.findByRole("alertdialog");
  expect(confirm).toHaveTextContent("Cluster prod must not use it any more");
  await user.click(within(confirm).getByLabelText(/Take over even if/));
  await user.type(within(confirm).getByLabelText("Type s3-dr to confirm"), "s3-dr");
  await user.click(within(confirm).getByRole("button", { name: "Take over" }));
  await waitFor(() => expect(takeOver).toEqual({ force: true }));
});

it("hides store changes and the key actions from operators", async () => {
  const user = userEvent.setup();
  mockLists({ node: [NODE], "storage-pool": [pool("bk", { backups: true })] });
  stores([LOCAL, DR]);
  renderPage(<BackupStoresPage />, { as: { ...ADMIN, role: "operator" } });
  await user.click(await screen.findByRole("button", { name: "Open s3-dr" }));
  const sheet = await screen.findByRole("dialog", { name: "Backup store s3-dr" });
  expect(within(sheet).queryByRole("button", { name: "Verify now" })).toBeNull();
  expect(within(sheet).queryByRole("button", { name: "Show the recovery key" })).toBeNull();
  expect(sheet).toHaveTextContent("Only cluster administrators can show the recovery key");
  expect(screen.queryByRole("button", { name: "Create a backup store" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Delete local" })).toBeNull();
});
