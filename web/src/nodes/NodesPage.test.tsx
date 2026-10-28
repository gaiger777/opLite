import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { NodesPage } from "@/nodes/NodesPage";
import { NODE, vm } from "@/test/fixtures";
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
  expect(await screen.findByRole("menuitem", { name: "Remove from cluster" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
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
  expect(await screen.findByText(/oplctl cluster leave/)).toBeInTheDocument();
});

it("hides node changes from operators", async () => {
  cluster();
  mockLists({ node: [NODE] });
  renderPage(<NodesPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("node-a")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Actions for node-a" })).not.toBeInTheDocument();
  // 바꿀 수 없는 사람에게는 유지보수·제거 안내도 보이지 않는다.
  expect(screen.queryByText(/oplctl cluster leave/)).not.toBeInTheDocument();
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
