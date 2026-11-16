import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { CephStatusView, CephView, Node, Principal, Task } from "@/api/types";
import { CephPage } from "@/ceph/CephPage";
import { resetTasks } from "@/tasks/tasks";
import { CEPH, CEPH_STATUS, NET_NODE, NODE, task } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const VIEWER: Principal = { ...ADMIN, name: "view", role: "viewer" };

const NODE_B: Node = {
  ...NODE,
  id: "n2",
  name: "node-b",
  status: { ...NODE.status, online: false },
};
const NODE_D: Node = { ...NODE, id: "n4", name: "node-d" };
const WITNESS: Node = { ...NODE, id: "n5", name: "quorum", spec: { ...NODE.spec, witness: true } };

/** `GET /ceph`와 `GET /ceph/status`의 답 */
function ceph(view: CephView, status: CephStatusView | Response = CEPH_STATUS) {
  server.use(
    http.get(apiUrl("/ceph"), () => HttpResponse.json(view)),
    http.get(apiUrl("/ceph/status"), () =>
      status instanceof Response ? status : HttpResponse.json(status),
    ),
  );
}

/** 부트스트랩 Task(노드 n1) */
function initTask(state: Task["status"]["state"], extra: Partial<Task["status"]> = {}): Task {
  const t = task("t-init", "ceph-init", "c1");
  return { ...t, status: { ...t.status, state, ...extra } };
}

/** `POST /ceph`에 보낸 본문을 모은다. */
function acceptInit() {
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl("/ceph"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(task("t-init", "ceph-init", "c1"), { status: 202 });
    }),
  );
  return sent;
}

it("sets up Ceph from the wizard and follows the bootstrap task", async () => {
  mockLists({
    node: [NET_NODE, NODE_B, WITNESS],
    task: [
      initTask("running", {
        progress: { stage: "installing-packages", done_bytes: 0, total_bytes: 0 },
      }),
    ],
  });
  const sent = acceptInit();
  renderPage(<CephPage />);
  const user = userEvent.setup();
  await screen.findByRole("option", { name: "node-a" });
  const select = screen.getByLabelText("First node");
  // 위트니스는 빼고, 오프라인 노드는 막는다.
  const options = within(select).getAllByRole("option");
  expect(options.map((o) => o.textContent)).toEqual(["node-a", "node-b (offline)"]);
  expect(options[1]).toBeDisabled();
  // 노드 주소의 인터페이스 망으로 미리 채운다.
  expect(screen.getByLabelText("Public network")).toHaveValue("10.0.0.0/24");
  await user.click(screen.getByRole("button", { name: "Set up Ceph" }));
  expect(await screen.findByText("Set up Ceph on node-a")).toBeInTheDocument();
  expect(sent).toEqual([{ node: "n1", public_network: "10.0.0.0/24" }]);
  // 진행 단계: 지금 단계를 표시하고, 끝나기 전에는 다음으로 가지 못한다.
  const stages = screen.getByRole("list", { name: "Setup steps" });
  expect(within(stages).getByText("Installing packages")).toHaveAttribute("aria-current", "step");
  expect(within(stages).getByText("Checking the node")).toHaveClass("line-through");
  expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
});

it("sends the networks the admin typed", async () => {
  mockLists({ node: [NODE] });
  const sent = acceptInit();
  renderPage(<CephPage />);
  const user = userEvent.setup();
  // 인터페이스를 모르는 노드는 채우지 않는다(비우면 서버가 정한다).
  const pub = await screen.findByLabelText("Public network");
  expect(pub).toHaveValue("");
  await user.type(pub, "192.168.0.0/24");
  await user.type(screen.getByLabelText("Cluster network"), "10.9.0.0/24");
  await user.click(screen.getByRole("button", { name: "Set up Ceph" }));
  await waitFor(() =>
    expect(sent).toEqual([
      { node: "n1", public_network: "192.168.0.0/24", cluster_network: "10.9.0.0/24" },
    ]),
  );
});

it("shows why setting up failed and retries on the same node", async () => {
  mockLists({ node: [NET_NODE], task: [initTask("failed", { error: "apt-get install failed" })] });
  const sent = acceptInit();
  renderPage(<CephPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Set up Ceph" }));
  expect(
    await screen.findByText("Setting up Ceph failed: apt-get install failed"),
  ).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Retry" }));
  await waitFor(() =>
    expect(sent).toEqual([{ node: "n1", public_network: "10.0.0.0/24" }, { node: "n1" }]),
  );
});

it("goes through the monitor, OSD, pool and client steps", async () => {
  mockLists({ node: [NET_NODE], task: [initTask("succeeded")] });
  // 만든 뒤 다시 읽은 Ceph(mon 하나)
  let created = false;
  server.use(
    http.post(apiUrl("/ceph"), () => {
      created = true;
      return HttpResponse.json(task("t-init", "ceph-init", "c1"), { status: 202 });
    }),
    http.get(apiUrl("/ceph"), () =>
      created
        ? HttpResponse.json({ ...CEPH, mons: [CEPH.mons[0]], mgrs: ["n1"], osd_nodes: [] })
        : HttpResponse.json({ code: "not_found", message: "no Ceph" }, { status: 404 }),
    ),
    http.get(apiUrl("/ceph/status"), () => HttpResponse.json({ ...CEPH_STATUS, osd_hosts_up: 1 })),
  );
  renderPage(<CephPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Set up Ceph" }));
  await user.click(await screen.findByRole("button", { name: "Next" }));
  expect(await screen.findByText("Monitors now: 1 (node-a)")).toBeInTheDocument();
  expect(screen.getByText(/^3\. Monitors/)).toHaveAttribute("aria-current", "step");
  await user.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByRole("button", { name: "Add OSD" })).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Next" }));
  expect(await screen.findByText("Hosts with OSDs up: 1")).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByRole("button", { name: "Install client packages" })).toBeInTheDocument();
  // 마치면 상태 화면이다.
  await user.click(screen.getByRole("button", { name: "Finish" }));
  expect(await screen.findByText("Health")).toBeInTheDocument();
});

it("tells other roles that no Ceph is set up", async () => {
  mockLists({ node: [NODE] });
  renderPage(<CephPage />, { as: VIEWER });
  expect(await screen.findByText(/No Ceph cluster is set up/)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Set up Ceph" })).not.toBeInTheDocument();
});

it("shows the health, daemons, OSDs, placement groups and pools", async () => {
  mockLists({ node: [NODE] });
  ceph(CEPH);
  renderPage(<CephPage />, { as: VIEWER });
  expect(await screen.findByText("HEALTH_WARN")).toBeInTheDocument();
  const checks = screen.getByRole("list", { name: "Health checks" });
  expect(within(checks).getByText("OSD_DOWN").closest("li")).toHaveTextContent(
    "WRN OSD_DOWN 1 osds down",
  );
  expect(within(checks).getByText("MON_DOWN").closest("li")).toHaveTextContent("(muted)");
  const mons = screen.getByRole("table", { name: "Monitors: 3 (2 in quorum)" });
  expect(within(mons).getByText("node-c").closest("tr")).toHaveTextContent("Out of quorum");
  expect(screen.getByText("node-a (active) · standby: node-b")).toBeInTheDocument();
  expect(screen.getByText(/3 GiB used, 77 GiB of 80 GiB available/)).toBeInTheDocument();
  const pgs = screen.getByRole("list", { name: "Placement group states" });
  expect(
    within(pgs)
      .getAllByRole("listitem")
      .map((li) => li.textContent),
  ).toEqual(["120 active+clean", "9 active+undersized"]);
  const osds = screen.getByRole("table", { name: "OSDs: 3 up, 3 in, 4 total on 2 hosts" });
  expect(within(osds).getByText("node-a: 1 of 2 up")).toBeInTheDocument();
  expect(within(osds).getByText("osd.1").closest("tr")).toHaveTextContent("/dev/sdcssddownin");
  expect(within(osds).getByText("osd.3").closest("tr")).toHaveTextContent("osd.3——upout—");
  const pools = screen.getByRole("table", { name: "Pools" });
  expect(within(pools).getByRole("link", { name: "vms" })).toHaveAttribute(
    "href",
    "/storage-pools",
  );
  expect(within(pools).getByText("vmpool").closest("tr")).toHaveTextContent(
    "vmpoolvms21 GiB (3.8%)25 GiB",
  );
  // 뷰어는 바꾸지 못한다.
  expect(screen.queryByRole("button", { name: "Add monitor" })).not.toBeInTheDocument();
});

it("warns when the status is old and says when it cannot be read at all", async () => {
  mockLists({ node: [NODE] });
  ceph(CEPH, HttpResponse.json({ ...CEPH_STATUS, stale: true, error: "no monitor answered" }));
  const { unmount } = renderPage(<CephPage />);
  expect(await screen.findByText(/It cannot be read now: no monitor answered/)).toBeInTheDocument();
  unmount();
  ceph(
    CEPH,
    HttpResponse.json(
      { code: "ceph_unreachable", message: "cannot read the Ceph status from any monitor node: x" },
      { status: 503 },
    ),
  );
  renderPage(<CephPage />);
  expect(
    await screen.findByText(
      "Could not read the Ceph status: cannot read the Ceph status from any monitor node: x",
    ),
  ).toBeInTheDocument();
  // 상태를 읽지 못해도 동작 단추는 있다.
  expect(screen.getByRole("button", { name: "Add OSD" })).toBeEnabled();
});

it("shows the last error of an unfinished setup and retries it", async () => {
  mockLists({ node: [NODE] });
  ceph({
    ...CEPH,
    phase: "bootstrapping",
    mons: [],
    mgrs: [],
    osd_nodes: [],
    last_error: "timed out waiting for quorum",
  });
  const sent = acceptInit();
  renderPage(<CephPage />);
  const user = userEvent.setup();
  expect(await screen.findByText("timed out waiting for quorum")).toBeInTheDocument();
  expect(screen.getByText("Setting up Ceph did not finish.")).toBeInTheDocument();
  // 준비되기 전에는 더하기를 막는다(클라이언트 꾸러미는 된다).
  expect(screen.getByRole("button", { name: "Add monitor" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Install client packages" })).toBeEnabled();
  await user.click(screen.getByRole("button", { name: "Retry" }));
  expect(await screen.findByText("Set up Ceph on node-a")).toBeInTheDocument();
  expect(sent).toEqual([{ node: "n1" }]);
});

it("moves an unfinished setup off an offline node", async () => {
  // n2(오프라인)에서 돌다 만 부트스트랩 Task는 버려진 것으로 보고, 온라인 노드를 골라 다시 한다.
  const stuck = task("t-old", "ceph-init", "c1");
  mockLists({
    node: [NODE, NODE_B, NODE_D, WITNESS],
    task: [
      {
        ...stuck,
        spec: { ...stuck.spec, node: "n2" },
        status: { ...stuck.status, state: "running" },
      },
    ],
  });
  ceph({
    ...CEPH,
    phase: "bootstrapping",
    mons: [],
    mgrs: [],
    osd_nodes: [],
    init_node: "n2",
    tasks: ["t-old"],
  });
  const sent = acceptInit();
  renderPage(<CephPage />);
  const user = userEvent.setup();
  const select = await screen.findByLabelText("node-b is offline; set Ceph up on");
  expect(
    within(select)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["node-a", "node-d"]);
  await user.selectOptions(select, "n4");
  await user.click(screen.getByRole("button", { name: "Retry" }));
  await waitFor(() => expect(sent).toEqual([{ node: "n4" }]));
});

it("adds a monitor and warns about an even count", async () => {
  mockLists({ node: [NODE, NODE_D, WITNESS] });
  ceph(CEPH);
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl("/ceph/mons"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(task("t-mon", "ceph-mon-add", "c1"), { status: 202 });
    }),
  );
  renderPage(<CephPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Add monitor" }));
  const dialog = await screen.findByRole("dialog");
  // mon이 있는 노드와 위트니스는 고를 수 없다.
  expect(
    within(dialog)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["node-d"]);
  expect(within(dialog).getByText(/An odd number of monitors/)).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Add monitor" }));
  expect(await screen.findByText("Add a Ceph monitor on node-d")).toBeInTheDocument();
  expect(sent).toEqual([{ node: "n4" }]);
});

it("does not add a sixth monitor", async () => {
  mockLists({ node: [NODE, NODE_D] });
  const five = ["n1", "n2", "n3", "n6", "n7"].map((node, i) => ({
    node,
    name: `m${i}`,
    addr: `10.0.0.${i + 1}`,
  }));
  ceph({ ...CEPH, mons: five });
  renderPage(<CephPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Add monitor" }));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByText(/already has 5 monitors/)).toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Add monitor" })).toBeDisabled();
});
