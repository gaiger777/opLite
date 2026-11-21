import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Network, Vm } from "@/api/types";
import { planSummary, planText } from "@/migration/migration";
import { MigrationWizard } from "@/migration/MigrationWizard";
import { NetworksPage } from "@/networks/NetworksPage";
import i18n from "@/i18n";
import { expectNoAxeViolations } from "@/test/axe";
import {
  migrationRes,
  providerNetwork,
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
  const warned = vi.spyOn(toast, "warning");
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
        // 서버의 머리는 문장이다(리뷰 1 M1 — 수는 계획에서 센다).
        { status: 201, headers: { "orbivisor-warning": "the plan has 4 warning(s); see plan" } },
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
  expect(within(sheet).getByText(/^One mode for the whole migration/)).toBeVisible();
  // 막힌 VM은 뺀다. 다시 계획해도 그 줄은 남아 다시 넣을 수 있다(리뷰 1 I6 — 미리 보기는 늘 모든 VM).
  await user.click(within(db).getByRole("checkbox", { name: "Move db" }));
  await user.click(within(sheet).getByRole("button", { name: "Plan again" }));
  await waitFor(() => expect(sent).toHaveLength(2));
  const again = within(sheet).getByRole("table", { name: "Migration plan" });
  expect(within(again).getByRole("checkbox", { name: "Move db" })).not.toBeChecked();
  // 이름은 보내지 않는다(서버가 고른다 — 리뷰 1 I2). VM 빼기는 만들 때만 `vms`.
  await user.click(within(sheet).getByRole("button", { name: "Create and start (1 interface)" }));
  await waitFor(() => expect(sent).toHaveLength(4));
  expect(sent).toEqual([
    { url: "?dry_run=true", body: { spec: { source: "lan", target: "t-lan" } } },
    { url: "?dry_run=true", body: { spec: { source: "lan", target: "t-lan" } } },
    { url: "", body: { spec: { source: "lan", target: "t-lan", vms: [WEB.id] } } },
    { url: "start", body: {} },
  ]);
  expect(warned).toHaveBeenCalledWith("The plan has 4 warnings.");
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
  expect(sent[1]).toEqual({ spec: { source: "lan", target: "lan-sdn", mode: "live" } });
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

/** 목록이 온 뒤(대상 t-lan이 골라진 뒤) 계획을 만든다. */
async function planOnTLan(user: ReturnType<typeof userEvent.setup>) {
  const existing = await screen.findByLabelText("An existing SDN network");
  await waitFor(() => expect(existing).toBeEnabled());
  await user.click(existing);
  expect(screen.getByLabelText("SDN network")).toHaveValue("t-lan");
  await user.click(screen.getByRole("button", { name: "Make the plan" }));
}

/** 두 VM 모두 옮길 수 있는 계획(`web` 켜짐 라이브, `db` 꺼짐) */
const MOVABLE = {
  nics: [
    { ...PLAN.nics[0]!, warnings: [] },
    {
      ...PLAN.nics[1]!,
      mode: "offline",
      ips: ["192.168.10.6"],
      blockers: [],
      warnings: [],
    },
  ],
  bridge: { ...PLAN.bridge, warnings: [] },
};

function created(id: string, plan: unknown) {
  return {
    ...migrationRes(id, { source: "lan", target: "t-lan" }, { phase: "planned", vms: [] }),
    plan,
  };
}

it("starts only the plan on screen: a changed choice needs a new plan (review 1 C1)", async () => {
  cluster();
  const sent: string[] = [];
  server.use(
    http.post(apiUrl("/network-migrations"), ({ request }) => {
      sent.push(new URL(request.url).search);
      return HttpResponse.json(MOVABLE);
    }),
  );
  const user = userEvent.setup();
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  await planOnTLan(user);
  const start = await screen.findByRole("button", { name: "Create and start (2 interfaces)" });
  expect(start).toBeEnabled();
  // 방식을 바꾸면 화면의 계획과 달라 시작하지 못한다.
  await user.selectOptions(screen.getByLabelText("Mode"), "Restart");
  expect(start).toBeDisabled();
  expect(screen.getByText(/plan again before starting/)).toBeVisible();
  // 되돌리면 다시 된다. 주소를 고쳐도 다시 계획해야 한다.
  await user.selectOptions(screen.getByLabelText("Mode"), "Automatic");
  expect(start).toBeEnabled();
  await user.type(screen.getByLabelText("Address for db interface 1"), "192.168.10.9");
  expect(start).toBeDisabled();
  await user.click(screen.getByRole("button", { name: "Plan again" }));
  await waitFor(() => expect(sent).toHaveLength(2));
  await waitFor(() => expect(start).toBeEnabled());
});

it("asks again when the server's plan at create differs, especially a VM that would restart (review 1 C1)", async () => {
  cluster();
  const sent: string[] = [];
  // 그새 db가 켜져 `auto`가 다시 켬으로 계획한다.
  const later = {
    ...MOVABLE,
    nics: [MOVABLE.nics[0], { ...MOVABLE.nics[1]!, mode: "restart" }],
  };
  const RES = created("lan-to-t-lan", later);
  server.use(
    http.post(apiUrl("/network-migrations"), ({ request }) => {
      const dry = new URL(request.url).searchParams.get("dry_run") === "true";
      sent.push(dry ? "dry" : "create");
      return dry ? HttpResponse.json(MOVABLE) : HttpResponse.json(RES, { status: 201 });
    }),
    http.post(apiUrl(`/network-migrations/${RES.id}/start`), () => {
      sent.push("start");
      return HttpResponse.json(sdnTask("t1", "sdn-init", N1.id), { status: 202 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  await planOnTLan(user);
  await user.click(await screen.findByRole("button", { name: "Create and start (2 interfaces)" }));
  const dialog = await screen.findByRole("alertdialog", {
    name: "The plan for lan-to-t-lan changed",
  });
  expect(
    within(dialog).getByText(
      "db interface 1: stopped → restart — the VM is shut down and started again",
    ),
  ).toBeVisible();
  expect(within(dialog).getByRole("alert")).toHaveTextContent(
    "1 running VM would now lose its network briefly or be restarted.",
  );
  expect(sent).toEqual(["dry", "create"]);
  await user.click(within(dialog).getByRole("button", { name: "Start with the new plan" }));
  await waitFor(() => expect(sent).toEqual(["dry", "create", "start"]));
});

it("keeps a created migration planned when the plan changed and the user cancels", async () => {
  cluster();
  const later = {
    ...MOVABLE,
    nics: [{ ...MOVABLE.nics[0]!, ips: ["192.168.10.77"] }, MOVABLE.nics[1]],
  };
  const RES = created("lan-to-t-lan", later);
  let made = false;
  const started: string[] = [];
  server.use(
    http.post(apiUrl("/network-migrations"), ({ request }) => {
      if (new URL(request.url).searchParams.get("dry_run") === "true") {
        return HttpResponse.json(MOVABLE);
      }
      made = true;
      return HttpResponse.json(RES, { status: 201 });
    }),
    http.get(apiUrl("/network-migrations"), () =>
      HttpResponse.json({ revision: 13, items: made ? [RES] : [] }),
    ),
    http.post(apiUrl(`/network-migrations/${RES.id}/start`), () => {
      started.push("start");
      return HttpResponse.json(sdnTask("t1", "sdn-init", N1.id), { status: 202 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  await planOnTLan(user);
  await user.click(await screen.findByRole("button", { name: "Create and start (2 interfaces)" }));
  const dialog = await screen.findByRole("alertdialog");
  expect(
    within(dialog).getByText("web interface 1: address 192.168.10.5 → 192.168.10.77"),
  ).toBeVisible();
  await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
  // 진행 단계에 계획된 옮기기(시작·지우기)
  expect(await screen.findByText("planned")).toBeVisible();
  expect(screen.getByRole("button", { name: "Start" })).toBeVisible();
  expect(started).toEqual([]);
});

it("shows the progress step with Start when starting fails after the create (review 1 M3)", async () => {
  cluster();
  const RES = created("lan-to-t-lan", MOVABLE);
  let made = false;
  server.use(
    http.post(apiUrl("/network-migrations"), ({ request }) => {
      if (new URL(request.url).searchParams.get("dry_run") === "true") {
        return HttpResponse.json(MOVABLE);
      }
      made = true;
      return HttpResponse.json(RES, { status: 201 });
    }),
    http.get(apiUrl("/network-migrations"), () =>
      HttpResponse.json({ revision: 13, items: made ? [RES] : [] }),
    ),
    http.post(apiUrl(`/network-migrations/${RES.id}/start`), () =>
      HttpResponse.json(
        { code: "sdn_busy", message: "another SDN task is running" },
        { status: 409 },
      ),
    ),
  );
  const user = userEvent.setup();
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  await planOnTLan(user);
  await user.click(await screen.findByRole("button", { name: "Create and start (2 interfaces)" }));
  expect(await screen.findByText("planned")).toBeVisible();
  expect(screen.getByRole("button", { name: "Start" })).toBeVisible();
});

it("keeps showing a migration it opened after it finishes (review 1 I1)", async () => {
  const running = migrationRes(
    "lan-to-t-lan",
    { source: "lan", target: "t-lan" },
    { phase: "moved", vms: [] },
  );
  cluster([running]);
  const user = userEvent.setup();
  const onClose = vi.fn();
  const { queryClient } = renderPage(<MigrationWizard source={LAN} onClose={onClose} />);
  expect(await screen.findByText("moved")).toBeVisible();
  server.use(
    http.get(apiUrl("/network-migrations"), () =>
      HttpResponse.json({
        revision: 14,
        items: [{ ...running, status: { phase: "finished", vms: [] } }],
      }),
    ),
  );
  await queryClient.invalidateQueries({ queryKey: ["network-migrations"] });
  expect(await screen.findByText("finished")).toBeVisible();
  expect(screen.getByRole("button", { name: "Delete the record" })).toBeVisible();
  const buttons = screen.getAllByRole("button", { name: "Close" });
  await user.click(buttons.find((b) => b.textContent === "Close")!);
  expect(onClose).toHaveBeenCalled();
});

it("offers only bridge nodes that have the source bridge when resuming (review 1 M5)", async () => {
  const N2 = sdnNode(2, readyChassis(2), { bridges: [{ name: "br9", vlan_filtering: false }] });
  const paused = migrationRes(
    "lan-to-t-lan",
    { source: "lan", target: "t-lan" },
    {
      phase: "paused",
      vms: [],
      error:
        'the bridge node "node-c" was removed from the cluster; resume with another bridge node',
    },
  );
  cluster([paused]);
  mockLists({ node: [N1, N2], network: [LAN, TLAN], vm: [WEB, DB] });
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  const picker = await screen.findByLabelText("New bridge node");
  const options = within(picker)
    .getAllByRole("option")
    .map((o) => o.textContent);
  expect(options).toEqual(["Automatic (the node with the most VMs)", "node-a"]);
});

it("says how many VMs it cannot show and translates the new server sentences", async () => {
  cluster();
  server.use(
    http.post(apiUrl("/network-migrations"), () =>
      HttpResponse.json({
        ...MOVABLE,
        hidden: 2,
        bridge: {
          ...MOVABLE.bridge,
          warnings: [
            "IPAM does not know the addresses other hosts use on the source network: keep them out of the target's pools while the bridge is up",
          ],
        },
      }),
    ),
  );
  const user = userEvent.setup();
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  await planOnTLan(user);
  expect(
    await screen.findByText("2 VMs you cannot see are on the source network; they are not moved."),
  ).toBeVisible();
  expect(
    screen.getByText(
      "IPAM does not know the addresses other hosts use on the source network: keep them out of the target's pools while the bridge is up",
    ),
  ).toBeVisible();
  const t = i18n.t;
  expect(planText(t, 'address 10.0.0.5 is already in use on "t-lan"')).toBe(
    "10.0.0.5 is already in use on t-lan",
  );
  // 이어 가기 Task 이름(리뷰 1 M4)
  expect(t("migration.taskStep.resume")).toBe("resume");
  // ko는 단추와 같은 "이어 하기"(리뷰 2 m8)
  expect(i18n.getResourceBundle("ko", "translation").migration.taskStep.resume).toBe("이어 하기");
});

it("doesn't create the new SDN network twice when planning fails (review 1 M2)", async () => {
  cluster();
  const sent: string[] = [];
  let plans = 0;
  server.use(
    http.post(apiUrl("/networks"), () => {
      sent.push("network");
      return HttpResponse.json({ ...TLAN, name: "lan-sdn" }, { status: 201 });
    }),
    http.post(apiUrl("/network-migrations"), async ({ request }) => {
      sent.push(JSON.stringify(await request.json()));
      plans += 1;
      return plans === 1
        ? HttpResponse.json({ code: "sdn_busy", message: "busy" }, { status: 409 })
        : HttpResponse.json(MOVABLE);
    }),
  );
  const user = userEvent.setup();
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  await user.click(await screen.findByLabelText("A new SDN network with the same subnet"));
  await user.type(screen.getByLabelText("Subnet (CIDR)"), "192.168.10.0/24");
  await user.click(screen.getByRole("button", { name: "Make the plan" }));
  expect(await screen.findByRole("alert")).toBeVisible();
  expect(screen.getByLabelText("An existing SDN network")).toBeChecked();
  await user.click(screen.getByRole("button", { name: "Make the plan" }));
  await waitFor(() => expect(plans).toBe(2));
  expect(sent.filter((x) => x === "network")).toHaveLength(1);
  expect(JSON.parse(sent.at(-1)!)).toEqual({ spec: { source: "lan", target: "lan-sdn" } });
});

it("sends no bridge for a provider target so a second migration into it is not refused (review 2 #2)", async () => {
  const PROV = providerNetwork("prov", "phys", [{ cidr: "192.168.10.0/24" }]);
  mockLists({ node: [N1], network: [LAN, PROV], vm: [WEB, DB] });
  server.use(
    http.get(apiUrl("/sdn"), () => HttpResponse.json(sdnView([N1], [N1]))),
    http.get(apiUrl("/network-migrations"), () => HttpResponse.json({ revision: 12, items: [] })),
  );
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl("/network-migrations"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json({
        ...MOVABLE,
        bridge: { needed: false, node: null, error: null, warnings: [] },
      });
    }),
  );
  const user = userEvent.setup();
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  const existing = await screen.findByLabelText("An existing SDN network");
  await waitFor(() => expect(existing).toBeChecked());
  expect(screen.getByLabelText("SDN network")).toHaveValue("prov");
  // 공급자 대상에는 다리 칸이 없다.
  expect(screen.queryByLabelText(/bridge/i, { selector: "button[role=checkbox]" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Make the plan" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({ spec: { source: "lan", target: "prov", bridge: false } });
});

it("picks the existing network once the list arrives when opened before it (review 2 m5)", async () => {
  cluster();
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  server.use(
    http.get(apiUrl("/networks"), async () => {
      await gate;
      return HttpResponse.json({ revision: 1, items: [LAN, TLAN] });
    }),
  );
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  const plan = await screen.findByRole("button", { name: "Make the plan" });
  // 목록이 오기 전에는 계획을 만들 수 없다.
  expect(plan).toBeDisabled();
  release();
  await waitFor(() => expect(screen.getByLabelText("An existing SDN network")).toBeChecked());
  expect(screen.getByLabelText("SDN network")).toHaveValue("t-lan");
  expect(plan).toBeEnabled();
});

it("replaces an offline bridge node and translates per-interface errors (review 2 #3, m2, m8)", async () => {
  const OFF = sdnNode(1, readyChassis(1, "leader"), {
    bridges: [{ name: "br0", vlan_filtering: false }],
    online: false,
  });
  const N2 = sdnNode(2, readyChassis(2), { bridges: [{ name: "br0", vlan_filtering: false }] });
  const failed = migrationRes(
    "lan-to-t-lan",
    { source: "lan", target: "t-lan" },
    {
      phase: "failed",
      bridge: { node: OFF.id, physnet: "opl-mig-1" },
      error: `the migration bridge opl-mig-1 on node ${OFF.id} is not ready after 120s: the node did not report it`,
      vms: [
        {
          vm: WEB.id,
          nic: 0,
          mac: "52:54:00:00:00:01",
          mode: "live",
          ips: ["192.168.10.5"],
          address_source: "given",
          from: { network: "lan" },
          state: "failed",
          error: "the NIC was changed outside the migration",
        },
      ],
    },
  );
  cluster([failed]);
  mockLists({ node: [OFF, N2], network: [LAN, TLAN], vm: [WEB, DB] });
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl(`/network-migrations/${failed.id}/resume`), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(sdnTask("t1", "sdn-init", N2.id), { status: 202 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  expect(
    await screen.findByText(
      "the bridge on node node-a is not ready after 120 s: if the node is down, resume with another bridge node",
    ),
  ).toBeVisible();
  expect(screen.getByText("the interface was changed outside the migration")).toBeVisible();
  const picker = screen.getByLabelText("New bridge node");
  // 꺼진 다리 노드는 후보가 아니다.
  expect(
    within(picker)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["Automatic (the node with the most VMs)", "node-b"]);
  expect(screen.getByText(/The bridge node node-a is offline/)).toBeVisible();
  await user.selectOptions(picker, "node-b");
  await user.click(screen.getByRole("button", { name: "Resume" }));
  await waitFor(() => expect(sent).toEqual([{ bridge_node: N2.id }]));
  expect(await screen.findByText("Resumed lan-to-t-lan.")).toBeInTheDocument();
});

it("sums up what starting does and says when a VM is no longer blocked (review 2 m6, m8, m1)", async () => {
  cluster();
  // 화면의 계획은 db가 막혔고, 만들 때 서버 계획은 막히지 않았다.
  const RES = created("lan-to-t-lan", MOVABLE);
  server.use(
    http.post(apiUrl("/network-migrations"), ({ request }) =>
      new URL(request.url).searchParams.get("dry_run") === "true"
        ? HttpResponse.json(PLAN)
        : HttpResponse.json(RES, { status: 201 }),
    ),
  );
  const user = userEvent.setup();
  renderPage(<MigrationWizard source={LAN} onClose={() => {}} />);
  await planOnTLan(user);
  expect(await screen.findByText("1 VM moves live (about a second without network)")).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Create and start (1 interface)" }));
  const dialog = await screen.findByRole("alertdialog", {
    name: "The plan for lan-to-t-lan changed",
  });
  expect(within(dialog).getByText("db interface 1: no longer blocked")).toBeVisible();
  // 바뀐 것이 많아도 단추가 화면 밖으로 나가지 않게 대화상자 안에서 넘긴다.
  expect(dialog).toHaveClass("max-h-[90svh]", "overflow-y-auto");
});

it("counts VMs per mode without the left-out ones", () => {
  const plan = MOVABLE as unknown as Parameters<typeof planSummary>[0];
  expect(planSummary(plan)).toEqual({ restart: 0, live: 1, offline: 1 });
  expect(planSummary(plan, new Set([DB.id]))).toEqual({ restart: 0, live: 1, offline: 0 });
});
