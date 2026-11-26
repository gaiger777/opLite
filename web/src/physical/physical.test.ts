import { capacityRow, capacityRows, overflow } from "@/physical/capacity";
import { buildChain, carrierOf, faultList, lowerPorts } from "@/physical/chain";
import { threadGroups, threadsOf } from "@/physical/threads";
import { vmPath } from "@/physical/vmPath";
import type { NodeTopologyInfo } from "@/numa/numa";
import { vm, volume } from "@/test/fixtures";
import {
  CAP_A,
  CAP_B,
  CAP_C,
  CAPACITY,
  LAN,
  NODE_A,
  NODE_B,
  OVERLAY,
  PHYSICAL,
  RAILS,
  TENANT,
} from "@/test/physical";

const TOPO: NodeTopologyInfo = {
  node: "node-a",
  host_nodes: [],
  vms: [
    { vm: "db", host_nodes: [1], vcpus: [8, 9], siblings: "", emulator: "", page: "2M", pages: 64 },
    { vm: "web", host_nodes: [], vcpus: [], siblings: "", emulator: "", pages: 0 },
  ],
  sets: { reserved: "0", dedicated: "8-15", shared: "1-7" },
  warnings: [],
};

describe("network chain", () => {
  it("lays out ports, bonds, VLANs, bridges, the underlay and networks", () => {
    const chain = buildChain(NODE_A, RAILS, new Map([[LAN, 3]]));
    expect(chain.items.map((i) => [i.kind, i.label])).toEqual([
      ["ethernet", "eno1"],
      ["ethernet", "eno2"],
      ["bond", "bond0"],
      ["vlan", "bond0.100"],
      ["ethernet", "eno3"],
      ["bridge", "vmbr0"],
      ["bridge", "vmbr100"],
      ["underlay", "10.0.0.1"],
      ["network", "lan"],
      ["network", "overlay"],
      ["network", "tenant"],
    ]);
    // 쓰지 않는 포트는 칸이 아니라 수로만
    expect(chain.unused).toBe(1);
    expect(chain.edges.map((e) => `${e.from}>${e.to}`).sort()).toEqual(
      [
        "i:eno1>i:bond0",
        "i:eno2>i:bond0",
        "i:bond0>i:vmbr0",
        "i:bond0.100>i:vmbr100",
        "i:bond0>i:bond0.100",
        "i:eno3>u:underlay",
        "u:underlay>n:" + OVERLAY,
        "i:vmbr0>n:" + LAN,
        "i:vmbr100>n:" + TENANT,
      ].sort(),
    );
    const lan = chain.items.find((i) => i.label === "lan")!;
    expect([lan.health, lan.vms]).toEqual(["degraded", 3]);
    expect(chain.items.find((i) => i.label === "tenant")!.note).toBe("VLAN 100");
    expect(chain.items.find((i) => i.label === "bond0.100")!.note).toBe("VLAN 100");
  });

  it("paints the path from a fault port to the networks it reaches", () => {
    const chain = buildChain(NODE_A, RAILS);
    const fault = (label: string) => chain.items.find((i) => i.label === label)?.fault;
    expect(["eno1", "bond0", "bond0.100", "vmbr0", "vmbr100", "lan", "tenant"].map(fault)).toEqual(
      Array(7).fill("degraded"),
    );
    // 장애에서 닿지 않는 것은 그대로
    expect(["eno2", "eno3", "10.0.0.1", "overlay"].map(fault)).toEqual(Array(4).fill(undefined));
    expect(chain.edges.filter((e) => e.fault).map((e) => e.from)).toEqual(
      expect.arrayContaining(["i:eno1", "i:bond0", "i:vmbr0"]),
    );
    expect(chain.edges.find((e) => e.from === "i:eno2")!.fault).toBeUndefined();

    // 둘이 닿으면 down이 이긴다.
    const down = buildChain(
      {
        ...NODE_A,
        impact: [
          ...NODE_A.impact!,
          { port: "bond0", severity: "down", bridges: [], networks: [], vms: 0, vm_ids: [] },
        ],
      },
      RAILS,
    );
    expect(down.items.find((i) => i.label === "lan")!.fault).toBe("down");
    expect(down.items.find((i) => i.label === "eno1")!.fault).toBe("degraded");
  });

  it("walks back from a network to its carrier and physical ports", () => {
    const chain = buildChain(NODE_A, RAILS);
    expect(carrierOf(chain, TENANT)!.label).toBe("vmbr100");
    expect(lowerPorts(chain, "i:vmbr100").map((p) => p.label)).toEqual(["eno1", "eno2"]);
    expect(carrierOf(chain, OVERLAY)!.kind).toBe("underlay");
    expect(lowerPorts(chain, "u:underlay").map((p) => p.label)).toEqual(["eno3"]);
    // 이 노드에 없는 네트워크는 칸이 없다.
    const b = buildChain(NODE_B, RAILS);
    expect(b.items.map((i) => i.label)).toEqual(["eth0", "vmbr0", "lan"]);
    expect(carrierOf(b, OVERLAY)).toBeUndefined();
  });

  it("lists faults across the cluster", () => {
    expect(faultList(PHYSICAL.nodes)).toEqual([
      { id: "n1", node: "node-a", port: "eno1", severity: "degraded", vms: 2 },
    ]);
  });
});

describe("capacity table", () => {
  it("measures CPU on the shared threads, memory and local pools", () => {
    const a = capacityRow(CAP_A, NODE_A);
    // 16 − 예약 1 − 전용 8 = 공유 7, 공유 vCPU 14
    expect(a.cpu).toEqual({ total: 7, allocated: 14, used: expect.closeTo(2.8) });
    expect(a.cpuOvercommit).toBe(2);
    expect(overflow(a.cpu)).toBe(7);
    expect(a.memory).toEqual({ total: 65536, allocated: 32768, used: 20480 });
    expect(overflow(a.memory)).toBe(0);
    expect(a.storage.allocated).toBe(150 * 1024 ** 3);
    expect(a.problems).toEqual(["overcommit", "hot", "link"]);
    expect(capacityRow(CAP_B).problems).toEqual(["overcommit", "pool"]);
    expect(capacityRow(CAP_C).problems).toEqual([]);
    expect(capacityRow({ ...CAP_C, online: false }).problems).toEqual(["offline"]);
  });

  it("sorts and filters to the nodes with problems", () => {
    const names = (opts: Parameters<typeof capacityRows>[2]) =>
      capacityRows(CAPACITY.nodes, PHYSICAL.nodes, opts).map((r) => r.node);
    expect(names({})).toEqual(["node-a", "node-b", "node-c"]);
    expect(names({ sort: "node", desc: true })).toEqual(["node-c", "node-b", "node-a"]);
    // 메모리 할당 ÷ 총: a 0.5, b 1.5, c 0.25
    expect(names({ sort: "memory", desc: true })).toEqual(["node-b", "node-a", "node-c"]);
    expect(names({ sort: "cpu" })).toEqual(["node-b", "node-c", "node-a"]);
    // 총이 0인데 할당이 없으면 0(b의 풀은 비었다)
    expect(names({ sort: "storage", desc: true })).toEqual(["node-a", "node-b", "node-c"]);
    expect(names({ problemsOnly: true })).toEqual(["node-a", "node-b"]);
  });
});

describe("thread grid", () => {
  it("groups threads by NUMA node with reserved, dedicated and pinned VMs", () => {
    const groups = threadGroups(CAP_A, TOPO);
    expect(groups.map((g) => [g.numa, g.cells.length])).toEqual([
      [0, 8],
      [1, 8],
    ]);
    expect(groups[0]!.cells[0]).toEqual({ cpu: 0, kind: "reserved" });
    expect(groups[0]!.cells[1]).toEqual({ cpu: 1, kind: "shared" });
    expect(groups[1]!.cells.slice(0, 3)).toEqual([
      { cpu: 8, kind: "dedicated", vm: "db" },
      { cpu: 9, kind: "dedicated", vm: "db" },
      { cpu: 10, kind: "dedicated" },
    ]);
    expect(threadsOf(TOPO, "db")).toEqual([8, 9]);
    expect(threadsOf(TOPO, "web")).toEqual([]);
  });

  it("falls back to one group without NUMA", () => {
    // 토폴로지의 집합으로
    const one = threadGroups({ ...CAP_A, numa: [] }, TOPO);
    expect(one).toHaveLength(1);
    expect(one[0]!.numa).toBeUndefined();
    expect(one[0]!.cells.map((c) => c.kind).slice(0, 2)).toEqual(["reserved", "shared"]);
    // 아무것도 모르면 스레드 수만큼 공유
    const bare = threadGroups(CAP_B);
    expect(bare[0]!.cells).toHaveLength(8);
    expect(bare[0]!.cells.every((c) => c.kind === "shared" && c.vm === undefined)).toBe(true);
  });
});

describe("VM path", () => {
  it("follows vCPUs, memory, disks and NICs down to the ports", () => {
    const db = vm("db", "running", {
      spec: {
        ...vm("db", "running").spec,
        hugepages: "2M",
        disks: [
          { volume: "db-root", bus: "virtio-blk" },
          { volume: "db-data", bus: "virtio-blk" },
          { volume: "gone", bus: "virtio-blk" },
        ],
        nics: [
          { network: "tenant", model: "virtio" },
          { network: "overlay", model: "virtio" },
          { network: "nowhere", model: "virtio" },
        ],
      },
    });
    const path = vmPath(db, {
      topology: TOPO,
      capacity: CAP_A,
      physical: NODE_A,
      rails: RAILS,
      volumes: [volume("db-root", "fast"), volume("db-data", "ceph")],
      sharedPools: CAPACITY.shared_pools,
    });
    expect([path.vcpus, path.pinned, path.numa, path.hugepages]).toEqual([2, [8, 9], [1], "2M"]);
    expect(path.disks).toEqual([
      { volume: "db-root", pool: "fast", ready: true },
      { volume: "db-data", pool: "ceph", ready: true },
      { volume: "gone", pool: undefined, ready: undefined },
    ]);
    const [tenant, overlay, nowhere] = path.nics;
    expect(tenant!.carrier!.label).toBe("vmbr100");
    expect(tenant!.state).toBe("degraded");
    expect(tenant!.ports.map((p) => [p.label, p.health])).toEqual([
      ["eno1", "down"],
      ["eno2", "up"],
    ]);
    expect(overlay!.ports.map((p) => p.label)).toEqual(["eno3"]);
    expect(nowhere).toEqual({
      network: "nowhere",
      state: undefined,
      carrier: undefined,
      ports: [],
    });
  });

  it("leaves a shared, unplaced VM without pins or ports", () => {
    const web = vm("web", "stopped");
    const path = vmPath(web, { rails: RAILS, volumes: [] });
    expect([path.pinned, path.numa, path.disks, path.nics]).toEqual([[], [], [], []]);
  });
});
