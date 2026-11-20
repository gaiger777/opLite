import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Node, Principal } from "@/api/types";
import { SdnPage } from "@/sdn/SdnPage";
import { expectNoAxeViolations } from "@/test/axe";
import { resetTasks } from "@/tasks/tasks";
import { NODE, readyChassis, sdnNode, sdnTask, sdnView } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const VIEWER: Principal = { ...ADMIN, name: "view", role: "viewer" };
const WITNESS: Node = { ...NODE, id: "n9", name: "quorum", spec: { ...NODE.spec, witness: true } };

/** 서버 `GET /cluster`의 모양(투표 멤버 `voters`) */
function cluster(nodes: Node[], voters: string[]) {
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "ab:cd",
        leader: nodes[0]!.id,
        node: nodes[0]!.id,
        members: nodes.map((n) => ({
          id: n.id,
          name: n.name,
          address: n.spec.address,
          cluster_address: n.spec.address,
          cluster_addresses: [n.spec.address],
          voter: voters.includes(n.id),
          role: voters.includes(n.id) ? "voter" : n.spec.witness ? "voter" : "learner",
          online: n.status.online,
          witness: n.spec.witness ?? false,
          fencing: null,
          version: "0.65.0",
        })),
      }),
    ),
  );
}

it("tells viewers that SDN is not set up and offers admins the wizard", async () => {
  mockLists({ node: [sdnNode(1)] });
  cluster([sdnNode(1)], ["n1"]);
  const { unmount } = renderPage(<SdnPage />, { as: VIEWER });
  expect(
    await screen.findByText("SDN (OVN) is not set up. An administrator can set it up here."),
  ).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Start the SDN setup" })).not.toBeInTheDocument();
  unmount();
  renderPage(<SdnPage />);
  expect(await screen.findByRole("button", { name: "Start the SDN setup" })).toBeInTheDocument();
});

it("sets up the centrals one at a time and installs the chassis", async () => {
  const a = sdnNode(1);
  const b = sdnNode(2);
  const c = sdnNode(3);
  const d = sdnNode(4, undefined, { online: false });
  const mac = sdnNode(5, undefined, { hypervisor: "qemu-hvf" });
  const nodes = [a, b, c, d, mac, WITNESS];
  cluster(nodes, ["n1", "n2", "n3"]);
  // 서버처럼: 첫 세우기 뒤 준비, 중앙을 더할 때마다 늘고 짝수가 되면 경고 헤더(ASCII)
  const centrals: Node[] = [];
  const posts: unknown[] = [];
  server.use(
    http.get(apiUrl("/sdn"), () =>
      HttpResponse.json(
        centrals.length === 0 ? { centrals: [], chassis: [], tasks: [] } : sdnView(centrals, nodes),
      ),
    ),
    http.post(apiUrl("/sdn"), async ({ request }) => {
      posts.push(["init", await request.json()]);
      centrals.push(a);
      return HttpResponse.json(sdnTask("t-init", "sdn-init", "n1"), { status: 202 });
    }),
    http.post(apiUrl("/sdn/centrals"), async ({ request }) => {
      const body = (await request.json()) as { node: string };
      posts.push(["add", body]);
      centrals.push(nodes.find((n) => n.id === body.node)!);
      const headers: Record<string, string> =
        centrals.length % 2 === 0
          ? {
              "orbivisor-warning": `SDN (OVN) will have ${centrals.length} central nodes; an even number tolerates no more failures than one fewer; use 3 or 5`,
            }
          : {};
      return HttpResponse.json(sdnTask(`t-add-${body.node}`, "sdn-central-add", body.node), {
        status: 202,
        headers,
      });
    }),
    http.post(apiUrl("/sdn/chassis"), async ({ request }) => {
      posts.push(["chassis", await request.json()]);
      return HttpResponse.json([], {
        status: 202,
        headers: {
          "orbivisor-warning": "skipped offline nodes: node-d; install them when they are back",
        },
      });
    }),
  );
  mockLists({
    node: nodes,
    task: [
      sdnTask("t-init", "sdn-init", "n1", { state: "succeeded" }),
      sdnTask("t-add-n2", "sdn-central-add", "n2", { state: "succeeded" }),
      sdnTask("t-add-n3", "sdn-central-add", "n3", { state: "succeeded" }),
    ],
  });
  renderPage(<SdnPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Start the SDN setup" }));
  // 투표 멤버 셋을 미리 고르고, 막힌 노드는 까닭과 함께 고를 수 없다(위트니스는 없음).
  const pick = await screen.findByRole("group", { name: "Central nodes" });
  await waitFor(() => expect(within(pick).getByLabelText("node-b")).toBeChecked());
  expect(within(pick).getByLabelText("node-a")).toBeChecked();
  expect(within(pick).getByLabelText("node-c")).toBeChecked();
  expect(within(pick).getByLabelText("node-d")).toBeDisabled();
  expect(within(pick).getByText("offline")).toBeInTheDocument();
  expect(within(pick).getByText("macOS node")).toBeInTheDocument();
  expect(within(pick).queryByText("quorum")).not.toBeInTheDocument();
  await user.click(within(pick).getByLabelText("node-c"));
  // 둘이면 짝수 경고
  expect(
    screen.getByText("2 central nodes tolerate no more failures than one fewer. Use 3 or 5."),
  ).toBeInTheDocument();
  await user.click(within(pick).getByLabelText("node-c"));
  await user.click(screen.getByRole("button", { name: "Install" }));
  await waitFor(() =>
    expect(posts).toEqual([
      ["init", { node: "n1" }],
      ["add", { node: "n2" }],
      ["add", { node: "n3" }],
    ]),
  );
  // 중앙이 둘이 될 때의 서버 경고를 그대로 보인다.
  expect(await screen.findByText(/will have 2 central nodes/)).toBeInTheDocument();
  const next = screen.getByRole("button", { name: "Next" });
  await waitFor(() => expect(next).toBeEnabled());
  await user.click(next);
  await user.click(await screen.findByRole("button", { name: "Install on all nodes" }));
  expect(await screen.findByText(/skipped offline nodes: node-d/)).toBeInTheDocument();
  expect(posts.at(-1)).toEqual(["chassis", { all: true }]);
});

it("shows why the setup failed and retries on the same node", async () => {
  const a = sdnNode(1);
  cluster([a], ["n1"]);
  const posts: unknown[] = [];
  server.use(
    http.post(apiUrl("/sdn"), async ({ request }) => {
      posts.push(await request.json());
      return HttpResponse.json(sdnTask("t-init", "sdn-init", "n1"), { status: 202 });
    }),
  );
  mockLists({
    node: [a],
    task: [
      sdnTask("t-init", "sdn-init", "n1", {
        state: "failed",
        error: "apt-get install ovn-central failed",
        progress: { stage: "installing-packages", done_bytes: 0, total_bytes: 0 },
      }),
    ],
  });
  renderPage(<SdnPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Start the SDN setup" }));
  await user.type(await screen.findByLabelText(/Central address/), "10.0.0.1");
  await user.click(screen.getByRole("button", { name: "Install" }));
  expect(
    await screen.findByText("The setup failed: apt-get install ovn-central failed"),
  ).toBeInTheDocument();
  const stages = screen.getByRole("list", { name: "Setup steps" });
  expect(within(stages).getByText("Checking the node")).toHaveClass("line-through");
  await user.click(screen.getByRole("button", { name: "Retry" }));
  await waitFor(() => expect(posts).toEqual([{ node: "n1", address: "10.0.0.1" }, { node: "n1" }]));
});

it("shows centrals, roles, sync and chassis, and removes a central", async () => {
  const a = sdnNode(1, readyChassis(1, "leader"));
  const b = sdnNode(2, readyChassis(2, "follower"));
  const c = sdnNode(3, { chassis: "down", error: "connection refused" });
  const mac = sdnNode(4, undefined, { hypervisor: "qemu-hvf" });
  const nodes = [a, b, c, mac];
  cluster(nodes, ["n1", "n3"]);
  const removed: string[] = [];
  server.use(
    http.get(apiUrl("/sdn"), () =>
      HttpResponse.json(
        sdnView([a, b], nodes, {
          sync: { term: 3, index: 9, at: 1795107585, nb_cfg: 9, hv_cfg: 8 },
        }),
      ),
    ),
    http.delete(apiUrl("/sdn/centrals/:node"), ({ params }) => {
      removed.push(String(params.node));
      return HttpResponse.json(sdnTask("t-rm", "sdn-central-remove", String(params.node)), {
        status: 202,
      });
    }),
  );
  mockLists({ node: nodes });
  const { container } = renderPage(<SdnPage />);
  const centrals = await screen.findByRole("table", { name: "Central nodes" });
  const rowA = within(centrals).getByText("node-a").closest("tr")!;
  expect(rowA).toHaveTextContent("10.0.0.1");
  expect(rowA).toHaveTextContent("leader");
  expect(rowA).toHaveTextContent("active");
  expect(within(centrals).getByText("node-b").closest("tr")).toHaveTextContent("standby");
  // 경고 띠: 짝수, 투표 멤버가 아닌 중앙
  // 투표 멤버는 `GET /cluster`를 읽은 뒤에 안다.
  await waitFor(() =>
    expect(screen.getByRole("alert")).toHaveTextContent("not voting cluster members: node-b"),
  );
  expect(screen.getByRole("alert")).toHaveTextContent("2 central nodes tolerate no more failures");
  expect(screen.getByText("Chassis applied 8 of 9.")).toBeInTheDocument();
  const chassis = screen.getByRole("table", { name: "Chassis" });
  const rowC = within(chassis).getByText("node-c").closest("tr")!;
  expect(rowC).toHaveTextContent("Down");
  expect(rowC).toHaveTextContent("connection refused");
  expect(within(chassis).getByText("node-a").closest("tr")).toHaveTextContent(
    "OVN 24.03.8, OVS 3.3.9",
  );
  // macOS 노드는 chassis가 될 수 없다.
  expect(
    within(chassis).getByRole("button", { name: "Install the chassis on node-d" }),
  ).toBeDisabled();
  await expectNoAxeViolations(container, { disable: ["region"] });
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Remove central node-b" }));
  await user.click(await screen.findByRole("button", { name: "Remove" }));
  await waitFor(() => expect(removed).toEqual(["n2"]));
});

it("shows the state read-only to viewers", async () => {
  const a = sdnNode(1, readyChassis(1, "leader"));
  cluster([a], ["n1"]);
  server.use(http.get(apiUrl("/sdn"), () => HttpResponse.json(sdnView([a], [a]))));
  mockLists({ node: [a] });
  renderPage(<SdnPage />, { as: VIEWER });
  expect(await screen.findByRole("table", { name: "Central nodes" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Add a central" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Install on all nodes" })).not.toBeInTheDocument();
});
