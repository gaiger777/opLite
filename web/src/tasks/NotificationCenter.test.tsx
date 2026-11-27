import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { AlertView } from "@/alerts/alerts";
import type { Principal, Task } from "@/api/types";
import { AppLayout } from "@/layout/AppLayout";
import { setCenterOpen } from "@/tasks/center";
import { expectNoAxeViolations } from "@/test/axe";
import { backup, backupJob, NODE, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const VIEWER: Principal = {
  user: "00000000-0000-0000-0000-000000000003",
  name: "viewer",
  role: "viewer",
};

const web = vm("web", "running");
const db = vm("db", "stopped");
const job = backupJob("nightly", 1);
const bk = backup("db-1", db, 1, { spec: { job: job.id } });

function t(n: number, op: object, target: string, status: Partial<Task["status"]>): Task {
  return {
    id: `019a0f3c-8a10-7000-8000-${String(n).padStart(12, "0")}`,
    name: `t${n}`,
    revision: 1,
    spec: { op, target, node: "n1" },
    status: { state: "pending", error: null, ...status },
  } as Task;
}

const TASKS = [
  t(1, { kind: "vm-migrate", node: "n2" }, web.id, {
    state: "running",
    progress: { stage: "copying-disks", done_bytes: 2 * 1024 ** 3, total_bytes: 4 * 1024 ** 3 },
  }),
  t(2, { kind: "vm-start" }, db.id, { state: "failed", error: "no firmware" }),
  t(3, { kind: "vm-backup", backup: bk.id }, db.id, { state: "failed", error: "pool full" }),
  t(4, { kind: "vm-stop" }, web.id, { state: "succeeded" }),
];

function alert(severity: AlertView["severity"], name: string): AlertView {
  return {
    rule: `r-${severity}`,
    rule_name: `${severity} rule`,
    severity,
    state: "firing",
    since: 1_793_761_200_000,
    subject: { kind: "node", id: NODE.id, name },
  } as AlertView;
}

beforeEach(() => {
  mockLists({ vm: [web, db], node: [NODE], backup: [bk], "backup-job": [job], task: TASKS });
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
    http.get(apiUrl("/alerts"), () =>
      HttpResponse.json({
        alerts: [alert("info", "node-b"), alert("critical", "node-a")],
        unreachable: [],
        unsupported: [],
      }),
    ),
  );
});
afterEach(() => {
  setCenterOpen(false);
  localStorage.clear();
});

function shell(as = ADMIN) {
  return renderPage(<AppLayout />, { path: "*", url: "/", as });
}

it("counts running tasks and ringing alerts on the bell and opens the center with Shift+N", async () => {
  const { router } = shell();
  // 도는 Task 1 + critical 경보 1(info는 세지 않음)
  const bell = await screen.findByRole("button", { name: "Tasks and alerts (2 active)" });
  expect(bell).toHaveAttribute("aria-keyshortcuts", "Shift+N");
  expect(bell).toHaveTextContent("2");
  fireEvent.keyDown(document.body, { key: "N", shiftKey: true });
  const center = await screen.findByRole("dialog", { name: "Tasks and alerts" });

  const running = await within(center).findByRole("list", { name: "Running (1)" });
  const migrate = await within(running).findByRole("link", { name: "Migrate web" });
  // 작업 이름은 상세 서랍을 연다(계획 77).
  expect(migrate).toHaveAttribute("href", "/?task=019a0f3c-8a10-7000-8000-000000000001");
  expect(within(running).getByRole("progressbar", { name: "Migrate web" })).toHaveAttribute(
    "aria-valuenow",
    "50",
  );
  expect(running).toHaveTextContent("Copying disks 50% (2.0/4.0 GiB)");

  const recent = within(center).getByRole("list", { name: "Recent" });
  expect(
    within(recent)
      .getAllByRole("listitem")
      .map((li) => li.textContent),
  ).toEqual([
    expect.stringContaining("Stop web"),
    expect.stringContaining("Back up db"),
    expect.stringContaining("Start db"),
  ]);
  expect(recent).toHaveTextContent("no firmware");
  // 정지는 다시 하기가 없다(성공).
  expect(
    within(recent)
      .getAllByRole("button")
      .map((b) => b.getAttribute("aria-label")),
  ).toEqual(["Retry: Back up db", "Retry: Start db"]);

  const alerts = await within(center).findByRole("list", { name: "Active alerts" });
  // 심각도 높은 것부터
  expect(within(alerts).getAllByRole("listitem")[0]).toHaveTextContent("critical rule");
  expect(within(center).getByRole("heading", { name: "Alerts (1 firing)" })).toBeInTheDocument();
  await expectNoAxeViolations(center, { disable: ["region"] });

  // 깊은 링크는 서랍을 닫고 그 화면으로 간다.
  await userEvent.click(within(alerts).getByRole("link", { name: "node-a" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/nodes/node-a"));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
});

it("retries a failed power operation and runs a backup job again", async () => {
  const start = vi.fn();
  const run = vi.fn();
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ params }) => {
      start(params.key, params.action);
      return HttpResponse.json(t(9, { kind: "vm-start" }, db.id, {}), { status: 202 });
    }),
    http.post(apiUrl("/backup-jobs/:key/run"), ({ params }) => {
      run(params.key);
      return HttpResponse.json(job);
    }),
  );
  shell();
  await userEvent.click(await screen.findByRole("button", { name: /^Tasks and alerts/ }));
  const center = await screen.findByRole("dialog", { name: "Tasks and alerts" });
  await userEvent.click(await within(center).findByRole("button", { name: "Retry: Start db" }));
  await waitFor(() => expect(start).toHaveBeenCalledWith(db.id, "start"));
  await userEvent.click(within(center).getByRole("button", { name: "Retry: Back up db" }));
  await waitFor(() => expect(run).toHaveBeenCalledWith(job.id));
  expect(
    await screen.findByText("nightly starts at the scheduler's next check."),
  ).toBeInTheDocument();
});

it("shows no retry to viewers", async () => {
  shell(VIEWER);
  await userEvent.click(await screen.findByRole("button", { name: /^Tasks and alerts/ }));
  const center = await screen.findByRole("dialog", { name: "Tasks and alerts" });
  const recent = await within(center).findByRole("list", { name: "Recent" });
  expect(within(recent).queryAllByRole("button")).toEqual([]);
});

it("asks before retrying a force stop and sends one start per click", async () => {
  const calls = vi.fn();
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), async ({ params, request }) => {
      calls(params.key, params.action, new URL(request.url).searchParams.get("mode"));
      await new Promise((r) => setTimeout(r, 50));
      return HttpResponse.json(t(9, { kind: "vm-start" }, db.id, {}), { status: 202 });
    }),
  );
  mockLists({
    vm: [web, db],
    node: [NODE],
    task: [
      t(2, { kind: "vm-start" }, db.id, { state: "failed", error: "no firmware" }),
      t(5, { kind: "vm-force-stop" }, web.id, { state: "failed", error: "busy" }),
    ],
  });
  shell();
  await userEvent.click(await screen.findByRole("button", { name: /^Tasks and alerts/ }));
  const center = await screen.findByRole("dialog", { name: "Tasks and alerts" });

  // 강제 정지는 다른 곳처럼 확인을 거친다.
  await userEvent.click(
    await within(center).findByRole("button", { name: "Retry: Force stop web" }),
  );
  const confirm = await screen.findByRole("alertdialog", { name: "Force stop web?" });
  expect(calls).not.toHaveBeenCalled();
  await userEvent.click(within(confirm).getByRole("button", { name: "Force stop" }));
  await waitFor(() => expect(calls).toHaveBeenCalledWith(web.id, "stop", "force"));
  calls.mockClear();

  // 시작은 바로 보내되, 요청 중에는 단추를 막는다.
  const startAgain = await within(center).findByRole("button", { name: "Retry: Start db" });
  await userEvent.dblClick(startAgain);
  await waitFor(() => expect(calls).toHaveBeenCalledWith(db.id, "start", null));
  await waitFor(() => expect(startAgain).toBeEnabled());
  expect(calls).toHaveBeenCalledTimes(1);
});
