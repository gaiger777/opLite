import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";

import type { Task } from "@/api/types";
import { openTask } from "@/tasks/center";
import { TaskDrawer } from "@/tasks/TaskDrawer";
import { TasksPage } from "@/tasks/TasksPage";
import { expectNoAxeViolations } from "@/test/axe";
import { backup, NODE, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";

const web = vm("web", "running");
const db = vm("db", "stopped");
const app = vm("app", "stopped");
const bk = backup("web-1", web, 1);
// 만든 시각(UUIDv7) 2025-10-23T04:03:37.104Z
const FAILED = "019a0f3c-8a10-7000-8000-000000000001";
const OLD = "019a0f3c-8a10-7000-8000-000000000002";
const RECORD = "019a0f3c-8a10-7000-8000-000000000003";
const DROPPED = "019a0f3c-8a10-7000-8000-000000000004";
const WAITING = "019a0f3c-8a10-7000-8000-000000000005";
const SKEWED = "019a0f3c-8a10-7000-8000-000000000006";
const CODED = "019a0f3c-8a10-7000-8000-000000000008";
const FIXFIRST = "019a0f3c-8a10-7000-8000-000000000007";
const TRANSIENT = "019a0f3c-8a10-7000-8000-000000000009";
const NOT_ENOUGH =
  'not enough space in storage pool "local" on node "svc02": need 21474836480, available 3221225472';
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
  {
    // 기록 Task(HA 검사 등): 처음부터 끝난 채 만들어 시작과 끝이 같다.
    id: RECORD,
    name: RECORD,
    revision: 1,
    spec: { op: { kind: "ha-check" }, target: NODE.id, node: NODE.id },
    status: {
      state: "succeeded",
      error: null,
      started_at: 1_761_192_218_000,
      finished_at: 1_761_192_218_000,
    },
  },
  {
    // 기다리다 정리 작업이 실패로 끝낸 Task: 끝 시각만 있다.
    id: DROPPED,
    name: DROPPED,
    revision: 2,
    spec: { op: { kind: "vm-start" }, target: web.id, node: NODE.id },
    status: { state: "failed", error: "node is gone", finished_at: 1_761_192_230_000 },
  },
  {
    id: WAITING,
    name: WAITING,
    revision: 1,
    spec: { op: { kind: "vm-stop" }, target: web.id, node: NODE.id },
    status: { state: "pending", error: null },
  },
  {
    // 시작과 끝을 다른 노드가 적었고 그 시계가 어긋났다: 끝이 시작보다 앞선다.
    id: SKEWED,
    name: SKEWED,
    revision: 3,
    spec: { op: { kind: "vm-reboot" }, target: web.id, node: NODE.id },
    status: {
      state: "failed",
      error: "x",
      started_at: 1_761_192_230_000,
      finished_at: 1_761_192_229_000,
    },
  },
  {
    // 원인 코드가 있는 실패(계획 77-2): 다시 할 수 있는 시작(이 VM의 가장 새 전원 Task)
    id: CODED,
    name: CODED,
    revision: 3,
    spec: { op: { kind: "vm-start" }, target: web.id, node: NODE.id },
    status: {
      state: "failed",
      error: NOT_ENOUGH,
      failure: {
        code: "pool_full",
        params: {
          node: "svc02",
          pool: "local",
          need_bytes: "21474836480",
          free_bytes: "3221225472",
        },
        retryable: true,
      },
    },
  },
  {
    // 먼저 고쳐야 하는 실패: 단추는 "고친 뒤 다시 하기"(리뷰 I7).
    id: FIXFIRST,
    name: FIXFIRST,
    revision: 3,
    spec: { op: { kind: "vm-start" }, target: db.id, node: NODE.id },
    status: {
      state: "failed",
      error: "failed to start: QEMU failed to start: qemu-system-x86_64: Could not open 'x'",
      failure: { code: "qemu_start_failed", params: { vm: "db" } },
    },
  },
  {
    // 잠깐의 문제일 수 있는 실패: "다시 하기"와 바로 다시 해 보라는 안내(리뷰 I7)
    id: TRANSIENT,
    name: TRANSIENT,
    revision: 3,
    spec: { op: { kind: "vm-start" }, target: app.id, node: NODE.id },
    status: {
      state: "failed",
      error: "timed out after 600s",
      failure: { code: "task_timeout", retryable: true },
    },
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
  mockLists({ vm: [web, db, app], node: [NODE], backup: [bk], task: TASKS });
});

it("opens one drawer with who, when, where and the whole error from the task list", async () => {
  const { router } = page();
  // 목록의 오류는 잘리지 않고 2줄로 접혀 있다가 펼친다(마우스를 올릴 필요 없이).
  const row = (await screen.findByRole("link", { name: "Back up web" })).closest("tr")!;
  expect(row).toHaveTextContent(LONG);
  // 단추 이름에 무엇의 오류인지 넣고(목록에 여럿이어도 구별된다), 펼치는 글을 가리킨다(리뷰 S6).
  const more = within(row).getByRole("button", { name: "Show all: Back up web" });
  expect(more).toHaveAttribute("aria-expanded", "false");
  expect(document.getElementById(more.getAttribute("aria-controls")!)).toHaveTextContent(LONG);
  await userEvent.click(more);
  expect(within(row).getByRole("button", { name: "Show less: Back up web" })).toHaveAttribute(
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

  // 닫으면 주소에서 빠진다. 기록을 바꿔 써서 "뒤로"가 서랍을 다시 열지 않는다(리뷰 S1).
  await userEvent.keyboard("{Escape}");
  await waitFor(() => expect(router.state.location.search).toBe(""));
  expect(router.state.historyAction).toBe("REPLACE");
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  // 포커스는 연 링크로 돌아간다.
  await waitFor(() => expect(within(row).getByRole("link", { name: "Back up web" })).toHaveFocus());
});

/** 주소로 그 Task의 서랍을 열고, 다 읽을 때까지 기다린다 */
async function drawerOf(id: string) {
  const view = page(`/tasks?task=${id}`);
  const drawer = await screen.findByRole("dialog");
  await within(drawer).findByText(id);
  return { drawer, unmount: view.unmount };
}

it("tells a record task, a task that never started and a waiting one apart from an old node", async () => {
  // 기록 Task: 시작 시각이 있고 걸린 시간은 0초다(리뷰 I1).
  let open = await drawerOf(RECORD);
  expect(open.drawer).not.toHaveTextContent("older node version");
  expect(open.drawer).toHaveTextContent("Took0 s");
  open.unmount();
  // 기다리다 끝난 Task: 끝 시각만 있다 — 옛 노드가 아니라 시작하지 않았다.
  open = await drawerOf(DROPPED);
  expect(open.drawer).toHaveTextContent("StartedNot started (ended while waiting)");
  expect(open.drawer).not.toHaveTextContent("older node version");
  expect(open.drawer).not.toHaveTextContent("Took");
  open.unmount();
  open = await drawerOf(WAITING);
  expect(open.drawer).toHaveTextContent("StartedNot started yet (waiting)");
  expect(open.drawer).toHaveTextContent("FinishedNot finished yet");
  open.unmount();
  // 노드 시계가 어긋나 끝이 시작보다 앞서면 걸린 시간을 보이지 않는다(0초로 속이지 않는다 — 리뷰 S4).
  open = await drawerOf(SKEWED);
  expect(open.drawer).toHaveTextContent("Finished");
  expect(open.drawer).not.toHaveTextContent("Took");
});

it("gives focus back to the bell when the link that opened the drawer is gone", async () => {
  // 센터 안의 링크처럼: 누르면 서랍을 열고 자신은 사라진다(리뷰 S2).
  function Opener() {
    const [gone, setGone] = useState(false);
    return gone ? null : (
      <button
        type="button"
        onClick={() => {
          openTask(FAILED);
          setGone(true);
        }}
      >
        Open from the center
      </button>
    );
  }
  renderPage(
    <>
      <button type="button" aria-keyshortcuts="Shift+N">
        Bell
      </button>
      <Opener />
      <TaskDrawer />
    </>,
    { path: "/tasks", url: "/tasks" },
  );
  await userEvent.click(await screen.findByRole("button", { name: "Open from the center" }));
  await screen.findByRole("dialog", { name: "Back up web" });
  await userEvent.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  await waitFor(() => expect(screen.getByRole("button", { name: "Bell" })).toHaveFocus());
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
    const list = await screen.findByRole("list", { name: "Task history" });
    const card = (await within(list).findByRole("link", { name: "Back up web" })).closest("li")!;
    expect(card).toHaveTextContent("Back up web");
    expect(card).toHaveTextContent(LONG);
  } finally {
    window.matchMedia = original;
  }
});

it("explains a coded failure: what, why, what to do, related pages and the raw text folded", async () => {
  const { drawer } = await drawerOf(CODED);
  const explain = within(drawer).getByRole("region", { name: "What went wrong" });
  expect(explain).toHaveTextContent("Storage pool local on node svc02 is too full.");
  expect(explain).toHaveTextContent("The task needs 20 GiB but only 3 GiB is free.");
  expect(explain).toHaveTextContent("What to do");
  // 원인 값의 화면
  expect(within(explain).getByRole("link", { name: "storage pool local" })).toHaveAttribute(
    "href",
    "/storage-pools?q=local",
  );
  expect(within(explain).getByRole("link", { name: "node svc02" })).toHaveAttribute(
    "href",
    "/nodes/svc02",
  );
  // 다시 할 수 있는 시작은 단추가 있다.
  expect(within(explain).getByRole("button", { name: "Retry" })).toBeInTheDocument();
  // 원문은 "자세히(원문)"에 접혀 있고 코드와 복사가 함께 있다.
  const raw = within(drawer).getByText("Details (raw message)").closest("details")!;
  expect(raw).not.toHaveAttribute("open");
  expect(raw).toHaveTextContent(NOT_ENOUGH);
  expect(raw).toHaveTextContent("Error code pool_full");
  await expectNoAxeViolations(drawer, { disable: ["region"] });
});

it("offers a retry after fixing when the cause must be fixed first, and lists the summary above the raw text", async () => {
  const { drawer, unmount } = await drawerOf(FIXFIRST);
  const explain = within(drawer).getByRole("region", { name: "What went wrong" });
  expect(explain).toHaveTextContent("VM db could not start.");
  // 단추는 남기되 고친 뒤 다시 하라고 한다(리뷰 I7).
  expect(within(explain).queryByRole("button", { name: "Retry" })).toBeNull();
  expect(within(explain).getByRole("button", { name: "Retry after fixing" })).toBeEnabled();
  expect(explain).toHaveTextContent(
    'Fix the cause as described under "What to do", then try again.',
  );
  unmount();
  // 잠깐의 문제일 수 있는 실패는 "다시 하기"와 바로 다시 해 보라는 안내
  const transient = await drawerOf(TRANSIENT);
  const again = within(transient.drawer).getByRole("region", { name: "What went wrong" });
  expect(within(again).getByRole("button", { name: "Retry" })).toBeEnabled();
  expect(again).toHaveTextContent("This may have been a passing problem — try again now.");
  transient.unmount();
  // 목록: 사전의 제목 한 줄 + 원문
  page();
  const rows = (await screen.findAllByRole("link", { name: "Start web" })).map((link) =>
    link.closest("tr")!,
  );
  const row = rows.find((r) => r.textContent?.includes(NOT_ENOUGH))!;
  expect(row).toHaveTextContent("Storage pool local on node svc02 is too full.");
  expect(row).toHaveTextContent(NOT_ENOUGH);
});
