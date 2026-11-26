// 네트워크 층의 배치(계획 75 §4.2): 포트 줄은 블록 앞 바닥(배치를 바꾸지 않음), 레일은 판 앞쪽 띠(판 깊이에 더함 — 격자와
// 겹치지 않음), 선은 포트에서 레일로.
import { boxBounds } from "@/components/iso/iso";
import {
  layoutCluster,
  NODE,
  PORT_D,
  PORT_MAX,
  PORT_Y,
  RAIL_D,
  RAIL_MAX,
  railBand,
  SLAB_PAD,
} from "@/clusterMap/layout";
import { toModel } from "@/clusterMap/model";
import { bigCluster, mapNode, mapVm, physicalTopology } from "@/test/clusterMap";

const nodes = [
  mapNode(1, "n1", { labels: { zone: "a" } }),
  mapNode(2, "n2", { labels: { zone: "a" } }),
  mapNode(3, "n3", { labels: { zone: "a" } }),
  mapNode(4, "n4", { labels: { zone: "a" } }),
  mapNode(5, "n5", { labels: { zone: "b" } }),
  mapNode(6, "w", { labels: { zone: "b" }, spec: { witness: true } }),
];
const vms = [mapVm(1, "web", nodes[0]!.id), mapVm(2, "loose", null, "stopped")];

function scenes(physical = physicalTopology(nodes, { n1: ["eno1"], n2: ["eno3"] })) {
  const model = toModel({ nodes, vms, physical });
  return {
    model,
    plain: layoutCluster(model),
    net: layoutCluster(model, { rails: true }),
    bare: layoutCluster(toModel({ nodes, vms })),
  };
}

it("does not move anything without rails, and ports fit in the gap in front of the block", () => {
  const { plain, bare } = scenes();
  const text = (s: typeof plain) =>
    JSON.stringify([s.nodes.map((n) => n.box), s.slabs.map((x) => x.box)]);
  expect(text(plain)).toBe(text(bare));
  expect(plain.rails.size).toBe(0);
  expect(plain.wires).toEqual([]);
  const n1 = plain.byKey.get(`node:${nodes[0]!.id}`)!;
  expect(n1.net!.ports.map((p) => [p.name, p.tone, p.bond, p.fault])).toEqual([
    ["eno1", "down", "bond0", "degraded"],
    ["eno2", "up", "bond0", null],
    ["eno3", "up", null, null],
  ]);
  expect(n1.net!.bonds.map((b) => [b.name, b.tone])).toEqual([["bond0", "degraded"]]);
  for (const node of plain.nodes.filter((n) => n.net)) {
    for (const p of node.net!.ports) {
      // 블록 앞면 뒤, 블록 너비 안, 다음 줄 블록 앞
      expect(p.box.y).toBe(node.box.y + node.box.d + PORT_Y);
      expect(p.box.x + p.box.w).toBeLessThanOrEqual(node.box.x + node.box.w);
      const next = plain.nodes.find((m) => m.slab === node.slab && m.row === node.row + 1);
      if (next) {
        expect(p.box.y + PORT_D).toBeLessThan(next.box.y);
      }
    }
    // 본딩 테두리는 구성 포트를 두른다
    for (const b of node.net!.bonds) {
      const members = node.net!.ports.filter((p) => p.bond === b.name);
      expect(b.box.x).toBeLessThan(members[0]!.box.x);
      expect(b.box.x + b.box.w).toBeGreaterThan(members.at(-1)!.box.x + members.at(-1)!.box.w);
    }
  }
  // 증인·물리 관계가 없는 노드는 줄이 없다
  expect(plain.nodes.find((n) => n.kind === "witness")!.net).toBeNull();
});

it("lays rails along each slab's front below the grid, deepening the slab by the rail band", () => {
  const { plain, net } = scenes();
  for (const slab of net.slabs.filter((s) => s.kind !== "unplaced")) {
    const before = plain.slabs.find((s) => s.key === slab.key)!;
    expect(slab.rails.map((r) => r.carrier)).toEqual(["underlay", "vmbr0"]);
    expect(slab.box.d - before.box.d).toBe(railBand(slab.rails.length));
    const front = Math.max(
      ...net.nodes.filter((n) => n.slab === slab.key).map((n) => n.box.y + NODE),
    );
    for (const r of slab.rails) {
      // 격자(+ 포트 줄) 앞, 판 앞 여백(공유 풀 띠) 뒤
      expect(r.box.y).toBeGreaterThan(front + PORT_Y + PORT_D);
      expect(r.box.y + RAIL_D).toBeLessThanOrEqual(slab.box.y + slab.box.d - SLAB_PAD);
      expect(r.box.x).toBeGreaterThan(slab.box.x);
      expect(r.box.x + r.box.w).toBeLessThan(slab.box.x + slab.box.w);
    }
  }
  // 판끼리, 판과 배치 안 됨 판이 겹치지 않는다
  const rects = net.slabs.map((s) => s.box);
  for (let i = 0; i < rects.length; i++) {
    for (let j = i + 1; j < rects.length; j++) {
      const a = rects[i]!;
      const b = rects[j]!;
      const apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.d <= b.y || b.y + b.d <= a.y;
      expect(apart).toBe(true);
    }
  }
  const counts = net.slabs[0]!.rails.map((r) => [r.carrier, r.nodes, r.down, r.degraded]);
  expect(counts).toEqual([
    ["underlay", 4, 1, 0],
    ["vmbr0", 4, 0, 1],
  ]);
});

it("runs a wire from each node's port to its rail, coloured by the node's rail state", () => {
  const { net } = scenes();
  const n1 = `node:${nodes[0]!.id}`;
  const n2 = `node:${nodes[1]!.id}`;
  const wires = net.wires.filter((w) => w.node === n1 || w.node === n2);
  expect(
    wires.map((w) => [
      net.byKey.get(w.node)!.name,
      net.rails.get(w.rail)!.carrier,
      w.port?.split("/")[1],
      w.tone,
    ]),
  ).toEqual([
    ["n1", "underlay", "eno3", "up"],
    ["n1", "vmbr0", "eno1", "degraded"],
    ["n2", "underlay", "eno3", "down"],
    ["n2", "vmbr0", "eno1", "up"],
  ]);
  for (const w of net.wires) {
    const rail = net.rails.get(w.rail)!;
    expect(w.to).toBe(rail.box.y);
    expect(w.y).toBeLessThan(w.to);
    expect(w.lane).toBeGreaterThan(rail.box.x);
    expect(w.lane).toBeLessThan(rail.box.x + rail.box.w);
  }
  // 증인에는 선이 없다
  expect(net.wires.some((w) => net.byKey.get(w.node)!.kind === "witness")).toBe(false);
});

it("shows at most 8 ports and 8 rails, counting the rest", () => {
  const phys = physicalTopology(nodes, {}, 9);
  for (let i = 0; i < 10; i++) {
    phys.networks.push({
      id: `019a0f3c-0000-7000-8f20-${String(i).padStart(12, "0")}`,
      name: `x${i}`,
      kind: "bridge",
      carrier: `br${i}`,
      nodes: [{ node: "n1", state: "up" }],
    });
  }
  const s = layoutCluster(toModel({ nodes, vms, physical: phys }), { rails: true });
  const n1 = s.byKey.get(`node:${nodes[0]!.id}`)!;
  expect(n1.net!.ports).toHaveLength(PORT_MAX);
  expect(n1.net!.more).toBe(3 + 9 - PORT_MAX);
  const a = s.slabs[0]!;
  expect(a.rails).toHaveLength(RAIL_MAX);
  expect(a.railMore).toBe(12 - RAIL_MAX);
  // 판 b는 n1이 없어 br* 레일을 쓰지 않는다
  expect(s.slabs[1]!.rails.map((r) => r.carrier)).toEqual(["underlay", "vmbr0"]);
  // 레일 띠 끝(마지막 레일)도 공유 풀 띠 자리 앞
  const last = a.rails.at(-1)!.box;
  expect(last.y + RAIL_D).toBeLessThanOrEqual(a.box.y + a.box.d - SLAB_PAD);
});

it("keeps the rail band inside the picture bounds of a big cluster", () => {
  const input = bigCluster(30, 5, 3);
  const s = layoutCluster(toModel({ ...input, physical: physicalTopology(input.nodes) }), {
    rails: true,
  });
  for (const r of s.rails.values()) {
    const b = boxBounds(r.box);
    expect(b.minX).toBeGreaterThanOrEqual(s.bounds.minX);
    expect(b.maxX).toBeLessThanOrEqual(s.bounds.maxX);
    expect(b.maxY).toBeLessThanOrEqual(s.bounds.maxY);
  }
});
