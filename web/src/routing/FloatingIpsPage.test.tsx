import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Vm } from "@/api/types";
import { FloatingIpsPage } from "@/routing/FloatingIpsPage";
import { expectNoAxeViolations } from "@/test/axe";
import {
  floatingIpView,
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
const EXT = providerNetwork("ext", "ext", [
  {
    cidr: "203.0.113.0/24",
    gateway: "203.0.113.1",
    floating_pools: [{ start: "203.0.113.100", end: "203.0.113.199" }],
  },
]);
const T1 = sdnNetwork("t1", [{ cidr: "10.10.0.0/24" }]);
const T2 = sdnNetwork("t2", [{ cidr: "10.20.0.0/24" }]);
const R1 = routerView(
  "r1",
  { external: { network: "ext", ips: ["203.0.113.100"] }, interfaces: [{ network: "t1" }] },
  { gateways: [{ node: "node-a", priority: 32767, node_priority: 100, active: true }] },
);

function withNics(v: Vm, nics: Vm["spec"]["nics"]): Vm {
  return { ...v, spec: { ...v.spec, nics } };
}

const WEB = withNics(vm("web", "running"), [
  { network: "t1", model: "virtio", mac: "52:54:00:00:00:01", ips: ["10.10.0.2"] },
  { network: "t2", model: "virtio", mac: "52:54:00:00:00:02", ips: ["10.20.0.2"] },
]);
const DB = withNics(vm("db", "stopped"), [
  { network: "t1", model: "virtio", mac: "52:54:00:00:00:03", ips: ["10.10.0.3"] },
]);

function lists(fips: unknown[]) {
  server.use(
    http.get(apiUrl("/sdn"), () => HttpResponse.json(sdnView([N1], [N1]))),
    http.get(apiUrl("/routers"), () => HttpResponse.json({ revision: 9, items: [R1] })),
    http.get(apiUrl("/floating-ips"), () => HttpResponse.json({ revision: 9, items: fips })),
  );
}

it("shows the empty state", async () => {
  mockLists({ node: [N1] });
  const { container } = renderPage(<FloatingIpsPage />);
  expect(await screen.findByText("No floating IPs")).toBeVisible();
  await expectNoAxeViolations(container);
});

it("lists floating IPs with their target and state", async () => {
  mockLists({ node: [N1], network: [EXT, T1, T2], vm: [WEB, DB] });
  lists([
    floatingIpView(
      "203.0.113.120",
      "ext",
      { kind: "distributed", node: "node-a" },
      { target: { vm: WEB.id, mac: "52:54:00:00:00:01" }, vm_name: "web" },
    ),
    // 서버는 붙은 유동 IP만 중앙·분산으로 센다(꺼진 VM이면 중앙).
    floatingIpView(
      "203.0.113.121",
      "ext",
      { kind: "centralized", reason: "the VM is not running" },
      { target: { vm: DB.id, mac: "52:54:00:00:00:03" }, vm_name: "db" },
    ),
    floatingIpView("203.0.113.123", "ext"),
    // 다른 폴더의 VM(서버가 `vm_name`을 주지 않음)
    floatingIpView(
      "203.0.113.122",
      "ext",
      { kind: "unrouted", reason: "the VM no longer exists" },
      { target: { vm: "019a0f3c-0000-7000-8000-00000000ffff", mac: "52:54:00:00:00:09" } },
    ),
  ]);
  renderPage(<FloatingIpsPage />);
  const first = (await screen.findByText("203.0.113.120")).closest("tr")!;
  expect(within(first).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(within(first).getByText("distributed · node-a")).toBeVisible();
  expect(within(first).getByRole("button", { name: "Detach 203-0-113-120" })).toBeVisible();
  const second = screen.getByText("203.0.113.121").closest("tr")!;
  expect(within(second).getByText("centralized")).toBeVisible();
  expect(within(second).getByText("the VM is not running")).toBeVisible();
  const free = screen.getByText("203.0.113.123").closest("tr")!;
  expect(within(free).getAllByText("not attached")).toHaveLength(2);
  expect(within(free).getByRole("button", { name: "Attach 203-0-113-123" })).toBeVisible();
  const third = screen.getByText("203.0.113.122").closest("tr")!;
  expect(within(third).getByText("a VM you cannot see")).toBeVisible();
  expect(within(third).getByText("not routed")).toBeVisible();
});

it("attaches only to routed interfaces and says why others are blocked", async () => {
  mockLists({ node: [N1], network: [EXT, T1, T2], vm: [WEB, DB] });
  const free = floatingIpView("203.0.113.121", "ext");
  lists([
    floatingIpView(
      "203.0.113.120",
      "ext",
      { kind: "centralized", reason: "the VM is not running" },
      { target: { vm: DB.id, mac: "52:54:00:00:00:03" }, vm_name: "db" },
    ),
    free,
  ]);
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.patch(apiUrl(`/floating-ips/${free.id}`), async ({ request }) => {
      sent.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json(free);
    }),
  );
  renderPage(<FloatingIpsPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Attach 203-0-113-121" }));
  const dialog = await screen.findByRole("dialog", { name: "Attach 203.0.113.121" });
  const nic = within(dialog).getByLabelText("Interface");
  await within(nic).findByRole("option", { name: "web #1 (t1)" });
  expect(within(nic).getByRole("option", { name: "web #1 (t1)" })).toBeEnabled();
  expect(
    within(nic).getByRole("option", { name: "web #2 (t2) — t2 is not on a router to ext" }),
  ).toBeDisabled();
  expect(
    within(nic).getByRole("option", { name: "db #1 (t1) — has 203.0.113.120" }),
  ).toBeDisabled();
  await user.selectOptions(nic, `${WEB.id}/52:54:00:00:00:01`);
  await user.click(within(dialog).getByRole("button", { name: "Attach" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({
    ifMatch: "8",
    body: { target: { vm: WEB.id, mac: "52:54:00:00:00:01" } },
  });
});

it("creates a floating IP from an external network", async () => {
  mockLists({ node: [N1], network: [EXT, T1], vm: [WEB] });
  lists([]);
  let body: unknown;
  server.use(
    http.post(apiUrl("/floating-ips"), async ({ request }) => {
      body = await request.json();
      // 서버는 리소스(보기가 아님)를 201로 돌려준다 — 주소는 서버가 고른다.
      const { id, name, revision, spec, status } = floatingIpView("203.0.113.101", "ext");
      return HttpResponse.json({ id, name, revision, spec, status }, { status: 201 });
    }),
  );
  renderPage(<FloatingIpsPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Create a floating IP" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a floating IP" });
  expect(within(dialog).getByLabelText("External network")).toHaveValue("ext");
  await user.selectOptions(within(dialog).getByLabelText("Attach to (optional)"), [
    `${WEB.id}/52:54:00:00:00:01`,
  ]);
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  await waitFor(() => expect(body).toBeDefined());
  expect(body).toEqual({
    spec: { network: "ext", target: { vm: WEB.id, mac: "52:54:00:00:00:01" } },
  });
  expect(await screen.findByText("Created floating IP 203.0.113.101.")).toBeVisible();
});
