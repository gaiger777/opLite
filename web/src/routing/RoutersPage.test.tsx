import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Principal } from "@/api/types";
import { RoutersPage } from "@/routing/RoutersPage";
import type { Router } from "@/routing/routing";
import { expectNoAxeViolations } from "@/test/axe";
import {
  providerNetwork,
  readyChassis,
  routerView,
  sdnNetwork,
  sdnNode,
  sdnView,
} from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
});

const VIEWER: Principal = { ...ADMIN, name: "view", role: "viewer" };
const N1 = sdnNode(1, readyChassis(1, "leader"));
const EXT = providerNetwork("ext", "ext", [{ cidr: "203.0.113.0/24", gateway: "203.0.113.1" }]);
const T1 = sdnNetwork("t1", [{ cidr: "10.10.0.0/24" }]);
const T2 = sdnNetwork("t2", [{ cidr: "10.20.0.0/24", gateway: "10.20.0.254" }]);
const BARE = sdnNetwork("bare");

/** SDN이 세워진 클러스터 */
function sdnIsReady() {
  server.use(http.get(apiUrl("/sdn"), () => HttpResponse.json(sdnView([N1], [N1]))));
}

/** 서버처럼: 목록(`live=false`)은 NB의 지금 주인을 비우고, `GET /routers/{key}`가 그것을 채운다. 부른 질의를 적는다. */
const asked: string[] = [];
function routers(items: Router[]) {
  server.use(
    http.get(apiUrl("/routers"), ({ request }) => {
      asked.push(`list ${new URL(request.url).search}`);
      const spec = items.map((r) => ({
        ...r,
        hosting: undefined,
        gateways: r.gateways?.map((g) => ({ ...g, active: false })),
      }));
      return HttpResponse.json({ revision: 9, items: spec });
    }),
    http.get(apiUrl("/routers/:key"), ({ params, request }) => {
      asked.push(`get ${String(params.key)} ${new URL(request.url).search}`);
      return HttpResponse.json(items.find((r) => r.id === params.key));
    }),
  );
}

beforeEach(() => {
  asked.length = 0;
});

/** 서버 `router_view`의 모양: 외부·게이트웨이 순위·지금 주인·IPv6 윗단 경로 */
const R1 = routerView(
  "r1",
  {
    external: { network: "ext", ips: ["203.0.113.100", "2001:db8::5"] },
    interfaces: [{ network: "t1" }],
  },
  {
    gateways: [
      { node: "node-a", priority: 32767, node_priority: 200, active: true },
      { node: "node-b", priority: 32766, node_priority: 100, active: false },
    ],
    hosting: "node-a",
    upstream_routes: ["fd00:20::/64 via 2001:db8::5"],
  },
);

it("shows the empty state and blocks creating before SDN is set up", async () => {
  mockLists({ node: [N1] });
  const { container } = renderPage(<RoutersPage />);
  expect(await screen.findByText("No routers")).toBeVisible();
  expect(
    await screen.findByText(
      "Routers and floating IPs need SDN (OVN) set up first, under Networks › SDN.",
    ),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Create a router" })).toBeDisabled();
  await expectNoAxeViolations(container);
});

it("lists routers with the active gateway and opens the drawer", async () => {
  sdnIsReady();
  mockLists({ node: [N1], network: [EXT, T1, T2, BARE] });
  routers([R1, routerView("r2", {}, {})]);
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.patch(apiUrl(`/routers/${R1.id}`), async ({ request }) => {
      sent.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json({ ...R1, revision: 8 });
    }),
  );
  renderPage(<RoutersPage />);
  const row = (await screen.findByRole("button", { name: "Open router r1" })).closest("tr")!;
  // 목록은 NB를 읽지 않는다(리뷰 1 웹 I1): 게이트웨이 칸은 순위가 가장 높은 후보다.
  expect(within(row).getByText("node-a")).toBeVisible();
  expect(asked).toEqual(["list ?live=false"]);
  expect(within(row).getByText("203.0.113.100, 2001:db8::5")).toBeVisible();
  // 인터페이스가 남은 라우터는 지울 수 없다.
  expect(within(row).getByRole("button", { name: "Delete r1" })).toBeDisabled();
  const r2 = screen.getByRole("button", { name: "Open router r2" }).closest("tr")!;
  expect(within(r2).getAllByText("None")[0]).toBeVisible();

  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Open router r1" }));
  const sheet = await screen.findByRole("dialog", { name: "Router r1" });
  const gateways = within(sheet).getByRole("table", { name: "Gateway nodes" });
  expect(within(gateways).getAllByRole("row")).toHaveLength(3);
  // 지금 주인은 연 서랍의 라우터 하나만 NB까지 읽어 보인다.
  expect(await within(gateways).findByText("active")).toBeVisible();
  expect(asked).toContain(`get ${R1.id} `);
  expect(asked.filter((a) => a.startsWith("get"))).toHaveLength(1);
  expect(within(sheet).getByText("fd00:20::/64 via 2001:db8::5")).toBeVisible();
  // 붙일 수 있는 것: 서브넷 있는 오버레이이고 다른 라우터에 없는 것(t2). 공급자·서브넷 없는 것은 빠진다.
  const add = within(sheet).getByLabelText("Add an interface");
  expect(
    within(add)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["t2"]);
  await user.click(within(sheet).getByRole("button", { name: "Add" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({
    ifMatch: "7",
    body: { interfaces: [{ network: "t1" }, { network: "t2" }] },
  });
});

it("warns when no gateway node has the physnet and shows server errors", async () => {
  sdnIsReady();
  mockLists({ node: [N1], network: [EXT, T1] });
  const lonely = routerView(
    "r1",
    { external: { network: "ext", ips: ["203.0.113.100"] }, interfaces: [{ network: "t1" }] },
    { gateway_error: "no gateway node has physnet ext ready" },
  );
  routers([lonely]);
  server.use(
    http.patch(apiUrl(`/routers/${lonely.id}`), () =>
      HttpResponse.json(
        {
          code: "fip_in_use",
          message:
            'floating IPs 203-0-113-120 are routed through router "r1" this way; detach them first',
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<RoutersPage />);
  const row = (await screen.findByRole("button", { name: "Open router r1" })).closest("tr")!;
  expect(within(row).getByText("No gateway node")).toBeVisible();
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Open router r1" }));
  const sheet = await screen.findByRole("dialog", { name: "Router r1" });
  expect(within(sheet).getAllByRole("alert")[0]).toHaveTextContent(
    /no gateway node has physnet ext ready\. The router's outside port stays off/,
  );
  await user.click(within(sheet).getByRole("button", { name: "Remove interface t1" }));
  expect(
    await within(sheet).findByText(/A floating IP is in the way: floating IPs 203-0-113-120/),
  ).toBeVisible();
});

it("adds a static route after checking it", async () => {
  sdnIsReady();
  mockLists({ node: [N1], network: [EXT, T1] });
  routers([R1]);
  const sent: unknown[] = [];
  server.use(
    http.patch(apiUrl(`/routers/${R1.id}`), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(R1);
    }),
  );
  renderPage(<RoutersPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Open router r1" }));
  const sheet = await screen.findByRole("dialog", { name: "Router r1" });
  await user.type(within(sheet).getByLabelText("Destination"), "0.0.0.0/0");
  await user.type(within(sheet).getByLabelText("Next hop"), "10.10.0.254");
  await user.click(within(sheet).getByRole("button", { name: "Add a route" }));
  expect(
    within(sheet).getByText("With an external network the default route is set automatically."),
  ).toBeVisible();
  await user.clear(within(sheet).getByLabelText("Destination"));
  await user.type(within(sheet).getByLabelText("Destination"), "10.30.0.0/16");
  await user.click(within(sheet).getByRole("button", { name: "Add a route" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({ routes: [{ destination: "10.30.0.0/16", nexthop: "10.10.0.254" }] });
});

it("creates a router with an external network, SNAT off and interfaces", async () => {
  sdnIsReady();
  mockLists({ node: [N1], network: [EXT, T1, T2] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/routers"), async ({ request }) => {
      body = await request.json();
      // 서버는 리소스(보기가 아님)를 201로 돌려주고, 게이트웨이 후보가 없으면 경고 헤더를 단다.
      return HttpResponse.json(routerView("r9", { external: { network: "ext", snat: false } }), {
        status: 201,
        headers: { "orbivisor-warning": "no gateway node has physnet ext ready" },
      });
    }),
  );
  renderPage(<RoutersPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Create a router" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a router" });
  await user.type(within(dialog).getByLabelText("Name"), "r9");
  await user.selectOptions(within(dialog).getByLabelText("External network"), "ext");
  await user.click(within(dialog).getByLabelText(/SNAT/));
  await user.click(within(dialog).getByRole("checkbox", { name: "t2" }));
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  await waitFor(() => expect(body).toBeDefined());
  expect(body).toEqual({
    name: "r9",
    spec: { external: { network: "ext", snat: false }, interfaces: [{ network: "t2" }] },
  });
  expect(await screen.findByText("no gateway node has physnet ext ready")).toBeVisible();
});

it("hides changes from viewers", async () => {
  sdnIsReady();
  mockLists({ node: [N1], network: [EXT, T1] });
  routers([R1]);
  renderPage(<RoutersPage />, { as: VIEWER });
  await screen.findByRole("button", { name: "Open router r1" });
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete r1" })).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Open router r1" }));
  const sheet = await screen.findByRole("dialog", { name: "Router r1" });
  expect(within(sheet).queryByLabelText("Add an interface")).not.toBeInTheDocument();
  expect(within(sheet).queryByRole("button", { name: "Change" })).not.toBeInTheDocument();
});

it("offers only provider networks with a subnet as externals and only networks the user configures as interfaces", async () => {
  // 리뷰 1 웹 M3: 서브넷 없는 공급자는 서버가 400 invalid_router로 거절한다.
  // 리뷰 1 #1: 인터페이스는 그 네트워크의 network.config가 있어야 한다(network.assign만으로는 안 된다).
  sdnIsReady();
  const EMPTY_EXT = providerNetwork("ext2", "ext");
  const SHARED = { ...sdnNetwork("shared", [{ cidr: "10.40.0.0/24" }]), id: "net-shared" };
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: ["system.read"],
        root_contents: ["system.read", "network.config", "network.assign"],
        folders: {},
        // 다른 폴더의 네트워크는 붙이기(network.assign)만 받았다.
        contents: { "f-other": ["system.read", "network.assign"] },
        objects: [],
      }),
    ),
  );
  const NOT_MINE = { ...SHARED, spec: { ...SHARED.spec, folder: "f-other" } };
  mockLists({ node: [N1], network: [EXT, EMPTY_EXT, T1, NOT_MINE] });
  routers([]);
  renderPage(<RoutersPage />, { as: { ...ADMIN, role: "none" } });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Create a router" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a router" });
  expect(
    within(within(dialog).getByLabelText("External network"))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["None", "ext"]);
  expect(within(dialog).getByRole("checkbox", { name: "t1" })).toBeVisible();
  expect(within(dialog).queryByRole("checkbox", { name: "shared" })).not.toBeInTheDocument();
});

it("shows a placeholder when the server hides gateway node names", async () => {
  sdnIsReady();
  mockLists({ node: [N1], network: [EXT, T1, T2, BARE] });
  // 루트 cluster.read가 없는 사용자에게 서버는 노드 이름을 비운다(리뷰 1 M-4).
  const hidden = {
    ...R1,
    hosting: "",
    gateways: R1.gateways?.map((g) => ({ ...g, node: "" })),
  };
  routers([hidden]);
  renderPage(<RoutersPage />);
  const row = (await screen.findByRole("button", { name: "Open router r1" })).closest("tr")!;
  expect(within(row).getByText("(node name hidden)")).toBeVisible();
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Open router r1" }));
  const sheet = await screen.findByRole("dialog", { name: "Router r1" });
  const gateways = within(sheet).getByRole("table", { name: "Gateway nodes" });
  expect(within(gateways).getAllByText("(node name hidden)")).toHaveLength(2);
});
