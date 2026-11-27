import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { RecoveryPlanSpec } from "@/dr/api";
import { forgetPlanEdits } from "@/dr/edits";
import { NewPlanPage, PlanPage } from "@/dr/PlanPage";
import { expectNoAxeViolations } from "@/test/axe";
import { DR_NODE, plan, PLAN_ID, report, run, RUN_ID } from "@/test/dr";
import { backup, backupJob, pool, providerNetwork, remote, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const VIEWER = { ...ADMIN, role: "viewer" as const };

beforeEach(() => forgetPlanEdits());

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

it("saves again with the new revision when only the status moved", async () => {
  const user = userEvent.setup();
  setup();
  const sent: (string | null)[] = [];
  server.use(
    http.put(apiUrl(`/dr-plans/${PLAN_ID}`), async ({ request }) => {
      const ifMatch = request.headers.get("If-Match");
      sent.push(ifMatch);
      if (ifMatch === "4") {
        return HttpResponse.json(
          { code: "revision_mismatch", message: "revision mismatch" },
          { status: 409 },
        );
      }
      const body = (await request.json()) as { spec: RecoveryPlanSpec };
      return HttpResponse.json(plan("web-tier", { revision: 8, spec: body.spec }));
    }),
    // 보호 상태만 바뀌어 판이 7이 됐다(spec은 그대로)
    http.get(apiUrl(`/dr-plans/${PLAN_ID}`), () =>
      HttpResponse.json(plan("web-tier", { revision: 7, status: { protection: [] } })),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  await user.click(await screen.findByRole("button", { name: "Move web up" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(sent).toEqual(["4", "7"]));
  expect((await screen.findAllByText("web-tier saved.")).length).toBeGreaterThan(0);
  expect(screen.queryByText(/Someone else changed it first/)).not.toBeInTheDocument();
});

it("on a real conflict offers to overwrite or to reload", async () => {
  const user = userEvent.setup();
  setup();
  const sent: (string | null)[] = [];
  server.use(
    http.put(apiUrl(`/dr-plans/${PLAN_ID}`), async ({ request }) => {
      const ifMatch = request.headers.get("If-Match");
      sent.push(ifMatch);
      if (ifMatch === "4") {
        return HttpResponse.json(
          { code: "revision_mismatch", message: "revision mismatch" },
          { status: 409 },
        );
      }
      const body = (await request.json()) as { spec: RecoveryPlanSpec };
      return HttpResponse.json(plan("web-tier", { revision: 10, spec: body.spec }));
    }),
    // 다른 사람이 RPO를 바꿨다
    http.get(apiUrl(`/dr-plans/${PLAN_ID}`), () =>
      HttpResponse.json(plan("web-tier", { revision: 9, spec: { rpo_target_secs: 3600 } })),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  await user.click(await screen.findByRole("button", { name: "Move web up" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByText(/Someone else changed it first/)).toBeInTheDocument();
  // 고친 것은 그대로다
  const order = () =>
    screen
      .getAllByRole("listitem", { name: /^Group \d: / })
      .map((g) => g.getAttribute("aria-label"));
  expect(order()).toEqual(["Group 1: web", "Group 2: db"]);
  await user.click(screen.getByRole("button", { name: "Overwrite with mine" }));
  await waitFor(() => expect(sent).toEqual(["4", "9"]));
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Overwrite with mine" })).not.toBeInTheDocument(),
  );
});

it("reloads the plan and drops the draft on a conflict when asked", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.put(apiUrl(`/dr-plans/${PLAN_ID}`), () =>
      HttpResponse.json(
        { code: "revision_mismatch", message: "revision mismatch" },
        { status: 409 },
      ),
    ),
    http.get(apiUrl(`/dr-plans/${PLAN_ID}`), () =>
      HttpResponse.json(plan("web-tier", { revision: 9, spec: { rpo_target_secs: 3600 } })),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  await user.click(await screen.findByRole("button", { name: "Move web up" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  await user.click(await screen.findByRole("button", { name: "Reload (discard my changes)" }));
  const groups = screen.getAllByRole("listitem", { name: /^Group \d: / });
  expect(groups.map((g) => g.getAttribute("aria-label"))).toEqual(["Group 1: db", "Group 2: web"]);
});

it("keeps the draft when leaving the plan tab and coming back", async () => {
  const user = userEvent.setup();
  setup();
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  await user.click(await screen.findByRole("button", { name: "Move web up" }));
  await user.click(screen.getByRole("tab", { name: "Overview" }));
  expect(await screen.findByRole("table", { name: "Protection" })).toBeInTheDocument();
  await user.click(screen.getByRole("tab", { name: "Plan" }));
  const groups = await screen.findAllByRole("listitem", { name: /^Group \d: / });
  expect(groups.map((g) => g.getAttribute("aria-label"))).toEqual(["Group 1: web", "Group 2: db"]);
  await user.click(screen.getByRole("button", { name: "Discard changes" }));
  await waitFor(() =>
    expect(
      screen
        .getAllByRole("listitem", { name: /^Group \d: / })
        .map((g) => g.getAttribute("aria-label")),
    ).toEqual(["Group 1: db", "Group 2: web"]),
  );
});

it("marks half-written map rows and warns about a test network failovers use", async () => {
  const user = userEvent.setup();
  setup();
  const sent: unknown[] = [];
  server.use(
    http.put(apiUrl(`/dr-plans/${PLAN_ID}`), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(plan());
    }),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  await screen.findByRole("button", { name: "Move web up" });
  const tests = screen.getByRole("region", { name: "Test network map" });
  await user.click(within(tests).getByRole("button", { name: "Add" }));
  await user.type(within(tests).getByLabelText("Source name 1"), "lan");
  await user.type(within(tests).getByLabelText("Name here 1"), "dr-lan");
  // 경고만 한다(고르는 것은 운영자다)
  expect(within(tests).getByText(/Failovers use this network too/)).toBeInTheDocument();
  await user.click(within(tests).getByRole("button", { name: "Add" }));
  await user.type(within(tests).getByLabelText("Source name 2"), "db");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(within(tests).getByText("Fill in both names or remove the row.")).toBeInTheDocument();
  expect(within(tests).getByLabelText("Name here 2")).toHaveAttribute("aria-invalid", "true");
  expect(sent).toHaveLength(0);
  await user.click(within(tests).getByRole("button", { name: "Remove mapping 2" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toMatchObject({ spec: { test_networks: { lan: "dr-lan" } } });
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
  const dialog = await screen.findByRole("dialog", { name: "Test failover of web-tier" });
  await user.click(within(dialog).getByRole("button", { name: "Start test" }));
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

it("links the other plan that already holds a VM", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.put(apiUrl(`/dr-plans/${PLAN_ID}`), () =>
      HttpResponse.json(
        {
          code: "dr_vm_in_other_plan",
          message: "VM is already in plan",
          details: { vm: "db1", plan: "db-tier" },
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  await user.click(await screen.findByRole("button", { name: "Save" }));
  const alert = await screen.findByText(/VM db1 is already in plan db-tier/);
  expect(alert.closest("[role=alert]")).toContainElement(
    screen.getByRole("link", { name: "Open plan db-tier" }),
  );
  expect(screen.getByRole("link", { name: "Open plan db-tier" })).toHaveAttribute(
    "href",
    "/dr-plans/db-tier",
  );
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

it("lists what the cleanup removes from the server's leftovers", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.get(apiUrl("/dr-plans/web-tier/report"), () =>
      HttpResponse.json(
        report({ leftovers: { run: RUN_ID, vms: ["db1-drt"], networks: ["drt-web-tier-0"] } }),
      ),
    ),
  );
  // 이름만 시험 VM 같은 VM은 서버가 지우지 않으므로 세지 않는다
  mockLists({ vm: [vm("db1-drt", "running"), vm("web1-drt", "running")] });
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await waitFor(() => expect(screen.getByRole("button", { name: "Clean up" })).toBeEnabled());
  await user.click(screen.getByRole("button", { name: "Clean up" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Clean up the test of web-tier" });
  const impact = await within(dialog).findByRole("list");
  expect(impact).toHaveTextContent("db1-drt");
  expect(impact).toHaveTextContent("drt-web-tier-0");
  expect(impact).not.toHaveTextContent("web1-drt");
});

it("counts the leftovers again when the cleanup opens", async () => {
  const user = userEvent.setup();
  setup();
  let reads = 0;
  server.use(
    http.get(apiUrl("/dr-plans/web-tier/report"), () => {
      reads += 1;
      // 남은 시험 VM은 VM 화면에서 손으로 지웠다(리뷰 2 웹 7)
      return HttpResponse.json(
        report({
          leftovers: { run: RUN_ID, vms: reads === 1 ? ["db1-drt", "web1-drt"] : ["db1-drt"] },
        }),
      );
    }),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await waitFor(() => expect(screen.getByRole("button", { name: "Clean up" })).toBeEnabled());
  await user.click(screen.getByRole("button", { name: "Clean up" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Clean up the test of web-tier" });
  await waitFor(() => expect(within(dialog).getByRole("list")).not.toHaveTextContent("web1-drt"));
  expect(within(dialog).getByRole("list")).toHaveTextContent("db1-drt");
});

it("disables the cleanup when no test left anything", async () => {
  setup();
  mockLists({ vm: [vm("db1-drt", "running")], network: [] });
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await screen.findByRole("table", { name: "Protection" });
  expect(screen.getByRole("button", { name: "Clean up" })).toBeDisabled();
});

it("shows the gaps and the VMs to shut down before a failover", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.get(apiUrl("/dr-plans/web-tier/report"), () =>
      HttpResponse.json(
        report({
          protection: [
            { vm: "db1", ok: true, backup: "db1-copy", age_secs: 3600 },
            { vm: "web1", ok: false, issue: "over-rpo", backup: "w", age_secs: 34 * 3600 },
            { vm: "web2", ok: false, issue: "no-recovery-point" },
          ],
        }),
      ),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await screen.findByRole("table", { name: "Protection" });
  await user.click(screen.getByRole("button", { name: "Failover" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Fail over web-tier" });
  const impact = within(dialog).getByRole("list");
  expect(impact).toHaveTextContent("3 VMs in 2 groups");
  expect(impact).toHaveTextContent("No recovery point (not recovered): web2");
  expect(impact).toHaveTextContent("Older than the RPO target: web1 1d 10h");
  expect(impact).not.toHaveTextContent("Shut down on");
  // 모드는 기본값이 없다
  await user.type(within(dialog).getByLabelText(/Type/), "web-tier");
  expect(within(dialog).getByRole("button", { name: "Fail over" })).toBeDisabled();
  await user.click(within(dialog).getByRole("radio", { name: /Planned/ }));
  expect(impact).toHaveTextContent("Shut down on prod: db1, web1, web2");
  expect(within(dialog).getByRole("button", { name: "Fail over" })).toBeEnabled();
  expect(within(dialog).queryByText(/No data is lost/)).not.toBeInTheDocument();
});

it("fills the reprotect dialog from the existing job and keeps its retention", async () => {
  const user = userEvent.setup();
  setup();
  mockLists({
    "storage-pool": [pool("local"), pool("bk", { backups: true })],
    "backup-job": [
      backupJob("dr-reprotect-web-tier", 1, {
        spec: {
          pool: "bk",
          schedule: { at: "04:30", days: ["sat"] },
          keep: { last: 5, daily: 7, weekly: 0, monthly: 2 },
          replicate: { remote: "prod", pool: "prod-bk" },
        },
      }),
    ],
  });
  server.use(
    http.get(apiUrl("/dr-plans/web-tier/report"), () =>
      HttpResponse.json(
        report({
          last_failover: {
            id: RUN_ID,
            name: "f",
            kind: "failover-disaster",
            state: "succeeded",
            started_at: 1,
            failed_vms: [],
          },
        }),
      ),
    ),
    http.get(apiUrl("/remotes/prod/api/storage-pools"), () =>
      HttpResponse.json({ revision: 1, items: [pool("prod-bk", { backups: true })] }),
    ),
  );
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/reprotect`), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(backupJob("dr-reprotect-web-tier", 1));
    }),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await user.click(await screen.findByRole("button", { name: "Reprotect" }));
  const dialog = await screen.findByRole("dialog", { name: "Reprotect web-tier" });
  expect(await within(dialog).findByLabelText("Daily at (UTC)")).toHaveValue("04:30");
  await user.click(within(dialog).getByRole("button", { name: "Create schedule" }));
  await waitFor(() =>
    expect(sent).toEqual([
      {
        remote: "prod",
        pool: "prod-bk",
        backup_pool: "bk",
        schedule: { at: "04:30", days: ["sat"] },
        keep: { last: 5, daily: 7, weekly: 0, monthly: 2 },
      },
    ]),
  );
});

it("asks before a test to skip VMs without a recovery point and to use routed test networks", async () => {
  const user = userEvent.setup();
  setup();
  mockLists({ network: [providerNetwork("ext", "physnet1")] });
  server.use(
    http.get(apiUrl("/dr-plans/web-tier"), () =>
      HttpResponse.json(plan("web-tier", { spec: { test_networks: { lan: "ext" } } })),
    ),
  );
  const point = new Date(2026, 10, 26).getTime() / 1000;
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/test`), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(run(), { status: 202 });
    }),
    // 고른 시각 이전에는 db1의 사본도 없다(리뷰 2 웹 1)
    http.get(apiUrl("/dr-plans/web-tier/report"), ({ request }) => {
      const at = new URL(request.url).searchParams.get("point_in_time");
      return HttpResponse.json(
        at === String(point)
          ? report({
              point_in_time: point,
              protection: [
                { vm: "db1", ok: false },
                { vm: "web2", ok: false },
              ],
            })
          : report(),
      );
    }),
  );
  const { container } = renderPage(<PlanPage />, {
    path: "/dr-plans/:name",
    url: "/dr-plans/web-tier",
  });
  await screen.findByRole("table", { name: "Protection" });
  await user.click(screen.getByRole("button", { name: "Test" }));
  const dialog = await screen.findByRole("dialog", { name: "Test failover of web-tier" });
  // 보고의 복구 지점 없는 VM과 밖과 닿는 시험망(업링크)
  expect(within(dialog).getByText(/1 VM has no recovery point/)).toBeInTheDocument();
  expect(await within(dialog).findByText(/can reach outside .*: ext\./)).toBeInTheDocument();
  await user.click(within(dialog).getByLabelText("Skip web2"));
  await user.click(within(dialog).getByLabelText(/I understand/));
  await user.click(within(dialog).getByText("Advanced"));
  await user.type(within(dialog).getByLabelText(/Recovery point at or before/), "2026-11-26T00:00");
  // 그 시각의 목록: db1도 없다. 다른 시점에서 고른 것은 버린다.
  const db1 = await within(dialog).findByLabelText("Skip db1");
  expect(db1).not.toBeChecked();
  expect(within(dialog).getByLabelText("Skip web2")).not.toBeChecked();
  await user.click(within(dialog).getByLabelText("Skip web2"));
  await expectNoAxeViolations(container);
  await user.click(within(dialog).getByRole("button", { name: "Start test" }));
  // 고르지 않은 db1은 건너뛰지 않는다(allow_missing은 보내지 않음)
  await waitFor(() =>
    expect(sent).toEqual([
      { skip_vms: ["web2"], allow_routed_test_networks: true, point_in_time: point },
    ]),
  );
});

it("shows the VMs the server names when a test lacks recovery points", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.get(apiUrl("/dr-plans/web-tier/report"), () =>
      HttpResponse.json(report({ protection: [] })),
    ),
  );
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/test`), async ({ request }) => {
      const body = await request.json();
      sent.push(body);
      return (body as { skip_vms?: string[] } | null)?.skip_vms
        ? HttpResponse.json(run(), { status: 202 })
        : HttpResponse.json(
            {
              code: "dr_missing_recovery_points",
              message: "no recovery point for db1",
              details: { vms: ["db1"] },
            },
            { status: 400 },
          );
    }),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await screen.findByRole("table", { name: "Protection" });
  await user.click(screen.getByRole("button", { name: "Test" }));
  const dialog = await screen.findByRole("dialog", { name: "Test failover of web-tier" });
  expect(within(dialog).queryByLabelText(/^Skip /)).not.toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Start test" }));
  expect(
    await within(dialog).findByText(/without a recovery point: db1\. Choose/),
  ).toBeInTheDocument();
  await user.click(within(dialog).getByLabelText("Skip db1"));
  await user.click(within(dialog).getByRole("button", { name: "Start test" }));
  await waitFor(() => expect(sent).toEqual([{}, { skip_vms: ["db1"] }]));
});

it("drops the server's list of missing VMs when the recovery point time changes", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.get(apiUrl("/dr-plans/web-tier/report"), () =>
      HttpResponse.json(report({ protection: [] })),
    ),
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/test`), () =>
      HttpResponse.json(
        { code: "dr_missing_recovery_points", message: "no", details: { vms: ["db1"] } },
        { status: 400 },
      ),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await screen.findByRole("table", { name: "Protection" });
  await user.click(screen.getByRole("button", { name: "Test" }));
  const dialog = await screen.findByRole("dialog", { name: "Test failover of web-tier" });
  await user.click(within(dialog).getByRole("button", { name: "Start test" }));
  expect(await within(dialog).findByLabelText("Skip db1")).toBeInTheDocument();
  await user.click(within(dialog).getByText("Advanced"));
  await user.type(within(dialog).getByLabelText(/Recovery point at or before/), "2026-11-26T00:00");
  // 그 시각의 보고에는 빈 곳이 없고, 다른 시점의 서버 목록은 동의 근거로 쓰지 않는다
  await waitFor(() => expect(within(dialog).queryByLabelText("Skip db1")).not.toBeInTheDocument());
});

it("lets a failover cancel a running test and sends disaster options", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.get(apiUrl("/dr-plans/web-tier"), () =>
      HttpResponse.json(plan("web-tier", { status: { run: RUN_ID } })),
    ),
  );
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/failover`), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(run({ spec: { kind: "failover-disaster" } }), { status: 202 });
    }),
    http.get(apiUrl(`/dr-plans/${PLAN_ID}/runs`), () =>
      HttpResponse.json({ revision: 1, items: [run()] }),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  // 시험이 도는 동안 시험·뒷정리는 막히고 장애 조치는 된다
  await waitFor(() => expect(screen.getByRole("button", { name: "Failover" })).toBeEnabled());
  expect(screen.getByRole("button", { name: "Test" })).toBeDisabled();
  await user.click(screen.getByRole("button", { name: "Failover" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Fail over web-tier" });
  expect(within(dialog).getByText(/A test of this plan is running/)).toBeInTheDocument();
  await user.click(within(dialog).getByRole("radio", { name: /Disaster/ }));
  await user.click(within(dialog).getByText("Advanced"));
  await user.type(within(dialog).getByLabelText(/Recovery point at or before/), "2026-11-26T06:30");
  // 그 시각의 보고(같은 빈 곳)를 받은 뒤 고른다
  await user.click(await within(dialog).findByLabelText("Skip web2"));
  await user.type(within(dialog).getByLabelText(/Type/), "web-tier");
  await user.click(within(dialog).getByRole("button", { name: "Fail over" }));
  await waitFor(() =>
    expect(sent).toEqual([
      {
        mode: "disaster",
        confirm: "web-tier",
        skip_vms: ["web2"],
        point_in_time: new Date(2026, 10, 26, 6, 30).getTime() / 1000,
      },
    ]),
  );
});

it("keeps the failover blocked while another failover runs", async () => {
  setup();
  server.use(
    http.get(apiUrl("/dr-plans/web-tier"), () =>
      HttpResponse.json(plan("web-tier", { status: { run: RUN_ID } })),
    ),
    http.get(apiUrl(`/dr-plans/${PLAN_ID}/runs`), () =>
      HttpResponse.json({ revision: 1, items: [run({ spec: { kind: "failover-disaster" } })] }),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await screen.findByRole("table", { name: "Protection" });
  expect(screen.getByRole("button", { name: "Failover" })).toBeDisabled();
});

it("explains the planned migration preflight problems and where to fix them", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/failover`), () =>
      HttpResponse.json(
        {
          code: "dr_planned_preflight",
          message: "a planned failover would stop production VMs and then fail",
          details: {
            problems: [
              { code: "token_role", message: "the token is operator" },
              { code: "no_backup_pool", vm: "web2", message: "no backup" },
              { code: "something_new", message: "a new server problem" },
              { code: "dr_readiness", vm: "db1", message: "network lan is not here" },
              { code: "dr_name_taken", vm: "web1", message: "taken" },
            ],
          },
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await user.click(await screen.findByRole("button", { name: "Failover" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Fail over web-tier" });
  await user.click(within(dialog).getByRole("radio", { name: /Planned/ }));
  // planned는 새 백업을 떠 오므로 복구 지점 고르기는 없다. 전부 아니면 전무다.
  expect(within(dialog).queryByText("Advanced")).not.toBeInTheDocument();
  expect(dialog).toHaveTextContent("It moves all VMs or none");
  await user.type(within(dialog).getByLabelText(/Type/), "web-tier");
  await user.click(within(dialog).getByRole("button", { name: "Fail over" }));
  const error = await within(dialog).findByText(/nothing was shut down/);
  expect(error).toHaveTextContent("not an admin token");
  expect(error).toHaveTextContent("web2 has no backup on the source — set a source backup pool");
  expect(error).toHaveTextContent("a new server problem");
  expect(error).toHaveTextContent("nothing to restore onto");
  expect(error).toHaveTextContent("network lan is not here");
  expect(error).toHaveTextContent("a VM web1 that this plan did not recover");
});

it("warns about unfenced source VMs and fences them with the plan name", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.get(apiUrl("/dr-plans/web-tier"), () =>
      HttpResponse.json(
        plan("web-tier", { status: { source_fence_pending: [{ vm: "db1" }, { vm: "web1" }] } }),
      ),
    ),
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/fence-source`), () =>
      HttpResponse.json({
        results: [
          { vm: "db1", ok: true, result: "stopped" },
          { vm: "web1", ok: false, result: "failed", error: "source said 500" },
          { vm: "web2", ok: true, result: "different_vm" },
          { vm: "web3", ok: false, result: "different_cluster" },
          { vm: "web4", ok: true, result: "not_found" },
        ],
        pending: [{ vm: "web1" }, { vm: "web3" }],
      }),
    ),
  );
  const { container } = renderPage(<PlanPage />, {
    path: "/dr-plans/:name",
    url: "/dr-plans/web-tier",
  });
  const band = await screen.findByText(/not stopped on prod: db1, web1/);
  expect(band).toHaveTextContent("run twice");
  await expectNoAxeViolations(container);
  await user.click(screen.getByRole("button", { name: "Fence source VMs" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Fence the VMs on prod" });
  const confirm = within(dialog).getByRole("button", { name: "Fence" });
  expect(confirm).toBeDisabled();
  await user.type(within(dialog).getByLabelText(/Type/), "web-tier");
  await user.click(confirm);
  const result = await screen.findByRole("region", { name: "Fence result" });
  expect(within(result).getByText("db1").parentElement).toHaveTextContent("stopped");
  expect(within(result).getByText("web1").parentElement).toHaveTextContent(
    "not stoppedsource said 500",
  );
  expect(within(result).getByText("web2").parentElement).toHaveTextContent(
    "a different VM with that name",
  );
  expect(within(result).getByText("web3").parentElement).toHaveTextContent(
    "points at another cluster",
  );
  expect(within(result).getByText("web4").parentElement).toHaveTextContent("not on the source");
  expect(result).toHaveTextContent("Still to fence: web1, web3");
});

it("clears the fence list after asking", async () => {
  const user = userEvent.setup();
  setup();
  const sent: string[] = [];
  server.use(
    http.get(apiUrl("/dr-plans/web-tier"), () =>
      HttpResponse.json(
        plan("web-tier", {
          status: { source_fence_pending: sent.length ? [] : [{ vm: "db1", source_id: RUN_ID }] },
        }),
      ),
    ),
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/clear-fence`), () => {
      sent.push("clear");
      return HttpResponse.json(plan());
    }),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await user.click(await screen.findByRole("button", { name: "Clear fence" }));
  const dialog = await screen.findByRole("alertdialog", {
    name: "Clear the fence list of web-tier",
  });
  expect(dialog).toHaveTextContent("Does nothing on prod");
  expect(dialog).toHaveTextContent("VMs removed from the list: db1");
  await user.click(within(dialog).getByRole("button", { name: "Clear fence" }));
  await waitFor(() => expect(sent).toEqual(["clear"]));
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Clear fence" })).not.toBeInTheDocument(),
  );
});

it("tells why fencing could not start", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.get(apiUrl("/dr-plans/web-tier"), () =>
      HttpResponse.json(plan("web-tier", { status: { source_fence_pending: [{ vm: "db1" }] } })),
    ),
    http.post(apiUrl(`/dr-plans/${PLAN_ID}/actions/fence-source`), () =>
      HttpResponse.json(
        { code: "dr_source_unreachable", message: "cannot be reached" },
        { status: 409 },
      ),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  await user.click(await screen.findByRole("button", { name: "Fence source VMs" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Fence the VMs on prod" });
  await user.type(within(dialog).getByLabelText(/Type/), "web-tier");
  await user.click(within(dialog).getByRole("button", { name: "Fence" }));
  // 계획된 이전의 문구가 아니라 fence의 것
  expect(await within(dialog).findByText(/Fence it when it comes back/)).toBeInTheDocument();
});

it("shows failover readiness per VM and the problems the last test found", async () => {
  setup();
  server.use(
    http.get(apiUrl("/dr-plans/web-tier"), () =>
      HttpResponse.json(
        plan("web-tier", {
          status: {
            last_test: {
              run: RUN_ID,
              at: 1,
              rto_secs: 60,
              result: "succeeded",
              readiness_issues: 1,
            },
          },
        }),
      ),
    ),
    http.get(apiUrl("/dr-plans/web-tier/report"), () =>
      HttpResponse.json(
        report({
          protection: [
            { vm: "db1", ok: true, backup: "db1-copy", age_secs: 3600 },
            { vm: "web1", ok: false, issue: "verify-failed", backup: "w", age_secs: 7200 },
          ],
          failover_readiness: [
            { vm: "db1", ok: true },
            {
              vm: "web1",
              ok: false,
              issues: [
                { kind: "network", name: "lan" },
                { kind: "pool", name: "fast" },
              ],
            },
          ],
        }),
      ),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier" });
  const table = await screen.findByRole("table", { name: "Protection" });
  expect(within(table).getByRole("columnheader", { name: "Failover readiness" })).toBeVisible();
  expect(within(table).getByRole("row", { name: /db1/ })).toHaveTextContent("Ready");
  const web1 = within(table).getByRole("row", { name: /web1/ });
  expect(web1).toHaveTextContent("Newest copy failed verification");
  expect(web1).toHaveTextContent("network lan missing");
  expect(web1).toHaveTextContent("pool fast missing");
  expect(screen.getByText("1 VM not ready for failover")).toBeInTheDocument();
  // 그 시험 실행으로 잇는다(위 표는 지금 센 것 — 리뷰 2 웹 9)
  expect(screen.getByText(/The last test found 1 VM not ready/)).toContainElement(
    screen.getByRole("link", { name: "Open that test" }),
  );
  expect(screen.getByRole("link", { name: "Open that test" })).toHaveAttribute(
    "href",
    `/dr-runs/${RUN_ID}`,
  );
});

it("tells to clean up before deleting a plan with test leftovers", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.delete(apiUrl(`/dr-plans/${PLAN_ID}`), () =>
      HttpResponse.json({ code: "dr_cleanup_required", message: "left over" }, { status: 409 }),
    ),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  await user.click(await screen.findByRole("button", { name: "Delete plan" }));
  const dialog = await screen.findByRole("alertdialog");
  await user.type(within(dialog).getByLabelText(/Type/), "web-tier");
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByText(/Use Clean up first/)).toBeInTheDocument();
});

it("edits the source backup pool of the plan and of a VM", async () => {
  const user = userEvent.setup();
  setup();
  const sent: { spec: RecoveryPlanSpec }[] = [];
  server.use(
    http.put(apiUrl(`/dr-plans/${PLAN_ID}`), async ({ request }) => {
      const body = (await request.json()) as { spec: RecoveryPlanSpec };
      sent.push(body);
      return HttpResponse.json(plan("web-tier", { revision: 5, spec: body.spec }));
    }),
  );
  renderPage(<PlanPage />, { path: "/dr-plans/:name", url: "/dr-plans/web-tier?tab=plan" });
  // 계획의 칸(기본 절)이 먼저, VM마다의 칸이 뒤에
  const [planPool] = await screen.findAllByRole("textbox", { name: "Source backup pool" });
  await user.type(planPool!, "prod-bk");
  const db1 = within(screen.getByRole("list", { name: "VMs of db" })).getByRole("listitem");
  await user.type(within(db1).getByLabelText("Source backup pool"), "prod-fast");
  // IP 규칙 옆 도움말: 되살린 VM과 cloud-init
  await user.click(within(db1).getByRole("button", { name: "About IP rules and cloud-init" }));
  expect(await screen.findByText(/new SSH host keys/)).toBeInTheDocument();
  expect(screen.getByText(/without cloud-init keeps its IP/)).toBeInTheDocument();
  await user.keyboard("{Escape}");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]!.spec.source_backup_pool).toBe("prod-bk");
  expect(sent[0]!.spec.groups[0]!.vms[0]!.source_backup_pool).toBe("prod-fast");
});
