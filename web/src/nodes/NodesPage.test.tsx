import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { applyEvent, listKey, type List } from "@/api/live";
import type { Node } from "@/api/types";
import { NodesPage } from "@/nodes/NodesPage";
import { NET_NODE, NODE, poolState, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const DOWN = {
  ...NODE,
  id: "n2",
  name: "node-b",
  revision: 7,
  status: {
    ...NODE.status,
    online: false,
    bridges: [{ name: "vmbr0", vlan_filtering: true }],
    arch: "aarch64" as const,
    underlay: "fd00::2",
  },
};

function cluster() {
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
  );
}

it("lists nodes with their state, capacity and VMs", async () => {
  cluster();
  mockLists({ node: [NODE, DOWN], vm: [vm("web", "running"), vm("db", "stopped")] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const a = (await within(table).findByText("node-a")).closest("tr")!;
  expect(a).toHaveTextContent("Online");
  expect(a).toHaveTextContent("16 GiB");
  expect(await within(a).findByText("(this node)")).toBeInTheDocument();
  // 두 VM 모두 node-a에 고정돼 있다.
  const vmsColumn = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "VMs");
  expect(within(a).getAllByRole("cell")[vmsColumn]).toHaveTextContent("2");
  const b = within(table).getByText("node-b").closest("tr")!;
  expect(b).toHaveTextContent("Offline");
  expect(b).toHaveTextContent("vmbr0");
  const archColumn = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Architecture");
  expect(within(b).getAllByRole("cell")[archColumn]).toHaveTextContent("aarch64");
  // 옛 버전으로 등록한 노드는 아키텍처를 모른다.
  expect(within(a).getAllByRole("cell")[archColumn]).toHaveTextContent("—");
  // VXLAN 패킷을 주고받는 주소. 정하지 못한 노드는 없다.
  const underlayColumn = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Underlay address");
  expect(within(b).getAllByRole("cell")[underlayColumn]).toHaveTextContent("fd00::2");
  expect(within(a).getAllByRole("cell")[underlayColumn]).toHaveTextContent("—");
});

it("marks witness nodes", async () => {
  cluster();
  const witness = { ...NODE, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  mockLists({ node: [NODE, witness] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const w = (await within(table).findByText("node-w")).closest("tr")!;
  expect(within(w).getByText("Witness")).toBeInTheDocument();
  const a = within(table).getByText("node-a").closest("tr")!;
  expect(within(a).queryByText("Witness")).not.toBeInTheDocument();
});

it("shows how each node fences", async () => {
  cluster();
  const fenced = {
    ...NODE,
    status: {
      ...NODE.status,
      ha: { fencing: "watchdog" as const, recover_after_secs: 22, fence_after_secs: 10 },
    },
  };
  // 이웃이 전원을 읽지 못한 노드는 경고 배지가 붙는다.
  const unreachable = {
    ...DOWN,
    status: {
      ...DOWN.status,
      fence_check: { at: 1_800_000_000, by: "node-a", error: "ipmi: connection timed out" },
    },
  };
  mockLists({ node: [fenced, unreachable] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const a = (await within(table).findByText("node-a")).closest("tr")!;
  const column = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Fencing");
  expect(within(a).getAllByRole("cell")[column]).toHaveTextContent("WatchdogRecovers after 22 s");
  expect(within(a).queryByText("Power fence unreachable")).not.toBeInTheDocument();
  // 펜싱이 꺼진 노드는 `ha`가 없다.
  const b = within(table).getByText("node-b").closest("tr")!;
  expect(within(b).getAllByRole("cell")[column]).toHaveTextContent("—");
  expect(within(b).getByText("Power fence unreachable")).toHaveAttribute(
    "title",
    "ipmi: connection timed out",
  );
});

it("shows the health checks of each node and opens its interfaces", async () => {
  cluster();
  const degraded = {
    ...DOWN,
    status: { ...DOWN.status, checks: [{ name: "lan", state: "degraded" as const }] },
  };
  const witness = { ...NODE, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  mockLists({ node: [NET_NODE, degraded, witness] });
  renderPage(<NodesPage />, { as: { ...ADMIN, role: "viewer" } });
  const table = await screen.findByRole("table", { name: "Nodes" });
  const column = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Health checks");
  const cell = (name: string) =>
    within(within(table).getByText(name).closest("tr")!).getAllByRole("cell")[column]!;
  await within(table).findByText("node-a");
  // 실패한 검사 수, 제목에 실패·degraded 검사의 이름과 오류
  expect(within(cell("node-a")).getByText("1 failing")).toHaveAttribute(
    "title",
    "stor: nfs pool is not mounted\nlan: bond0: eno2 down",
  );
  expect(cell("node-b")).toHaveTextContent("Degraded");
  // 위트니스는 검사하지 않는다.
  expect(cell("node-w")).toHaveTextContent("—");
  // 읽기만 하는 사람도 연다.
  await userEvent.click(
    within(cell("node-a")).getByRole("button", {
      name: "Network interfaces and health checks of node-a: 1 failing",
    }),
  );
  const dialog = await screen.findByRole("dialog", { name: "Network interfaces of node-a" });
  const interfaces = within(dialog).getByRole("table", { name: "Interfaces" });
  const rows = within(interfaces).getAllByRole("row");
  const row = (name: string) => rows.find((r) => r.firstChild?.textContent === name)!;
  const bond = row("bond0");
  expect(bond).toHaveTextContent("Bond802.3ad");
  expect(bond).toHaveTextContent("10 Gb/s");
  expect(bond).toHaveTextContent("vmbr0");
  expect(within(bond).getByText("Degraded")).toBeInTheDocument();
  // 본딩 포트는 아래 줄에 보이고, 끊긴 포트는 빨간 글자와 상태 문구다.
  const port = rows[rows.indexOf(bond) + 2]!;
  expect(port).toHaveTextContent("Port of bond0:eno2");
  expect(within(port).getByText("Down (no link)")).toHaveClass("text-destructive");
  expect(within(rows[rows.indexOf(bond) + 1]!).getByText("Up")).not.toHaveClass("text-destructive");
  expect(row("vmbr0")).toHaveTextContent("10.0.0.1/24");
  const checks = within(dialog).getByRole("table", { name: "Health checks" });
  // 실패가 시작된 때(노드 시계)와 오류
  expect(within(checks).getByText("stor").closest("tr")).toHaveTextContent(
    /^storFailing.*2026.*nfs pool is not mounted$/,
  );
  expect(within(checks).getByText("cluster-link-1").closest("tr")).toHaveTextContent(
    "cluster-link-1OK—",
  );
});

it("does not mark bond and port states it does not know as failures", async () => {
  cluster();
  const bond0 = NET_NODE.status.interfaces![0]!;
  const unknown = {
    ...NET_NODE,
    status: {
      ...NET_NODE.status,
      interfaces: [
        {
          ...bond0,
          bond: {
            ...bond0.bond!,
            state: "unknown" as const,
            ports: [{ name: "eno1", state: "unknown" as const }],
          },
        },
      ],
    },
  };
  mockLists({ node: [unknown] });
  renderPage(<NodesPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await user.click(await screen.findByRole("menuitem", { name: "Network interfaces…" }));
  const dialog = await screen.findByRole("dialog", { name: "Network interfaces of node-a" });
  const rows = within(within(dialog).getByRole("table", { name: "Interfaces" })).getAllByRole(
    "row",
  );
  const bond = rows.find((r) => r.firstChild?.textContent === "bond0")!;
  expect(within(bond).getByText("Unknown")).not.toHaveClass("text-destructive");
  const port = rows[rows.indexOf(bond) + 1]!;
  expect(within(port).getByText("Unknown")).not.toHaveClass("text-destructive");
});

it("opens the interfaces of a node from its menu", async () => {
  cluster();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, interfaces: [] } }] });
  renderPage(<NodesPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await user.click(await screen.findByRole("menuitem", { name: "Network interfaces…" }));
  const dialog = await screen.findByRole("dialog", { name: "Network interfaces of node-a" });
  expect(
    within(dialog).getByText(
      "The node reports no interfaces (witness nodes and macOS nodes do not).",
    ),
  ).toBeInTheDocument();
  expect(within(dialog).getByText("The node reports no health checks.")).toBeInTheDocument();
});

it("shows the state of each pool on the node", async () => {
  cluster();
  const pools = {
    nas: poolState("nas", { ready: false, error: "mount.nfs: access denied by server" }),
    local: poolState("local"),
  };
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, pools } }, DOWN] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const poolsColumn = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Storage pools");
  const a = (await within(table).findByText("node-a")).closest("tr")!;
  // 이름 순으로 보인다.
  expect(within(a).getAllByRole("cell")[poolsColumn]).toHaveTextContent(
    "local: Readynas: Not readymount.nfs: access denied by server",
  );
  const b = within(table).getByText("node-b").closest("tr")!;
  expect(within(b).getAllByRole("cell")[poolsColumn]).toHaveTextContent("—");
});

it("turns maintenance on and off with the revision it read", async () => {
  cluster();
  mockLists({ node: [DOWN] });
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.put(apiUrl("/nodes/n2"), async ({ request }) => {
      const body = (await request.json()) as { spec: typeof DOWN.spec };
      sent.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({ ...DOWN, revision: DOWN.revision + sent.length, spec: body.spec });
    }),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Enter maintenance" }));
  expect(await screen.findByText("node-b is in maintenance.")).toBeInTheDocument();
  // 응답을 바로 목록에 반영하므로 watch 이벤트를 기다리지 않고 다시 바꿀 수 있다.
  await userEvent.click(screen.getByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "End maintenance" }));
  expect(await screen.findByText("node-b is out of maintenance.")).toBeInTheDocument();
  expect(sent).toEqual([
    { ifMatch: "7", body: { labels: {}, spec: { address: "10.0.0.1", maintenance: true } } },
    { ifMatch: "8", body: { labels: {}, spec: { address: "10.0.0.1", maintenance: false } } },
  ]);
});

it("says why maintenance could not change", async () => {
  cluster();
  mockLists({ node: [DOWN] });
  server.use(
    http.put(apiUrl("/nodes/n2"), () =>
      HttpResponse.json({ code: "revision_mismatch", message: "stale" }, { status: 412 }),
    ),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Enter maintenance" }));
  expect(
    await screen.findByText("node-b: Someone else changed it first. Reload and try again."),
  ).toBeInTheDocument();
});

it("does not remove this node even when it shows offline", async () => {
  cluster();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, online: false } }] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  const remove = await screen.findByRole("menuitem", { name: "Remove from cluster" });
  expect(remove).toHaveAttribute("aria-disabled", "true");
  expect(remove).toHaveAccessibleDescription("You are connected through this node.");
});

it("removes only offline nodes, after typing the name", async () => {
  cluster();
  mockLists({ node: [NODE, DOWN] });
  let removed = false;
  server.use(
    http.delete(apiUrl("/nodes/n2"), () => {
      removed = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  expect(await screen.findByRole("menuitem", { name: "Remove from cluster" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await userEvent.keyboard("{Escape}");

  await userEvent.click(screen.getByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Remove from cluster" }));
  const dialog = await screen.findByRole("alertdialog");
  const confirm = within(dialog).getByRole("button", { name: "Remove from cluster" });
  expect(confirm).toBeDisabled();
  await userEvent.type(within(dialog).getByLabelText("Type node-b to confirm"), "node-b");
  await userEvent.click(confirm);
  expect(await screen.findByText("Removed node-b from the cluster.")).toBeInTheDocument();
  expect(removed).toBe(true);
});

it("tells how to remove a live node", async () => {
  cluster();
  mockLists({ node: [NODE] });
  renderPage(<NodesPage />);
  expect(await screen.findByText(/orbivisorctl cluster leave/)).toBeInTheDocument();
});

it("hides node changes from operators", async () => {
  cluster();
  mockLists({ node: [NODE] });
  renderPage(<NodesPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("node-a")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Actions for node-a" })).not.toBeInTheDocument();
  // 바꿀 수 없는 사람에게는 유지보수·제거 안내도 보이지 않는다.
  expect(screen.queryByText(/orbivisorctl cluster leave/)).not.toBeInTheDocument();
  expect(screen.queryByText(/No new VMs are placed/)).not.toBeInTheDocument();
});

it("does not count VMs it could not read", async () => {
  cluster();
  mockLists({ node: [NODE] });
  server.use(
    http.get(apiUrl("/vms"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const a = (await within(table).findByText("node-a")).closest("tr")!;
  const vmsColumn = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "VMs");
  await vi.waitFor(() => expect(within(a).getAllByRole("cell")[vmsColumn]).toHaveTextContent("—"));
});

/** 업데이트를 확인하는 노드. 기본은 2026-11-05 확인, 패키지 둘(하나는 새 의존성). */
function updatable(extra: Partial<(typeof NODE)["status"]["updates"] & object> = {}) {
  return {
    ...NODE,
    status: {
      ...NODE.status,
      version: "0.9.0",
      updates: {
        checked_at: 1_793_000_000,
        packages: [
          { name: "orbivisor", current: "0.9.0", candidate: "0.10.0" },
          { name: "libfoo1", current: null, candidate: "1.2-3" },
        ],
        error: null,
        ...extra,
      },
    },
  };
}

it("shows each node's version and how many updates it has", async () => {
  cluster();
  const failing = {
    ...DOWN,
    status: { ...DOWN.status, updates: { checked_at: null, packages: [], error: "apt-get: boom" } },
  };
  const witness = { ...NODE, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  mockLists({ node: [updatable(), failing, witness] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const headers = within(table).getAllByRole("columnheader");
  const version = headers.findIndex((h) => h.textContent === "Version");
  const updates = headers.findIndex((h) => h.textContent === "Updates");
  // 버전은 주소 다음이다.
  expect(headers[version - 1]).toHaveTextContent("API address");
  const a = (await within(table).findByText("node-a")).closest("tr")!;
  expect(within(a).getAllByRole("cell")[version]).toHaveTextContent("0.9.0");
  expect(within(a).getAllByRole("cell")[updates]).toHaveTextContent("2");
  const b = within(table).getByText("node-b").closest("tr")!;
  expect(within(b).getAllByRole("cell")[version]).toHaveTextContent("—");
  expect(within(b).getAllByRole("cell")[updates]).toHaveTextContent("Check failed");
  // 업데이트를 확인하지 않는 노드(위트니스 등)
  const w = within(table).getByText("node-w").closest("tr")!;
  expect(within(w).getAllByRole("cell")[updates]).toHaveTextContent("—");
  expect(within(w).queryByRole("button", { name: /^Updates for node-w/ })).not.toBeInTheDocument();
  // 배지 버튼의 이름에 확인 상태가 들어간다.
  expect(within(a).getByRole("button", { name: "Updates for node-a: 2 available" })).toBeVisible();
  expect(within(b).getByRole("button", { name: "Updates for node-b: check failed" })).toBeVisible();
});

it("names the badge of a node that has not checked yet or has more updates", async () => {
  cluster();
  const more = {
    ...updatable({ truncated: true }),
    id: "n2",
    name: "node-b",
  };
  mockLists({ node: [updatable({ checked_at: null, packages: [] }), more] });
  renderPage(<NodesPage />);
  expect(
    await screen.findByRole("button", { name: "Updates for node-a: not checked yet" }),
  ).toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "Updates for node-b: 2+ available" }),
  ).toBeInTheDocument();
});

it("lists the updates of a node", async () => {
  cluster();
  mockLists({ node: [updatable({ truncated: true })] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  expect(dialog).toHaveTextContent("Last checked");
  const rows = within(within(dialog).getByRole("table")).getAllByRole("row");
  expect(rows.map((r) => r.textContent)).toEqual([
    "PackageInstalledCandidate",
    "orbivisor0.9.00.10.0",
    "libfoo1(new)1.2-3",
  ]);
  expect(dialog).toHaveTextContent("Only the first 2 packages are shown.");
});

it("shows why the last check failed", async () => {
  cluster();
  mockLists({ node: [updatable({ packages: [], error: "apt-get update: E: no network" })] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  expect(dialog).toHaveTextContent("The last check failed:apt-get update: E: no network");
  expect(within(dialog).queryByRole("table")).not.toBeInTheDocument();
});

it("says when a node has not checked yet or has nothing to update", async () => {
  cluster();
  mockLists({ node: [updatable({ checked_at: null, packages: [] })] });
  const { unmount } = renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  expect(await screen.findByRole("dialog")).toHaveTextContent("Not checked yet.");
  unmount();

  mockLists({ node: [updatable({ packages: [] })] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  expect(await screen.findByRole("dialog")).toHaveTextContent("No updates available.");
});

it("checks again and upgrades from the row menu", async () => {
  cluster();
  mockLists({ node: [updatable()] });
  const posted: string[] = [];
  server.use(
    http.post(apiUrl("/nodes/n1/updates/refresh"), () => {
      posted.push("refresh");
      return HttpResponse.json(task("t1", "node-update-check", "n1"), { status: 202 });
    }),
    http.post(apiUrl("/nodes/n1/upgrade"), () => {
      posted.push("upgrade");
      return HttpResponse.json(task("t2", "node-upgrade", "n1"), { status: 202 });
    }),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Updates…" }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Check again" }));
  expect(await screen.findByText("Check updates on node-a")).toBeInTheDocument();

  await userEvent.click(within(dialog).getByRole("button", { name: "Upgrade" }));
  const confirm = await screen.findByRole("alertdialog", { name: "Upgrade node-a?" });
  expect(confirm).toHaveTextContent(
    "VMs keep running, but orbivisord restarts briefly. Upgrade other nodes after this upgrade finishes.",
  );
  await userEvent.click(within(confirm).getByRole("button", { name: "Upgrade" }));
  expect(await screen.findByText("Upgrade node-a")).toBeInTheDocument();
  expect(posted).toEqual(["refresh", "upgrade"]);
});

it("says why the upgrade did not start", async () => {
  cluster();
  mockLists({ node: [updatable()] });
  server.use(
    http.post(apiUrl("/nodes/n1/upgrade"), () =>
      HttpResponse.json(
        { code: "upgrade_in_progress", message: 'node "node-b" is still being upgraded' },
        { status: 409 },
      ),
    ),
    http.post(apiUrl("/nodes/n1/updates/refresh"), () =>
      HttpResponse.json({ code: "node_offline", message: "offline" }, { status: 409 }),
    ),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Check again" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("The node is offline.");

  await userEvent.click(within(dialog).getByRole("button", { name: "Upgrade" }));
  const confirm = await screen.findByRole("alertdialog");
  await userEvent.click(within(confirm).getByRole("button", { name: "Upgrade" }));
  expect(await within(confirm).findByRole("alert")).toHaveTextContent(
    'An upgrade is in progress: node "node-b" is still being upgraded',
  );
});

it("waits for a running upgrade before checking or upgrading again", async () => {
  cluster();
  const running = {
    ...task("t2", "node-upgrade", "n1"),
    status: { state: "running" as const, error: null },
  };
  mockLists({ node: [updatable()], task: [running] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  expect(await within(dialog).findByRole("button", { name: "Upgrading…" })).toBeDisabled();
  expect(within(dialog).getByRole("button", { name: "Check again" })).toBeDisabled();
  expect(within(dialog).queryByRole("button", { name: "Upgrade" })).not.toBeInTheDocument();
});

it("waits for a running check before checking or upgrading", async () => {
  cluster();
  const running = {
    ...task("t1", "node-update-check", "n1"),
    status: { state: "running" as const, error: null },
  };
  mockLists({ node: [updatable()], task: [running] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  expect(await within(dialog).findByRole("button", { name: "Checking…" })).toBeDisabled();
  // 확인이 apt를 쓰는 동안은 업그레이드도 하지 않는다.
  expect(within(dialog).getByRole("button", { name: "Upgrade" })).toBeDisabled();
});

it("has no update menu for nodes that do not check for updates", async () => {
  cluster();
  mockLists({ node: [NODE] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  expect(await screen.findByRole("menuitem", { name: "Enter maintenance" })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Updates…" })).not.toBeInTheDocument();
});

it("lets viewers read the updates but not act on them", async () => {
  cluster();
  mockLists({ node: [updatable()] });
  renderPage(<NodesPage />, { as: { ...ADMIN, role: "viewer" } });
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  expect(within(dialog).getByText("orbivisor")).toBeInTheDocument();
  expect(within(dialog).queryByRole("button", { name: "Check again" })).not.toBeInTheDocument();
  expect(within(dialog).queryByRole("button", { name: "Upgrade" })).not.toBeInTheDocument();
});

it("drains a node after saying how many running VMs move", async () => {
  cluster();
  const other = vm("api", "running", {
    spec: { ...vm("api", "running").spec, node: "n2" },
  });
  mockLists({
    node: [NODE, { ...DOWN, status: { ...DOWN.status, online: true } }],
    vm: [vm("web", "running"), vm("db", "running"), vm("old", "stopped"), other],
  });
  let posted = 0;
  server.use(
    http.post(apiUrl("/nodes/n1/drain"), () => {
      posted += 1;
      return HttpResponse.json(task("t1", "node-drain", "n1"), { status: 202 });
    }),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Drain node…" }));
  const confirm = await screen.findByRole("alertdialog", { name: "Drain node-a?" });
  // node-a에서 켜 둔 VM 둘만 센다(꺼진 VM과 다른 노드의 VM은 빼고).
  await vi.waitFor(() => expect(confirm).toHaveTextContent("2 running VMs will be moved"));
  expect(confirm).toHaveTextContent(
    "VMs with disks on node-local pools are moved by copying their disks to a node with a pool of the same name (VMs with snapshots cannot be moved).",
  );
  await userEvent.click(within(confirm).getByRole("button", { name: "Drain" }));
  expect(await screen.findByText("Drain node-a")).toBeInTheDocument();
  expect(posted).toBe(1);
});

it("says why the drain did not start", async () => {
  cluster();
  mockLists({ node: [NODE] });
  server.use(
    http.post(apiUrl("/nodes/n1/drain"), () =>
      HttpResponse.json(
        { code: "node_busy", message: 'node "node-a" is already being drained' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Drain node…" }));
  const confirm = await screen.findByRole("alertdialog");
  await userEvent.click(within(confirm).getByRole("button", { name: "Drain" }));
  expect(await within(confirm).findByRole("alert")).toHaveTextContent(
    "The node is already being drained.",
  );
});

it("drains only online nodes that run VMs and are not being drained", async () => {
  cluster();
  const witness = { ...NODE, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  const draining = {
    ...task("t1", "node-drain", "n1"),
    status: { state: "running" as const, error: null },
  };
  mockLists({ node: [NODE, DOWN, witness], task: [draining] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  const busy = await screen.findByRole("menuitem", { name: "Drain node…" });
  await vi.waitFor(() => expect(busy).toHaveAttribute("aria-disabled", "true"));
  expect(busy).toHaveAccessibleDescription("The node is being drained.");
  await userEvent.keyboard("{Escape}");

  await userEvent.click(screen.getByRole("button", { name: "Actions for node-b" }));
  const offline = await screen.findByRole("menuitem", { name: "Drain node…" });
  expect(offline).toHaveAttribute("aria-disabled", "true");
  expect(offline).toHaveAccessibleDescription("The node is offline.");
  await userEvent.keyboard("{Escape}");

  // 위트니스는 VM을 돌리지 않는다.
  await userEvent.click(screen.getByRole("button", { name: "Actions for node-w" }));
  expect(await screen.findByRole("menuitem", { name: "Enter maintenance" })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Drain node…" })).not.toBeInTheDocument();
});

it("says why the node cannot be drained", async () => {
  cluster();
  mockLists({ node: [NODE] });
  server.use(
    http.post(apiUrl("/nodes/n1/drain"), () =>
      HttpResponse.json(
        {
          code: "node_unavailable",
          message: 'node "node-a" is offline; HA recovers the HA VMs of an offline node',
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Drain node…" }));
  const confirm = await screen.findByRole("alertdialog");
  await userEvent.click(within(confirm).getByRole("button", { name: "Drain" }));
  expect(await within(confirm).findByRole("alert")).toHaveTextContent(
    'This node cannot be drained: node "node-a" is offline; HA recovers the HA VMs of an offline node',
  );
});

it("does not drain a macOS node", async () => {
  cluster();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, hypervisor: "qemu-hvf" } }] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  const item = await screen.findByRole("menuitem", { name: "Drain node…" });
  expect(item).toHaveAttribute("aria-disabled", "true");
  expect(item).toHaveAccessibleDescription("VMs on a macOS node cannot be moved.");
});

it("asks before ending maintenance that stops a running drain", async () => {
  cluster();
  const node = {
    ...DOWN,
    status: { ...DOWN.status, online: true },
    spec: { ...DOWN.spec, maintenance: true },
  };
  const draining = {
    ...task("t1", "node-drain", "n2"),
    status: { state: "running" as const, error: null },
  };
  mockLists({ node: [node], task: [draining] });
  const sent: unknown[] = [];
  server.use(
    http.put(apiUrl("/nodes/n2"), async ({ request }) => {
      const body = (await request.json()) as { spec: typeof DOWN.spec };
      sent.push(body.spec);
      return HttpResponse.json({ ...node, revision: node.revision + 1, spec: body.spec });
    }),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-b" }));
  // Task 목록을 읽은 뒤(비우는 중으로 보인 뒤) 고른다.
  await vi.waitFor(async () =>
    expect(await screen.findByRole("menuitem", { name: "Drain node…" })).toHaveAttribute(
      "aria-disabled",
      "true",
    ),
  );
  await userEvent.click(screen.getByRole("menuitem", { name: "End maintenance" }));
  const confirm = await screen.findByRole("alertdialog", { name: "End maintenance of node-b?" });
  expect(confirm).toHaveTextContent("Ending maintenance stops the running drain.");
  expect(sent).toEqual([]);
  await userEvent.click(within(confirm).getByRole("button", { name: "End maintenance" }));
  await vi.waitFor(() => expect(sent).toEqual([{ address: "10.0.0.1", maintenance: false }]));
  await vi.waitFor(() => expect(confirm).not.toBeInTheDocument());
});

it("edits node labels with the revision of the node when saving", async () => {
  cluster();
  const labeled = { ...DOWN, labels: { zone: "rack-a" } };
  mockLists({ node: [NODE, labeled] });
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.put(apiUrl("/nodes/n2"), async ({ request }) => {
      const body = (await request.json()) as { labels: Record<string, string> };
      sent.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({ ...labeled, revision: 10, labels: body.labels });
    }),
  );
  const { queryClient } = renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  await within(table).findByText("node-b");
  const labelsColumn = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Labels");
  const row = (name: string) => within(table).getByText(name).closest("tr")!;
  expect(within(row("node-b")).getAllByRole("cell")[labelsColumn]).toHaveTextContent("zone=rack-a");
  expect(within(row("node-a")).getAllByRole("cell")[labelsColumn]).toHaveTextContent("—");
  await userEvent.click(screen.getByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Edit labels" }));
  const dialog = await screen.findByRole("dialog", { name: "Labels of node-b" });
  const input = within(dialog).getByLabelText("Labels");
  expect(input).toHaveValue("zone=rack-a");
  expect(within(dialog).getByRole("button", { name: "Save" })).toBeDisabled();
  await userEvent.type(input, ", ssd");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText("Use KEY=VALUE pairs separated by commas.")).toBeVisible();
  await userEvent.type(input, "=true");
  // 대화 상자를 연 사이 노드 status가 바뀌어 revision이 올랐다.
  const bumped = { ...labeled, revision: 9, status: { ...labeled.status, cpus: 16 } };
  act(() => {
    queryClient.setQueryData<List<Node>>(
      listKey("node"),
      (list) => list && applyEvent(list, { type: "put", kind: "node", resource: bumped }),
    );
  });
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Labels of node-b saved.")).toBeInTheDocument();
  expect(sent).toEqual([
    { ifMatch: "9", body: { labels: { zone: "rack-a", ssd: "true" }, spec: DOWN.spec } },
  ]);
  // 응답을 곧바로 목록에 반영한다.
  expect(within(row("node-b")).getAllByRole("cell")[labelsColumn]).toHaveTextContent(
    "ssd=true, zone=rack-a",
  );
});

it("does not overwrite node labels changed while the dialog was open", async () => {
  cluster();
  const labeled = { ...DOWN, labels: { zone: "rack-a" } };
  mockLists({ node: [NODE, labeled] });
  const sent: unknown[] = [];
  server.use(
    http.put(apiUrl("/nodes/n2"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(labeled);
    }),
  );
  const { queryClient } = renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  await within(table).findByText("node-b");
  await userEvent.click(screen.getByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Edit labels" }));
  const dialog = await screen.findByRole("dialog", { name: "Labels of node-b" });
  await userEvent.type(within(dialog).getByLabelText("Labels"), ", ssd=true");
  // 대화 상자를 연 사이 다른 곳에서 레이블을 바꿨다.
  const relabeled = { ...labeled, revision: 9, labels: { zone: "rack-b" } };
  act(() => {
    queryClient.setQueryData<List<Node>>(
      listKey("node"),
      (list) => list && applyEvent(list, { type: "put", kind: "node", resource: relabeled }),
    );
  });
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    within(dialog).getByText(
      "The labels of this node changed elsewhere. Close this dialog and open it again.",
    ),
  ).toBeVisible();
  expect(sent).toEqual([]);
});

it("marks a frozen node and unfreezes it", async () => {
  cluster();
  const frozen = {
    ...DOWN,
    status: { ...DOWN.status, shutdown: { action: "freeze" as const, at: 1_800_000_000 } },
  };
  // 켜져 있는 노드의 표시는 얼림이 아니다(곧 스스로 지운다).
  const back = {
    ...NODE,
    status: { ...NODE.status, shutdown: { action: "freeze" as const, at: 1_800_000_000 } },
  };
  mockLists({ node: [back, frozen] });
  let unfrozen = 0;
  server.use(
    http.post(apiUrl("/nodes/n2/unfreeze"), () => {
      unfrozen += 1;
      return unfrozen === 1
        ? HttpResponse.json(
            { code: "node_online", message: 'node "node-b" is online' },
            { status: 409 },
          )
        : HttpResponse.json(DOWN);
    }),
  );
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  expect((await within(table).findByText("node-b")).closest("tr")).toHaveTextContent("Frozen");
  expect(within(table).getByText("node-a").closest("tr")).not.toHaveTextContent("Frozen");
  await userEvent.click(screen.getByRole("button", { name: "Actions for node-a" }));
  expect(await screen.findByRole("menuitem", { name: "Edit labels" })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Unfreeze" })).not.toBeInTheDocument();
  await userEvent.keyboard("{Escape}");
  await userEvent.click(screen.getByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Unfreeze" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Unfreeze node-b?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Unfreeze" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The node is online again; it clears the mark itself.",
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Unfreeze" }));
  expect(
    await screen.findByText("node-b unfrozen. Its HA VMs are recovered on other nodes."),
  ).toBeInTheDocument();
});

it("sets up power-off fencing for a node, then tests it", async () => {
  cluster();
  const witness = { ...NODE, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  mockLists({ node: [NODE, DOWN, witness] });
  const put: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/nodes/n2/fence"), () =>
      HttpResponse.json({ code: "not_found", message: "no fencing" }, { status: 404 }),
    ),
    http.put(apiUrl("/nodes/n2/fence"), async ({ request }) => {
      const body = (await request.json()) as { methods: { kind: string }[] };
      put.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({
        node: "node-b",
        revision: 3,
        methods: [
          { kind: "command", off: "pdu off b", status: "pdu status b" },
          {
            kind: "ipmi",
            address: "10.0.0.12",
            port: 623,
            user: "admin",
            has_password: true,
            lanplus: true,
          },
        ],
      });
    }),
    http.post(apiUrl("/nodes/n2/fence-test"), () =>
      HttpResponse.json({
        checked_by: "node-a",
        methods: [
          { kind: "command", power: "on" },
          { kind: "ipmi", power: "unknown", error: "connection timed out" },
        ],
      }),
    ),
  );
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  await within(table).findByText("node-w");
  // 위트니스는 HA VM을 돌리지 않아 펜싱하지 않는다.
  await userEvent.click(screen.getByRole("button", { name: "Actions for node-w" }));
  expect(await screen.findByRole("menuitem", { name: "Edit labels" })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Power-off fencing…" })).not.toBeInTheDocument();
  await userEvent.keyboard("{Escape}");
  await userEvent.click(screen.getByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Power-off fencing…" }));
  const dialog = await screen.findByRole("dialog", { name: "Power-off fencing of node-b" });
  expect(await within(dialog).findByText(/No methods/)).toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Save" })).toBeDisabled();
  // 저장된 설정이 없으면 시험·지우기가 없다.
  expect(within(dialog).queryByRole("button", { name: "Test" })).not.toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Add a method" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Add a method" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getAllByText("Enter a value.")).toHaveLength(4);
  expect(put).toEqual([]);
  const first = within(dialog).getByRole("listitem", { name: "Method 1" });
  await userEvent.type(within(first).getByLabelText("BMC address"), "10.0.0.12");
  await userEvent.type(within(first).getByLabelText("User"), "admin");
  await userEvent.type(within(first).getByLabelText("Password"), "s3cret");
  const second = within(dialog).getByRole("listitem", { name: "Method 2" });
  await userEvent.selectOptions(within(second).getByLabelText("Method 2"), "command");
  await userEvent.type(within(second).getByLabelText("Power-off command"), "pdu off b");
  await userEvent.type(within(second).getByLabelText("Status command"), "pdu status b");
  await userEvent.click(within(second).getByRole("button", { name: "Move method 2 up" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Saved the power-off fencing of node-b.")).toBeInTheDocument();
  expect(put).toEqual([
    {
      ifMatch: null,
      body: {
        methods: [
          { kind: "command", off: "pdu off b", status: "pdu status b" },
          {
            kind: "ipmi",
            address: "10.0.0.12",
            port: 623,
            user: "admin",
            lanplus: true,
            password: "s3cret",
          },
        ],
      },
    },
  ]);
  // 저장한 뒤에는 시험할 수 있고, 저장된 비밀번호는 비워 두면 그대로 둔다.
  expect(within(dialog).getByText("A password is saved. Leave empty to keep it.")).toBeVisible();
  await userEvent.click(within(dialog).getByRole("button", { name: "Test" }));
  const result = await within(dialog).findByRole("table", { name: "Test result" });
  expect(within(dialog).getByText("Read by node-a. The power was not changed.")).toBeVisible();
  const rows = within(result).getAllByRole("row");
  expect(rows[1]).toHaveTextContent("1CommandOn");
  expect(rows[2]).toHaveTextContent("2IPMIUnknownconnection timed out");
});

it("keeps the saved password and removes power-off fencing", async () => {
  cluster();
  mockLists({ node: [NODE, DOWN] });
  const saved = {
    node: "node-b",
    revision: 7,
    methods: [
      {
        kind: "redfish",
        url: "https://bmc-b",
        user: "root",
        has_password: true,
        insecure: false,
      },
    ],
  };
  const put: { ifMatch: string | null; body: unknown }[] = [];
  const deleted: (string | null)[] = [];
  server.use(
    http.get(apiUrl("/nodes/n2/fence"), () => HttpResponse.json(saved)),
    http.put(apiUrl("/nodes/n2/fence"), async ({ request }) => {
      put.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json({ ...saved, revision: 8 });
    }),
    http.delete(apiUrl("/nodes/n2/fence"), ({ request }) => {
      deleted.push(request.headers.get("If-Match"));
      return new HttpResponse(null, { status: 204 });
    }),
    http.post(apiUrl("/nodes/n2/fence-test"), () =>
      HttpResponse.json(
        { code: "no_fence_checker", message: "no other online node" },
        { status: 409 },
      ),
    ),
  );
  renderPage(<NodesPage />);
  await screen.findByRole("table", { name: "Nodes" });
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Power-off fencing…" }));
  const dialog = await screen.findByRole("dialog", { name: "Power-off fencing of node-b" });
  expect(await within(dialog).findByLabelText("BMC URL")).toHaveValue("https://bmc-b");
  await userEvent.click(within(dialog).getByRole("button", { name: "Test" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "No other online node can read this node's power.",
  );
  await userEvent.type(within(dialog).getByLabelText("System ID"), "1");
  // 고친 뒤에는 저장된 설정과 달라 시험하지 않는다.
  expect(within(dialog).getByRole("button", { name: "Test" })).toBeDisabled();
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  await vi.waitFor(() =>
    expect(put).toEqual([
      {
        ifMatch: "7",
        body: {
          methods: [
            {
              kind: "redfish",
              url: "https://bmc-b",
              user: "root",
              insecure: false,
              system_id: "1",
            },
          ],
        },
      },
    ]),
  );
  // 저장된 비밀번호를 지우면 빈 글을 보낸다.
  await userEvent.click(within(dialog).getByLabelText("Clear the saved password"));
  expect(within(dialog).getByLabelText("Password")).toBeDisabled();
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(put).toHaveLength(2));
  expect(put[1]).toEqual({
    ifMatch: "8",
    body: {
      methods: [
        {
          kind: "redfish",
          url: "https://bmc-b",
          user: "root",
          insecure: false,
          password: "",
        },
      ],
    },
  });
  await userEvent.click(within(dialog).getByRole("button", { name: "Remove fencing" }));
  expect(await screen.findByText("Removed the power-off fencing of node-b.")).toBeInTheDocument();
  expect(deleted).toEqual(["8"]);
  expect(within(dialog).getByText(/No methods/)).toBeInTheDocument();
});
