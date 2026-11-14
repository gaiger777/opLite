import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { HaCheck, Node } from "@/api/types";
import { HaChecksPage } from "@/ha-checks/HaChecksPage";
import { haCheck, NET_NODE, NODE, pool } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const NODES: Node[] = [
  NET_NODE,
  { ...NODE, id: "n2", name: "node-b" },
  { ...NODE, id: "w", name: "wit", spec: { ...NODE.spec, witness: true } },
];
const POOLS = [
  pool("local"),
  pool("nas", { backend: { kind: "nfs", server: "nas", export: "/x" } }),
];
const OPERATOR = { ...ADMIN, name: "ops", role: "operator" as const };

const stor = haCheck("stor", 1, {
  target: { kind: "pool", name: "nas" },
  grace_secs: 15,
  nodes: { "node-b": { kind: "pool", name: "nas" } },
});
const lan = haCheck("lan", 2, {
  role: "vm",
  target: { kind: "ping", addresses: ["10.0.0.1", "10.0.0.2"], via: "bond0", need: 2 },
  on_fail: "fence",
  enabled: false,
});

/** 검사 목록. 바꾼 뒤 다시 읽는 수를 센다. */
function checks(...list: HaCheck[]) {
  const reads = { n: 0 };
  server.use(
    http.get(apiUrl("/ha-checks"), () => {
      reads.n += 1;
      return HttpResponse.json({ revision: 1, items: list });
    }),
  );
  return reads;
}

function cells(table: HTMLElement, name: string) {
  const headers = within(table).getAllByRole("columnheader");
  const row = within(table).getByText(name).closest("tr")!;
  return (header: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === header)];
}

it("lists checks with their target, reaction and grace", async () => {
  mockLists({ node: NODES, "storage-pool": POOLS });
  checks(stor, lan);
  renderPage(<HaChecksPage />);
  const table = await screen.findByRole("table", { name: "HA checks" });
  await within(table).findByText("stor");
  const s = cells(table, "stor");
  expect(s("Role")).toHaveTextContent("Storage");
  expect(s("Target")).toHaveTextContent("pool:nas");
  // 대응을 정하지 않으면 역할의 기본이다.
  expect(s("On failure")).toHaveTextContent("Restart elsewhere(role default)");
  expect(s("Grace")).toHaveTextContent("15 s");
  expect(s("Node overrides")).toHaveTextContent("node-b=pool:nas");
  expect(s("State")).toHaveTextContent("On");
  const l = cells(table, "lan");
  expect(l("Target")).toHaveTextContent("ping:10.0.0.1,10.0.0.2@bond0 need=2");
  expect(l("On failure")).toHaveTextContent("Power off the node");
  // 서버가 쓰지 않은 기본 grace
  expect(l("Grace")).toHaveTextContent("10 s");
  expect(l("Node overrides")).toHaveTextContent("—");
  expect(l("State")).toHaveTextContent("Off");
  expect(
    within(table)
      .getAllByRole("row")
      .map((r) => r.firstChild?.textContent),
  ).toEqual(["Name", "lan", "stor"]);
});

it("creates a check with a node override and reads the list again", async () => {
  mockLists({ node: NODES, "storage-pool": POOLS });
  const reads = checks(stor);
  let body: unknown;
  server.use(
    http.post(apiUrl("/ha-checks"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(haCheck("uplink", 3), { status: 201 });
    }),
  );
  renderPage(<HaChecksPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create HA check" });
  await user.type(within(dialog).getByLabelText("Name"), "cluster-link-9");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(
    within(dialog).getByText(
      "Names starting with cluster-link- are for the nodes' own cluster link checks.",
    ),
  ).toBeVisible();
  expect(
    within(dialog).getByText("Use an interface name of 1–15 characters without /, : or spaces."),
  ).toBeVisible();
  await user.clear(within(dialog).getByLabelText("Name"));
  await user.type(within(dialog).getByLabelText("Name"), "uplink");
  await user.selectOptions(within(dialog).getByLabelText("Role"), "vm");
  // VM 네트워크 검사는 풀을 보지 않는다.
  expect(
    within(within(dialog).getByLabelText("Check")).queryByRole("option", { name: "Pool" }),
  ).not.toBeInTheDocument();
  // 인터페이스 후보는 노드들이 보고한 이름이다.
  const iface = within(dialog).getByLabelText("Interface");
  expect(iface).toHaveAttribute("list", "ha-check-ifaces");
  expect(
    [...document.querySelectorAll("#ha-check-ifaces option")].map((o) => o.getAttribute("value")),
  ).toEqual(["bond0", "eno1", "eno2", "vmbr0"]);
  await user.type(iface, "vmbr0");
  await user.selectOptions(within(dialog).getByLabelText("On failure"), "restart_elsewhere");
  await user.type(within(dialog).getByLabelText("Grace (seconds)"), "30");
  await user.click(within(dialog).getByRole("button", { name: "Add node override" }));
  // 위트니스는 고를 수 없고, 남은 첫 노드가 골라진다.
  const node = within(dialog).getByLabelText("Node of override 1");
  expect(within(node).queryByRole("option", { name: "wit" })).not.toBeInTheDocument();
  expect(node).toHaveValue("node-a");
  await user.selectOptions(node, "node-b");
  await user.type(within(dialog).getByLabelText("Interface on node-b"), "br-vm");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("HA check uplink created.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "uplink",
    spec: {
      role: "vm",
      target: { kind: "link", iface: "vmbr0" },
      on_fail: "restart_elsewhere",
      grace_secs: 30,
      nodes: { "node-b": { kind: "link", iface: "br-vm" } },
      enabled: true,
    },
  });
  // watch 종류가 아니므로 만든 뒤 목록을 다시 읽는다.
  expect(reads.n).toBe(2);
});

it("picks a shared pool for a storage pool check", async () => {
  mockLists({ node: NODES, "storage-pool": POOLS });
  checks();
  let body: unknown;
  server.use(
    http.post(apiUrl("/ha-checks"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(haCheck("nas", 3), { status: 201 });
    }),
  );
  renderPage(<HaChecksPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create HA check" });
  await user.type(within(dialog).getByLabelText("Name"), "nas");
  await user.selectOptions(within(dialog).getByLabelText("Check"), "pool");
  const pools = within(dialog).getByLabelText("Storage pool");
  // 로컬 풀은 고를 수 없다.
  expect(within(pools).queryByRole("option", { name: "local" })).not.toBeInTheDocument();
  // 스토리지 역할은 라이브 마이그레이션을 쓰지 않는다.
  expect(
    within(within(dialog).getByLabelText("On failure")).queryByRole("option", {
      name: "Live migrate",
    }),
  ).not.toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Choose a shared storage pool.")).toBeVisible();
  await user.selectOptions(pools, "nas");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("HA check nas created.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "nas",
    spec: { role: "storage", target: { kind: "pool", name: "nas" }, grace_secs: 10, enabled: true },
  });
});

it("replaces a check with its revision and keeps unchanged checks from saving", async () => {
  mockLists({ node: NODES, "storage-pool": POOLS });
  checks(lan);
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.put(apiUrl(`/ha-checks/${lan.id}`), async ({ request }) => {
      sent.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json(lan);
    }),
  );
  renderPage(<HaChecksPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Edit HA check lan" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit lan" });
  expect(within(dialog).getByLabelText("Addresses")).toHaveValue("10.0.0.1, 10.0.0.2");
  expect(within(dialog).getByLabelText("Need")).toHaveValue("2");
  expect(within(dialog).getByRole("button", { name: "Save" })).toBeDisabled();
  await user.clear(within(dialog).getByLabelText("Need"));
  await user.type(within(dialog).getByLabelText("Need"), "3");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    within(dialog).getByText("Need is a whole number from 1 to the number of addresses."),
  ).toBeVisible();
  await user.clear(within(dialog).getByLabelText("Need"));
  await user.click(within(dialog).getByLabelText("Check on all nodes"));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("HA check lan saved.")).toBeInTheDocument();
  expect(sent).toEqual([
    {
      ifMatch: "1",
      body: {
        labels: {},
        spec: {
          role: "vm",
          target: { kind: "ping", addresses: ["10.0.0.1", "10.0.0.2"], via: "bond0", need: 1 },
          on_fail: "fence",
          grace_secs: 10,
          enabled: true,
        },
      },
    },
  ]);
});

it("says why the cluster cannot take checks yet", async () => {
  mockLists({ node: NODES, "storage-pool": POOLS });
  checks();
  server.use(
    http.post(apiUrl("/ha-checks"), () =>
      HttpResponse.json(
        { code: "cluster_upgrading", message: "node node-b does not support ha-checks yet" },
        { status: 409 },
      ),
    ),
  );
  renderPage(<HaChecksPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create HA check" });
  await user.type(within(dialog).getByLabelText("Name"), "up");
  await user.type(within(dialog).getByLabelText("Interface"), "eth0");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The cluster is being upgraded: node node-b does not support ha-checks yet",
  );
});

it("deletes a check and reads the list again", async () => {
  mockLists({ node: NODES, "storage-pool": POOLS });
  const reads = checks(stor);
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/ha-checks/${stor.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<HaChecksPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Delete HA check stor" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete HA check stor?" });
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("HA check stor deleted.")).toBeInTheDocument();
  expect(deleted).toBe(true);
  expect(reads.n).toBe(2);
});

it("lets only admins change checks", async () => {
  mockLists({ node: NODES, "storage-pool": POOLS });
  checks(stor);
  renderPage(<HaChecksPage />, { as: OPERATOR });
  const table = await screen.findByRole("table", { name: "HA checks" });
  await within(table).findByText("stor");
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Edit HA check stor" })).not.toBeInTheDocument();
});
