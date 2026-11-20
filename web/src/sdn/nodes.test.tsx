// 노드 목록·상세의 SDN 칸과 SDN 보기의 권한(계획 65)
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { Permissions } from "@/auth/permissions";
import { AREAS, viewVisible } from "@/layout/nav";
import { NodeDetailPage } from "@/nodes/NodeDetailPage";
import { NodesPage } from "@/nodes/NodesPage";
import { NODE, readyChassis, sdnNode, sdnTask, sdnView } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

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

it("shows each node's chassis state and central role", async () => {
  cluster();
  const b = sdnNode(2, readyChassis(2, "leader"));
  const c = sdnNode(3, { chassis: "configuring" });
  mockLists({ node: [NODE, b, c] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  await within(table).findByText("node-c");
  const headers = within(table).getAllByRole("columnheader");
  const cell = (name: string) => {
    const row = within(table).getAllByText(name)[0]!.closest("tr")!;
    return within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === "SDN")];
  };
  expect(cell("node-b")).toHaveTextContent("Ready");
  expect(cell("node-b")).toHaveTextContent("central (leader)");
  expect(cell("node-c")).toHaveTextContent("Configuring");
  expect(cell("node-c")).not.toHaveTextContent("central");
  // SDN을 세우지 않은 노드
  expect(cell("node-a")).toHaveTextContent("—");
});

it("shows the SDN state in a node's header only once SDN is set up", async () => {
  cluster();
  mockLists({ node: [sdnNode(1, readyChassis(1, "follower"))], vm: [] });
  renderPage(<NodeDetailPage />, { path: "/nodes/:name", url: "/nodes/node-a" });
  expect(await screen.findByText("central (follower)")).toBeInTheDocument();
});

it("shows the SDN view to users who can read the cluster", () => {
  const view = AREAS.flatMap((a) => a.tabs)
    .flatMap((t) => t.views ?? [])
    .find((v) => v.to === "/sdn")!;
  expect(view.kind).toBe("sdn");
  expect(viewVisible(view, Permissions.fromRole("viewer"))).toBe(true);
  expect(viewVisible(view, Permissions.fromRole("none"))).toBe(false);
});

it("marks a node holding leftover central databases", async () => {
  // 리뷰 2 웹 I-3: 중앙 목록에 없는 노드의 중앙 역할은 "남은 중앙 DB"다.
  cluster();
  const b = sdnNode(2, readyChassis(2, "leader"));
  const c = sdnNode(3, readyChassis(3, "follower"));
  mockLists({ node: [b, c] });
  server.use(
    http.get(apiUrl("/sdn"), () =>
      HttpResponse.json(sdnView([b], [b, c], { strays: [{ node: c.id, name: "node-c" }] })),
    ),
  );
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const row = (await within(table).findByText("node-c")).closest("tr")!;
  await waitFor(() => expect(row).toHaveTextContent("leftover central DB"));
  expect(within(table).getByText("node-b").closest("tr")).toHaveTextContent("central (leader)");
});

it("offers removing an OVN central from the SDN when removing its node is refused", async () => {
  cluster();
  const down = sdnNode(2, readyChassis(2, "leader"), { online: false });
  mockLists({ node: [NODE, down], vm: [] });
  const removed: string[] = [];
  server.use(
    http.delete(apiUrl("/nodes/n2"), () =>
      HttpResponse.json(
        {
          code: "sdn_central_on_node",
          message:
            'node "node-b" is an OVN central (10.0.0.2); remove it with `orbivisorctl sdn central remove node-b` first',
        },
        { status: 409 },
      ),
    ),
    http.delete(apiUrl("/sdn/centrals/:node"), ({ params }) => {
      removed.push(String(params.node));
      return HttpResponse.json(sdnTask("t-kick", "sdn-central-remove", "n1"), { status: 202 });
    }),
  );
  renderPage(<NodeDetailPage />, { path: "/nodes/:name", url: "/nodes/node-b" });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Actions for node-b" }));
  await user.click(await screen.findByRole("menuitem", { name: "Remove from cluster" }));
  const dialog = await screen.findByRole("alertdialog");
  await user.type(within(dialog).getByLabelText("Type node-b to confirm"), "node-b");
  await user.click(within(dialog).getByRole("button", { name: "Remove from cluster" }));
  expect(await within(dialog).findByText(/node-b is an OVN central/)).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Remove it from the SDN" }));
  await waitFor(() => expect(removed).toEqual(["n2"]));
});
