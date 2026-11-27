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
