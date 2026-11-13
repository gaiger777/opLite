import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Node } from "@/api/types";
import { PlacementRulesPage } from "@/placement/PlacementRulesPage";
import { NODE, placementRule } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

function node(id: string, name: string): Node {
  return { ...NODE, id, name };
}

const NODES = [node("n1", "node-a"), node("n2", "node-b")];
const VIEWER = { ...ADMIN, name: "viewer", role: "viewer" as const };

const spread = placementRule("spread", 1);
const zone = placementRule("zone", 2, {
  kind: "node-affinity",
  vms: undefined,
  vm_tag: "app=db",
  node_tag: "zone=rack-a",
  ha_may_violate: true,
});
const prefer = placementRule("prefer", 3, {
  kind: "soft-node-affinity",
  vms: ["web1"],
  nodes: ["node-b"],
  weight: 20,
  enabled: false,
});

function cells(table: HTMLElement, name: string) {
  const headers = within(table).getAllByRole("columnheader");
  const row = within(table).getByText(name).closest("tr")!;
  return (header: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === header)];
}

it("lists rules with their kind, VMs, nodes, weight and state", async () => {
  mockLists({ "placement-rule": [zone, spread, prefer], node: NODES });
  renderPage(<PlacementRulesPage />);
  const table = await screen.findByRole("table", { name: "Placement rules" });
  await within(table).findByText("spread");
  const s = cells(table, "spread");
  expect(s("Rule")).toHaveTextContent("Different nodes (hard)");
  expect(s("VMs")).toHaveTextContent("web1, web2");
  expect(s("Nodes")).toHaveTextContent("—");
  expect(s("Weight")).toHaveTextContent("—");
  expect(s("State")).toHaveTextContent("On");
  const z = cells(table, "zone");
  expect(z("VMs")).toHaveTextContent("app=db");
  expect(z("Nodes")).toHaveTextContent("zone=rack-a");
  expect(z("State")).toHaveTextContent("OnHA may break");
  const p = cells(table, "prefer");
  expect(p("Rule")).toHaveTextContent("On these nodes if possible");
  expect(p("Weight")).toHaveTextContent("20");
  expect(p("State")).toHaveTextContent("Off");
});

it("creates a node rule chosen by node label and sends only what the kind takes", async () => {
  mockLists({ "placement-rule": [spread], node: NODES });
  let body: unknown;
  server.use(
    http.post(apiUrl("/placement-rules"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(zone, { status: 201 });
    }),
  );
  renderPage(<PlacementRulesPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create placement rule" });
  await user.type(within(dialog).getByLabelText("Name"), "zone");
  // VM끼리 규칙은 VM이 둘 이상이고 노드를 고르지 않는다.
  await user.type(within(dialog).getByLabelText("VM names"), "db");
  expect(within(dialog).queryByLabelText("Choose nodes")).not.toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Name at least two VMs.")).toBeVisible();
  await user.selectOptions(within(dialog).getByLabelText("Rule"), "node-affinity");
  await user.selectOptions(within(dialog).getByLabelText("Choose nodes"), "tag");
  await user.type(within(dialog).getByLabelText("Node label"), "rack-a");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Use KEY=VALUE, such as zone=rack-a.")).toBeVisible();
  await user.clear(within(dialog).getByLabelText("Node label"));
  await user.type(within(dialog).getByLabelText("Node label"), "zone=rack-a");
  // hard 규칙에는 벌점이 없고 HA가 어길 수 있는지를 고른다.
  expect(within(dialog).queryByLabelText("Weight")).not.toBeInTheDocument();
  await user.click(within(dialog).getByLabelText("HA recovery may break this rule"));
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Placement rule zone created.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "zone",
    spec: {
      kind: "node-affinity",
      vms: ["db"],
      node_tag: "zone=rack-a",
      ha_may_violate: true,
      enabled: true,
    },
  });
});

it("shows a contradiction the server finds", async () => {
  mockLists({ "placement-rule": [spread], node: NODES });
  server.use(
    http.post(apiUrl("/placement-rules"), () =>
      HttpResponse.json(
        {
          code: "rule_conflict",
          message: 'placement rule "together" contradicts other rules: spread',
          details: { rules: ["spread"] },
        },
        { status: 422 },
      ),
    ),
  );
  renderPage(<PlacementRulesPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create placement rule" });
  await user.type(within(dialog).getByLabelText("Name"), "together");
  await user.selectOptions(within(dialog).getByLabelText("Rule"), "affinity");
  await user.type(within(dialog).getByLabelText("VM names"), "web1, web2");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    'The rule contradicts other placement rules: placement rule "together" contradicts other rules: spread',
  );
});

it("turns a soft rule back on and changes its weight", async () => {
  mockLists({ "placement-rule": [prefer], node: NODES });
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.put(apiUrl(`/placement-rules/${prefer.id}`), async ({ request }) => {
      sent.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json(prefer);
    }),
  );
  renderPage(<PlacementRulesPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Edit placement rule prefer" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit prefer" });
  expect(within(dialog).getByLabelText("node-b")).toBeChecked();
  expect(within(dialog).getByRole("button", { name: "Save" })).toBeDisabled();
  const weight = within(dialog).getByLabelText("Weight");
  await user.clear(weight);
  await user.type(weight, "101");
  await user.click(within(dialog).getByLabelText("On (an off rule is ignored)"));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText("From 1 to 100.")).toBeVisible();
  await user.clear(weight);
  await user.type(weight, "80");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Placement rule prefer saved.")).toBeInTheDocument();
  expect(sent).toEqual([
    {
      ifMatch: "1",
      body: {
        labels: {},
        spec: {
          kind: "soft-node-affinity",
          vms: ["web1"],
          nodes: ["node-b"],
          weight: 80,
          enabled: true,
        },
      },
    },
  ]);
});

it("lets only admins change rules", async () => {
  mockLists({ "placement-rule": [spread], node: NODES });
  renderPage(<PlacementRulesPage />, { as: VIEWER });
  const table = await screen.findByRole("table", { name: "Placement rules" });
  await within(table).findByText("spread");
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Delete placement rule spread" }),
  ).not.toBeInTheDocument();
});
