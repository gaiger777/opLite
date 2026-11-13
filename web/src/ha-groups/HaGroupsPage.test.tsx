import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Node } from "@/api/types";
import { HaGroupsPage } from "@/ha-groups/HaGroupsPage";
import { haGroup, NODE, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

function node(id: string, name: string, witness = false): Node {
  return { ...NODE, id, name, spec: { ...NODE.spec, witness } };
}

const NODES = [
  node("n1", "node-a"),
  node("n2", "node-b"),
  node("n3", "node-c"),
  node("w", "wit", true),
];
const OPERATOR = { ...ADMIN, name: "ops", role: "operator" as const };

const rack = haGroup("rack", 1, {
  nodes: [
    { node: "node-b", priority: 1 },
    { node: "node-a", priority: 2 },
  ],
  restricted: true,
});
const plain = haGroup("plain", 2, { nodes: [{ node: "node-c", priority: 0 }] });
const db = vm("db", "running", {
  spec: {
    ...vm("db", "running").spec,
    ha: { state: "started", group: "rack", max_restart: 1, max_relocate: 1, priority: 50 },
  },
});

function cells(table: HTMLElement, name: string) {
  const headers = within(table).getAllByRole("columnheader");
  const row = within(table).getByText(name).closest("tr")!;
  return (header: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === header)];
}

it("lists groups with their nodes by priority, options and VMs", async () => {
  mockLists({ "ha-group": [rack, plain], node: NODES, vm: [db, vm("web", "running")] });
  renderPage(<HaGroupsPage />);
  const table = await screen.findByRole("table", { name: "HA groups" });
  await within(table).findByText("rack");
  const row = cells(table, "rack");
  // 우선순위가 높은 노드부터
  expect(row("Nodes and priority")).toHaveTextContent("node-a (2), node-b (1)");
  expect(row("Options")).toHaveTextContent("Restricted");
  expect(await within(row("VMs")!).findByText("db")).toBeInTheDocument();
  expect(cells(table, "plain")("Options")).toHaveTextContent("—");
  expect(cells(table, "plain")("VMs")).toHaveTextContent("—");
  // 이름 순
  expect(
    within(table)
      .getAllByRole("row")
      .map((r) => r.firstChild?.textContent),
  ).toEqual(["Name", "plain", "rack"]);
});

it("creates a group from the chosen nodes and priorities", async () => {
  mockLists({ "ha-group": [rack], node: NODES });
  let body: unknown;
  server.use(
    http.post(apiUrl("/ha-groups"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(haGroup("new", 3), { status: 201 });
    }),
  );
  renderPage(<HaGroupsPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create HA group" });
  // 위트니스는 고를 수 없다.
  expect(within(dialog).queryByLabelText("wit")).not.toBeInTheDocument();
  await user.type(within(dialog).getByLabelText("Name"), "rack");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("An HA group with this name already exists.")).toBeVisible();
  expect(within(dialog).getByText("Choose at least one node.")).toBeVisible();
  await user.clear(within(dialog).getByLabelText("Name"));
  await user.type(within(dialog).getByLabelText("Name"), "new");
  await user.click(within(dialog).getByLabelText("node-b"));
  await user.click(within(dialog).getByLabelText("node-c"));
  const priority = within(dialog).getByLabelText("Priority of node-c");
  await user.clear(priority);
  await user.type(priority, "1001");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Priorities are whole numbers from 0 to 1000.")).toBeVisible();
  await user.clear(priority);
  await user.type(priority, "5");
  await user.click(within(dialog).getByLabelText("Only on the group's nodes"));
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("HA group new created.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "new",
    spec: {
      nodes: [
        { node: "node-c", priority: 5 },
        { node: "node-b", priority: 0 },
      ],
      restricted: true,
      nofailback: false,
    },
  });
});

it("replaces a group with its revision and keeps unchanged groups from saving", async () => {
  mockLists({ "ha-group": [rack], node: NODES });
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.put(apiUrl(`/ha-groups/${rack.id}`), async ({ request }) => {
      sent.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json(rack);
    }),
  );
  renderPage(<HaGroupsPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Edit HA group rack" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit rack" });
  expect(within(dialog).getByLabelText("node-a")).toBeChecked();
  expect(within(dialog).getByLabelText("Priority of node-a")).toHaveValue("2");
  expect(within(dialog).getByRole("button", { name: "Save" })).toBeDisabled();
  await user.click(within(dialog).getByLabelText("No failback"));
  await user.click(within(dialog).getByLabelText("node-b"));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("HA group rack saved.")).toBeInTheDocument();
  expect(sent).toEqual([
    {
      ifMatch: "1",
      body: {
        labels: {},
        spec: { nodes: [{ node: "node-a", priority: 2 }], restricted: true, nofailback: true },
      },
    },
  ]);
});

it("says why a group in use cannot be deleted", async () => {
  mockLists({ "ha-group": [rack], node: NODES, vm: [db] });
  server.use(
    http.delete(apiUrl(`/ha-groups/${rack.id}`), () =>
      HttpResponse.json(
        { code: "group_in_use", message: 'HA group "rack" is used by VM "db"' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<HaGroupsPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Delete HA group rack" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete HA group rack?" });
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    'The HA group is in use. Take the VMs out of the group first: HA group "rack" is used by VM "db"',
  );
});

it("lets only admins change groups", async () => {
  mockLists({ "ha-group": [rack], node: NODES });
  renderPage(<HaGroupsPage />, { as: OPERATOR });
  const table = await screen.findByRole("table", { name: "HA groups" });
  await within(table).findByText("rack");
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Edit HA group rack" })).not.toBeInTheDocument();
});
