import { screen, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { DashboardPage, usage } from "@/pages/DashboardPage";
import { NODE, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

it("summarizes nodes, VMs and failures", async () => {
  const down = { ...NODE, id: "n2", name: "node-b", status: { ...NODE.status, online: false } };
  const broken = vm("db", "failed", {
    status: { phase: "failed", node: null, pid: null, last_error: "no firmware" },
  });
  mockLists({ node: [NODE, down], vm: [vm("web", "running"), vm("app", "running"), broken] });
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
  renderPage(<DashboardPage />);
  expect(await screen.findByText("1 / 2")).toBeInTheDocument();
  expect(screen.getByText("2 running · 0 stopped · 1 failed")).toBeInTheDocument();
  expect(await screen.findByText("node-a", { selector: "div" })).toBeInTheDocument();
  expect(screen.getByRole("group", { name: "Leader" })).toHaveTextContent("node-a");
  const table = screen.getByRole("table", { name: "Nodes" });
  expect(within(table).getByRole("columnheader", { name: "Active VMs" })).toBeInTheDocument();
  const row = within(table).getByText("node-a", { selector: "td" }).closest("tr")!;
  expect(row).toHaveTextContent("4 / 8");
  expect(row).toHaveTextContent("4 GiB / 16 GiB");
  expect(screen.getByText("Offline")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "db" })).toBeInTheDocument();
  expect(screen.getByText("no firmware")).toBeInTheDocument();
});

const cluster = (leader: string | null, members: { id: string; name?: string | null }[] = []) =>
  http.get(apiUrl("/cluster"), () =>
    HttpResponse.json({
      name: "lab",
      ca_fingerprint: "x",
      leader,
      node: "n1",
      members: members.map((m) => ({ cluster_address: "a", online: true, voter: true, ...m })),
    }),
  );

it("counts only VMs whose QEMU runs on the node", () => {
  // 실패한 VM도 status.node가 남는다.
  const failed = vm("db", "failed", {
    status: { phase: "failed", node: "n1", pid: null, last_error: "boom" },
  });
  const stopping = vm("app", "stopping", {
    status: { phase: "stopping", node: "n1", pid: 7, last_error: null },
  });
  const elsewhere = vm("far", "running", {
    status: { phase: "running", node: "n2", pid: 8, last_error: null },
  });
  expect(usage(NODE, [vm("web", "running"), failed, stopping, elsewhere])).toEqual({
    vms: 2,
    vcpus: 4,
    memoryMib: 4096,
  });
});

it("shows node capacity in GiB with one decimal", async () => {
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, memory_mib: 15890 } }], vm: [] });
  server.use(cluster("n1"));
  renderPage(<DashboardPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  expect(await within(table).findByText("0 MiB / 15.5 GiB")).toBeInTheDocument();
});

it("shows loading instead of zeros, then errors", async () => {
  let fail: (() => void) | undefined;
  server.use(
    http.get(
      apiUrl("/nodes"),
      () =>
        new Promise<Response>((resolve) => {
          fail = () =>
            resolve(HttpResponse.json({ code: "forbidden", message: "denied" }, { status: 403 }));
        }),
    ),
    http.get(apiUrl("/vms"), () => new Promise<never>(() => {})),
    http.get(apiUrl("/tasks"), () => HttpResponse.json({ revision: 1, items: [] })),
    http.get(apiUrl("/cluster"), () => new Promise<never>(() => {})),
  );
  renderPage(<DashboardPage />);
  const nodes = await screen.findByRole("group", { name: "Nodes" });
  expect(nodes).toHaveTextContent("Loading…");
  expect(nodes).not.toHaveTextContent("0 / 0");
  expect(screen.getByRole("group", { name: "Virtual machines" })).toHaveTextContent("Loading…");
  expect(screen.getByRole("group", { name: "Leader" })).toHaveTextContent("Loading…");
  const table = screen.getByRole("table", { name: "Nodes" });
  expect(table).toHaveTextContent("Loading…");

  // 요청이 핸들러에 닿은 뒤에 실패시킨다(부하가 크면 화면이 먼저 그려진다).
  await vi.waitFor(() => expect(fail).toBeDefined());
  fail!();
  expect(await within(nodes).findByText("You do not have permission to do this.")).toBeVisible();
  expect(within(table).getByText("You do not have permission to do this.")).toBeVisible();
});

it("says when there are no nodes", async () => {
  mockLists({});
  server.use(cluster(null));
  renderPage(<DashboardPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  expect(await within(table).findByText("No nodes.")).toBeInTheDocument();
  expect(await screen.findByRole("group", { name: "Leader" })).toHaveTextContent("No leader");
});

it("names the leader from cluster members, then from its id", async () => {
  const id = "019a0f3c-0000-7000-8000-00000000abcd";
  mockLists({ node: [NODE] });
  server.use(cluster(id, [{ id, name: "joining" }]));
  const { unmount } = renderPage(<DashboardPage />);
  const leader = await screen.findByRole("group", { name: "Leader" });
  expect(await within(leader).findByText("joining")).toBeInTheDocument();
  unmount();

  server.use(cluster(id, [{ id, name: null }]));
  renderPage(<DashboardPage />);
  expect(
    await within(await screen.findByRole("group", { name: "Leader" })).findByText("0000abcd"),
  ).toBeInTheDocument();
});

it("names the recent tasks table", async () => {
  mockLists({ node: [NODE] });
  server.use(cluster("n1"));
  renderPage(<DashboardPage />);
  expect(await screen.findByRole("table", { name: "Recent tasks" })).toBeInTheDocument();
});
