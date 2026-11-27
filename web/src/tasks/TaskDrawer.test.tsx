import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { Task } from "@/api/types";
import { openTask } from "@/tasks/center";
import { TaskDrawer } from "@/tasks/TaskDrawer";
import { TasksPage } from "@/tasks/TasksPage";
import { expectNoAxeViolations } from "@/test/axe";
import { backup, NODE, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";

const web = vm("web", "running");
const bk = backup("web-1", web, 1);
// 만든 시각(UUIDv7) 2025-10-23T04:03:37.104Z
const FAILED = "019a0f3c-8a10-7000-8000-000000000001";
const OLD = "019a0f3c-8a10-7000-8000-000000000002";
const LONG =
  "QMP command failed: GenericError: TLS credentials support requires the migration to be configured with tls-creds; " +
  "the object 'migrate-tls' was not found on node svc02 while setting up the outgoing connection (see journalctl -u orbivisord)";

const TASKS = [
  {
    id: FAILED,
    name: FAILED,
    revision: 3,
    labels: {
      "requested-by": "00000000-0000-0000-0000-000000000001",
      "requested-by-name": "admin",
      "target-name": "web",
      "target-kind": "vm",
    },
    spec: { op: { kind: "vm-backup", backup: bk.id }, target: web.id, node: NODE.id },
    status: {
      state: "failed",
      error: LONG,
      started_at: 1_761_192_218_000,
      finished_at: 1_761_192_230_000,
    },
  },
  {
    // 옛 노드가 끝낸 Task: 시각·이름 레이블이 없고, 대상은 지워졌다.
    id: OLD,
    name: OLD,
    revision: 2,
    spec: {
      op: { kind: "volume-delete" },
      target: "019a0f3c-0000-7000-8000-00000000beef",
      node: NODE.id,
    },
    status: { state: "succeeded", error: null },
  },
] as Task[];

function page(url = "/tasks") {
  return renderPage(
    <>
      <TasksPage />
      <TaskDrawer />
    </>,
    { path: "/tasks", url },
  );
}

beforeEach(() => {
  mockLists({ vm: [web], node: [NODE], backup: [bk], task: TASKS });
});

it("opens one drawer with who, when, where and the whole error from the task list", async () => {
  const { router } = page();
  // 목록의 오류는 잘리지 않고 2줄로 접혀 있다가 펼친다(마우스를 올릴 필요 없이).
  const row = (await screen.findByRole("link", { name: "Back up web" })).closest("tr")!;
  expect(row).toHaveTextContent(LONG);
  const more = within(row).getByRole("button", { name: "Show all" });
  expect(more).toHaveAttribute("aria-expanded", "false");
  await userEvent.click(more);
  expect(within(row).getByRole("button", { name: "Show less" })).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  expect(row).toHaveTextContent("admin");

  await userEvent.click(within(row).getByRole("link", { name: "Back up web" }));
  await waitFor(() => expect(router.state.location.search).toBe(`?task=${FAILED}`));
  const drawer = await screen.findByRole("dialog", { name: "Back up web" });
  expect(within(drawer).getByText(LONG).tagName).toBe("PRE");
  expect(within(drawer).getByRole("button", { name: "Copy the error text" })).toBeInTheDocument();
  expect(within(drawer).getByRole("link", { name: "web" })).toHaveAttribute(
    "href",
    "/vms/web?tab=backups",
  );
  expect(within(drawer).getByRole("link", { name: NODE.name })).toHaveAttribute(
    "href",
    `/nodes/${NODE.name}`,
  );
  // 이어진 대상: 만든 백업
  expect(within(drawer).getByRole("link", { name: "web-1" })).toHaveAttribute(
    "href",
    "/backups?q=web-1",
  );
  expect(drawer).toHaveTextContent("requested by admin");
  expect(drawer).toHaveTextContent("Took12 s");
  expect(drawer).toHaveTextContent(`Task id${FAILED}`);
  expect(drawer).toHaveTextContent("(vm-backup)");
  await expectNoAxeViolations(drawer, { disable: ["region"] });

  // 닫으면 주소에서 빠진다.
  await userEvent.keyboard("{Escape}");
  await waitFor(() => expect(router.state.location.search).toBe(""));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("opens from the address and shows what an old task did not record", async () => {
  page(`/tasks?task=${OLD}`);
  const drawer = await screen.findByRole("dialog", { name: "Delete volume 0000beef" });
  expect(drawer).toHaveTextContent("StartedNot recorded (older node version)");
  expect(drawer).toHaveTextContent("FinishedNot recorded (older node version)");
  // 사람이 요청하지 않은(또는 옛) Task는 "자동"
  expect(drawer).toHaveTextContent("Requested byAutomatic");
  expect(within(drawer).queryByRole("button", { name: "Copy the error text" })).toBeNull();
});

it("says when the task is gone and opens from a failure toast", async () => {
  const { router } = page("/tasks?task=019a0f3c-8a10-7000-8000-00000000dead");
  expect(await screen.findByText(/This task no longer exists/)).toBeInTheDocument();
  act(() => openTask(FAILED));
  await waitFor(() => expect(router.state.location.search).toBe(`?task=${FAILED}`));
  expect(await screen.findByRole("dialog", { name: "Back up web" })).toBeInTheDocument();
});

it("keeps the error in the phone cards", async () => {
  const original = window.matchMedia;
  window.matchMedia = ((query: string) => ({
    ...original(query),
    matches: query === "(max-width: 767px)",
  })) as typeof window.matchMedia;
  try {
    page();
    const list = await screen.findByRole("list", { name: "Tasks" });
    const card = (await within(list).findByRole("link", { name: "Back up web" })).closest("li")!;
    expect(card).toHaveTextContent("Back up web");
    expect(card).toHaveTextContent(LONG);
  } finally {
    window.matchMedia = original;
  }
});
