import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { REFRESH_ON } from "@/api/live";
import type { Task } from "@/api/types";
import type { DrRun } from "@/dr/api";
import { RunPage } from "@/dr/RunPage";
import { expectNoAxeViolations } from "@/test/axe";
import { DR_NODE, NOW, plan, PLAN_ID, run, RUN_ID } from "@/test/dr";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

function setup(first: DrRun = run()) {
  let current = first;
  mockLists({});
  server.use(http.get(apiUrl(`/dr-runs/${RUN_ID}`), () => HttpResponse.json(current)));
  return (next: DrRun) => (current = next);
}

const open = (as = ADMIN) =>
  renderPage(<RunPage />, { path: "/dr-runs/:id", url: `/dr-runs/${RUN_ID}`, as });

it("shows group bands with a phase per VM, its new VM and task", async () => {
  setup();
  const { container } = open();
  expect(
    await screen.findByRole("heading", { level: 1, name: "Test failover · web-tier" }),
  ).toBeInTheDocument();
  const db = screen.getByRole("listitem", { name: "Group 1: db" });
  expect(db).toHaveAttribute("aria-current", "step");
  expect(db).toHaveTextContent("ready when: Guest agent answers");
  const db1 = within(db).getByRole("listitem", { name: "db1" });
  expect(db1).toHaveTextContent("Waiting");
  expect(db1).toHaveTextContent("from db1-copy");
  expect(within(db1).getByRole("link", { name: "db1-drt" })).toHaveAttribute(
    "href",
    "/vms/db1-drt",
  );
  expect(within(db1).getByRole("link", { name: /Task/ })).toHaveAttribute("href", "/tasks");
  const web = screen.getByRole("listitem", { name: "Group 2: web" });
  expect(web).not.toHaveAttribute("aria-current");
  expect(within(web).getByRole("listitem", { name: "web1" })).toHaveTextContent("Pending");
  expect(screen.getByRole("link", { name: "Plan web-tier" })).toHaveAttribute(
    "href",
    "/dr-plans/web-tier",
  );
  // 도는 동안 RTO 시계는 시작부터 지금까지다
  expect(screen.getByTestId("rto-clock")).not.toHaveTextContent("—");
  await expectNoAxeViolations(container);
});

it("follows the run when a watch event comes", async () => {
  const next = setup();
  const { queryClient } = open();
  expect(await screen.findByRole("listitem", { name: "db1" })).toHaveTextContent("Waiting");
  next(
    run({
      status: {
        state: "partial",
        started_at: NOW - 900,
        finished_at: NOW - 146,
        rto_secs: 754,
        steps: [
          { group: "db", vm: "db1", phase: "ready", at: NOW - 400, new_vm: "db1-drt" },
          { group: "web", vm: "web1", phase: "ready", at: NOW - 146, new_vm: "web1-drt" },
          { group: "web", vm: "web2", phase: "failed", at: NOW - 200, error: "no recovery point" },
        ],
      },
    }),
  );
  // LiveProvider가 `dr-run` 사건에 다시 읽는 키(REFRESH_ON)
  for (const queryKey of REFRESH_ON["dr-run"]!) {
    await queryClient.invalidateQueries({ queryKey });
  }
  await waitFor(() =>
    expect(screen.getByRole("listitem", { name: "db1" })).toHaveTextContent("Ready"),
  );
  const web2 = screen.getByRole("listitem", { name: "web2" });
  expect(web2).toHaveTextContent("Failed");
  expect(web2).toHaveTextContent("no recovery point");
  expect(screen.getByText("Partial")).toBeInTheDocument();
  expect(screen.getByTestId("rto-clock")).toHaveTextContent("12m 34s");
  expect(screen.queryByRole("button", { name: "Cancel run" })).not.toBeInTheDocument();
});

it("cancels the running run of the plan", async () => {
  const user = userEvent.setup();
  setup();
  const sent: string[] = [];
  server.use(
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/cancel`), () => {
      sent.push("cancel");
      return HttpResponse.json(run({ status: { cancel_requested: true } }));
    }),
  );
  open();
  await user.click(await screen.findByRole("button", { name: "Cancel run" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Cancel the run of web-tier" });
  await user.click(within(dialog).getByRole("button", { name: "Cancel run" }));
  await waitFor(() => expect(sent).toEqual(["cancel"]));
});

it("offers viewers no cancel", async () => {
  setup();
  open({ ...ADMIN, role: "viewer" });
  expect(await screen.findByRole("listitem", { name: "db1" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Cancel run" })).not.toBeInTheDocument();
});

it("shows a cleanup run as one band of deleted VMs", async () => {
  setup(
    run({
      spec: { kind: "cleanup" },
      status: {
        state: "succeeded",
        finished_at: NOW,
        steps: [{ vm: "db1-drt", phase: "deleted", at: NOW }],
      },
    }),
  );
  open();
  const band = await screen.findByRole("listitem", { name: "Cleanup" });
  expect(within(band).getByRole("listitem", { name: "db1-drt" })).toHaveTextContent("Deleted");
  // 뒷정리에는 RTO가 없다: 걸린 시간
  expect(screen.getByText("Elapsed")).toBeInTheDocument();
  expect(screen.queryByText("RTO")).not.toBeInTheDocument();
  expect(screen.getByTestId("rto-clock")).toHaveTextContent("1m 30s");
});

/** 시각 `secs`에 만든 UUIDv7 모양의 Task */
function taskAt(secs: number, n: number, op: Task["spec"]["op"], state: Task["status"]["state"]) {
  const ms = (secs * 1000).toString(16).padStart(12, "0");
  return {
    id: `${ms.slice(0, 8)}-${ms.slice(8)}-7000-8000-${String(n).padStart(12, "0")}`,
    name: "t",
    revision: 1,
    labels: {},
    spec: { node: "n", op, target: "019a0f3c-0000-7000-8000-0000000000f1" },
    status: { state, error: state === "failed" ? "boom" : null },
  } as Task;
}

const dotStates = (item: HTMLElement) =>
  [...item.querySelectorAll("[data-state]")].map((d) => d.getAttribute("data-state"));

it("marks the phase a VM failed in and hides a new VM the run never made", async () => {
  setup(
    run({
      status: {
        state: "partial",
        finished_at: NOW,
        steps: [
          {
            group: "db",
            vm: "db1",
            phase: "failed",
            at: NOW - 10,
            new_vm: "db1-drt",
            backup: "db1-copy",
            backup_id: "b-1",
            error: "start failed",
          },
          // 이름이 겹쳐 복원 요청이 거절됐다(Task 없음): 그 이름의 VM은 남의 것이다
          {
            group: "web",
            vm: "web1",
            phase: "failed",
            at: NOW - 10,
            new_vm: "web1-drt",
            backup_id: "b-2",
            error: "name taken",
          },
          { group: "web", vm: "web2", phase: "skipped", at: NOW - 10 },
        ],
      },
    }),
  );
  mockLists({
    task: [
      taskAt(
        NOW - 60,
        1,
        { kind: "backup-restore", backup: "b-1", name: "db1-drt", pool: "local" },
        "succeeded",
      ),
      taskAt(NOW - 30, 2, { kind: "vm-start" }, "failed"),
    ],
  });
  open();
  const db1 = await screen.findByRole("listitem", { name: "db1" });
  await waitFor(() => expect(dotStates(db1)).toEqual(["done", "failed", "todo", "todo"]));
  expect(within(db1).getByRole("link", { name: "db1-drt" })).toBeInTheDocument();
  const web1 = screen.getByRole("listitem", { name: "web1" });
  expect(dotStates(web1)).toEqual(["todo", "todo", "todo", "todo"]);
  expect(web1).toHaveTextContent("Failed");
  expect(within(web1).queryByRole("link", { name: "web1-drt" })).not.toBeInTheDocument();
});

it("tells the source prepare stage of a planned migration", async () => {
  setup(
    run({
      spec: { kind: "failover-planned" },
      status: {
        steps: [
          {
            group: "db",
            vm: "db1",
            phase: "preparing",
            at: NOW - 10,
            prepare: { stage: "replicate", source_backup: "db1-final" },
          },
        ],
      },
    }),
  );
  open();
  expect(await screen.findByRole("listitem", { name: "db1" })).toHaveTextContent(
    "replicating here · backup db1-final",
  );
});

it("shows the upgrade notice while the engine waits for old nodes", async () => {
  setup();
  mockLists({
    node: [{ ...DR_NODE, name: "node-b", status: { ...DR_NODE.status, features: [] } }],
  });
  open();
  expect(await screen.findByText(/do not support DR plans yet \(node-b\)/)).toBeInTheDocument();
});

it("tells that a cancelled test cleans up by itself and links that cleanup", async () => {
  const user = userEvent.setup();
  const next = setup();
  const CLEANUP = "019a0f3c-0000-7000-8076-0000000000b2";
  server.use(
    http.get(apiUrl(`/dr-plans/${PLAN_ID}`), () =>
      HttpResponse.json(plan("web-tier", { status: { run: CLEANUP } })),
    ),
  );
  const { queryClient } = open();
  await user.click(await screen.findByRole("button", { name: "Cancel run" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Cancel the run of web-tier" });
  expect(dialog).toHaveTextContent("When the test ends, its cleanup starts by itself.");
  await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
  next(run({ status: { state: "cancelled", finished_at: NOW } }));
  await queryClient.invalidateQueries({ queryKey: REFRESH_ON["dr-run"]![0] });
  expect(
    await screen.findByRole("link", { name: "Open the cleanup that started after the cancel" }),
  ).toHaveAttribute("href", `/dr-runs/${CLEANUP}`);
});

it("shows the server's failed phase, notes, tried copies, retries and the disk copy", async () => {
  const now = Math.floor(Date.now() / 1000);
  setup(
    run({
      spec: { kind: "failover-disaster" },
      status: {
        progress_at: now - 60,
        steps: [
          // 서버가 적은 실패 단계가 Task 어림보다 먼저다
          {
            group: "db",
            vm: "db1",
            phase: "failed",
            failed_in: "starting",
            at: NOW - 10,
            new_vm: "db1",
            error: "start failed",
            tried: ["db1-copy-2", "db1-copy-1"],
            notes: ["restore from db1-copy-2 failed: bad chunk; trying db1-copy-1"],
          },
          {
            group: "web",
            vm: "web1",
            phase: "waiting",
            at: NOW - 10,
            new_vm: "web1",
            copy_task: "019a0f3c-0000-7000-8005-0000000000c1",
          },
          {
            group: "web",
            vm: "web2",
            phase: "preparing",
            at: NOW - 10,
            retrying_since: now - 300,
            last_error: "cannot read the source cluster: timed out",
          },
        ],
      },
    }),
  );
  open();
  const db1 = await screen.findByRole("listitem", { name: "db1" });
  expect(dotStates(db1)).toEqual(["done", "failed", "todo", "todo"]);
  expect(db1).toHaveTextContent("tried db1-copy-2, db1-copy-1");
  expect(within(db1).getByRole("list", { name: "Notes" })).toHaveTextContent("bad chunk");
  const web1 = screen.getByRole("listitem", { name: "web1" });
  expect(within(web1).getByRole("link", { name: /Disk copy/ })).toHaveAttribute("href", "/tasks");
  const web2 = screen.getByRole("listitem", { name: "web2" });
  expect(web2).toHaveTextContent(/Retrying for 5m \ds: cannot read the source cluster: timed out/);
  // 1분 전에 나아갔으므로 멈춤이 아니다
  expect(screen.queryByText(/No progress for/)).not.toBeInTheDocument();
});

it("warns when a running run made no progress for 30 minutes", async () => {
  setup(run({ status: { progress_at: Math.floor(Date.now() / 1000) - 3600 } }));
  open();
  expect(await screen.findByRole("alert")).toHaveTextContent(/No progress for 1h 0m/);
});

it("tells a skipped VM was already recovered by this plan, not failed", async () => {
  setup(
    run({
      spec: { kind: "failover-disaster" },
      status: {
        state: "succeeded",
        finished_at: NOW,
        steps: [
          {
            group: "db",
            vm: "db1",
            phase: "skipped",
            at: NOW - 10,
            error: "already recovered by this plan",
          },
        ],
      },
    }),
  );
  open();
  const db1 = await screen.findByRole("listitem", { name: "db1" });
  expect(db1).toHaveTextContent("Already recovered by this plan — skipped.");
  expect(db1).not.toHaveTextContent("already recovered by this plan");
});

it("links a test a failover cancelled and the failover back to it", async () => {
  const FAILOVER = "019a0f3c-0000-7000-8076-0000000000c3";
  setup(run({ status: { state: "cancelled", preempted: true, finished_at: NOW, steps: [] } }));
  server.use(
    // 계획의 지금 실행은 장애 조치다 — 뒷정리로 잘못 잇지 않는다
    http.get(apiUrl(`/dr-plans/${PLAN_ID}`), () =>
      HttpResponse.json(plan("web-tier", { status: { run: FAILOVER } })),
    ),
    http.get(apiUrl(`/dr-plans/${PLAN_ID}/runs`), () =>
      HttpResponse.json({
        revision: 1,
        items: [
          {
            ...run({ spec: { kind: "failover-disaster" }, status: { preempted_test: RUN_ID } }),
            id: FAILOVER,
          },
          run({ status: { state: "cancelled", preempted: true } }),
        ],
      }),
    ),
  );
  open();
  expect(
    await screen.findByText(/A failover of this plan cancelled this test/),
  ).toBeInTheDocument();
  expect(await screen.findByRole("link", { name: "Open the failover" })).toHaveAttribute(
    "href",
    `/dr-runs/${FAILOVER}`,
  );
  expect(
    screen.queryByRole("link", { name: "Open the cleanup that started after the cancel" }),
  ).not.toBeInTheDocument();
});

it("links the test a failover cancelled", async () => {
  const TEST = "019a0f3c-0000-7000-8076-0000000000c4";
  setup(run({ spec: { kind: "failover-disaster" }, status: { preempted_test: TEST } }));
  open();
  expect(await screen.findByRole("link", { name: "Open that test" })).toHaveAttribute(
    "href",
    `/dr-runs/${TEST}`,
  );
});
