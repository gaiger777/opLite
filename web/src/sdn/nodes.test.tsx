// 노드 목록·상세의 SDN 칸과 SDN 보기의 권한(계획 65)
import { screen, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { Permissions } from "@/auth/permissions";
import { AREAS, viewVisible } from "@/layout/nav";
import { NodeDetailPage } from "@/nodes/NodeDetailPage";
import { NodesPage } from "@/nodes/NodesPage";
import { NODE, readyChassis, sdnNode } from "@/test/fixtures";
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
