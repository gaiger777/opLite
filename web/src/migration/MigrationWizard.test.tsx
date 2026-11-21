import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Network, Vm } from "@/api/types";
import { planText } from "@/migration/migration";
import { MigrationWizard } from "@/migration/MigrationWizard";
import { NetworksPage } from "@/networks/NetworksPage";
import i18n from "@/i18n";
import { expectNoAxeViolations } from "@/test/axe";
import {
  migrationRes,
  readyChassis,
  routerView,
  sdnNetwork,
  sdnNode,
  sdnTask,
  sdnView,
  vm,
} from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
});

const N1 = sdnNode(1, readyChassis(1, "leader"), {
  bridges: [{ name: "br0", vlan_filtering: false }],
});
/** 단순 브리지 네트워크(서버 모양: `vlan`이 없으면 빠진다) */
const LAN = {
  id: "019a0f3c-0000-7000-8003-000000000lan",
  name: "lan",
  revision: 3,
  spec: { bridge: "br0" },
  status: {},
} as unknown as Network;
const TLAN = sdnNetwork("t-lan", [{ cidr: "192.168.10.0/24", gateway: "192.168.10.1" }]);

function onLan(v: Vm, mac: string): Vm {
  return {
    ...v,
    spec: { ...v.spec, nics: [{ network: "lan", model: "virtio", mac }] },
    status: { ...v.status, node: N1.id },
  };
}
const WEB = onLan(vm("web", "running"), "52:54:00:00:00:01");
const DB = onLan(vm("db", "stopped"), "52:54:00:00:00:02");

/** 서버 `MigrationPlan`(빈 칸 없이 — `mac`·`bridge.node`·`bridge.error`는 없으면 null) */
const PLAN = {
  nics: [
    {
      vm: WEB.id,
      name: "web",
      nic: 0,
      mac: "52:54:00:00:00:01",
      mode: "live",
      ips: ["192.168.10.5"],
      address_source: "guest-agent",
      blockers: [],
      warnings: [
        "the cloud-init instance-id changes: on its next boot the guest runs its per-instance setup again (for example new SSH host keys)",
        "the NIC has no security groups: traffic is not filtered on SDN either",
        "about a second without network",
      ],
    },
    {
      vm: DB.id,
      name: "db",
      nic: 0,
      mac: "52:54:00:00:00:02",
      mode: "blocked",
      ips: ["192.168.10.5"],
      address_source: "cloud-init",
      blockers: ["address 192.168.10.5 is also planned for another NIC"],
      warnings: [],
    },
  ],
  bridge: {
    needed: true,
    node: N1.id,
    error: null,
    warnings: [
      "a DHCP server on the source network also answers moved VMs over the bridge: reserve the same addresses there or turn it off while migrating",
    ],
  },
};

function cluster(migrations: unknown[] = []) {
  mockLists({ node: [N1], network: [LAN, TLAN], vm: [WEB, DB] });
  server.use(
    http.get(apiUrl("/sdn"), () => HttpResponse.json(sdnView([N1], [N1]))),
    http.get(apiUrl("/network-migrations"), () =>
      HttpResponse.json({ revision: 12, items: migrations }),
    ),
  );
}

it("plans, shows blockers and the cloud-init warning, then creates and starts", async () => {
  cluster();
  const sent: { url: string; body: unknown }[] = [];
  let created = false;
  const RES = migrationRes(
    "lan-to-t-lan",
    { source: "lan", target: "t-lan", vms: [WEB.id] },
    {
      phase: "planned",
      vms: [
        {
          vm: WEB.id,
          nic: 0,
          mac: "52:54:00:00:00:01",
          mode: "live",
          ips: ["192.168.10.5"],
          address_source: "guest-agent",
          from: { network: "lan" },
          state: "pending",
        },
      ],
    },
  );
  server.use(
    http.post(apiUrl("/network-migrations"), async ({ request }) => {
      const url = new URL(request.url);
      sent.push({ url: url.search, body: await request.json() });
      if (url.searchParams.get("dry_run") === "true") {
        return HttpResponse.json(PLAN);
      }
      created = true;
      return HttpResponse.json(
        { ...RES, plan: PLAN },
        { status: 201, headers: { "orbivisor-warning": "3" } },
      );
    }),
    http.post(apiUrl(`/network-migrations/${RES.id}/start`), async ({ request }) => {
      sent.push({ url: "start", body: await request.json() });
      return HttpResponse.json(sdnTask("t1", "sdn-init", N1.id), { status: 202 });
    }),
    http.get(apiUrl("/network-migrations"), () =>
      HttpResponse.json({
        revision: 13,
        items: created ? [{ ...RES, status: { ...RES.status, phase: "running" } }] : [],
      }),
    ),
  );
  const user = userEvent.setup();
  renderPage(<NetworksPage />);
  await user.click(await screen.findByRole("button", { name: "Migrate lan to SDN" }));
  const sheet = await screen.findByRole("dialog", { name: "Migrate lan to SDN" });
  expect(within(sheet).getByLabelText("SDN network")).toHaveValue("t-lan");
  await user.click(within(sheet).getByRole("button", { name: "Make the plan" }));

  // ② 계획 표
  expect(await within(sheet).findByRole("alert")).toHaveTextContent(
    "Some VMs use cloud-init: their instance-id changes",
  );
  const table = within(sheet).getByRole("table", { name: "Migration plan" });
  const web = within(table).getByText("web").closest("tr")!;
  expect(within(web).getByText("live")).toBeVisible();
  expect(within(web).getByText("from the guest agent")).toBeVisible();
  expect(within(web).getByText("about a second without network")).toBeVisible();
  const db = within(table).getByText("db").closest("tr")!;
  expect(within(db).getByText("blocked")).toBeVisible();
  expect(within(db).getByText("192.168.10.5 is also planned for another interface")).toBeVisible();
  expect(within(sheet).getByText("Bridge on node-a")).toBeVisible();
  // 막힌 VM은 빼고 다시 계획한다(`vms`)
  await user.click(within(db).getByRole("checkbox", { name: "Move db" }));
  await user.click(within(sheet).getByRole("button", { name: "Plan again" }));
  await waitFor(() => expect(sent).toHaveLength(2));
  await user.click(within(sheet).getByRole("button", { name: "Create and start (1 interface)" }));
  await waitFor(() => expect(sent).toHaveLength(4));
  expect(sent).toEqual([
    {
      url: "?dry_run=true",
      body: { name: "lan-to-t-lan", spec: { source: "lan", target: "t-lan" } },
    },
    {
      url: "?dry_run=true",
      body: { name: "lan-to-t-lan", spec: { source: "lan", target: "t-lan", vms: [WEB.id] } },
    },
    {
      url: "",
      body: { name: "lan-to-t-lan", spec: { source: "lan", target: "t-lan", vms: [WEB.id] } },
    },
    { url: "start", body: {} },
  ]);
  // ③ 진행
  expect(await within(sheet).findByText("running")).toBeVisible();
  expect(within(sheet).getByText("0 of 1 moved")).toBeVisible();
});

it("creates a new SDN network with the same subnet before planning", async () => {
  cluster();
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl("/networks"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json({ ...TLAN, name: "lan-sdn" }, { status: 201 });
    }),
    http.post(apiUrl("/network-migrations"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json({
        nics: [],
        bridge: {
          needed: true,
          node: null,
          error: 'no SDN chassis has the source bridge "br0" to lay the migration bridge on',
          warnings: [],
        },
      });
    }),
  );
  const user = userEvent.setup();
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  await user.click(await screen.findByLabelText("A new SDN network with the same subnet"));
  await user.type(screen.getByLabelText("Subnet (CIDR)"), "192.168.10.0/24");
  await user.type(screen.getByLabelText("Gateway (optional)"), "192.168.10.1");
  await user.selectOptions(screen.getByLabelText("Mode"), "Live");
  await user.click(screen.getByRole("button", { name: "Make the plan" }));
  await waitFor(() => expect(sent).toHaveLength(2));
  expect(sent[0]).toEqual({
    name: "lan-sdn",
    spec: { sdn: {}, subnets: [{ cidr: "192.168.10.0/24", gateway: "192.168.10.1" }] },
  });
  expect(sent[1]).toEqual({
    name: "lan-to-lan-sdn",
    spec: { source: "lan", target: "lan-sdn", mode: "live" },
  });
  // 다리를 놓을 수 없으면 시작할 수 없다.
  expect(await screen.findByText("no OVN node has the source bridge br0")).toBeVisible();
  expect(screen.getByRole("button", { name: "Create and start (0 interfaces)" })).toBeDisabled();
});

it("pauses, rolls back chosen VMs and finishes with a router", async () => {
  const MOVED = {
    vm: WEB.id,
    nic: 0,
    mac: "52:54:00:00:00:01",
    mode: "live",
    ips: ["192.168.10.5"],
    address_source: "given",
    from: { network: "lan", security_groups: ["web"] },
    state: "moved",
    at: 1795107585,
  };
  const PENDING = {
    ...MOVED,
    vm: DB.id,
    mac: "52:54:00:00:00:02",
    mode: "offline",
    state: "pending",
    at: undefined,
  };
  const running = migrationRes(
    "lan-to-t-lan",
    { source: "lan", target: "t-lan" },
    {
      phase: "running",
      vms: [MOVED, PENDING],
      bridge: { node: N1.id, physnet: "mig0a1b2c3d" },
      task: "019a0f3c-0000-7000-8000-00000000aaaa",
    },
  );
  cluster([running]);
  server.use(
    http.get(apiUrl("/routers"), () =>
      HttpResponse.json({ revision: 9, items: [routerView("r1", { interfaces: [] })], routed: [] }),
    ),
  );
  const sent: { what: string; body?: unknown }[] = [];
  server.use(
    http.post(apiUrl(`/network-migrations/${running.id}/pause`), () => {
      sent.push({ what: "pause" });
      return HttpResponse.json({ ...running, status: { ...running.status, phase: "paused" } });
    }),
  );
  const user = userEvent.setup();
  const { container, unmount } = renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  expect(await screen.findByText("1 of 2 moved")).toBeVisible();
  expect(screen.getByText("Bridge on node-a")).toBeVisible();
  expect(screen.getByRole("progressbar", { name: "Migration progress" })).toHaveAttribute(
    "aria-valuenow",
    "1",
  );
  await expectNoAxeViolations(container, { disable: ["region"] });
  // 도는 동안은 멈추기만
  expect(screen.queryByRole("button", { name: /Roll back/ })).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Pause" }));
  await waitFor(() => expect(sent).toEqual([{ what: "pause" }]));
  unmount();

  // 멈춘 뒤: 고른 VM 되돌리기, 끝내기(라우터)
  const paused = { ...running, status: { ...running.status, phase: "paused", task: undefined } };
  cluster([paused]);
  server.use(
    http.get(apiUrl("/routers"), () =>
      HttpResponse.json({ revision: 9, items: [routerView("r1", { interfaces: [] })], routed: [] }),
    ),
    http.post(apiUrl(`/network-migrations/${paused.id}/rollback`), async ({ request }) => {
      sent.push({ what: "rollback", body: await request.json() });
      return HttpResponse.json(sdnTask("t2", "sdn-init", N1.id), { status: 202 });
    }),
    http.post(apiUrl(`/network-migrations/${paused.id}/finish`), async ({ request }) => {
      sent.push({ what: "finish", body: await request.json() });
      return HttpResponse.json(sdnTask("t3", "sdn-init", N1.id), { status: 202 });
    }),
  );
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  await user.click(await screen.findByRole("checkbox", { name: "Pick web to roll back" }));
  await user.click(screen.getByRole("button", { name: "Roll back 1 VM" }));
  await user.click(await screen.findByRole("button", { name: "Roll back" }));
  await waitFor(() => expect(sent).toHaveLength(2));
  expect(sent[1]).toEqual({ what: "rollback", body: { vms: [WEB.id] } });
  await user.click(screen.getByRole("button", { name: "Finish…" }));
  expect(screen.getByText("1 interface is still on the source network.")).toBeVisible();
  await user.selectOptions(screen.getByLabelText("Attach to a router (optional)"), "r1");
  await user.click(screen.getByRole("button", { name: "Finish" }));
  await waitFor(() => expect(sent).toHaveLength(3));
  expect(sent[2]).toEqual({ what: "finish", body: { router: "r1" } });
});

it("translates the server's plan sentences it knows", () => {
  const t = i18n.t;
  expect(planText(t, "the MTU shrinks from 1500 to 1442 (DHCP guests get the new MTU)")).toBe(
    "the MTU shrinks from 1500 to 1442 (DHCP guests get the new MTU)",
  );
  expect(planText(t, 'address 10.0.0.9 is outside the subnets of "t-lan"')).toBe(
    "10.0.0.9 is outside the subnets of t-lan",
  );
  expect(
    planText(
      t,
      'the bridge node "node-b" was removed from the cluster; resume with another bridge node (`orbivisorctl network migration resume m --bridge-node NODE`)',
    ),
  ).toBe("the bridge node node-b was removed from the cluster: resume with another bridge node");
  expect(planText(t, "something new")).toBe("something new");
});
