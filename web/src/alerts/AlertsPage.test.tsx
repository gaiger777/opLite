import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { AlertRule } from "@/alerts/alerts";
import { AlertsPage } from "@/alerts/AlertsPage";
import { renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const RULE: AlertRule = {
  id: "r1",
  name: "cpu-hot",
  revision: 7,
  labels: {},
  spec: {
    enabled: true,
    condition: { kind: "node_metric", metric: "cpu", op: "gt", value: 90 },
    for_secs: 300,
    severity: "critical",
    targets: ["ops-mail"],
    repeat_secs: 14_400,
    send_resolved: true,
  },
  status: {},
};

const VIEWER = {
  user: "00000000-0000-0000-0000-000000000002",
  name: "eve",
  role: "viewer" as const,
};

function rules(items: AlertRule[] = [RULE]) {
  return http.get(apiUrl("/alert-rules"), () => HttpResponse.json({ revision: 1, items }));
}

it("lists active alerts and nodes that did not answer", async () => {
  server.use(
    http.get(apiUrl("/alerts"), () =>
      HttpResponse.json({
        alerts: [
          {
            rule: "r1",
            rule_name: "cpu-hot",
            severity: "critical",
            condition: "node_metric",
            subject: { kind: "node", id: "n1", name: "node-a" },
            state: "pending",
            since: 1_800_000_000_000,
            value: 93.5,
            threshold: 90,
          },
        ],
        unreachable: ["node-b"],
        unsupported: ["node-c"],
      }),
    ),
  );
  renderPage(<AlertsPage />, { path: "/alerts" });
  expect(await screen.findByRole("heading", { level: 1, name: "Alerts" })).toBeInTheDocument();
  expect(screen.getByRole("tab", { name: "Active" })).toHaveAttribute("aria-selected", "true");
  const list = await screen.findByRole("list", { name: "Active" });
  expect(list).toHaveTextContent("cpu-hot");
  expect(list).toHaveTextContent("Pending");
  expect(list).toHaveTextContent("value 93.5 · threshold 90");
  expect(
    screen.getByText(/did not answer, so their alerts are missing: node-b/),
  ).toBeInTheDocument();
  expect(screen.getByText(/older version without alerts: node-c/)).toBeInTheDocument();
});

it("shows rules read-only to a viewer", async () => {
  server.use(rules());
  renderPage(<AlertsPage />, { path: "/alerts", url: "/alerts?tab=rules", as: VIEWER });
  const table = await screen.findByRole("table", { name: "Rules" });
  const row = await within(table).findByRole("row", { name: /cpu-hot/ });
  expect(row).toHaveTextContent("Node CPU % > 90");
  expect(row).toHaveTextContent("for 300 s");
  expect(row).toHaveTextContent("Critical");
  expect(row).toHaveTextContent("ops-mail");
  expect(screen.queryByRole("button", { name: "Create rule" })).not.toBeInTheDocument();
  expect(within(row).queryByRole("button", { name: "Edit cpu-hot" })).not.toBeInTheDocument();
});

it("creates a rule with the chosen targets", async () => {
  const sent: unknown[] = [];
  server.use(
    rules([]),
    http.get(apiUrl("/notification-targets"), () =>
      HttpResponse.json({
        revision: 1,
        items: [
          {
            id: "t1",
            name: "ops-mail",
            revision: 1,
            enabled: true,
            target: {
              kind: "webhook",
              url: "https://h/…",
              format: "json",
              header_names: [],
              has_token: false,
            },
          },
        ],
      }),
    ),
    http.post(apiUrl("/alert-rules"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(RULE, { status: 201 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<AlertsPage />, { path: "/alerts", url: "/alerts?tab=rules" });
  expect(await screen.findByText("No alert rules.")).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Create rule" }));
  const dialog = await screen.findByRole("dialog", { name: "Create alert rule" });
  await user.type(within(dialog).getByLabelText("Name"), "pool-full");
  await user.selectOptions(within(dialog).getByLabelText("Condition"), "pool_usage");
  const percent = within(dialog).getByLabelText("Used above (%)");
  await user.clear(percent);
  await user.type(percent, "90");
  // 대상을 고르지 않으면 보내지 않는다.
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Choose 1 to 16 notification targets.")).toBeInTheDocument();
  await user.click(within(dialog).getByRole("checkbox", { name: "ops-mail" }));
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Alert rule pool-full created.")).toBeInTheDocument();
  expect(sent).toEqual([
    {
      name: "pool-full",
      spec: {
        enabled: true,
        condition: { kind: "pool_usage", percent: 90 },
        for_secs: 300,
        severity: "warning",
        targets: ["ops-mail"],
        repeat_secs: 14_400,
        send_resolved: true,
      },
    },
  ]);
});

it("edits and deletes a rule with the revision it read", async () => {
  const puts: { ifMatch: string | null; body: unknown }[] = [];
  const deletes: (string | null)[] = [];
  server.use(
    rules(),
    http.get(apiUrl("/notification-targets"), () => HttpResponse.json({ revision: 1, items: [] })),
    http.put(apiUrl("/alert-rules/:key"), async ({ request }) => {
      puts.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json(RULE);
    }),
    http.delete(apiUrl("/alert-rules/:key"), ({ request }) => {
      deletes.push(request.headers.get("If-Match"));
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<AlertsPage />, { path: "/alerts", url: "/alerts?tab=rules" });
  await user.click(await screen.findByRole("button", { name: "Edit cpu-hot" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit alert rule cpu-hot" });
  // 지금 없는 대상도 골라 둔 채로 보인다.
  expect(within(dialog).getByRole("checkbox", { name: "ops-mail" })).toBeChecked();
  const value = within(dialog).getByLabelText("Threshold");
  await user.clear(value);
  await user.type(value, "95");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Alert rule cpu-hot saved.")).toBeInTheDocument();
  expect(puts).toEqual([
    {
      ifMatch: "7",
      body: {
        labels: {},
        spec: { ...RULE.spec, condition: { ...RULE.spec.condition, value: 95 } },
      },
    },
  ]);

  await user.click(screen.getByRole("button", { name: "Delete cpu-hot" }));
  const confirm = await screen.findByRole("alertdialog", { name: "Delete alert rule cpu-hot?" });
  await user.click(within(confirm).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Alert rule cpu-hot deleted.")).toBeInTheDocument();
  expect(deletes).toEqual(["7"]);
});

it("lists sent notifications for the chosen period", async () => {
  const since: number[] = [];
  server.use(
    http.get(apiUrl("/alerts/history"), ({ request }) => {
      since.push(Number(new URL(request.url).searchParams.get("since")));
      return HttpResponse.json({
        records: [
          {
            at: 1_800_000_000_000,
            node: "node-a",
            rule: "cpu-hot",
            subject: "node-a",
            state: "firing",
            target: "ops-mail",
            ok: true,
          },
          {
            at: 1_799_999_000_000,
            node: "node-a",
            state: "test",
            target: "phone",
            ok: false,
            error: "connection refused",
          },
        ],
        unreachable: [],
        unsupported: [],
        truncated: true,
      });
    }),
  );
  const user = userEvent.setup();
  renderPage(<AlertsPage />, { path: "/alerts", url: "/alerts?tab=history" });
  const table = await screen.findByRole("table", { name: "Sent notifications" });
  await within(table).findByText("connection refused");
  const rows = within(table).getAllByRole("row");
  expect(rows[1]).toHaveTextContent("Firingcpu-hotnode-aops-mailnode-aSent");
  expect(rows[2]).toHaveTextContent("Test——phonenode-aFailedconnection refused");
  expect(screen.getByText("Only the newest notifications are shown.")).toBeInTheDocument();
  await user.selectOptions(screen.getByLabelText("Period"), "week");
  await vi.waitFor(() => expect(since).toHaveLength(2));
  // 24시간 전부터, 그다음 7일 전부터
  expect(since[0]! - since[1]!).toBeGreaterThan(5.9 * 86_400_000);
});
