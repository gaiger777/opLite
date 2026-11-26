import { leafPorts, nodeNet, physicalLayer, railsOf } from "@/clusterMap/network";
import type { PhysicalPort } from "@/physical/api";
import { mapNode, netId, physicalTopology } from "@/test/clusterMap";

const A = mapNode(1, "n1");
const B = mapNode(2, "n2");

const port = (name: string, extra: Partial<PhysicalPort>): PhysicalPort => ({
  name,
  kind: "ethernet",
  state: "up",
  health: "up",
  role: "uplink",
  ...extra,
});

it("follows a bridge down to its physical ports through a VLAN on a bond", () => {
  const ports = [
    port("eno1", { master: "bond0" }),
    port("eno2", { master: "bond0" }),
    port("bond0", { kind: "bond", bond: { mode: "802.3ad", state: "up", members: [] } }),
    port("bond0.100", { kind: "vlan", parent: "bond0", master: "vmbr100" }),
    port("vmbr100", { kind: "bridge", role: "other" }),
  ];
  expect(leafPorts(ports, "vmbr100")).toEqual(["eno1", "eno2"]);
  expect(leafPorts(ports, "bond0.100")).toEqual(["eno1", "eno2"]);
  expect(leafPorts(ports, "eno2")).toEqual(["eno2"]);
  expect(leafPorts(ports, "nope")).toEqual([]);
});

it("puts a bond's ports next to each other in the row, uplinks and underlay only", () => {
  const np = physicalTopology([A]).nodes[0]!;
  // 본딩 구성 포트가 노드가 알린 차례에서 떨어져 있어도 첫 구성 포트 자리에 모은다
  np.ports = [np.ports[0]!, np.ports[4]!, ...np.ports.filter((_, i) => i !== 0 && i !== 4)];
  const rails = railsOf(physicalTopology([A]).networks);
  const net = nodeNet(np, rails);
  expect(net.ports.map((p) => [p.name, p.bond, p.role])).toEqual([
    ["eno1", "bond0", "uplink"],
    ["eno2", "bond0", "uplink"],
    ["eno3", null, "underlay"],
  ]);
  expect(net.bonds.map((b) => [b.name, b.health, b.mode])).toEqual([["bond0", "up", "802.3ad"]]);
  expect(net.carriers).toEqual({ underlay: ["eno3"], vmbr0: ["eno1", "eno2"] });
  expect(net.faults).toBe(0);
  expect(net.severity).toBeNull();
});

it("marks the faulted port and the node's worst fault", () => {
  const { nodes } = physicalLayer(physicalTopology([A, B], { n1: ["eno1", "eno3"] }));
  const n1 = nodes.get(A.id)!;
  expect(n1.ports.map((p) => [p.name, p.health, p.fault?.severity ?? null])).toEqual([
    ["eno1", "down", "degraded"],
    ["eno2", "up", null],
    ["eno3", "down", "down"],
  ]);
  expect(n1.bonds[0]!.health).toBe("degraded");
  expect([n1.faults, n1.severity]).toEqual([2, "down"]);
  expect(nodes.get(B.id)!.faults).toBe(0);
});

it("groups networks by carrier into rails with each node's worst state", () => {
  const rails = railsOf(physicalTopology([A, B], { n1: ["eno1"], n2: ["eno3"] }).networks);
  expect(rails.map((r) => [r.carrier, r.kind, r.networks.map((n) => n.name)])).toEqual([
    ["underlay", "vxlan", ["overlay"]],
    ["vmbr0", "bridge", ["lan", "v100"]],
  ]);
  expect(rails[1]!.networks[1]).toEqual({ id: netId(2), name: "v100", vlan: 100 });
  expect([...rails[1]!.states]).toEqual([
    ["n1", "degraded"],
    ["n2", "up"],
  ]);
  expect([...rails[0]!.states]).toEqual([
    ["n1", "up"],
    ["n2", "down"],
  ]);
  // 한 네트워크라도 쓰면 그 상태, 모두 없으면 absent
  const mixed = railsOf([
    {
      id: netId(1),
      name: "a",
      kind: "bridge",
      carrier: "br",
      nodes: [{ node: "x", state: "absent" }],
    },
    {
      id: netId(2),
      name: "b",
      kind: "bridge",
      carrier: "br",
      nodes: [{ node: "x", state: "down" }],
    },
    {
      id: netId(3),
      name: "c",
      kind: "bridge",
      carrier: "br",
      nodes: [{ node: "y", state: "absent" }],
    },
  ]);
  expect([...mixed[0]!.states]).toEqual([
    ["x", "down"],
    ["y", "absent"],
  ]);
});

it("finds a provider network's port through the node's physnet bridge", () => {
  const np = physicalTopology([A]).nodes[0]!;
  np.physnets = [{ name: "phys1", bridge: "vmbr0", ready: true }];
  const net = nodeNet(
    np,
    railsOf([
      {
        id: netId(9),
        name: "prov",
        kind: "sdn-provider",
        carrier: "phys1",
        nodes: [{ node: "n1", state: "up" }],
      },
    ]),
  );
  expect(net.carriers).toEqual({ phys1: ["eno1", "eno2"] });
});
