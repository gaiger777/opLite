import { ipsOverride, parseIps } from "@/sdn/ips";
import { nodeBlocker, sdnReady, sdnWarnings, suggestCentrals, type SdnView } from "@/sdn/sdn";
import { emptySubnetRow, subnetErrors, subnetRow, toSubnet } from "@/sdn/subnets";
import { NODE, readyChassis, sdnNode, sdnView } from "@/test/fixtures";
import { toCloudInit } from "@/vms/cloudInit";
import { emptyForm, fromVm, toSpec, validate } from "@/vms/form";
import { runForm, toRun } from "@/imports/imports";
import { vm, vmImport } from "@/test/fixtures";

it("reads NIC addresses: one per family, empty is automatic", () => {
  expect(parseIps("")).toEqual({ ips: [] });
  expect(parseIps(" 10.10.0.5 ")).toEqual({ ips: ["10.10.0.5"] });
  expect(parseIps("10.10.0.5, fd00::5")).toEqual({ ips: ["10.10.0.5", "fd00::5"] });
  expect(parseIps("10.10.0.5+fd00::5")).toEqual({ ips: ["10.10.0.5", "fd00::5"] });
  expect(parseIps("10.10.0.5, 10.10.0.6")).toEqual({ error: "sdn.errors.ipFamily" });
  expect(parseIps("10.10.0.05")).toEqual({ error: "sdn.errors.ip" });
  expect(parseIps("10.10.0.0/24")).toEqual({ error: "sdn.errors.ip" });
});

it("sends per-NIC addresses only when some are given", () => {
  const errors: Record<string, string> = {};
  expect(ipsOverride(["", ""], errors, "ips")).toBeUndefined();
  expect(ipsOverride(["", "10.0.0.9"], errors, "ips")).toEqual([[], ["10.0.0.9"]]);
  expect(errors).toEqual({});
  expect(ipsOverride(["x"], errors, "ips")).toBeUndefined();
  expect(errors).toEqual({ ips0: "sdn.errors.ip" });
});

it("blocks nodes that cannot run OVN, in the server's order", () => {
  expect(nodeBlocker(sdnNode(1), true)).toBeNull();
  expect(nodeBlocker({ ...sdnNode(1), spec: { ...NODE.spec, witness: true } }, true)).toBe(
    "sdn.blocked.witness",
  );
  expect(nodeBlocker(sdnNode(1, undefined, { hypervisor: "qemu-hvf" }), true)).toBe(
    "sdn.blocked.macos",
  );
  expect(nodeBlocker(NODE, true)).toBe("sdn.blocked.underlay");
  const maint = { ...sdnNode(1), spec: { ...sdnNode(1).spec, maintenance: true } };
  expect(nodeBlocker(maint, true)).toBe("sdn.blocked.maintenance");
  // chassis 설치는 유지보수 노드도 된다.
  expect(nodeBlocker(maint, false)).toBeNull();
  expect(nodeBlocker(sdnNode(1, undefined, { online: false }), false)).toBe("sdn.blocked.offline");
});

it("suggests three voting members as centrals", () => {
  const nodes = [sdnNode(4), sdnNode(1), sdnNode(2), sdnNode(3), NODE];
  expect(suggestCentrals(nodes, new Set(["n1", "n2", "n3", "n4"]))).toEqual(["n1", "n2", "n3"]);
  // 투표 멤버가 막혔으면 빼고, 모르면 첫 노드 하나
  expect(
    suggestCentrals([sdnNode(2, undefined, { online: false }), sdnNode(3)], new Set(["n2", "n3"])),
  ).toEqual(["n3"]);
  expect(suggestCentrals(nodes, undefined)).toEqual(["n1"]);
});

it("warns about even, non-voting and offline centrals", () => {
  const a = sdnNode(1, readyChassis(1, "leader"));
  const b = sdnNode(2, readyChassis(2, "follower"), { online: false });
  const view = sdnView([a, b], [a, b]) as SdnView;
  expect(sdnReady(view)).toBe(true);
  expect(sdnReady({ centrals: [], chassis: [], tasks: [] })).toBe(false);
  expect(sdnWarnings(view, [a, b], new Set(["n1"]))).toEqual([
    { key: "sdn.warn.even", values: { count: 2 } },
    { key: "sdn.warn.notVoter", values: { names: "node-b" } },
    { key: "sdn.warn.offline", values: { names: "node-b" } },
  ]);
});

it("edits subnets as rows and back to the server's shape", () => {
  const saved = {
    cidr: "10.10.0.0/24",
    gateway: "10.10.0.254",
    dns_servers: ["10.0.0.53"],
    pools: [{ start: "10.10.0.100", end: "10.10.0.199" }],
    description: "web",
  };
  const row = subnetRow(saved);
  expect(row).toMatchObject({ dhcp: true, saved: true, pools: "10.10.0.100-10.10.0.199" });
  expect(toSubnet(row)).toEqual(saved);
  expect(toSubnet({ ...row, dhcp: false })).toEqual({ ...saved, dhcp: false });
  // IPv6는 DHCP를 보내지 않는다(서버 기본 거짓).
  expect(toSubnet({ ...emptySubnetRow(), cidr: "fd00::/64" })).toEqual({ cidr: "fd00::/64" });
  expect(subnetRow({ cidr: "fd00::/64" }).dhcp).toBe(false);
  expect(
    subnetErrors([
      { ...emptySubnetRow(), cidr: "10.0.0.0/30" },
      {
        ...emptySubnetRow(),
        cidr: "10.1.0.0/16",
        gateway: "fd00::1",
        dns: "1.1.1.1 x",
        pools: "10.1.0.5",
      },
      { ...emptySubnetRow(), cidr: "10.2.0.0/24" },
    ]),
  ).toEqual({
    "subnets.0.cidr": "sdn.errors.cidr",
    "subnets.1.gateway": "sdn.errors.gateway",
    "subnets.1.dns": "sdn.errors.dns",
    "subnets.1.pools": "sdn.errors.pools",
    "subnets.2.cidr": "sdn.errors.family",
  });
});

it("puts SDN NIC addresses in the VM spec and leaves their cloud-init entry empty", () => {
  const v = {
    ...emptyForm("q35"),
    name: "web",
    nics: [
      {
        network: "tenant",
        model: "virtio" as const,
        mac: "",
        securityGroups: [],
        ips: "10.10.0.5",
      },
      { network: "lan", model: "virtio" as const, mac: "", securityGroups: [], ips: "" },
    ],
  };
  const sdn = [true, false];
  const spec = toSpec(v, "stopped", sdn);
  expect(spec.nics).toEqual([
    { network: "tenant", model: "virtio", mac: null, ips: ["10.10.0.5"] },
    { network: "lan", model: "virtio", mac: null },
  ]);
  expect(validate({ ...v, nics: [{ ...v.nics[0]!, ips: "bad" }] }, true, [], sdn)).toMatchObject({
    "nics.0.ips": "sdn.errors.ip",
  });
  const ci = { ...v.cloudInit, enabled: true };
  // 기본 주소면 network-config를 만들지 않는다.
  expect(toCloudInit(ci, 2, sdn)?.networks).toBeUndefined();
  const staticSecond = {
    ...ci,
    networks: [
      {
        ...ci.networks[0],
        ipv4: "static" as const,
        ipv4Address: "bad",
        ipv4Gateway: "",
        ipv6: "none" as const,
        ipv6Address: "",
        ipv6Gateway: "",
      },
      {
        ipv4: "static" as const,
        ipv4Address: "192.168.1.5/24",
        ipv4Gateway: "",
        ipv6: "none" as const,
        ipv6Address: "",
        ipv6Gateway: "",
      },
    ],
  };
  expect(toCloudInit(staticSecond, 2, sdn)?.networks).toEqual([
    {},
    { ipv4: { mode: "static", address: "192.168.1.5/24" } },
  ]);
  // 저장된 주소는 폼에 그대로 온다.
  const saved = vm("db", "stopped", {
    spec: {
      ...vm("db", "stopped").spec,
      nics: [{ network: "tenant", model: "virtio", ips: ["10.10.0.7", "fd00::7"] }],
    },
  });
  expect(fromVm(saved).nics[0]!.ips).toBe("10.10.0.7, fd00::7");
});

it("sends import addresses only for NICs mapped to SDN networks", () => {
  const form = { ...runForm(vmImport("web01", 1)), ips: ["10.10.0.5", "10.10.0.6"] };
  expect(toRun(form, [true, false]).ips).toEqual([["10.10.0.5"], []]);
  expect(toRun(form, [false, false]).ips).toBeUndefined();
  expect(toRun({ ...form, ips: undefined }).ips).toBeUndefined();
});

it("translates a failed turn-on that turned the lockdown off again and migration counts", async () => {
  const { lockdownText } = await import("@/sdn/sdn");
  const { planText } = await import("@/migration/migration");
  const t = (key: string, values?: Record<string, unknown>) =>
    values && Object.keys(values).length ? `${key} ${JSON.stringify(values)}` : key;
  expect(
    lockdownText(
      t,
      'the OVN central "n1" could not keep its address allow-list: x; turned the OVN lockdown off again',
    ),
  ).toBe('sdn.lockdown.text.notKept {"name":"n1"} sdn.lockdown.text.offAgain');
  expect(planText(t, '2 NIC(s) are still on network "lan"')).toBe(
    'migration.text.stillOnSource {"count":2,"network":"lan"}',
  );
});
