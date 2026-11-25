import {
  cephRoles,
  confirmWipe,
  defaultSize,
  diskChoice,
  healthTone,
  isManagedPool,
  monWarning,
  osdHosts,
  percentOf,
  pgSummary,
  publicNetworkFor,
  sameNodePath,
  sizeWarning,
} from "@/ceph/ceph";
import { CEPH, CEPH_STATUS, DISKS, NET_NODE, NODE, pool } from "@/test/fixtures";

it("maps the Ceph health to a tone", () => {
  expect(healthTone("HEALTH_OK")).toBe("ok");
  expect(healthTone("HEALTH_WARN")).toBe("warn");
  expect(healthTone("HEALTH_ERR")).toBe("err");
  expect(healthTone("")).toBe("unknown");
});

it("groups OSDs by host in name and id order", () => {
  const hosts = osdHosts(CEPH_STATUS);
  expect(hosts.map((h) => [h.host, h.osds.map((o) => o.id), h.up])).toEqual([
    ["node-a", [0, 1], 1],
    ["node-b", [2, 3], 2],
  ]);
  expect(osdHosts({ ...CEPH_STATUS, osds: [] })).toEqual([]);
});

it("summarises placement group states, most first", () => {
  expect(pgSummary(CEPH_STATUS.pgs)).toEqual([
    { state: "active+clean", count: 120, percent: (120 * 100) / 129, clean: true },
    { state: "active+undersized", count: 9, percent: (9 * 100) / 129, clean: false },
  ]);
  expect(pgSummary({ total: 0, states: [] })).toEqual([]);
});

it("defaults the pool size to min(3, hosts) and warns about 1 and 2", () => {
  expect([0, 1, 2, 3, 7].map(defaultSize)).toEqual([1, 1, 2, 3, 3]);
  expect(sizeWarning(1)).toBe("ceph.pool.sizeOne");
  expect(sizeWarning(2)).toBe("ceph.pool.sizeTwo");
  expect(sizeWarning(3)).toBeNull();
});

it("warns about an even number of monitors", () => {
  expect([1, 2, 3, 4, 5].map(monWarning)).toEqual([
    null,
    "ceph.mon.even",
    null,
    "ceph.mon.even",
    null,
  ]);
});

it("says whether a disk can take an OSD", () => {
  expect(DISKS.map(diskChoice)).toEqual([
    { kind: "blocked", reason: "system disk (/ is on it)" },
    { kind: "available" },
    { kind: "wipe", reasons: ["gpt partition table", "ext4 filesystem"] },
  ]);
});

it("accepts the device path, its name or its by-id link to confirm a wipe", () => {
  const [, sdb] = DISKS;
  for (const ok of ["/dev/sdb", "sdb", " sdb ", "/dev/disk/by-id/wwn-0x5000c500a1b2c3d4"]) {
    expect(confirmWipe(sdb!, ok)).toBe(true);
  }
  for (const bad of ["", "/dev/sdc", "sd", "dev/sdb", "wwn-0x5000c500a1b2c3d4"]) {
    expect(confirmWipe(sdb!, bad)).toBe(false);
  }
});

it("fills the public network from the interface holding the node's address", () => {
  // NET_NODE의 주소 10.0.0.1은 vmbr0의 10.0.0.1/24에 있다.
  expect(publicNetworkFor(NET_NODE)).toBe("10.0.0.0/24");
  expect(
    publicNetworkFor({ ...NET_NODE, spec: { ...NET_NODE.spec, address: "10.0.0.1:8443" } }),
  ).toBe("10.0.0.0/24");
  // 인터페이스가 없거나, 주소가 이름이거나 IPv6면 채우지 않는다(서버가 정한다).
  expect(publicNetworkFor(NODE)).toBe("");
  expect(
    publicNetworkFor({ ...NET_NODE, spec: { ...NET_NODE.spec, address: "node-a:8443" } }),
  ).toBe("");
  expect(
    publicNetworkFor({ ...NET_NODE, spec: { ...NET_NODE.spec, address: "[fd00::1]:8443" } }),
  ).toBe("");
  const wide = {
    ...NET_NODE,
    spec: { ...NET_NODE.spec, address: "172.16.5.9:8443" },
    status: {
      ...NET_NODE.status,
      interfaces: [{ name: "eth0", kind: "ethernet", state: "up", addresses: ["172.16.5.9/12"] }],
    },
  } as typeof NET_NODE;
  expect(publicNetworkFor(wide)).toBe("172.16.0.0/12");
});

it("lists the Ceph daemons of a node", () => {
  expect(cephRoles(CEPH, "n1")).toEqual(["mon", "mgr", "osd"]);
  expect(cephRoles(CEPH, "n3")).toEqual(["mon"]);
  expect(cephRoles(CEPH, "n9")).toEqual([]);
  expect(cephRoles(null, "n1")).toEqual([]);
});

it("knows the storage pools of the managed Ceph by their configuration", () => {
  const managed = pool("vms", {
    backend: { kind: "rbd", pool: "vmpool", user: "orbivisor", conf: "/etc/opl/ceph/ceph.conf" },
  });
  const outside = pool("ext", { backend: { kind: "rbd", pool: "rbd" } });
  // 이름 바꾸기 앞에 저장된 풀(옛 /etc/opl)도, 새 이름의 풀도 관리 풀이다.
  expect(isManagedPool(managed, CEPH)).toBe(true);
  const renamed = pool("vms2", {
    backend: {
      kind: "rbd",
      pool: "vmpool",
      user: "orbivisor",
      conf: "/etc/orbivisor/ceph/ceph.conf",
    },
  });
  expect(isManagedPool(renamed, CEPH)).toBe(true);
  expect(sameNodePath("/etc/opl-ceph/x", "/etc/orbivisor-ceph/x")).toBe(false);
  expect(isManagedPool(outside, CEPH)).toBe(false);
  expect(isManagedPool(managed, null)).toBe(false);
  expect(isManagedPool(pool("local"), CEPH)).toBe(false);
});

it("computes a usage percentage", () => {
  expect(percentOf(1, 4)).toBe(25);
  expect(percentOf(1, 3)).toBe(33.3);
  expect(percentOf(5, 0)).toBe(0);
  expect(percentOf(9, 4)).toBe(100);
});
