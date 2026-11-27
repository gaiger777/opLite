import { screen, within } from "@testing-library/react";

import type { Node } from "@/api/types";
import { BackupsPage } from "@/backups/BackupsPage";
import { describeTask } from "@/tasks/tasks";
import { taskTarget } from "@/tasks/target";
import { backup, NODE, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import i18n from "@/i18n";
import { rowAction } from "@/test/rowMenu";

// 계획 69: 백업 표의 "파일 찾아보기" — 권한·기능·도우미 이미지에 따라 숨김·막힘·링크
const web = vm("web", "running");
const ready = backup("web-1", web, 1);
const pending = backup("web-2", web, 2, { status: { phase: "pending", created_at: null } });
const IMAGE = {
  accel: "kvm",
  arch: "x86_64",
  image_version: "v1",
  max_sessions: 4,
  sessions: 0,
} as const;

function node(n: number, extra: Partial<Node["status"]>): Node {
  return { ...NODE, id: `n${n}`, name: `node-${n}`, status: { ...NODE.status, ...extra } };
}

async function row(name: string) {
  const table = await screen.findByRole("table", { name: "Backups" });
  return (await within(table).findByText(name)).closest("tr")!;
}

it("links ready backups to the file browser when a node has the helper image", async () => {
  mockLists({
    vm: [web],
    backup: [ready, pending],
    node: [
      node(1, { features: ["file-restore"] }),
      node(2, { features: ["file-restore"], file_restore: IMAGE }),
    ],
  });
  renderPage(<BackupsPage />);
  const link = await rowAction("Browse files of web-1");
  expect(link).toHaveAttribute("href", `/backups/${ready.id}/files`);
  // 만드는 중인 백업은 찾아볼 수 없다.
  expect(await rowAction("Browse files of web-2")).toHaveAttribute("aria-disabled", "true");
});

it("disables browsing with the reason when no node has the helper image", async () => {
  mockLists({ vm: [web], backup: [ready], node: [node(1, { features: ["file-restore"] })] });
  renderPage(<BackupsPage />);
  expect(
    await screen.findByText(
      "Browsing files needs the orbivisor-restore-image package on at least one node.",
    ),
  ).toBeInTheDocument();
  const browse = await rowAction("Browse files of web-1");
  expect(browse).toHaveAttribute("aria-disabled", "true");
  expect(browse).toHaveAccessibleDescription(
    "Browsing files needs the orbivisor-restore-image package on at least one node.",
  );
});

it("hides browsing while a node does not know file restore yet", async () => {
  mockLists({
    vm: [web],
    backup: [ready],
    node: [node(1, { features: ["file-restore"], file_restore: IMAGE }), NODE],
  });
  renderPage(<BackupsPage />);
  const r = await row("web-1");
  expect(within(r).getByRole("button", { name: "Restore web-1" })).toBeInTheDocument();
  expect(within(r).queryByRole("link", { name: /Browse files/ })).not.toBeInTheDocument();
  expect(within(r).queryByRole("button", { name: /Browse files/ })).not.toBeInTheDocument();
});

it("hides browsing without the backup.file_restore privilege (not in operator)", async () => {
  mockLists({
    vm: [web],
    backup: [ready],
    node: [node(1, { features: ["file-restore"], file_restore: IMAGE })],
  });
  renderPage(<BackupsPage />, { as: { ...ADMIN, role: "operator" } });
  const r = await row("web-1");
  expect(within(r).getByRole("button", { name: "Restore web-1" })).toBeInTheDocument();
  expect(within(r).queryByRole("link", { name: /Browse files/ })).not.toBeInTheDocument();
});

it("names file restore tasks", () => {
  const t = i18n.getFixedT("en");
  const lookup = {
    vm: new Map([[web.id, web]]),
    volume: new Map(),
    backup: new Map([[ready.id, ready]]),
    import: new Map(),
    node: new Map(),
  };
  const session = {
    ...task("t1", "file-restore-session", "019a0f3c-0000-7000-8010-000000000001"),
  };
  session.spec.op = { kind: "file-restore-session", backup: ready.id };
  const target = taskTarget(session, lookup);
  expect(target).toEqual({ name: "web-1", href: `/backups/${ready.id}/files` });
  expect(describeTask(session, t, target.name)).toBe("Browse files of backup web-1");
  const toGuest = task("t2", "file-restore-to-guest", web.id);
  expect(describeTask(toGuest, t, taskTarget(toGuest, lookup).name)).toBe(
    "Restore files from a backup into web",
  );
});
