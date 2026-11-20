import type { Node, Vm } from "@/api/types";
import { usageBars } from "@/folders/UsageBars";
import { sdnConfigOf } from "@/nodes/NodeSdnSection";
import { routeError } from "@/routing/RoutersPage";
import {
  fipOn,
  groupNics,
  knownPhysnets,
  nicChoices,
  physnetNodes,
  providerLabel,
  routingUsers,
  serverText,
  slowFipRefresh,
  validPhysnet,
} from "@/routing/routing";
import { emptySubnetRow, subnetErrors, subnetRow, toSubnet } from "@/sdn/subnets";
import {
  floatingIpView,
  NODE,
  providerNetwork,
  routerView,
  sdnNetwork,
  securityGroup,
  vm,
} from "@/test/fixtures";

const EXT = providerNetwork("ext", "ext", [{ cidr: "203.0.113.0/24", gateway: "203.0.113.1" }]);
const T1 = sdnNetwork("t1", [{ cidr: "10.10.0.0/24" }]);
const T2 = sdnNetwork("t2", [{ cidr: "10.20.0.0/24" }]);
const LAN = { id: "net-lan", name: "lan", revision: 1, spec: { bridge: "br0" }, status: {} };
const R1 = routerView("r1", {
  external: { network: "ext", ips: ["203.0.113.100"] },
  interfaces: [{ network: "t1" }],
});
/** 서버의 `routed`: R1이 t1을 ext로 내보낸다(읽을 수 있든 없든) */
const ROUTED = [{ network: "t1", external: "ext" }];

function withNics(v: Vm, nics: Vm["spec"]["nics"], extra: Partial<Vm["spec"]> = {}): Vm {
  return { ...v, spec: { ...v.spec, nics, ...extra } };
}

it("labels provider networks and finds physnets the nodes set or reported", () => {
  expect(providerLabel(EXT)).toBe("ext");
  expect(providerLabel(providerNetwork("e2", "ext", [], 100))).toBe("ext:100");
  const a: Node = {
    ...NODE,
    spec: { ...NODE.spec, sdn: { physnets: { ext: "vmbr1" } } },
    status: {
      ...NODE.status,
      sdn: {
        chassis: "ready",
        physnets: [
          { name: "ext", bridge: "vmbr1", ready: true },
          { name: "old", bridge: "vmbr2", ready: false, error: "no bridge vmbr2" },
        ],
      },
    },
  };
  const b: Node = {
    ...NODE,
    id: "n2",
    name: "node-b",
    spec: { ...NODE.spec, sdn: { gateway: 100 } },
  };
  expect(knownPhysnets([a, b])).toEqual(["ext", "old"]);
  expect(physnetNodes([a, b], "ext").map((n) => n.name)).toEqual(["node-a"]);
  expect(physnetNodes([a, b], "old")).toEqual([]);
  expect(validPhysnet("ext-1")).toBe(true);
  expect(validPhysnet("Ext")).toBe(false);
  expect(validPhysnet("e:1")).toBe(false);
});

it("says why an interface cannot take a floating IP", () => {
  const web = withNics(vm("web", "running"), [
    { network: "t1", model: "virtio", mac: "52:54:00:00:00:01", ips: ["10.10.0.2"] },
    { network: "t2", model: "virtio", mac: "52:54:00:00:00:02", ips: ["10.20.0.2"] },
    { network: "lan", model: "virtio", mac: "52:54:00:00:00:03" },
    // 공급자 네트워크에 곧바로 붙은 NIC는 후보가 아니다(리뷰 2 웹 m3).
    { network: "ext", model: "virtio", mac: "52:54:00:00:00:06", ips: ["203.0.113.20"] },
  ]);
  const bare = withNics(vm("bare", "stopped"), [
    { network: "t1", model: "virtio", mac: "52:54:00:00:00:04", ips: ["fd00::4"] },
  ]);
  const tmpl = withNics(
    vm("tmpl", "stopped"),
    [{ network: "t1", model: "virtio", mac: "52:54:00:00:00:05", ips: ["10.10.0.5"] }],
    { template: true },
  );
  const taken = floatingIpView(
    "203.0.113.120",
    "ext",
    { kind: "distributed", node: "node-a" },
    { target: { vm: web.id, mac: "52:54:00:00:00:01" }, vm_name: "web" },
  );
  const networks = [EXT, T1, T2, LAN] as never[];
  const choices = nicChoices("ext", [web, bare, tmpl], networks, ROUTED, [taken]);
  // 단순 NIC(lan)와 공급자 NIC(ext)는 후보가 아니다. 이름 순. 라우팅은 라우터 목록이 아니라 서버의 `routed`로 본다 —
  // 읽을 수 없는 라우터 뒤의 NIC도 막지 않는다(리뷰 2 웹 I1).
  expect(choices.map((c) => [c.vm.name, c.index, c.reason?.key])).toEqual([
    ["bare", 0, "fip.why.noIpv4"],
    ["tmpl", 0, "fip.why.template"],
    ["web", 0, "fip.why.taken"],
    ["web", 1, "fip.why.noRouter"],
  ]);
  // 붙어 있는 그 유동 IP 자신은 막지 않는다.
  expect(nicChoices("ext", [web], networks, ROUTED, [taken], taken.id)[0]!.reason).toBeUndefined();
  // 외부를 읽을 수 없는 라우터(빈 글)나 다른 외부로 가는 라우터는 이 유동 IP 네트워크로 가지 않는다.
  for (const routed of [
    [{ network: "t1", external: "" }],
    [{ network: "t1", external: "ext2" }],
    [],
  ]) {
    expect(nicChoices("ext", [web], networks, routed, [], taken.id)[0]!.reason?.key).toBe(
      "fip.why.noRouter",
    );
  }
  expect(fipOn([taken], web.id, "52:54:00:00:00:01")).toBe(taken);
  expect(fipOn([taken], web.id, "52:54:00:00:00:02")).toBeUndefined();
  expect(fipOn([taken], web.id, null)).toBeUndefined();
});

it("counts simple and SDN interfaces of a security group like the server", () => {
  const ssh = securityGroup("ssh");
  const a = withNics(vm("a", "running"), [
    { network: "lan", model: "virtio", security_groups: ["ssh"] },
    { network: "t1", model: "virtio", security_groups: ["web", "ssh"] },
  ]);
  const b = withNics(
    vm("b", "stopped"),
    [{ network: "t2", model: "virtio", security_groups: ["ssh"] }],
    // 템플릿도 센다(서버 `count_nics`).
    { template: true },
  );
  expect(groupNics(ssh, [a, b], [T1, T2, LAN] as never[])).toEqual({ simple: 1, sdn: 2 });
});

it("writes provider subnets with DHCP off by default, floating ranges and IPv6 modes", () => {
  const row = {
    ...emptySubnetRow(true),
    cidr: "203.0.113.0/24",
    floating: "203.0.113.100-203.0.113.199",
  };
  expect(row.dhcp).toBe(false);
  expect(toSubnet(row, true)).toEqual({
    cidr: "203.0.113.0/24",
    floating_pools: [{ start: "203.0.113.100", end: "203.0.113.199" }],
  });
  // 공급자에서 켠 DHCP만 보낸다. 오버레이는 끈 것만 보낸다(지금 그대로).
  expect(toSubnet({ ...row, dhcp: true }, true)).toMatchObject({ dhcp: true });
  expect(
    toSubnet({ ...emptySubnetRow(), cidr: "10.0.0.0/24", floating: "10.0.0.5-10.0.0.6" }),
  ).toEqual({ cidr: "10.0.0.0/24" });
  expect(subnetRow({ cidr: "203.0.113.0/24" }, true).dhcp).toBe(false);
  expect(subnetRow({ cidr: "10.0.0.0/24" }).dhcp).toBe(true);
  const v6 = { ...emptySubnetRow(), cidr: "fd00:10::/64", ipv6Mode: "slaac" as const };
  expect(toSubnet(v6)).toEqual({ cidr: "fd00:10::/64", ipv6_mode: "slaac" });
  expect(subnetRow({ cidr: "fd00:10::/64", ipv6_mode: "dhcpv6-stateful" }).ipv6Mode).toBe(
    "dhcpv6-stateful",
  );
  expect(
    subnetErrors(
      [
        { ...row, floating: "203.0.113.5" },
        { ...v6, cidr: "fd00:10::/80" },
      ],
      true,
    ),
  ).toEqual({
    "subnets.0.floating": "sdn.errors.floating",
    "subnets.1.ipv6Mode": "sdn.errors.slaac64",
  });
});

it("checks static routes like the server", () => {
  expect(routeError("10.20.0.0/16", "10.10.0.254", false)).toBeNull();
  expect(routeError("10.20.0.0", "10.10.0.254", false)?.key).toBe("router.errors.destination");
  expect(routeError("10.20.0.0/16", "fd00::1", false)?.key).toBe("router.errors.nexthop");
  expect(routeError("0.0.0.0/0", "10.10.0.254", true)?.key).toBe("router.errors.defaultRoute");
  expect(routeError("0.0.0.0/0", "10.10.0.254", false)).toBeNull();
});

it("sends an empty object to clear a node's SDN settings", () => {
  expect(sdnConfigOf({ rows: [], gateway: false, priority: "100" })).toEqual({
    errors: {},
    config: {},
  });
  expect(
    sdnConfigOf({ rows: [{ name: "ext", bridge: "vmbr1" }], gateway: true, priority: "200" }),
  ).toEqual({ errors: {}, config: { physnets: { ext: "vmbr1" }, gateway: 200 } });
  expect(
    sdnConfigOf({
      rows: [
        { name: "ext", bridge: "br-int" },
        { name: "ext", bridge: "vmbr1" },
      ],
      gateway: true,
      priority: "0",
    }).errors,
  ).toEqual({
    "physnets.0.bridge": "sdn.physnet.errors.bridge",
    "physnets.1.name": "sdn.physnet.errors.duplicate",
    priority: "sdn.physnet.errors.priority",
  });
});

it("draws router and floating IP bars for a network folder", () => {
  const F = "019a0f3c-0000-7000-8009-00000000000c";
  // 서버 `FolderUsage`: VM 칸은 늘, 라우터·유동 IP는 0이면 빠진다.
  const view = {
    folder: F,
    path: "/tenant",
    usage: { vcpus: 0, memory_mib: 0, disk_bytes: 0, vms: 0, routers: 1 },
    limits: [
      {
        folder: F,
        path: "/tenant",
        quota: { floating_ips: 4 },
        usage: { vcpus: 0, memory_mib: 0, disk_bytes: 0, vms: 0, routers: 1 },
      },
    ],
    binding: { floating_ips: F },
  };
  expect(usageBars(view, "network")).toEqual([
    { dim: "routers", used: 1 },
    { dim: "floating_ips", used: 0, limit: 4, boundBy: undefined },
  ]);
});

it("polls floating IPs on the VM page only when some exist, every 30 s", () => {
  // 리뷰 1 웹 I1: 유동 IP가 없는 클러스터에서 VM 상세가 10초마다 서버의 World 읽기를 부르지 않는다.
  expect(slowFipRefresh(undefined)).toBe(false);
  expect(slowFipRefresh(0)).toBe(false);
  expect(slowFipRefresh(2)).toBe(30_000);
});

it("counts the routers and floating IPs that keep a network from being deleted", () => {
  const r2 = routerView("r2", { external: { network: "ext" } });
  const fip = floatingIpView("203.0.113.120", "ext");
  // t1은 (읽을 수 없을 수도 있는) 라우터의 인터페이스, ext는 R1·r2의 외부이고 유동 IP 하나(리뷰 2 웹 m2)
  expect(routingUsers("t1", [], ROUTED, [])).toEqual({ routers: 1, fips: 0 });
  expect(routingUsers("ext", [R1, r2], ROUTED, [fip])).toEqual({ routers: 2, fips: 1 });
  expect(routingUsers("t2", [R1, r2], ROUTED, [fip])).toEqual({ routers: 0, fips: 0 });
});

it("translates the server reasons it knows and keeps the rest", () => {
  const t = (key: string, values?: Record<string, unknown>) => `${key} ${JSON.stringify(values)}`;
  expect(serverText(t, "no gateway node has physnet ext ready")).toBe(
    'routing.server.noGateway {"physnet":"ext"}',
  );
  expect(
    serverText(
      t,
      "no gateway node has physnet ext ready; the router's external gateway stays off until one has it (`orbivisorctl node sdn NODE --physnet NAME=BRIDGE --gateway`)",
    ),
  ).toBe('routing.server.noGatewayYet {"physnet":"ext"}');
  expect(serverText(t, "node n1, n2 does not have physnet ext ready")).toBe(
    'routing.server.nodeNoPhysnet {"node":"n1, n2","physnet":"ext"}',
  );
  expect(serverText(t, 'VM "web" is a template')).toBe('routing.server.template {"vm":"web"}');
  expect(serverText(t, "the target VM is a template")).toBe(
    'routing.server.templateHidden {"vm":""}',
  );
  expect(
    serverText(
      t,
      'network "t1" of the NIC is not an interface of a router whose external network is "ext"',
    ),
  ).toBe('routing.server.noRouter {"network":"t1","external":"ext"}');
  expect(serverText(t, "something new")).toBe("something new");
});
