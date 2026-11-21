import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Vm } from "@/api/types";
import { LoadBalancersPage } from "@/lb/LoadBalancersPage";
import { expectNoAxeViolations } from "@/test/axe";
import {
  lbView,
  providerNetwork,
  readyChassis,
  routerView,
  sdnNetwork,
  sdnNode,
  sdnView,
  vm,
} from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
});

const N1 = sdnNode(1, readyChassis(1, "leader"));
const EXT = providerNetwork("ext", "ext", [{ cidr: "203.0.113.0/24", gateway: "203.0.113.1" }]);
const T1 = sdnNetwork("t1", [{ cidr: "10.10.0.0/24" }]);
const T2 = sdnNetwork("t2", [{ cidr: "10.20.0.0/24" }]);
const R1 = routerView("r1", {
  external: { network: "ext", ips: ["203.0.113.100"] },
  interfaces: [{ network: "t1" }, { network: "t2" }],
});

function withNics(v: Vm, nics: Vm["spec"]["nics"], template = false): Vm {
  return { ...v, spec: { ...v.spec, nics, ...(template && { template }) } };
}
const WEB = withNics(vm("web", "running"), [
  { network: "t1", model: "virtio", mac: "52:54:00:00:00:01", ips: ["10.10.0.2"] },
]);
const API = withNics(vm("api", "running"), [
  { network: "t2", model: "virtio", mac: "52:54:00:00:00:02", ips: ["10.20.0.2"] },
]);
const BARE = withNics(vm("bare", "stopped"), [
  { network: "t1", model: "virtio", mac: "52:54:00:00:00:03" },
]);
const GOLD = withNics(
  vm("gold", "stopped"),
  [{ network: "t1", model: "virtio", mac: "52:54:00:00:00:04", ips: ["10.10.0.4"] }],
  true,
);

/** 서버 모양의 부하 분산기 하나: 리스너 tcp/80(건강 검사), 멤버 셋(살아 있음·죽음+보안 그룹 경고·닿지 않음) */
const WEB_LB = lbView(
  "web-lb",
  {
    network: "t1",
    vip: "10.10.0.50",
    external: { network: "ext", ip: "203.0.113.130" },
    listeners: [
      {
        protocol: "tcp",
        port: 80,
        members: [
          { vm: WEB.id, mac: "52:54:00:00:00:01", port: 8080 },
          { vm: API.id, mac: "52:54:00:00:00:02", port: 8080 },
          { ip: "10.10.0.9", port: 8080 },
        ],
        health: { interval_secs: 5, timeout_secs: 3, rise: 2, fall: 3 },
      },
    ],
    monitors: [
      { network: "t1", ip: "10.10.0.60" },
      { network: "t2", ip: "10.20.0.60" },
    ],
  },
  {
    external_state: { state: "unrouted", reason: "no gateway node has physnet ext ready" },
    members: [
      {
        listener: "tcp/80",
        vm: WEB.id,
        vm_name: "web",
        mac: "52:54:00:00:00:01",
        ip: "10.10.0.2",
        port: 8080,
        health: "online",
      },
      {
        listener: "tcp/80",
        vm: API.id,
        vm_name: "api",
        mac: "52:54:00:00:00:02",
        ip: "10.20.0.2",
        port: 8080,
        health: "offline",
        warning:
          "the security groups of the member NIC 52:54:00:00:00:02 block health checks from 10.20.0.60 to tcp/8080",
      },
      {
        listener: "tcp/80",
        ip: "10.10.0.9",
        port: 8080,
        health: "missing",
        reason: 'address 10.10.0.9 is not in a subnet of an interface of router "r1"',
      },
    ],
  },
);

function ready(lbs: unknown[]) {
  server.use(
    http.get(apiUrl("/sdn"), () => HttpResponse.json(sdnView([N1], [N1]))),
    http.get(apiUrl("/routers"), () =>
      HttpResponse.json({
        revision: 9,
        items: [R1],
        routed: [
          { network: "t1", external: "ext" },
          { network: "t2", external: "ext" },
        ],
      }),
    ),
    http.get(apiUrl("/load-balancers"), () => HttpResponse.json({ revision: 9, items: lbs })),
  );
}

it("shows the empty state before SDN is set up", async () => {
  mockLists({ node: [N1] });
  const { container } = renderPage(<LoadBalancersPage />);
  expect(await screen.findByText("No load balancers")).toBeVisible();
  expect(
    await screen.findByText("Load balancers need SDN (OVN) set up first, under Networks › SDN."),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Create a load balancer" })).toBeDisabled();
  await expectNoAxeViolations(container);
});

it("lists load balancers with a member health bar and opens the drawer", async () => {
  mockLists({ node: [N1], network: [EXT, T1, T2], vm: [WEB, API] });
  ready([WEB_LB]);
  const user = userEvent.setup();
  const { container } = renderPage(<LoadBalancersPage />);
  const row = (await screen.findByRole("button", { name: "Open load balancer web-lb" })).closest(
    "tr",
  )!;
  expect(within(row).getByText("10.10.0.50")).toBeVisible();
  expect(within(row).getByText("203.0.113.130")).toBeVisible();
  expect(within(row).getByText("tcp/80")).toBeVisible();
  expect(
    within(row).getByRole("img", {
      name: "1 up · 1 down · 0 unchecked · 1 missing (of 3)",
    }),
  ).toBeVisible();
  await expectNoAxeViolations(container);

  await user.click(within(row).getByRole("button", { name: "Open load balancer web-lb" }));
  const sheet = await screen.findByRole("dialog", { name: "Load balancer web-lb" });
  expect(within(sheet).getByText("not reachable")).toBeVisible();
  // 서버의 영어 까닭·경고는 아는 꼴이면 번역한다.
  expect(within(sheet).getByText("No gateway node has physical network ext ready")).toBeVisible();
  expect(
    within(sheet).getByText(
      "the security groups of member NIC 52:54:00:00:00:02 block health checks from 10.20.0.60 to tcp/8080",
    ),
  ).toBeVisible();
  const table = within(sheet).getByRole("table", { name: "Listener tcp/80" });
  expect(within(table).getByText("up")).toBeVisible();
  expect(within(table).getByText("down")).toBeVisible();
  expect(within(table).getByText("missing")).toBeVisible();
  expect(within(table).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(within(sheet).getByText("10.10.0.60 (t1), 10.20.0.60 (t2)")).toBeVisible();
  expect(
    within(sheet).getByText("Health check every 5 s, timeout 3 s, up after 2, down after 3"),
  ).toBeVisible();
});

it("creates a load balancer with VM members and says why other interfaces are blocked", async () => {
  mockLists({ node: [N1], network: [EXT, T1, T2], vm: [WEB, API, BARE, GOLD] });
  ready([WEB_LB]);
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl("/load-balancers"), async ({ request }) => {
      const body = (await request.json()) as { name: string; spec: Record<string, unknown> };
      sent.push(body);
      return HttpResponse.json(
        {
          id: "019a0f3c-0000-7000-800c-00000000000f",
          name: body.name,
          revision: 3,
          spec: { ...body.spec, vip: "10.10.0.51" },
          status: {},
        },
        {
          status: 201,
          headers: {
            "orbivisor-warning":
              "the security groups of the member NIC 52:54:00:00:00:02 do not admit tcp/80 from clients",
          },
        },
      );
    }),
  );
  const user = userEvent.setup();
  renderPage(<LoadBalancersPage />);
  await user.click(await screen.findByRole("button", { name: "Create a load balancer" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a load balancer" });
  await user.type(within(dialog).getByLabelText("Name"), "app-lb");
  expect(within(dialog).getByLabelText("VIP network")).toHaveValue("t1");
  const pick = within(dialog).getByLabelText("VM interface");
  // 라우터의 다른 인터페이스(t2)의 NIC도 된다. 템플릿·IPv4 없는 NIC는 까닭과 함께 막힌다.
  expect(within(pick).getByRole("option", { name: "api #1 (t2, 10.20.0.2)" })).toBeEnabled();
  expect(
    within(pick).getByRole("option", { name: "bare #1 (t1, —) — no IPv4 address on t1" }),
  ).toBeDisabled();
  expect(
    within(pick).getByRole("option", { name: "gold #1 (t1, 10.10.0.4) — a template" }),
  ).toBeDisabled();
  await user.selectOptions(pick, "api #1 (t2, 10.20.0.2)");
  await user.click(within(dialog).getByRole("button", { name: "Add" }));
  await user.clear(within(dialog).getByLabelText("Port"));
  await user.type(within(dialog).getByLabelText("Port"), "8080");
  await user.click(within(dialog).getByRole("checkbox", { name: /External VIP on ext/ }));
  await user.type(within(dialog).getByLabelText("Source affinity in seconds (optional)"), "60");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({
    name: "app-lb",
    spec: {
      network: "t1",
      listeners: [
        {
          protocol: "tcp",
          port: 80,
          members: [{ vm: API.id, mac: "52:54:00:00:00:02", port: 8080 }],
          health: { interval_secs: 5, timeout_secs: 3, rise: 2, fall: 3 },
        },
      ],
      external: { network: "ext" },
      affinity_secs: 60,
    },
  });
  expect(
    await screen.findByText("Created load balancer app-lb with VIP 10.10.0.51."),
  ).toBeInTheDocument();
  expect(
    await screen.findByText(
      "the security groups of member NIC 52:54:00:00:00:02 do not let clients reach tcp/80",
    ),
  ).toBeInTheDocument();
});

it("checks listeners before sending", async () => {
  mockLists({ node: [N1], network: [EXT, T1, T2], vm: [WEB] });
  ready([WEB_LB]);
  const user = userEvent.setup();
  renderPage(<LoadBalancersPage />);
  await user.click(await screen.findByRole("button", { name: "Create a load balancer" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a load balancer" });
  await user.type(within(dialog).getByLabelText("Name"), "x");
  await user.click(within(dialog).getByRole("button", { name: "Add a listener" }));
  const second = within(dialog).getByRole("group", { name: "Listener 2" });
  await user.type(within(second).getByLabelText("VIP port"), "80");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(
    await within(dialog).findByText("This protocol and port is already a listener."),
  ).toBeVisible();
});

it("edits only the changed fields with If-Match and deletes", async () => {
  mockLists({ node: [N1], network: [EXT, T1, T2], vm: [WEB, API] });
  ready([WEB_LB]);
  const sent: { method: string; ifMatch: string | null; body?: unknown }[] = [];
  server.use(
    http.patch(apiUrl(`/load-balancers/${WEB_LB.id}`), async ({ request }) => {
      sent.push({
        method: "PATCH",
        ifMatch: request.headers.get("If-Match"),
        body: await request.json(),
      });
      // 서버는 리소스(보기 아님)를 돌려준다.
      return HttpResponse.json({
        id: WEB_LB.id,
        name: WEB_LB.name,
        revision: 10,
        spec: WEB_LB.spec,
        status: {},
      });
    }),
    http.delete(apiUrl(`/load-balancers/${WEB_LB.id}`), ({ request }) => {
      sent.push({ method: "DELETE", ifMatch: request.headers.get("If-Match") });
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<LoadBalancersPage />);
  await user.click(await screen.findByRole("button", { name: "Edit web-lb" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit load balancer web-lb" });
  expect(within(dialog).getByLabelText("VIP network")).toBeDisabled();
  await user.type(within(dialog).getByLabelText("Description (optional)"), "front");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({
    method: "PATCH",
    ifMatch: "9",
    body: {
      description: "front",
      external: { network: "ext", ip: "203.0.113.130" },
      listeners: [
        {
          protocol: "tcp",
          port: 80,
          members: [
            { vm: WEB.id, mac: "52:54:00:00:00:01", port: 8080 },
            { vm: API.id, mac: "52:54:00:00:00:02", port: 8080 },
            { ip: "10.10.0.9", port: 8080 },
          ],
          health: { interval_secs: 5, timeout_secs: 3, rise: 2, fall: 3 },
        },
      ],
      affinity_secs: null,
    },
  });
  await user.click(await screen.findByRole("button", { name: "Delete web-lb" }));
  await user.click(await screen.findByRole("button", { name: "Delete" }));
  await waitFor(() => expect(sent).toHaveLength(2));
  expect(sent[1]).toEqual({ method: "DELETE", ifMatch: "9" });
});
