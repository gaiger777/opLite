import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Node, Vm } from "@/api/types";
import { startDownload } from "@/fileRestore/download";
import type { EntryView, GuestRestoreCaps, Session, VolumeView } from "@/fileRestore/fileRestore";
import { FileRestorePage } from "@/fileRestore/FileRestorePage";
import { resetTasks } from "@/tasks/tasks";
import { expectNoAxeViolations } from "@/test/axe";
import { backup, NODE, task, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

vi.mock("@/fileRestore/download", () => ({ startDownload: vi.fn() }));

afterEach(() => {
  resetTasks();
  vi.mocked(startDownload).mockClear();
});

const base = vm("web", "running");
const web: Vm = { ...base, spec: { ...base.spec, guest_agent: { freeze_fs: true } } };
const other: Vm = {
  ...vm("db", "running"),
  spec: { ...vm("db", "running").spec, guest_agent: { freeze_fs: false } },
};
const b1 = backup("web-20261123-030000", web, 1);
const node = (accel: string): Node => ({
  ...NODE,
  status: {
    ...NODE.status,
    features: ["file-restore"],
    file_restore: { accel, arch: "x86_64", image_version: "v1", max_sessions: 4, sessions: 1 },
  },
});

const SID = "019a0f3c-0000-7000-8010-000000000001";
const NOW = Math.floor(Date.now() / 1000);

const VOLUMES: VolumeView[] = [
  { id: "disk0", disk: 0, kind: "disk", mountable: false, size: 8 << 30, state: "unmounted" },
  {
    id: "disk0p1",
    disk: 0,
    kind: "partition",
    fs: "ext4",
    label: "root",
    mountable: true,
    size: 6 << 30,
    state: "unmounted",
    note: "journal not replayed — recent changes may be missing",
    partition: { number: 1, table: "gpt", type_name: "Linux filesystem" },
  },
  {
    id: "disk0p2",
    disk: 0,
    kind: "partition",
    fs: "crypto_LUKS",
    mountable: false,
    size: 2 << 30,
    state: "unmounted",
    note: "encrypted (LUKS) — not supported",
    partition: { number: 2, table: "gpt", type_name: "Linux filesystem" },
  },
  {
    id: "lvm/vg/data",
    disk: 1,
    kind: "logical_volume",
    fs: "xfs",
    mountable: true,
    size: 1 << 30,
    state: "unmounted",
  },
];

function session(phase: Session["phase"], extra: Partial<Session> = {}): Session {
  return {
    id: SID,
    backup: b1.id,
    backup_name: b1.name,
    node: NODE.id,
    opened_by: "u1",
    opened_at: NOW - 23,
    last_used_at: NOW,
    expires_at: phase === "ready" ? NOW + 14 * 60 : 0,
    phase,
    volumes: phase === "ready" ? VOLUMES : [],
    ...extra,
  };
}

function e(name: string, extra: Partial<EntryView> = {}): EntryView {
  return {
    name,
    kind: "file",
    mode: 0o644,
    size: 1024,
    mtime: 1_793_761_200,
    uid: 0,
    gid: 0,
    ...extra,
  };
}

/** 세션 API. `sessions`는 GET마다 차례로(마지막은 되풀이) 돌려준다. */
function mockSession(sessions: Session[], dirs: Record<string, EntryView[][]> = {}) {
  let n = 0;
  const calls = {
    list: [] as string[],
    links: [] as unknown[],
    restores: [] as unknown[],
    deleted: 0,
  };
  server.use(
    http.post(apiUrl("/backups/:key/file-restore"), () =>
      HttpResponse.json({ session: sessions[0], task: "t1" }, { status: 202 }),
    ),
    http.get(apiUrl("/file-restore/sessions/:id"), () =>
      HttpResponse.json(sessions[Math.min(n++, sessions.length - 1)]),
    ),
    http.delete(apiUrl("/file-restore/sessions/:id"), () => {
      calls.deleted += 1;
      sessions.splice(0, sessions.length, session("closed", { error: undefined }));
      n = 0;
      return new HttpResponse(null, { status: 204 });
    }),
    http.get(
      apiUrl("/file-restore/sessions/:id/volumes/:volume/entries"),
      ({ request, params }) => {
        const url = new URL(request.url);
        const path = url.searchParams.get("path") ?? `b64:${url.searchParams.get("path_b64")}`;
        const cursor = url.searchParams.get("cursor");
        calls.list.push(`${String(params.volume)} ${path}${cursor ? ` @${cursor}` : ""}`);
        const pages = dirs[`${String(params.volume)}:${path}`] ?? [[]];
        const i = cursor ? Number(cursor) : 0;
        return HttpResponse.json({
          entries: pages[i],
          ...(i + 1 < pages.length && { next: String(i + 1) }),
        });
      },
    ),
    http.post(apiUrl("/file-restore/sessions/:id/download-link"), async ({ request }) => {
      calls.links.push(await request.json());
      return HttpResponse.json({
        url: `/file-restore/sessions/${SID}/download/tok`,
        expires_in: 60,
      });
    }),
    http.post(apiUrl("/file-restore/sessions/:id/restore-to-vm"), async ({ request }) => {
      calls.restores.push(await request.json());
      return HttpResponse.json(task("t9", "file-restore-to-guest", web.id), { status: 202 });
    }),
  );
  return calls;
}

function open() {
  return renderPage(<FileRestorePage />, {
    path: "/backups/:id/files",
    url: `/backups/${b1.id}/files`,
  });
}

const ROOT = [
  e("etc", { kind: "dir", mode: 0o755 }),
  e("hello.txt", { size: 13 }),
  e("link", { kind: "symlink", mode: 0o777, link_target: "hello.txt" }),
  e("�.bin", { raw: btoa("\xff.bin") }),
];

it("shows the helper VM starting with the elapsed time and a slow-node hint, then the volumes", async () => {
  mockLists({ vm: [web], node: [node("tcg")], backup: [b1] });
  mockSession([session("booting"), session("ready")], { "disk0p1:/": [ROOT] });
  open();
  expect(await screen.findByRole("heading", { name: `Files in ${b1.name}` })).toBeInTheDocument();
  expect(screen.getByText(/^Starting 0:2\d$/)).toBeInTheDocument();
  expect(
    await screen.findByText("Slow node (no hardware acceleration): up to a minute"),
  ).toBeInTheDocument();
  // 2초 뒤 다시 물어 준비됨
  expect(await screen.findByText("Ready", {}, { timeout: 4000 })).toBeInTheDocument();
  expect(screen.getByText("Closes in 14 min without use")).toBeInTheDocument();
  const tree = screen.getByRole("navigation", { name: "Volumes" });
  // 디스크 자체(파티션 표)는 행이 아니다. 파티션·LV는 행이고, 막힌 것은 까닭이 있다.
  expect(within(tree).getByRole("heading", { name: /Disk 0/ })).toBeInTheDocument();
  const p1 = within(tree).getByRole("button", { name: /Partition 1/ });
  expect(p1).toHaveAttribute("aria-current", "true");
  expect(p1).toHaveAccessibleDescription("journal not replayed — recent changes may be missing");
  const p2 = within(tree).getByRole("button", { name: /Partition 2/ });
  expect(p2).toHaveAttribute("aria-disabled", "true");
  expect(p2).toHaveAccessibleDescription("encrypted (LUKS) — not supported");
  expect(within(tree).getByRole("button", { name: /LV vg\/data/ })).toBeInTheDocument();
  // 첫 찾아볼 수 있는 볼륨을 나열한다.
  const table = await screen.findByRole("table", { name: "Files in /" });
  await within(table).findByText("hello.txt");
  expect(within(table).getByText("→ hello.txt")).toBeInTheDocument();
  expect(within(table).getByText("drwxr-xr-x")).toBeInTheDocument();
  expect(within(table).getByText("(not UTF-8)")).toBeInTheDocument();
});

it("enters folders, pages, filters and selects with the keyboard", async () => {
  mockLists({ vm: [web], node: [node("kvm")], backup: [b1] });
  const etc = [
    [e("hosts"), e("nginx", { kind: "dir" })],
    [e("passwd"), e("shadow", { mode: 0o640 })],
  ];
  const calls = mockSession([session("ready")], {
    "disk0p1:/": [ROOT],
    "disk0p1:/etc": etc,
    "disk0p1:/etc/nginx": [[e("nginx.conf")]],
  });
  const user = userEvent.setup();
  open();
  const table = await screen.findByRole("table", { name: "Files in /" });
  await user.click(await within(table).findByRole("button", { name: "etc" }));
  const inEtc = await screen.findByRole("table", { name: "Files in /etc" });
  await within(inEtc).findByText("hosts");
  expect(within(inEtc).queryByText("passwd")).not.toBeInTheDocument();
  // 다음 쪽(jsdom에는 IntersectionObserver가 없어 단추로)
  await user.click(screen.getByRole("button", { name: "Load more" }));
  await within(inEtc).findByText("passwd");
  expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  expect(calls.list).toEqual(["disk0p1 /", "disk0p1 /etc", "disk0p1 /etc @1"]);
  // 이름 거르기
  await user.type(screen.getByRole("searchbox", { name: "Filter names in this folder" }), "sha");
  expect(within(inEtc).getAllByRole("row")).toHaveLength(2);
  await user.clear(screen.getByRole("searchbox", { name: "Filter names in this folder" }));
  // 키보드: 첫 행에서 ↓ 두 번, Space로 고르기, Backspace로 위로
  const rows = within(inEtc).getAllByRole("row").slice(1);
  rows[0]!.focus();
  await user.keyboard("{ArrowDown}{ArrowDown}");
  expect(rows[2]).toHaveFocus();
  await user.keyboard(" ");
  expect(within(rows[2]!).getByRole("checkbox", { name: "Select passwd" })).toBeChecked();
  expect(screen.getByText("1 selected")).toBeInTheDocument();
  // Enter로 폴더 들어가기
  rows[1]!.focus();
  await user.keyboard("{Enter}");
  await screen.findByRole("table", { name: "Files in /etc/nginx" });
  expect(screen.getByRole("navigation", { name: "Folder path" })).toHaveTextContent(
    "Partition 1etcnginx",
  );
  const nginxRows = within(screen.getByRole("table", { name: "Files in /etc/nginx" })).getAllByRole(
    "row",
  );
  nginxRows[1]!.focus();
  await user.keyboard("{Backspace}");
  await screen.findByRole("table", { name: "Files in /etc" });
  // 이동 경로로 루트에
  await user.click(
    within(screen.getByRole("navigation", { name: "Folder path" })).getByRole("button", {
      name: "Partition 1",
    }),
  );
  await screen.findByRole("table", { name: "Files in /" });
});

it("downloads through a one-time link: one file as is, several as zip, or tar.gz", async () => {
  mockLists({ vm: [web], node: [node("kvm")], backup: [b1] });
  const calls = mockSession([session("ready")], { "disk0p1:/": [ROOT] });
  const user = userEvent.setup();
  open();
  const table = await screen.findByRole("table", { name: "Files in /" });
  await within(table).findByText("hello.txt");
  // 고른 것이 없으면 지금 폴더(볼륨 루트) 전체
  await user.click(screen.getByRole("button", { name: "Download folder" }));
  await waitFor(() => expect(startDownload).toHaveBeenCalledTimes(1));
  expect(startDownload).toHaveBeenCalledWith(`/api/v1/file-restore/sessions/${SID}/download/tok`);
  await user.click(within(table).getByRole("checkbox", { name: "Select hello.txt" }));
  await user.click(screen.getByRole("button", { name: "Download" }));
  await waitFor(() => expect(startDownload).toHaveBeenCalledTimes(2));
  await user.click(within(table).getByRole("checkbox", { name: "Select �.bin" }));
  await user.click(screen.getByRole("button", { name: "Download 2 as zip" }));
  await waitFor(() => expect(startDownload).toHaveBeenCalledTimes(3));
  await user.click(screen.getByRole("button", { name: "Download format" }));
  await user.click(await screen.findByRole("menuitem", { name: /Download as tar\.gz/ }));
  await waitFor(() => expect(startDownload).toHaveBeenCalledTimes(4));
  const raw = btoa("\xff.bin");
  expect(calls.links).toEqual([
    { volume: "disk0p1", paths: [[]], format: "zip" },
    { volume: "disk0p1", paths: [["hello.txt"]], format: "auto" },
    { volume: "disk0p1", paths: [["hello.txt"], [{ b64: raw }]], format: "zip" },
    { volume: "disk0p1", paths: [["hello.txt"], [{ b64: raw }]], format: "tar.gz" },
  ]);
  // 흐름이 꽉 찼으면(429) 알린다.
  server.use(
    http.post(apiUrl("/file-restore/sessions/:id/download-link"), () =>
      HttpResponse.json(
        { code: "file_restore_busy", message: "4 streams are open" },
        { status: 429 },
      ),
    ),
  );
  await user.click(screen.getByRole("button", { name: "Download 2 as zip" }));
  expect(
    await screen.findByText(/^Too many browsing sessions or downloads are running/),
  ).toBeInTheDocument();
  expect(startDownload).toHaveBeenCalledTimes(4);
});

it("lists LVM volumes with an encoded id and non-UTF-8 folders by bytes", async () => {
  mockLists({ vm: [web], node: [node("kvm")], backup: [b1] });
  const raw = btoa("\xff");
  const calls = mockSession([session("ready")], {
    "disk0p1:/": [ROOT],
    "lvm/vg/data:/": [[e("�", { kind: "dir", raw })]],
    "lvm/vg/data:b64:L_8": [[e("inside.txt")]],
  });
  const user = userEvent.setup();
  open();
  await screen.findByText("hello.txt");
  await user.click(screen.getByRole("button", { name: /LV vg\/data/ }));
  const table = await screen.findByRole("table", { name: "Files in /" });
  await user.click(await within(table).findByRole("button", { name: "�" }));
  await screen.findByText("inside.txt");
  expect(calls.list).toEqual(["disk0p1 /", "lvm/vg/data /", "lvm/vg/data b64:L_8"]);
});

it("shows why a volume cannot be listed and closes the session", async () => {
  mockLists({ vm: [web], node: [node("kvm")], backup: [b1] });
  mockSession([session("ready")]);
  server.use(
    http.get(apiUrl("/file-restore/sessions/:id/volumes/:volume/entries"), () =>
      HttpResponse.json(
        { code: "volume_not_mountable", message: "mount: wrong fs type" },
        { status: 409 },
      ),
    ),
  );
  const user = userEvent.setup();
  open();
  expect(
    await screen.findByText("This volume cannot be browsed: mount: wrong fs type"),
  ).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Close" }));
  expect(await screen.findByText("This browsing session is closed")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Open again" })).toBeInTheDocument();
});

it("offers to open again when the session failed or cannot be opened", async () => {
  mockLists({ vm: [web], node: [node("kvm")], backup: [b1] });
  mockSession([session("failed", { error: "helper VM exited: kernel panic" })]);
  const user = userEvent.setup();
  open();
  expect(await screen.findByText("The helper VM stopped")).toBeInTheDocument();
  expect(screen.getByText("helper VM exited: kernel panic")).toBeInTheDocument();
  server.use(
    http.post(apiUrl("/backups/:key/file-restore"), () =>
      HttpResponse.json(
        { code: "file_restore_unavailable", message: "no node has the restore image" },
        { status: 409 },
      ),
    ),
  );
  await user.click(screen.getByRole("button", { name: "Open again" }));
  expect(await screen.findByText("Could not open the backup")).toBeInTheDocument();
  expect(
    screen.getByText("No node can open this backup: no node has the restore image"),
  ).toBeInTheDocument();
});

it("restores to a VM: original VM first, blocked choices explained", async () => {
  mockLists({ vm: [other, web], node: [node("kvm")], backup: [b1] });
  const calls = mockSession([session("ready")], { "disk0p1:/": [ROOT] });
  const caps: Record<string, GuestRestoreCaps> = {
    [web.id]: { os: "linux", file_write: true, exec: false, reason: "guest-exec is blocked" },
    [other.id]: {
      os: "linux",
      file_write: false,
      exec: false,
      reason: "the agent blocks guest-file-* (default on RHEL/Rocky: edit /etc/sysconfig/qemu-ga)",
    },
  };
  server.use(
    http.get(apiUrl("/vms/:key/guest/restore-caps"), ({ params }) =>
      HttpResponse.json(caps[String(params.key)]),
    ),
  );
  const user = userEvent.setup();
  open();
  const table = await screen.findByRole("table", { name: "Files in /" });
  await within(table).findByText("hello.txt");
  expect(screen.getByRole("button", { name: "Restore to VM" })).toBeDisabled();
  await user.click(within(table).getByRole("checkbox", { name: "Select etc" }));
  await user.click(screen.getByRole("button", { name: "Restore to VM" }));
  const dialog = await screen.findByRole("dialog", { name: "Restore 1 item to a VM" });
  expect(within(dialog).getByRole("combobox", { name: "VM" })).toHaveDisplayValue(
    "web (original VM)",
  );
  // 폴더를 골랐는데 exec가 없다 → 막고 까닭
  expect(
    await within(dialog).findByText(/The selection has folders, but the guest agent/),
  ).toBeInTheDocument();
  expect(within(dialog).getByRole("radio", { name: /Overwrite/ })).toBeDisabled();
  expect(within(dialog).getByRole("button", { name: "Restore" })).toBeDisabled();
  expect(within(dialog).getByRole("textbox", { name: "Folder in the guest" })).toHaveValue("/");
  await expectNoAxeViolations(dialog);
  // 다른 VM은 파일 쓰기를 막는다(RHEL 안내).
  await user.selectOptions(within(dialog).getByRole("combobox", { name: "VM" }), "db");
  expect(
    await within(dialog).findByText(
      /does not allow writing files\. .*edit \/etc\/sysconfig\/qemu-ga/,
    ),
  ).toHaveAttribute("role", "alert");
  await user.click(within(dialog).getByRole("button", { name: "Cancel" }));

  // 파일만 고르면 원래 VM에 keep으로
  await user.click(within(table).getByRole("checkbox", { name: "Select etc" }));
  await user.click(within(table).getByRole("checkbox", { name: "Select hello.txt" }));
  await user.click(screen.getByRole("button", { name: "Restore to VM" }));
  const again = await screen.findByRole("dialog", { name: "Restore 1 item to a VM" });
  await waitFor(() => expect(within(again).getByRole("button", { name: "Restore" })).toBeEnabled());
  const dir = within(again).getByRole("textbox", { name: "Folder in the guest" });
  await user.clear(dir);
  await user.type(dir, "srv");
  await user.click(within(again).getByRole("button", { name: "Restore" }));
  expect(
    await within(again).findByText("Use an absolute path that starts with /."),
  ).toBeInTheDocument();
  await user.clear(dir);
  await user.type(dir, "/srv/restored");
  await user.click(within(again).getByRole("radio", { name: /Skip/ }));
  await user.click(within(again).getByRole("button", { name: "Restore" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  expect(calls.restores).toEqual([
    {
      volume: "disk0p1",
      paths: [["hello.txt"]],
      vm: web.id,
      target_dir: "/srv/restored",
      conflict: "skip",
    },
  ]);
  expect(await screen.findByText("Restore files from a backup into web")).toBeInTheDocument();
});

it("has no accessibility violations on the browse page", async () => {
  mockLists({ vm: [web], node: [node("kvm")], backup: [b1] });
  mockSession([session("ready")], { "disk0p1:/": [ROOT] });
  const { container } = open();
  await screen.findByText("hello.txt");
  await expectNoAxeViolations(container, { disable: ["region"] });
});

it("treats a session the node forgot as ended", async () => {
  mockLists({ vm: [web], node: [node("kvm")], backup: [b1] });
  mockSession([session("booting")]);
  server.use(
    http.get(apiUrl("/file-restore/sessions/:id"), () =>
      HttpResponse.json({ code: "session_not_found", message: "no such session" }, { status: 404 }),
    ),
  );
  open();
  expect(await screen.findByText("The helper VM stopped")).toBeInTheDocument();
  expect(
    screen.getByText(/^This browsing session no longer exists \(the node may have restarted\)/),
  ).toBeInTheDocument();
});
