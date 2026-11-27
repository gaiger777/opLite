import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { HistoryPage } from "@/history/HistoryPage";
import { resetTasks } from "@/tasks/tasks";
import { expectNoAxeViolations } from "@/test/axe";
import { detail, entry, plan, TIMELINE, WEB_ID } from "@/test/declarative";
import { task } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const VIEWER = { ...ADMIN, role: "viewer" as const };

/** 타임라인을 답하고 받은 질의를 모은다 */
function mockTimeline(items = TIMELINE) {
  const queries: URLSearchParams[] = [];
  server.use(
    http.get(apiUrl("/history"), ({ request }) => {
      queries.push(new URL(request.url).searchParams);
      return HttpResponse.json({ items });
    }),
    http.get(apiUrl("/history/:kind/:key/:revision"), ({ params, request }) => {
      const row = items.find((e) => String(e.revision) === params.revision)!;
      const compare = new URL(request.url).searchParams.get("compare") ?? "prev";
      return HttpResponse.json(
        compare === "live"
          ? detail(row, {
              compare: "live",
              diff: [{ path: "vcpus", op: "replace", old: 2, new: 4 }],
            })
          : detail(row),
      );
    }),
    http.get(apiUrl("/apply-sets"), () => HttpResponse.json({ revision: 1, items: [] })),
  );
  return queries;
}

it("groups the timeline by day with source chips, reasons and changed fields, and passes axe", async () => {
  mockLists({});
  const queries = mockTimeline();
  const { container } = renderPage(<HistoryPage />, { path: "/history" });
  const days = await screen.findAllByRole("heading", { level: 2 });
  expect(days).toHaveLength(2);
  const first = within(days[0]!.closest("section")!).getAllByRole("button");
  expect(first).toHaveLength(1);
  expect(first[0]).toHaveTextContent("Changed");
  expect(first[0]).toHaveTextContent("VM web");
  expect(first[0]).toHaveTextContent("admin");
  expect(first[0]).toHaveTextContent("Web UI");
  // 전·후 값이 없는 옛 줄은 칸 이름만(계획 77-7)
  expect(first[0]).toHaveTextContent("memory changed");
  expect(first[0]).toHaveTextContent("“more memory for the sale”");
  const second = within(days[1]!.closest("section")!).getAllByRole("button");
  expect(second.map((b) => b.textContent)).toEqual([
    expect.stringContaining("ci (gitops)"),
    expect.stringContaining("Created"),
  ]);
  // 바뀐 칸은 셋만 사람 말로(서버가 준 전·후 값은 함께), 나머지는 수
  expect(second[0]).toHaveTextContent(
    "vCPUs 2 → 4 · label tier: web · network cards (NICs) changed · and 2 more",
  );
  expect(second[0]).toHaveTextContent("YAML apply");
  expect(within(second[0]!).getByText("YAML apply").closest("[title]")).toHaveAttribute(
    "title",
    "Changed by applying YAML files with orbivisorctl apply",
  );
  expect(queries[0]!.get("limit")).toBe("100");
  expect(queries[0]!.get("since")).not.toBeNull();
  await expectNoAxeViolations(container);
});

it("filters by kind, source, user and folder", async () => {
  const user = userEvent.setup();
  mockLists({});
  const queries = mockTimeline();
  renderPage(<HistoryPage />, { path: "/history" });
  await screen.findAllByRole("heading", { level: 2 });
  await user.selectOptions(screen.getByLabelText("Kind"), "security-group");
  await user.selectOptions(screen.getByLabelText("Source"), "terraform");
  await user.selectOptions(screen.getByLabelText("Period"), "all");
  await user.type(screen.getByLabelText("User"), "alice");
  await user.type(screen.getByLabelText("Folder"), "/prod");
  await user.click(screen.getByRole("button", { name: "Apply" }));
  await waitFor(() => {
    const last = queries.at(-1)!;
    expect(Object.fromEntries(last)).toEqual({
      limit: "100",
      kind: "security-group",
      source: "terraform",
      user: "alice",
      folder: "/prod",
    });
  });
});

it("opens a drawer with the before/after table, secret marks and a live comparison", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  renderPage(<HistoryPage />, { path: "/history" });
  const [first] = await screen.findAllByRole("button", { name: /more memory/ });
  await user.click(first!);
  const drawer = await screen.findByRole("dialog", { name: "VM web" });
  const table = await within(drawer).findByRole("table", { name: "Changed fields of web" });
  const rows = within(table).getAllByRole("row");
  // 칸은 사람 말 이름(원래 경로는 작게), 크기는 단위로
  expect(rows[1]).toHaveTextContent("memorymemory_mib");
  expect(rows[1]).toHaveTextContent("4 GiB");
  expect(rows[1]).toHaveTextContent("8 GiB");
  expect(drawer).toHaveTextContent("SourceWeb UIA person changed it in the web UI");
  // 비밀은 표시만
  expect(rows[2]).toHaveTextContent("(secret set)");
  expect(rows[2]).toHaveTextContent("(secret changed)");
  expect(drawer).toHaveTextContent("more memory for the sale");
  await user.selectOptions(within(drawer).getByLabelText("Compare with"), "live");
  expect(await within(drawer).findByText("vCPUs")).toBeInTheDocument();
  await user.click(within(drawer).getByText("Full configuration at this record"));
  expect(within(drawer).getByText(/Secrets are not kept/)).toBeInTheDocument();
});

it("restores a revision after showing the server's plan and applies that plan", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  const restores: unknown[] = [];
  let applied: unknown;
  server.use(
    http.post(apiUrl("/history/vm/:key/restore"), async ({ request, params }) => {
      expect(params.key).toBe(WEB_ID);
      restores.push(await request.json());
      return HttpResponse.json(plan());
    }),
    http.post(apiUrl("/apply"), async ({ request }) => {
      applied = await request.json();
      return HttpResponse.json(
        { plan: plan(), task: task("t1", "apply", WEB_ID) },
        { status: 202 },
      );
    }),
  );
  renderPage(<HistoryPage />, { path: "/history" });
  const [first] = await screen.findAllByRole("button", { name: /more memory/ });
  await user.click(first!);
  const drawer = await screen.findByRole("dialog", { name: "VM web" });
  await user.click(await within(drawer).findByRole("button", { name: "Restore these values…" }));
  const dialog = await screen.findByRole("dialog", {
    name: "Restore web to its values at record #81?",
  });
  expect(await within(dialog).findByText(/1 to change/)).toBeInTheDocument();
  expect(dialog).toHaveTextContent("secrets are not restored; the current password stays");
  expect(within(dialog).getByRole("table", { name: "Changed fields of VM web" })).toHaveTextContent(
    "8 GiB",
  );
  expect(restores).toEqual([{ revision: 81, dry_run: true }]);
  await user.click(within(dialog).getByRole("button", { name: "Restore" }));
  await waitFor(() => expect(applied).toEqual({ plan: "019a0f3c-0000-7000-8073-0000000000f1" }));
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
});

it("keeps Restore off for a blocked plan and explains a cluster upgrade", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  let calls = 0;
  server.use(
    http.post(apiUrl("/history/vm/:key/restore"), () => {
      calls++;
      return calls === 1
        ? HttpResponse.json(
            plan({
              summary: { create: 0, update: 0, delete: 0, unchanged: 0, blocked: 1 },
              steps: [
                {
                  n: 1,
                  kind: "Vm",
                  name: "web",
                  action: "blocked",
                  problems: [
                    { code: "data_lost", level: "error", message: "volume web-disk0 is gone" },
                  ],
                },
              ],
            }),
          )
        : HttpResponse.json(
            { code: "cluster_upgrading", message: "node b lacks config-history" },
            { status: 409 },
          );
    }),
  );
  renderPage(<HistoryPage />, { path: "/history" });
  const [first] = await screen.findAllByRole("button", { name: /more memory/ });
  await user.click(first!);
  const drawer = await screen.findByRole("dialog", { name: "VM web" });
  await user.click(await within(drawer).findByRole("button", { name: "Restore these values…" }));
  let dialog = await screen.findByRole("dialog", { name: /^Restore web/ });
  expect(await within(dialog).findByText(/volume web-disk0 is gone/)).toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Restore" })).toBeDisabled();
  await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await user.click(within(drawer).getByRole("button", { name: "Restore these values…" }));
  dialog = await screen.findByRole("dialog", { name: /^Restore web/ });
  expect(
    await within(dialog).findByText("The cluster is being upgraded: node b lacks config-history"),
  ).toBeInTheDocument();
});

it("asks for a new name when recreating a deleted resource whose name is taken", async () => {
  const user = userEvent.setup();
  mockLists({});
  const deleted = entry(90, { op: "delete", changed: [] });
  mockTimeline([deleted]);
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl("/history/vm/:key/restore"), async ({ request }) => {
      const body = (await request.json()) as { name?: string };
      bodies.push(body);
      return body.name
        ? HttpResponse.json(
            plan({ summary: { create: 1, update: 0, delete: 0, unchanged: 0, blocked: 0 } }),
          )
        : HttpResponse.json({ code: "name_taken", message: "web is taken" }, { status: 409 });
    }),
  );
  renderPage(<HistoryPage />, { path: "/history" });
  await user.click(await screen.findByRole("button", { name: /Deleted/ }));
  const drawer = await screen.findByRole("dialog", { name: "VM web" });
  await user.click(
    await within(drawer).findByRole("button", { name: "Recreate with these values…" }),
  );
  const dialog = await screen.findByRole("dialog", {
    name: "Recreate web with its values at record #90?",
  });
  await user.type(await within(dialog).findByLabelText("New name"), "web-old");
  await user.click(within(dialog).getByRole("button", { name: "Plan again" }));
  expect(await within(dialog).findByText(/1 to create/)).toBeInTheDocument();
  expect(bodies.at(-1)).toEqual({ revision: 90, dry_run: true, name: "web-old" });
  expect(within(dialog).getByRole("button", { name: "Recreate" })).toBeEnabled();
});

it("hides restoring from viewers", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  renderPage(<HistoryPage />, { path: "/history", as: VIEWER });
  const [first] = await screen.findAllByRole("button", { name: /more memory/ });
  expect(
    screen.queryByRole("button", { name: "Restore to a point in time…" }),
  ).not.toBeInTheDocument();
  await user.click(first!);
  const drawer = await screen.findByRole("dialog", { name: "VM web" });
  await within(drawer).findByRole("table", { name: "Changed fields of web" });
  expect(
    within(drawer).queryByRole("button", { name: "Restore these values…" }),
  ).not.toBeInTheDocument();
});

it("plans a point-in-time restore and allows data loss only on its own", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  const bodies: Record<string, unknown>[] = [];
  let applied: unknown;
  server.use(
    http.post(apiUrl("/history/restore"), async ({ request }) => {
      bodies.push((await request.json()) as Record<string, unknown>);
      return HttpResponse.json(
        plan({ summary: { create: 1, update: 1, delete: 1, unchanged: 3, blocked: 0 } }),
      );
    }),
    http.post(apiUrl("/apply"), async ({ request }) => {
      applied = await request.json();
      return HttpResponse.json(
        { plan: plan(), task: task("t2", "apply", WEB_ID) },
        { status: 202 },
      );
    }),
  );
  renderPage(<HistoryPage />, { path: "/history" });
  await user.click(await screen.findByRole("button", { name: "Restore to a point in time…" }));
  const dialog = await screen.findByRole("dialog", { name: "Restore to a point in time" });
  const preview = within(dialog).getByRole("button", { name: "Show the plan" });
  // 범위가 없으면 계획을 보지 못한다
  expect(preview).toBeDisabled();
  await user.type(within(dialog).getByLabelText("Folder"), "/prod/web");
  const dataLoss = within(dialog).getByRole("checkbox", { name: /their data is lost/ });
  expect(dataLoss).toBeDisabled();
  await user.click(within(dialog).getByRole("checkbox", { name: /Delete resources created/ }));
  expect(dataLoss).toBeEnabled();
  await user.click(dataLoss);
  await user.selectOptions(within(dialog).getByLabelText("Point in time by"), "revision");
  await user.type(within(dialog).getByLabelText("Record number"), "70");
  await user.click(preview);
  expect(
    await within(dialog).findByText(/1 to create, 1 to change, 1 to delete/),
  ).toBeInTheDocument();
  expect(bodies).toEqual([
    {
      at: { revision: 70 },
      scope: { folder: "/prod/web" },
      recreate_deleted: true,
      delete_created: true,
      allow_data_loss: true,
      dry_run: true,
    },
  ]);
  // 무엇이든 바꾸면 계획이 사라진다
  await user.click(within(dialog).getByRole("checkbox", { name: /Recreate resources deleted/ }));
  expect(within(dialog).queryByText(/1 to create/)).not.toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Restore" })).toBeDisabled();
  await user.click(preview);
  await within(dialog).findByText(/1 to create/);
  await user.click(within(dialog).getByRole("button", { name: "Restore" }));
  await waitFor(() => expect(applied).toEqual({ plan: plan().plan }));
});

it("drops a point-in-time plan that comes back after the inputs changed", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  server.use(
    http.post(apiUrl("/history/restore"), async () => {
      await held;
      return HttpResponse.json(
        plan({ summary: { create: 0, update: 0, delete: 2, unchanged: 0, blocked: 0 } }),
      );
    }),
  );
  renderPage(<HistoryPage />, { path: "/history" });
  await user.click(await screen.findByRole("button", { name: "Restore to a point in time…" }));
  const dialog = await screen.findByRole("dialog", { name: "Restore to a point in time" });
  await user.type(within(dialog).getByLabelText("Folder"), "/prod/web");
  const deleteCreated = within(dialog).getByRole("checkbox", { name: /Delete resources created/ });
  await user.click(deleteCreated);
  await user.click(within(dialog).getByRole("button", { name: "Show the plan" }));
  // 계획을 세우는 동안 지우기를 끈다 — 늦게 온 (지우기 둘) 계획은 보이지 않고 적용할 수 없다.
  await user.click(deleteCreated);
  release();
  await waitFor(() =>
    expect(within(dialog).getByRole("button", { name: "Show the plan" })).toBeEnabled(),
  );
  expect(within(dialog).queryByText(/2 to delete/)).not.toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Restore" })).toBeDisabled();
});

it("plans again when the restore plan went stale or is gone, and applies the new plan", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  const fresh = "019a0f3c-0000-7000-8073-0000000000f2";
  let planned = 0;
  const applied: unknown[] = [];
  server.use(
    http.post(apiUrl("/history/vm/:key/restore"), () => {
      planned++;
      return HttpResponse.json(plan(planned > 1 ? { plan: fresh } : {}));
    }),
    http.post(apiUrl("/apply"), async ({ request }) => {
      const body = (await request.json()) as { plan: string };
      applied.push(body);
      return body.plan === fresh
        ? HttpResponse.json({ plan: plan(), task: task("t1", "apply", WEB_ID) }, { status: 202 })
        : HttpResponse.json(
            { code: "plan_stale", message: 'vm "web" changed after the plan was made; plan again' },
            { status: 412 },
          );
    }),
  );
  renderPage(<HistoryPage />, { path: "/history" });
  const [first] = await screen.findAllByRole("button", { name: /more memory/ });
  await user.click(first!);
  const drawer = await screen.findByRole("dialog", { name: "VM web" });
  await user.click(await within(drawer).findByRole("button", { name: "Restore these values…" }));
  const dialog = await screen.findByRole("dialog", {
    name: "Restore web to its values at record #81?",
  });
  await within(dialog).findByText(/1 to change/);
  await user.click(within(dialog).getByRole("button", { name: "Restore" }));
  expect(
    await within(dialog).findByText(
      "The resource changed or the plan expired. Review the new plan and restore again.",
    ),
  ).toBeInTheDocument();
  expect(planned).toBe(2);
  await user.click(within(dialog).getByRole("button", { name: "Restore" }));
  await waitFor(() => expect(applied.at(-1)).toEqual({ plan: fresh }));
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
});

it("plans a point-in-time restore again when its plan went stale or is gone", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  const fresh = "019a0f3c-0000-7000-8073-0000000000f2";
  let planned = 0;
  const applied: unknown[] = [];
  server.use(
    http.post(apiUrl("/history/restore"), () => {
      planned++;
      return HttpResponse.json(plan(planned > 1 ? { plan: fresh } : {}));
    }),
    http.post(apiUrl("/apply"), async ({ request }) => {
      const body = (await request.json()) as { plan: string };
      applied.push(body);
      return body.plan === fresh
        ? HttpResponse.json({ plan: plan(), task: task("t2", "apply", WEB_ID) }, { status: 202 })
        : HttpResponse.json(
            { code: "plan_not_found", message: "no such plan; plan again" },
            { status: 404 },
          );
    }),
  );
  renderPage(<HistoryPage />, { path: "/history" });
  await user.click(await screen.findByRole("button", { name: "Restore to a point in time…" }));
  const dialog = await screen.findByRole("dialog", { name: "Restore to a point in time" });
  await user.type(within(dialog).getByLabelText("Folder"), "/prod/web");
  await user.click(within(dialog).getByRole("button", { name: "Show the plan" }));
  await within(dialog).findByText(/1 to change/);
  await user.click(within(dialog).getByRole("button", { name: "Restore" }));
  expect(
    await within(dialog).findByText(
      "The resource changed or the plan expired. Review the new plan and restore again.",
    ),
  ).toBeInTheDocument();
  await waitFor(() => expect(planned).toBe(2));
  await within(dialog).findByText(/1 to change/);
  await user.click(within(dialog).getByRole("button", { name: "Restore" }));
  await waitFor(() => expect(applied).toEqual([{ plan: plan().plan }, { plan: fresh }]));
});

it("explains that restoring replaces the whole spec and shows removed fields going back to default", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  server.use(
    http.post(apiUrl("/history/vm/:key/restore"), () =>
      HttpResponse.json(
        plan({
          steps: [
            {
              n: 1,
              kind: "Vm",
              name: "web",
              action: "update",
              diff: [
                { path: "memory_mib", op: "replace", old: 8192, new: 4096 },
                { path: "max_memory_mib", op: "remove", old: 16384 },
              ],
            },
          ],
        }),
      ),
    ),
  );
  renderPage(<HistoryPage />, { path: "/history" });
  const [first] = await screen.findAllByRole("button", { name: /more memory/ });
  await user.click(first!);
  const drawer = await screen.findByRole("dialog", { name: "VM web" });
  await user.click(await within(drawer).findByRole("button", { name: "Restore these values…" }));
  const dialog = await screen.findByRole("dialog", {
    name: "Restore web to its values at record #81?",
  });
  expect(dialog).toHaveTextContent(
    "Every field goes back to its value then — fields left at their defaults are reset; secrets added since stay.",
  );
  const table = await within(dialog).findByRole("table", { name: "Changed fields of VM web" });
  const row = within(table).getByRole("row", { name: /max_memory_mib/ });
  expect(row).toHaveTextContent("16 GiB");
  expect(row).toHaveTextContent("→ default");
  expect(within(table).getByRole("row", { name: /^memory\s?memory_mib/ })).not.toHaveTextContent(
    "default",
  );
});

it("offers to take over a Terraform-managed resource and plans with force_conflicts", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  const bodies: Record<string, unknown>[] = [];
  server.use(
    http.post(apiUrl("/history/vm/:key/restore"), async ({ request }) => {
      const body = (await request.json()) as Record<string, unknown>;
      bodies.push(body);
      return HttpResponse.json(
        body.force_conflicts
          ? plan()
          : plan({
              summary: { create: 0, update: 0, delete: 0, unchanged: 0, blocked: 1 },
              steps: [
                {
                  n: 1,
                  kind: "Vm",
                  name: "web",
                  action: "blocked",
                  problems: [
                    {
                      code: "managed_elsewhere",
                      level: "error",
                      message: "Vm web is managed by Terraform",
                    },
                  ],
                },
              ],
            }),
      );
    }),
  );
  renderPage(<HistoryPage />, { path: "/history" });
  const [first] = await screen.findAllByRole("button", { name: /more memory/ });
  await user.click(first!);
  const drawer = await screen.findByRole("dialog", { name: "VM web" });
  await user.click(await within(drawer).findByRole("button", { name: "Restore these values…" }));
  const dialog = await screen.findByRole("dialog", {
    name: "Restore web to its values at record #81?",
  });
  const force = await within(dialog).findByRole("checkbox", {
    name: "Take over from Terraform and restore anyway",
  });
  expect(dialog).toHaveTextContent(/next terraform apply changes it back/);
  expect(within(dialog).getByRole("button", { name: "Restore" })).toBeDisabled();
  await user.click(force);
  await within(dialog).findByText(/1 to change/);
  expect(bodies.at(-1)).toEqual({ revision: 81, dry_run: true, force_conflicts: true });
  expect(within(dialog).getByRole("button", { name: "Restore" })).toBeEnabled();
});

it("does not offer the Terraform takeover for other blocks", async () => {
  const user = userEvent.setup();
  mockLists({});
  mockTimeline();
  server.use(
    http.post(apiUrl("/history/vm/:key/restore"), () =>
      HttpResponse.json(
        plan({
          summary: { create: 0, update: 0, delete: 0, unchanged: 0, blocked: 1 },
          steps: [
            {
              n: 1,
              kind: "Vm",
              name: "web",
              action: "blocked",
              problems: [
                { code: "managed_elsewhere", level: "error", message: "managed by Terraform" },
                { code: "forbidden", level: "error", message: "no vm.config" },
              ],
            },
          ],
        }),
      ),
    ),
  );
  renderPage(<HistoryPage />, { path: "/history" });
  const [first] = await screen.findAllByRole("button", { name: /more memory/ });
  await user.click(first!);
  const drawer = await screen.findByRole("dialog", { name: "VM web" });
  await user.click(await within(drawer).findByRole("button", { name: "Restore these values…" }));
  const dialog = await screen.findByRole("dialog", {
    name: "Restore web to its values at record #81?",
  });
  await within(dialog).findByText(/no vm.config/);
  expect(within(dialog).queryByRole("checkbox")).not.toBeInTheDocument();
});
