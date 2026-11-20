// 노드 상세의 SDN 카드(계획 66): physnet 표와 게이트웨이, `PUT /nodes/{key}`의 `spec.sdn`
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Node, Principal } from "@/api/types";
import { NodeSdnSection, sdnConfigOf } from "@/nodes/NodeSdnSection";
import { readyChassis, sdnNode } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
});

const VIEWER: Principal = { ...ADMIN, name: "view", role: "viewer" };

/** 서버 모양: `spec.sdn`(빈 칸은 빠짐)과 에이전트가 알린 `status.sdn.physnets`·`gateway` */
const GW: Node = (() => {
  const n = sdnNode(1, {
    ...readyChassis(1, "leader"),
    gateway: true,
    physnets: [
      { name: "dmz", bridge: "vmbr2", ready: false, error: "bridge vmbr2 not found" },
      { name: "ext", bridge: "vmbr1", ready: true },
    ],
  });
  return {
    ...n,
    revision: 12,
    labels: { rack: "r1" },
    spec: { ...n.spec, sdn: { physnets: { dmz: "vmbr2", ext: "vmbr1" }, gateway: 200 } },
    status: { ...n.status, bridges: [{ name: "vmbr1", vlan_filtering: false }] },
  } as Node;
})();

it("shows the physnets, their state and the gateway priority", async () => {
  mockLists({ node: [GW] });
  renderPage(<NodeSdnSection node={GW} />);
  const table = await screen.findByRole("table", { name: "SDN physical networks" });
  const ext = within(table).getByText("ext").closest("tr")!;
  expect(within(ext).getByText("ready")).toBeVisible();
  const dmz = within(table).getByText("dmz").closest("tr")!;
  expect(within(dmz).getByText("not ready")).toBeVisible();
  expect(within(dmz).getByText("bridge vmbr2 not found")).toBeVisible();
  expect(screen.getByText("Gateway candidate, priority 200.")).toBeVisible();
});

it("saves the SDN settings with the node it opened and clears them with {}", async () => {
  mockLists({ node: [GW] });
  const sent: { ifMatch: string | null; body: { labels: unknown; spec: Node["spec"] } }[] = [];
  server.use(
    http.put(apiUrl(`/nodes/${GW.id}`), async ({ request }) => {
      const body = (await request.json()) as (typeof sent)[number]["body"];
      sent.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({ ...GW, revision: 13, spec: body.spec });
    }),
  );
  const user = userEvent.setup();
  renderPage(<NodeSdnSection node={GW} />);
  await user.click(await screen.findByRole("button", { name: "Edit" }));
  let dialog = await screen.findByRole("dialog", { name: "SDN settings of node-a" });
  // dmz를 빼고 우선순위를 바꾼다.
  await user.click(within(dialog).getByRole("button", { name: "Remove physical network 1" }));
  await user.clear(within(dialog).getByLabelText("Gateway priority"));
  await user.type(within(dialog).getByLabelText("Gateway priority"), "150");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({
    ifMatch: "12",
    body: {
      labels: { rack: "r1" },
      spec: { ...GW.spec, sdn: { physnets: { ext: "vmbr1" }, gateway: 150 } },
    },
  });

  await user.click(await screen.findByRole("button", { name: "Edit" }));
  dialog = await screen.findByRole("dialog", { name: "SDN settings of node-a" });
  await user.click(within(dialog).getByRole("button", { name: "Remove physical network 2" }));
  await user.click(within(dialog).getByRole("button", { name: "Remove physical network 1" }));
  await user.click(within(dialog).getByLabelText("Gateway candidate for routers"));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(sent).toHaveLength(2));
  expect(sent[1]!.body.spec.sdn).toEqual({});
});

it("shows which VMs still use a physnet the node would drop", async () => {
  mockLists({ node: [GW] });
  server.use(
    http.put(apiUrl(`/nodes/${GW.id}`), () =>
      HttpResponse.json(
        {
          code: "physnet_in_use",
          message: "VMs web1 run on node node-a with NICs on physnet ext",
          details: { vms: ["web1"] },
        },
        { status: 409 },
      ),
    ),
  );
  const user = userEvent.setup();
  renderPage(<NodeSdnSection node={GW} />);
  await user.click(await screen.findByRole("button", { name: "Edit" }));
  const dialog = await screen.findByRole("dialog", { name: "SDN settings of node-a" });
  await user.click(within(dialog).getByRole("button", { name: "Remove physical network 2" }));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "VMs on this node use that physical network: VMs web1 run on node node-a",
  );
});

it("checks the rows and hides editing from viewers", async () => {
  mockLists({ node: [GW] });
  const user = userEvent.setup();
  const { unmount } = renderPage(<NodeSdnSection node={GW} />);
  await user.click(await screen.findByRole("button", { name: "Edit" }));
  const dialog = await screen.findByRole("dialog", { name: "SDN settings of node-a" });
  await user.click(within(dialog).getByRole("button", { name: "Add a physical network" }));
  await user.type(within(dialog).getByLabelText("Physical network 3"), "ext");
  await user.type(within(dialog).getByLabelText("Bridge 3"), "br-int");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText("This physical network is listed twice.")).toBeVisible();
  expect(
    within(dialog).getByText("Use the name of a Linux bridge (1–15 characters, not br-int)."),
  ).toBeVisible();
  unmount();

  renderPage(<NodeSdnSection node={GW} />, { as: VIEWER });
  await screen.findByRole("table", { name: "SDN physical networks" });
  expect(screen.queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
});

it("does not suggest or accept bridges Orbivisor manages", async () => {
  // 리뷰 1 웹 M2: VXLAN `oplv<vni>`·physnet `oplp<h8>x`·`br-int`은 서버가 400 `invalid_node_sdn`으로 거절한다.
  const node = {
    ...GW,
    status: {
      ...GW.status,
      bridges: ["vmbr1", "oplv100", "oplp1234abcdx", "br-int"].map((name) => ({
        name,
        vlan_filtering: false,
      })),
    },
  } as Node;
  mockLists({ node: [node] });
  const user = userEvent.setup();
  renderPage(<NodeSdnSection node={node} />);
  await user.click(await screen.findByRole("button", { name: "Edit" }));
  await screen.findByRole("dialog", { name: "SDN settings of node-a" });
  const options = [...document.querySelectorAll("datalist option")].map((o) =>
    o.getAttribute("value"),
  );
  expect(options).toEqual(["vmbr1"]);
  for (const bridge of ["oplv100", "oplp1234abcdx", "oplp1234abcdb"]) {
    expect(
      sdnConfigOf({ rows: [{ name: "ext", bridge }], gateway: false, priority: "100" }).errors,
    ).toEqual({ "physnets.0.bridge": "sdn.physnet.errors.bridge" });
  }
});
