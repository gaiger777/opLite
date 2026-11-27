import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { REFRESH_ON } from "@/api/live";
import type { DrRun } from "@/dr/api";
import { RunPage } from "@/dr/RunPage";
import { expectNoAxeViolations } from "@/test/axe";
import { NOW, PLAN_ID, run, RUN_ID } from "@/test/dr";
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
        rto_secs: 20,
        steps: [{ vm: "db1-drt", phase: "deleted", at: NOW }],
      },
    }),
  );
  open();
  const band = await screen.findByRole("listitem", { name: "Cleanup" });
  expect(within(band).getByRole("listitem", { name: "db1-drt" })).toHaveTextContent("Deleted");
});
