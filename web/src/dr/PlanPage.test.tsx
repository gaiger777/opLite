import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { RecoveryPlanSpec } from "@/dr/api";
import { NewPlanPage, PlanPage } from "@/dr/PlanPage";
import { expectNoAxeViolations } from "@/test/axe";
import { DR_NODE, plan, PLAN_ID, report, run, RUN_ID } from "@/test/dr";
import { backup, pool, remote, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const VIEWER = { ...ADMIN, role: "viewer" as const };

function setup() {
  const prodVm = vm("db9", "running");
  mockLists({
    node: [DR_NODE],
    "storage-pool": [pool("local"), pool("fast")],
    backup: [
      backup("db9-copy", prodVm, 9, {
        spec: {
          origin: { cluster: "prod", ca_fingerprint: remote("prod").ca_fingerprint, backup: "b9" },
        },
      }),
    ],
  });
  server.use(
    http.get(apiUrl("/dr-plans/web-tier"), () => HttpResponse.json(plan())),
    http.get(apiUrl("/dr-plans/web-tier/report"), () => HttpResponse.json(report())),
    http.get(apiUrl(`/dr-plans/${PLAN_ID}/runs`), () =>
      HttpResponse.json({
        revision: 1,
        items: [run({ status: { state: "partial", rto_secs: 754 } })],
      }),
    ),
    http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items: [remote("prod")] })),
    http.get(apiUrl("/remotes/prod/api/vms"), () =>
      HttpResponse.json({ revision: 1, items: [vm("app1", "running")] }),
    ),
  );
}

it("shows protection, recent tests and the actions", async () => {
  setup();
  const { container } = renderPage(<PlanPage />, {
    path: "/dr-plans/:name",
    url: "/dr-plans/web-tier",
  });
  const table = await screen.findByRole("table", { name: "Protection" });
  const web2 = within(table).getByRole("row", { name: /web2/ });
  expect(web2).toHaveTextContent("No recovery point");
  const db1 = within(table).getByRole("row", { name: /db1/ });
  expect(db1).toHaveTextContent("db1-copy");
  expect(db1).toHaveTextContent("1h 0m");
  expect(db1).toHaveTextContent("Within RPO");
  const tests = screen.getByRole("table", { name: "Recent tests" });
  expect(within(tests).getByRole("link", { name: "Test failover" })).toHaveAttribute(
    "href",
    `/dr-runs/${RUN_ID}`,
  );
  expect(tests).toHaveTextContent("12m 34s");
  expect(tests).toHaveTextContent("web2");
  for (const name of ["Test", "Clean up", "Failover"]) {
    expect(screen.getByRole("button", { name })).toBeInTheDocument();
  }
  await expectNoAxeViolations(container);
});

it("saves the whole plan with If-Match after moving a group up", async () => {
  const user = userEvent.setup();
  setup();
  const sent: { ifMatch: string | null; spec: RecoveryPlanSpec }[] = [];
  server.use(
    http.put(apiUrl(`/dr-plans/${PLAN_ID}`), async ({ request }) => {
      const body = (await request.json()) as { spec: RecoveryPlanSpec };
      sent.push({ ifMatch: request.headers.get("If-Match"), spec: body.spec });
      return HttpResponse.json(plan("web-tier", { revision: 5, spec: body.spec }));
    }),
  );
  const { container } = renderPage(<PlanPage />, {
    path: "/dr-plans/:name",
    url: "/dr-plans/web-tier?tab=plan",
  });
  await user.click(await screen.findByRole("button", { name: "Move web up" }));
  const rpo = screen.getByLabelText("RPO target (hours)");
  await user.clear(rpo);
  await user.type(rpo, "12");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]!.ifMatch).toBe("4");
  expect(sent[0]!.spec.groups.map((g) => g.name)).toEqual(["web", "db"]);
  expect(sent[0]!.spec.rpo_target_secs).toBe(43_200);
  expect(sent[0]!.spec.networks).toEqual({ lan: "dr-lan" });
  expect(sent[0]!.spec.groups[0]!.vms[0]!.ip).toEqual([
    { nic: 0, address: "10.20.0.5/24", gateway: "10.20.0.1" },
  ]);
  expect(await screen.findByText("web-tier saved.")).toBeInTheDocument();
  await expectNoAxeViolations(container);
});

it("tells about a conflicting change and reloads the plan", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.put(apiUrl(`/dr-plans/${PLAN_ID}`), () =>
      HttpResponse.json(
        { code: "revision_mismatch", message: "revision mismatch" },
        { status: 409 },
      ),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  await user.click(await screen.findByRole("button", { name: "Move web up" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByText(/Someone else changed it first/)).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Reload" }));
  const groups = screen.getAllByRole("listitem", { name: /^Group \d: / });
  expect(groups.map((g) => g.getAttribute("aria-label"))).toEqual(["Group 1: db", "Group 2: web"]);
});

it("adds VMs from the source and its copies here", async () => {
  const user = userEvent.setup();
  setup();
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  const db = await screen.findByRole("listitem", { name: "Group 1: db" });
  await user.click(await within(db).findByRole("button", { name: "Add VMs" }));
  const dialog = await screen.findByRole("dialog", { name: "Add VMs to db" });
  const list = await within(dialog).findByRole("list", { name: "VMs" });
  expect(within(list).getByText("app1")).toBeInTheDocument();
  expect(within(list).getByText("db9")).toBeInTheDocument();
  // 이미 계획에 있는 VM은 없다
  expect(within(list).queryByText("web1")).not.toBeInTheDocument();
  await user.click(within(dialog).getByLabelText(/db9/));
  await user.click(within(dialog).getByRole("button", { name: "Add 1 VM" }));
  expect(within(db).getByRole("list", { name: "VMs of db" })).toHaveTextContent("db9");
});

it("asks for the plan name before a failover and warns about disaster mode", async () => {
  const user = userEvent.setup();
  setup();
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/failover`), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(run({ spec: { kind: "failover-disaster" } }), { status: 202 });
    }),
  );
  const { router } = renderPage(<PlanPage />, {
    path: "/dr-plans/:name",
    url: "/dr-plans/web-tier",
  });
  await user.click(await screen.findByRole("button", { name: "Failover" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Fail over web-tier" });
  const confirm = within(dialog).getByRole("button", { name: "Fail over" });
  expect(confirm).toBeDisabled();
  expect(
    within(dialog).queryByText(/after its last replicated backup is lost/),
  ).not.toBeInTheDocument();
  await user.click(within(dialog).getByRole("radio", { name: /Disaster/ }));
  expect(within(dialog).getByText(/after its last replicated backup is lost/)).toBeInTheDocument();
  await user.type(within(dialog).getByLabelText(/Type/), "web-tie");
  expect(confirm).toBeDisabled();
  await user.type(within(dialog).getByLabelText(/Type/), "r");
  await user.click(confirm);
  await waitFor(() => expect(sent).toEqual([{ mode: "disaster", confirm: "web-tier" }]));
  await waitFor(() => expect(router.state.location.pathname).toBe(`/dr-runs/${RUN_ID}`));
});

it("starts a test and opens its run", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/test`), () =>
      HttpResponse.json(run(), { status: 202 }),
    ),
  );
  const { router } = renderPage(<PlanPage />, {
    path: "/dr-plans/:name",
    url: "/dr-plans/web-tier",
  });
  await user.click(await screen.findByRole("button", { name: "Test" }));
  await waitFor(() => expect(router.state.location.pathname).toBe(`/dr-runs/${RUN_ID}`));
});

it("shows viewers the plan without actions or save", async () => {
  const user = userEvent.setup();
  setup();
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier", as: VIEWER });
  expect(await screen.findByRole("table", { name: "Protection" })).toBeInTheDocument();
  for (const name of ["Test", "Clean up", "Failover"]) {
    expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
  }
  await user.click(screen.getByRole("tab", { name: "Plan" }));
  expect(await screen.findByLabelText("RPO target (hours)")).toBeDisabled();
  expect(screen.queryByRole("button", { name: "Save" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Move web up" })).not.toBeInTheDocument();
});

it("creates a plan from the editor", async () => {
  const user = userEvent.setup();
  setup();
  const sent: { name: string; spec: RecoveryPlanSpec }[] = [];
  server.use(
    http.post(apiUrl("/dr-plans"), async ({ request }) => {
      const body = (await request.json()) as { name: string; spec: RecoveryPlanSpec };
      sent.push(body);
      return HttpResponse.json(plan(body.name, { spec: body.spec }), { status: 201 });
    }),
  );
  const { router } = renderPage(<NewPlanPage />, { path: "/dr-plans/-new" });
  await user.click(await screen.findByRole("button", { name: "Create" }));
  expect(await screen.findByText("Fix the marked fields.")).toBeInTheDocument();
  expect(sent).toHaveLength(0);
  await user.type(screen.getByLabelText("Name"), "app");
  await user.selectOptions(screen.getByLabelText("Source cluster"), "prod");
  await user.selectOptions(screen.getByLabelText("Restore pool"), "fast");
  await user.click(screen.getByRole("button", { name: "Add VMs" }));
  const dialog = await screen.findByRole("dialog");
  await user.click(await within(dialog).findByLabelText(/app1/));
  await user.click(within(dialog).getByRole("button", { name: "Add 1 VM" }));
  await user.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toMatchObject({
    name: "app",
    spec: {
      source: "prod",
      pool: "fast",
      rpo_target_secs: 86_400,
      groups: [{ name: "group-1", wait: "running", timeout_secs: 600, vms: [{ vm: "app1" }] }],
    },
  });
  await waitFor(() => expect(router.state.location.pathname).toBe("/dr-plans/app"));
});

it("keeps the plan read-only while some nodes lack DR plans", async () => {
  setup();
  mockLists({
    node: [
      DR_NODE,
      { ...DR_NODE, id: "n2", name: "node-b", status: { ...DR_NODE.status, features: [] } },
    ],
  });
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  expect(await screen.findByText(/do not support DR plans yet \(node-b\)/)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Failover" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Save" })).not.toBeInTheDocument();
});

it("lists what the cleanup removes", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.get(apiUrl(`/dr-plans/${PLAN_ID}/runs`), () =>
      HttpResponse.json({
        revision: 1,
        items: [
          run({
            status: {
              state: "succeeded",
              bubbles: [{ name: "drt-web-tier-0", source: "lan", state: "ready" }],
            },
          }),
        ],
      }),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await user.click(await screen.findByRole("button", { name: "Clean up" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Clean up the test of web-tier" });
  const impact = await within(dialog).findByRole("list");
  expect(impact).toHaveTextContent("db1-drt");
  expect(impact).toHaveTextContent("drt-web-tier-0");
});
