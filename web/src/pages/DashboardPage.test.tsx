import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { clusterCapacity, usage } from "@/dashboard/CapacityWidget";
import { setPin } from "@/lib/recent";
import { DashboardPage } from "@/pages/DashboardPage";
import { expectNoAxeViolations } from "@/test/axe";
import { NODE, pool, task, vm, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const VIEWER = { ...ADMIN, role: "viewer" as const };
const OPERATOR = { ...ADMIN, role: "operator" as const };

beforeEach(() => localStorage.clear());

const cluster = (leader: string | null, members: { id: string; name?: string | null }[] = []) =>
  http.get(apiUrl("/cluster"), () =>
    HttpResponse.json({
      name: "lab",
      ca_fingerprint: "x",
      leader,
      node: "n1",
      members: members.map((m) => ({
        cluster_address: "a",
        cluster_addresses: ["a"],
        online: true,
        voter: true,
        ...m,
      })),
    }),
  );

it("summarizes nodes, VMs, failures, alerts and the leader in the health strip", async () => {
  const down = { ...NODE, id: "n2", name: "node-b", status: { ...NODE.status, online: false } };
  const broken = vm("db", "failed");
  // 옮기는 중인 VM도 실행 중으로 센다.
  const app = vm("app", "running");
  const migrating = { ...app, status: { ...app.status, phase: "migrating" as const } };
  mockLists({ node: [NODE, down], vm: [vm("web", "running"), migrating, broken] });
  server.use(
    cluster("n1"),
    http.get(apiUrl("/alerts"), () =>
      HttpResponse.json({
        alerts: [
          {
            rule: "r1",
            rule_name: "hot",
            severity: "critical",
            state: "firing",
            since: 1,
            condition: "node_metric",
            subject: { kind: "node", id: "n1", name: "node-a" },
          },
          {
            rule: "r2",
            rule_name: "fyi",
            severity: "info",
            state: "firing",
            since: 1,
            condition: "node_metric",
            subject: { kind: "node", id: "n1", name: "node-a" },
          },
        ],
        unreachable: [],
        unsupported: [],
      }),
    ),
  );
  const { container } = renderPage(<DashboardPage />);
  const health = await screen.findByRole("region", { name: "Cluster health" });
  expect(await within(health).findByText("1 / 2")).toBeInTheDocument();
  const vms = within(health).getByRole("group", { name: "Virtual machines" });
  expect(vms).toHaveTextContent("2 running · 0 stopped · 1 failed");
  expect(within(vms).getByRole("link", { name: "1 failed" })).toHaveAttribute(
    "href",
    "/vms?phase=failed",
  );
  expect(await within(health).findByRole("group", { name: "Leader" })).toHaveTextContent("node-a");
  // info 경보는 울리는 수에 넣지 않는다.
  const alerts = within(health).getByRole("group", { name: "Alerts firing" });
  await vi.waitFor(() => expect(alerts).toHaveTextContent(/^Alerts firing1/));
  expect(within(alerts).getByRole("link", { name: "View alerts" })).toHaveAttribute(
    "href",
    "/alerts",
  );
  // 지난 홈의 노드별 표와 실패 목록은 없다(노드 부하·작업 위젯이 맡는다).
  expect(screen.queryByRole("table", { name: "Nodes" })).not.toBeInTheDocument();
  await expectNoAxeViolations(container);
});

it("counts only VMs whose QEMU runs on the node", () => {
  const at = (name: string, phase: "failed" | "stopping" | "running" | "migrating", node: string) =>
    vm(name, phase, {
      status: {
        phase,
        node,
        pid: null,
        last_error: null,
        snapshot: null,
        schedule_error: null,
      },
    });
  // 실패한 VM도 status.node가 남는다. 옮기는 중인 VM은 확정할 때까지 원본 노드에서 돈다.
  const list = [
    vm("web", "running"),
    at("db", "failed", "n1"),
    at("app", "stopping", "n1"),
    at("far", "running", "n2"),
    at("mig", "migrating", "n1"),
  ];
  expect(usage(NODE, list)).toEqual({ vms: 3, vcpus: 6, memoryMib: 6144 });
  // 오프라인 노드와 위트니스는 용량에 넣지 않는다.
  const off = { ...NODE, id: "n2", status: { ...NODE.status, online: false } };
  const witness = { ...NODE, id: "n3", spec: { ...NODE.spec, witness: true } };
  expect(clusterCapacity([NODE, off, witness], list)).toEqual({
    cpus: 8,
    vcpus: 6,
    memoryMib: 16384,
    usedMib: 6144,
  });
});

/** 섞인 판 클러스터(기능 `operator-dashboard`가 모두에 없음): 지난 홈의 위젯과 안내 한 줄 */
const upgrading = () =>
  http.get(apiUrl("/capacity"), () =>
    HttpResponse.json({ code: "cluster_upgrading", message: "upgrading" }, { status: 409 }),
  );

it("shows how much CPU and memory running VMs take", async () => {
  mockLists({ node: [NODE], vm: [vm("web", "running"), vm("off", "stopped")] });
  server.use(cluster("n1"), upgrading());
  renderPage(<DashboardPage />);
  const capacity = await screen.findByRole("region", { name: "Capacity" });
  const cpu = await within(capacity).findByRole("meter", { name: "vCPUs given to running VMs" });
  expect(cpu).toHaveAttribute("aria-valuenow", "25");
  expect(cpu).toHaveAttribute("aria-valuetext", "2 of 8 cores, 25 %");
  expect(
    within(capacity).getByRole("meter", { name: "Memory given to running VMs" }),
  ).toHaveAttribute("aria-valuetext", "2 GiB / 16 GiB, 13 %");
  // 섞인 판이라 대시보드 대신 안내 한 줄
  expect(screen.getByText(/^Upgrade every node to see the operator dashboard/)).toBeInTheDocument();
  expect(screen.queryByRole("complementary", { name: "Details" })).not.toBeInTheDocument();
});

it("shows loading instead of zeros, then errors", async () => {
  let fail: (() => void) | undefined;
  mockLists({});
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
    http.get(apiUrl("/cluster"), () => new Promise<never>(() => {})),
  );
  renderPage(<DashboardPage />);
  const nodes = await screen.findByRole("group", { name: "Nodes" });
  expect(nodes).toHaveTextContent("Loading…");
  expect(nodes).not.toHaveTextContent("0 / 0");
  expect(screen.getByRole("group", { name: "Virtual machines" })).toHaveTextContent("Loading…");
  expect(screen.getByRole("group", { name: "Leader" })).toHaveTextContent("Loading…");

  // 요청이 핸들러에 닿은 뒤에 실패시킨다(부하가 크면 화면이 먼저 그려진다).
  await vi.waitFor(() => expect(fail).toBeDefined());
  fail!();
  expect(await within(nodes).findByText("You do not have permission to do this.")).toBeVisible();
});

it("says when there is no leader", async () => {
  mockLists({});
  server.use(cluster(null));
  renderPage(<DashboardPage />);
  const leader = await screen.findByRole("group", { name: "Leader" });
  expect(await within(leader).findByText("No leader")).toBeInTheDocument();
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

it("shows the recent tasks in their own card", async () => {
  const web = vm("web", "running");
  mockLists({
    node: [NODE],
    vm: [web],
    task: [
      {
        ...task("019a0f3c-8a10-7000-8000-000000000009", "vm-start", web.id),
        status: { state: "succeeded", error: null },
      },
    ],
  });
  server.use(cluster("n1"));
  renderPage(<DashboardPage />);
  const card = await screen.findByRole("region", { name: "Recent tasks" });
  const table = within(card).getByRole("table", { name: "Recent tasks" });
  expect(await within(table).findByText("Start web")).toBeInTheDocument();
  expect(within(card).getByRole("link", { name: "All tasks" })).toHaveAttribute("href", "/tasks");
});

it("warns while nodes run different versions", async () => {
  const at = (id: string, version?: string) => ({
    ...NODE,
    id,
    name: id,
    status: { ...NODE.status, version },
  });
  // 버전을 모르는 노드(옛 노드)는 세지 않고, 같은 버전은 한 번만 보인다. Debian 순서로 보인다(~는
  // 정식 버전보다 낮다).
  mockLists({
    node: [at("a", "0.10.0"), at("b", "0.9.0"), at("c", "0.9.0"), at("d"), at("e", "0.10.0~rc1")],
    vm: [],
  });
  server.use(cluster("a"));
  const { unmount } = renderPage(<DashboardPage />);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Nodes run different Orbivisor versions (0.9.0, 0.10.0~rc1, 0.10.0). Finish the upgrade on every node.",
  );
  unmount();

  mockLists({ node: [at("a", "0.10.0"), at("b", "0.10.0"), at("d")], vm: [] });
  renderPage(<DashboardPage />);
  expect(await screen.findByText("3 / 3")).toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("shows the subscription notice above the dashboard", async () => {
  mockLists({ node: [NODE], vm: [] });
  server.use(
    cluster("n1"),
    http.get(apiUrl("/subscription"), () => HttpResponse.json({ state: "none", used_nodes: 1 })),
  );
  renderPage(<DashboardPage />);
  expect(await screen.findByRole("status")).toHaveTextContent("This cluster has no subscription.");
});

it("lists favorites with their state and a power button", async () => {
  const web = vm("web", "running");
  const db = vm("db", "stopped");
  const gone = vm("gone", "stopped");
  for (const ref of [web, db, gone]) {
    setPin(ADMIN.user, { kind: "vm", id: ref.id, name: ref.name }, true);
  }
  setPin(ADMIN.user, { kind: "node", id: NODE.id, name: NODE.name }, true);
  const local = pool("local");
  setPin(ADMIN.user, { kind: "storage-pool", id: local.id, name: "local" }, true);
  mockLists({ node: [NODE], vm: [web, db], "storage-pool": [local] });
  let started = false;
  server.use(
    cluster("n1"),
    http.post(apiUrl(`/vms/${db.id}/actions/start`), () => {
      started = true;
      return HttpResponse.json(task("t1", "vm-start", db.id), { status: 202 });
    }),
  );
  renderPage(<DashboardPage />);
  const card = await screen.findByRole("region", { name: "Favorites" });
  const list = await within(card).findByRole("list", { name: "Favorites" });
  // 지운 VM은 빠진다.
  await vi.waitFor(() => expect(within(list).getAllByRole("listitem")).toHaveLength(4));
  expect(within(list).getByRole("link", { name: "node-a" })).toHaveAttribute(
    "href",
    "/nodes/node-a",
  );
  expect(within(list).getByRole("link", { name: "local" })).toHaveAttribute(
    "href",
    "/storage-pools?q=local",
  );
  const webItem = within(list).getByRole("link", { name: "web" }).closest("li")!;
  expect(webItem).toHaveTextContent("Running");
  // 실행 중이면 정지(확인), 꺼져 있으면 시작(바로)
  await userEvent.click(within(webItem).getByRole("button", { name: "Stop web" }));
  expect(await screen.findByRole("alertdialog", { name: "Stop web?" })).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await userEvent.click(within(list).getByRole("button", { name: "Start db" }));
  await vi.waitFor(() => expect(started).toBe(true));
});

it("says when there are no favorites and hides power buttons from viewers", async () => {
  const web = vm("web", "running");
  setPin(ADMIN.user, { kind: "vm", id: web.id, name: "web" }, true);
  mockLists({ node: [NODE], vm: [web] });
  server.use(cluster("n1"));
  const { unmount } = renderPage(<DashboardPage />, { as: VIEWER });
  const card = await screen.findByRole("region", { name: "Favorites" });
  expect(await within(card).findByRole("link", { name: "web" })).toBeInTheDocument();
  expect(within(card).queryByRole("button")).not.toBeInTheDocument();
  unmount();

  localStorage.clear();
  renderPage(<DashboardPage />);
  expect(
    await screen.findByText(/^No favorites yet\. Mark VMs, nodes and storage pools/),
  ).toBeInTheDocument();
});

it("walks a new cluster through its first steps", async () => {
  mockLists({ node: [NODE] });
  server.use(cluster("n1"));
  const { unmount } = renderPage(<DashboardPage />);
  let steps = await screen.findByRole("region", { name: "First steps" });
  const items = () => within(steps).getAllByRole("listitem");
  expect(items()[0]).toHaveAttribute("aria-current", "step");
  expect(within(items()[0]!).getByRole("link", { name: "Create a pool" })).toHaveAttribute(
    "href",
    "/storage-pools?create=1",
  );
  unmount();

  // 풀이 있으면 첫 단계는 끝났고 ISO·이미지 단계가 지금 단계다.
  mockLists({ node: [NODE], "storage-pool": [pool("local")] });
  const second = renderPage(<DashboardPage />);
  steps = await screen.findByRole("region", { name: "First steps" });
  await vi.waitFor(() => expect(items()[1]).toHaveAttribute("aria-current", "step"));
  expect(items()[0]).toHaveTextContent("(done)");
  expect(within(items()[0]!).queryByRole("link")).not.toBeInTheDocument();
  expect(within(items()[1]!).getByRole("link", { name: "Upload an ISO" })).toHaveAttribute(
    "href",
    "/isos?create=1",
  );
  expect(within(items()[1]!).getByRole("link", { name: "or download an image" })).toHaveAttribute(
    "href",
    "/images",
  );
  second.unmount();

  mockLists({
    node: [NODE],
    "storage-pool": [pool("local")],
    volume: [volume("root", "local")],
  });
  const third = renderPage(<DashboardPage />);
  steps = await screen.findByRole("region", { name: "First steps" });
  await vi.waitFor(() => expect(items()[2]).toHaveAttribute("aria-current", "step"));
  expect(within(items()[2]!).getByRole("link", { name: "Create a VM" })).toHaveAttribute(
    "href",
    "/vms/-new",
  );
  third.unmount();

  // VM이 생기면 사라진다. viewer에게는 보이지 않는다.
  mockLists({ node: [NODE], "storage-pool": [pool("local")], vm: [vm("web", "stopped")] });
  const fourth = renderPage(<DashboardPage />);
  expect(await screen.findByRole("region", { name: "Favorites" })).toBeInTheDocument();
  await screen.findByText("1 / 1");
  expect(screen.queryByRole("region", { name: "First steps" })).not.toBeInTheDocument();
  fourth.unmount();
  mockLists({ node: [NODE] });
  renderPage(<DashboardPage />, { as: VIEWER });
  await screen.findByText("1 / 1");
  expect(screen.queryByRole("region", { name: "First steps" })).not.toBeInTheDocument();
});

it("tells an operator to ask an admin for the pool instead of leaving them stuck", async () => {
  mockLists({ node: [NODE] });
  server.use(cluster("n1"));
  renderPage(<DashboardPage />, { as: OPERATOR });
  const steps = await screen.findByRole("region", { name: "First steps" });
  const items = within(steps).getAllByRole("listitem");
  expect(items[0]).toHaveAttribute("aria-current", "step");
  expect(items[0]).toHaveTextContent("Ask an admin to create a storage pool.");
  // 풀이 없으면 뒤 단계(ISO 올리기·VM 만들기)의 버튼도 없다.
  expect(within(steps).queryByRole("link")).not.toBeInTheDocument();
});

it("puts alerts, the health strip and favorites first on a phone while upgrading", async () => {
  const media = vi.spyOn(window, "matchMedia").mockImplementation(
    (query: string) =>
      ({
        matches: query === "(max-width: 767px)",
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      }) as unknown as MediaQueryList,
  );
  try {
    mockLists({ node: [NODE], vm: [vm("web", "running")] });
    server.use(cluster("n1"), upgrading());
    renderPage(<DashboardPage />);
    await screen.findByText("1 / 1");
    const names = screen.getAllByRole("region").map((r) => {
      const id = r.getAttribute("aria-labelledby");
      return r.getAttribute("aria-label") ?? (id ? document.getElementById(id)?.textContent : "");
    });
    expect(names.slice(0, 3)).toEqual(["Active alerts", "Cluster health", "Favorites"]);
    // 휴대폰에서는 차트를 그리지 않는다.
    expect(screen.queryByRole("region", { name: "Cluster load" })).not.toBeInTheDocument();
  } finally {
    media.mockRestore();
  }
});

it("leaves out the cluster widgets for a folder-only user instead of loading forever", async () => {
  // 루트 권한이 없는 폴더 사용자(계획 62 리뷰 1): 노드·리더·용량·차트를 묻지 않고 한 줄로 알린다.
  const asked: string[] = [];
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: [],
        root_contents: [],
        folders: {},
        contents: {},
        objects: [],
      }),
    ),
    http.get(apiUrl("/cluster"), () => {
      asked.push("cluster");
      return HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 });
    }),
    http.get(apiUrl("/metrics/cluster"), () => {
      asked.push("metrics");
      return HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 });
    }),
    http.get(apiUrl("/alerts"), () =>
      HttpResponse.json({ alerts: [], unreachable: [], unsupported: [] }),
    ),
  );
  mockLists({ vm: [vm("web", "running")] });
  renderPage(<DashboardPage />, { as: { ...ADMIN, role: "none" } });
  expect(
    await screen.findByText(
      "Nodes, capacity and cluster charts need permission to read the cluster.",
    ),
  ).toBeInTheDocument();
  const health = screen.getByRole("region", { name: "Cluster health" });
  expect(within(health).getByRole("group", { name: "Virtual machines" })).toBeInTheDocument();
  expect(within(health).queryByRole("group", { name: "Nodes" })).not.toBeInTheDocument();
  expect(within(health).queryByRole("group", { name: "Leader" })).not.toBeInTheDocument();
  await vi.waitFor(() => expect(screen.queryByText("Loading…")).not.toBeInTheDocument());
  expect(screen.queryByText("You do not have permission to do this.")).not.toBeInTheDocument();
  expect(asked).toEqual([]);
});
