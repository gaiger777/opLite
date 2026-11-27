import { type Box, boxBounds, boxFace, type Point } from "@/components/iso/iso";
import {
  DENSE_MAX,
  GAP,
  layoutCluster,
  MAX_CELLS,
  type MapScene,
  NODE,
  nodeTop,
  PER_LAYER,
  PITCH,
  ROW_EXTRA,
  TILE_GAP,
  TILE_H,
} from "@/clusterMap/layout";
import { toModel } from "@/clusterMap/model";
import { load, mapNode, mapVm, placement, randomCluster, rng } from "@/test/clusterMap";

function shuffled<T>(xs: readonly T[], seed: number): T[] {
  const r = rng(seed);
  const out = [...xs];
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(r() * (i + 1));
    [out[i], out[j]] = [out[j]!, out[i]!];
  }
  return out;
}

const slabNames = (s: MapScene) => s.slabs.map((x) => [x.kind, x.name]);
const nodesOn = (s: MapScene, slab: string) =>
  s.nodes
    .filter((n) => n.slab === slab)
    .sort((a, b) => a.row - b.row || a.col - b.col)
    .map((n) => [n.name, n.row, n.col]);

it("lays out the same picture for the same input, whatever the order (property)", () => {
  for (let seed = 1; seed <= 40; seed++) {
    const c = randomCluster(seed);
    const a = layoutCluster(toModel(c));
    const b = layoutCluster(
      toModel({ nodes: shuffled(c.nodes, seed), vms: shuffled(c.vms, seed + 1) }),
    );
    // 색인(Map)은 같은 원소를 가리키므로 글로 견준다(깊은 비교가 느리다)
    const text = (x: MapScene) => JSON.stringify([x.slabs, x.nodes, x.links, x.bounds, x.bands]);
    expect(text(b)).toBe(text(a));
    expect([...b.tiles.keys()]).toEqual([...a.tiles.keys()]);
  }
});

it("groups nodes by zone, racks as rows, the no-zone slab last", () => {
  const nodes = [
    mapNode(1, "n1", { labels: { zone: "z10", rack: "r2" } }),
    mapNode(2, "n2", { labels: { zone: "z10", rack: "r1" } }),
    mapNode(3, "n3", { labels: { zone: "z10", rack: "r1" } }),
    mapNode(4, "n4", { labels: { zone: "z10" } }),
    mapNode(5, "n5", { labels: { zone: "z2" } }),
    mapNode(6, "n6"),
  ];
  const s = layoutCluster(toModel({ nodes, vms: [] }));
  // 값의 자연 차례(z2 < z10), 값 없는 노드는 끝
  expect(slabNames(s)).toEqual([
    ["group", "z2"],
    ["group", "z10"],
    ["nogroup", null],
  ]);
  // 랙마다 한 줄(r1 → r2), 랙 없는 노드는 끝 줄, 열 수 = 가장 긴 랙
  expect(nodesOn(s, "slab:=z10")).toEqual([
    ["n2", 0, 0],
    ["n3", 0, 1],
    ["n1", 1, 0],
    ["n4", 2, 0],
  ]);
  const z10 = s.slabs.find((x) => x.key === "slab:=z10")!;
  expect(z10.box.w).toBe(40 * 2 + 2 * NODE + GAP);
  expect(z10.count).toBe(4);
});

it("groups by rack, by a chosen label key, or not at all", () => {
  const nodes = [
    mapNode(1, "a", { labels: { zone: "x", rack: "r1", room: "B" } }),
    mapNode(2, "b", { labels: { zone: "x", rack: "r2", room: "A" } }),
    mapNode(3, "c", { labels: { zone: "y", rack: "r1" } }),
  ];
  const model = toModel({ nodes, vms: [], cluster: { name: "prod" } });
  expect(slabNames(layoutCluster(model, { group: { kind: "rack" } }))).toEqual([
    ["group", "r1"],
    ["group", "r2"],
  ]);
  expect(slabNames(layoutCluster(model, { group: { kind: "label", key: "room" } }))).toEqual([
    ["group", "A"],
    ["group", "B"],
    ["nogroup", null],
  ]);
  const none = layoutCluster(model, { group: { kind: "none" } });
  expect(slabNames(none)).toEqual([["all", "prod"]]);
  // 묶지 않으면 ⌈√n⌉열 격자(랙 줄 없음)
  expect(nodesOn(none, "slab:~all")).toEqual([
    ["a", 0, 0],
    ["b", 0, 1],
    ["c", 1, 0],
  ]);
});

it("draws one slab when no node has the label", () => {
  const s = layoutCluster(toModel({ nodes: [mapNode(1, "a"), mapNode(2, "b")], vms: [] }));
  expect(slabNames(s)).toEqual([["all", null]]);
  expect(layoutCluster(toModel({ nodes: [], vms: [] })).slabs).toEqual([]);
});

it("keeps every VM tile in its name-order cell when states change", () => {
  const n = mapNode(1, "a");
  const names = Array.from({ length: 20 }, (_, i) => `vm${i}`);
  const before = layoutCluster(
    toModel({ nodes: [n], vms: names.map((name, i) => mapVm(i, name, n.id, "running")) }),
  );
  const after = layoutCluster(
    toModel({
      nodes: [n],
      vms: names.map((name, i) => mapVm(i, name, n.id, i % 3 === 0 ? "failed" : "stopped")),
    }),
  );
  const cells = (s: MapScene) => s.nodes[0]!.grid!.cells.map((c) => [c.key, c.index, c.box]);
  expect(cells(after)).toEqual(cells(before));
  expect(after.nodes[0]!.grid!.cells[0]!.tone).toBe("failed");
  expect(after.nodes[0]!.grid!.cells[1]!.tone).toBe("stopped");
  // 이름 순: vm0, vm1, vm2 … vm10(숫자를 숫자로)
  expect(before.nodes[0]!.grid!.cells.map((c) => c.index)).toEqual(names.map((_, i) => i));
  expect(before.nodes[0]!.grid!.cells[10]!.key).toBe(`vm:${mapVm(10, "vm10", null).id}`);
  // 4 × 4 층, 넘치면 위 층
  expect(before.nodes[0]!.grid!.layers).toBe(2);
  expect(before.nodes[0]!.grid!.cells[16]!.layer).toBe(1);
  expect(before.nodes[0]!.stack).toBe(2 * TILE_H);
});

it("packs a busy node's VMs into one dense layer on its top and shows unreadable VMs as +N hidden", () => {
  const n = mapNode(1, "a");
  const vms = Array.from({ length: 70 }, (_, i) =>
    mapVm(i, `vm${String(i).padStart(2, "0")}`, n.id),
  );
  const s = layoutCluster(toModel({ nodes: [n], vms }));
  const node = s.nodes[0]!;
  const grid = node.grid!;
  // 64칸을 넘으면 층을 쌓지 않고 칸을 줄여 한 층에 모두(이름 순 자리)
  expect(grid.cells).toHaveLength(70);
  expect(grid.layers).toBe(1);
  expect(grid.size).toBeLessThan(20);
  expect(grid.cells.every((c) => c.kind === "vm" && c.layer === 0)).toBe(true);
  expect(node.stack).toBe(grid.h);
  for (const c of grid.cells) {
    // 노드 윗면 안
    expect(c.box.x).toBeGreaterThanOrEqual(node.box.x);
    expect(c.box.y).toBeGreaterThanOrEqual(node.box.y);
    expect(c.box.x + c.box.w).toBeLessThanOrEqual(node.box.x + NODE);
    expect(c.box.y + c.box.d).toBeLessThanOrEqual(node.box.y + NODE);
  }
  // 읽을 수 없는 VM이 있으면 그 칸도
  const hidden = layoutCluster(
    toModel({ nodes: [n], vms, placement: placement([load(n, { vms: 75 })]), vmsLimited: true }),
  );
  const hc = hidden.nodes[0]!.grid!.cells;
  expect(hc).toHaveLength(71);
  expect(hc[70]).toMatchObject({ kind: "hidden", key: `hidden:node:${n.id}`, count: 5 });
  // 적으면 VM 뒤에 한 칸
  const few = layoutCluster(
    toModel({
      nodes: [n],
      vms: vms.slice(0, 3),
      placement: placement([load(n, { vms: 4 })]),
      vmsLimited: true,
    }),
  );
  expect(few.nodes[0]!.grid!.cells.map((c) => c.kind)).toEqual(["vm", "vm", "vm", "hidden"]);
  // 촘촘한 격자도 DENSE_MAX를 넘으면 "+N"
  const many = Array.from({ length: DENSE_MAX + 5 }, (_, i) => mapVm(i, `v${i}`, n.id));
  const huge = layoutCluster(toModel({ nodes: [n], vms: many })).nodes[0]!.grid!.cells;
  expect(huge).toHaveLength(DENSE_MAX);
  expect(huge.at(-1)).toMatchObject({ kind: "more", count: 6 });
});

it("folds a busy node's stopped VMs into one band that unfolds on request", () => {
  const n = mapNode(1, "a");
  // 131대 가운데 66대 꺼짐(계획 77 관찰의 devserver 노드)
  const vms = Array.from({ length: 131 }, (_, i) =>
    mapVm(i, `vm${String(i).padStart(3, "0")}`, n.id, i % 2 === 0 ? "stopped" : "running"),
  );
  const s = layoutCluster(toModel({ nodes: [n], vms }));
  const node = s.nodes[0]!;
  const cells = node.grid!.cells;
  expect(cells.filter((c) => c.kind === "vm")).toHaveLength(65);
  const fold = cells.at(-1)!;
  expect(fold).toMatchObject({ kind: "fold", key: `fold:node:${n.id}`, count: 66, open: false });
  // 덩어리는 윗면 앞쪽 띠(격자 앞, 윗면 안)
  expect(fold.box.w).toBeGreaterThan(cells[0]!.box.w * 5);
  expect(fold.box.y).toBeGreaterThan(Math.max(...cells.slice(0, -1).map((c) => c.box.y)));
  expect(fold.box.y + fold.box.d).toBeLessThanOrEqual(node.box.y + NODE);
  expect(s.tiles.get(fold.key)!.node).toBe(node);
  // 펼치면 꺼진 VM도 칸으로, 띠는 접기 단추로 남는다
  const open = layoutCluster(toModel({ nodes: [n], vms }), { open: new Set([node.key]) });
  const oc = open.nodes[0]!.grid!.cells;
  expect(oc.filter((c) => c.kind === "vm")).toHaveLength(131);
  expect(oc.at(-1)).toMatchObject({ kind: "fold", count: 66, open: true });
  // 꺼진 VM이 없으면 띠도 없다
  const on = vms.map((vm, i) => mapVm(i, vm.name, n.id));
  expect(
    layoutCluster(toModel({ nodes: [n], vms: on })).nodes[0]!.grid!.cells.some(
      (c) => c.kind === "fold",
    ),
  ).toBe(false);
});

it("keeps the not-placed tray no bigger than a node block", () => {
  const n = mapNode(1, "a");
  for (const count of [1, 5, 16, 17, 66, 300]) {
    const vms = Array.from({ length: count }, (_, i) =>
      mapVm(i, `vm${i}`, null, i % 3 === 0 ? "running" : "stopped", {
        status: { node: null },
      }),
    );
    const s = layoutCluster(toModel({ nodes: [n], vms }));
    const tray = s.slabs.find((x) => x.kind === "unplaced")!;
    expect(tray.count).toBe(count);
    expect(tray.box.w).toBeLessThanOrEqual(NODE);
    expect(tray.box.d).toBeLessThanOrEqual(NODE);
    const cells = s.byKey.get("unplaced")!.grid!.cells;
    for (const c of cells) {
      expect(c.box.x + c.box.w).toBeLessThanOrEqual(tray.box.x + tray.box.w);
      expect(c.box.y + c.box.d).toBeLessThanOrEqual(tray.box.y + tray.box.d);
    }
    // 16대를 넘으면 꺼진 VM을 접는다
    expect(cells.some((c) => c.kind === "fold")).toBe(count > 16);
  }
});

it("shows a migrating VM on its source and a ghost on the target's next cell", () => {
  const a = mapNode(1, "a");
  const b = mapNode(2, "b");
  const web = mapVm(1, "web", a.id, "migrating", {
    status: { node: a.id, migration: { node: b.id, task: "t1" } },
  });
  const s = layoutCluster(toModel({ nodes: [a, b], vms: [web, mapVm(2, "db", b.id)] }));
  const src = s.byKey.get(`node:${a.id}`)!.grid!.cells;
  const dst = s.byKey.get(`node:${b.id}`)!.grid!.cells;
  expect(src[0]).toMatchObject({ key: `vm:${web.id}`, tone: "migrating" });
  expect(dst.map((c) => [c.key, c.tone])).toEqual([
    [`vm:${mapVm(2, "db", null).id}`, "running"],
    [`ghost:${web.id}`, "ghost"],
  ]);
  expect(s.tiles.get(`ghost:${web.id}`)!.node.key).toBe(`node:${b.id}`);
  // 화살표: 원본 위 → 대상 위, 높이는 거리의 0.3배
  expect(s.links).toHaveLength(1);
  const link = s.links[0]!;
  expect(link.ends).toEqual([`node:${a.id}`, `node:${b.id}`]);
  expect(link.from).toEqual(nodeTop(s.byKey.get(`node:${a.id}`)!));
  expect(link.lift).toBe(
    Math.round(Math.hypot(link.to.x - link.from.x, link.to.y - link.from.y) * 0.3),
  );
});

it("spaces rows by the layers stacked on the row in front", () => {
  const nodes = Array.from({ length: 4 }, (_, i) => mapNode(i, `n${i}`));
  // 2 × 2 격자: 둘째 줄(n2)에 33대 → 3층
  const vms = Array.from({ length: 33 }, (_, i) => mapVm(i, `vm${i}`, nodes[2]!.id));
  const s = layoutCluster(toModel({ nodes, vms }));
  const y = (name: string) => s.nodes.find((n) => n.name === name)!.box.y;
  expect(y("n2") - y("n0")).toBe(PITCH + 3 * ROW_EXTRA);
  expect(y("n3")).toBe(y("n2"));
  const empty = layoutCluster(toModel({ nodes, vms: [] }));
  expect(empty.nodes.find((n) => n.name === "n2")!.box.y - empty.nodes[0]!.box.y).toBe(PITCH);
});

it("puts witnesses as pillars at the back edge and unplaced VMs on a tray in front", () => {
  const w = mapNode(1, "w", { spec: { witness: true } });
  const a = mapNode(2, "a");
  const s = layoutCluster(
    toModel({
      nodes: [w, a],
      vms: [
        mapVm(1, "new", null, "stopped"),
        mapVm(2, "x", a.id),
        mapVm(3, "y", "gone", "stopped"),
      ],
    }),
  );
  const pillar = s.byKey.get(`node:${w.id}`)!;
  const block = s.byKey.get(`node:${a.id}`)!;
  expect(pillar.kind).toBe("witness");
  expect(pillar.grid).toBeNull();
  expect(pillar.row).toBe(-1);
  expect(pillar.box.w).toBeLessThan(block.box.w);
  expect(pillar.box.y + pillar.box.d).toBeLessThanOrEqual(block.box.y);
  // 증인 띠가 먼저(뒤에 있다)
  expect(pillar.band).toBeLessThan(block.band);
  const tray = s.byKey.get("unplaced")!;
  expect(tray.kind).toBe("unplaced");
  expect(tray.grid!.cells.map((c) => c.key)).toEqual([
    `vm:${mapVm(1, "new", null).id}`,
    `vm:${mapVm(3, "y", null).id}`,
  ]);
  expect(tray.grid!.layers).toBe(1);
  const traySlab = s.slabs.at(-1)!;
  expect(traySlab).toMatchObject({ kind: "unplaced", count: 2 });
  // 판들 앞(가장 큰 y), 그리기 마지막
  expect(traySlab.box.y).toBeGreaterThan(s.slabs[0]!.box.y + s.slabs[0]!.box.d);
  expect(s.nodes.at(-1)).toBe(tray);
});

it("tags node states", () => {
  const nodes = [
    mapNode(1, "off", { status: { online: false } }),
    mapNode(2, "frz", { status: { online: false, shutdown: { action: "freeze", at: 1 } } }),
    mapNode(3, "mnt", { spec: { maintenance: true } }),
    mapNode(4, "big"),
  ];
  const moving = mapVm(1, "web", nodes[2]!.id, "migrating", {
    spec: { power: "running" },
    status: { node: nodes[2]!.id, migration: { node: nodes[3]!.id, task: "t" } },
  });
  const s = layoutCluster(
    toModel({
      nodes,
      vms: [moving],
      cluster: { name: "c", leader: nodes[3]!.id },
      placement: placement([
        load(nodes[3]!, { mem_allocated_mib: 20000, mem_total_mib: 16384, cpu_use_pct: 950 }),
      ]),
    }),
  );
  const tags = Object.fromEntries(s.nodes.map((n) => [n.name, n.tags]));
  expect(tags).toEqual({
    off: ["offline"],
    frz: ["offline", "frozen"],
    mnt: ["maintenance", "draining"],
    big: ["leader", "overcommit"],
  });
  expect(s.nodes.find((n) => n.name === "big")!.heat).toBe("hot");
  expect(s.nodes.find((n) => n.name === "off")!.heat).toBe("unknown");
});

it("marks tiles on offline nodes as last known and counts states for summary bars", () => {
  const n = mapNode(1, "a", { status: { online: false } });
  const s = layoutCluster(
    toModel({
      nodes: [n],
      vms: [mapVm(1, "a", n.id), mapVm(2, "b", n.id, "failed"), mapVm(3, "c", n.id, "starting")],
    }),
  );
  const item = s.nodes[0]!;
  expect(item.grid!.cells.every((c) => c.stale)).toBe(true);
  expect(item.counts).toEqual({ running: 1, stopped: 0, failed: 1, busy: 1 });
  expect(item.bars.map((b) => [b.tone, b.count])).toEqual([
    ["running", 1],
    ["busy", 1],
    ["failed", 1],
  ]);
});

/** 점이 볼록 다각형 안(경계 제외)인가 */
function inside(p: Point, poly: readonly Point[]): boolean {
  let sign = 0;
  for (let i = 0; i < poly.length; i++) {
    const a = poly[i]!;
    const b = poly[(i + 1) % poly.length]!;
    const cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
    if (Math.abs(cross) < 1e-6) {
      return false;
    }
    const s = Math.sign(cross);
    if (sign !== 0 && s !== sign) {
      return false;
    }
    sign = s;
  }
  return true;
}

function samples(poly: readonly Point[]): Point[] {
  const [a, b, c, d] = poly as [Point, Point, Point, Point];
  const out: Point[] = [];
  for (let i = 1; i < 8; i++) {
    for (let j = 1; j < 8; j++) {
      const u = i / 8;
      const v = j / 8;
      const top = { x: a.x + (b.x - a.x) * u, y: a.y + (b.y - a.y) * u };
      const bottom = { x: d.x + (c.x - d.x) * u, y: d.y + (c.y - d.y) * u };
      out.push({ x: top.x + (bottom.x - top.x) * v, y: top.y + (bottom.y - top.y) * v });
    }
  }
  return out;
}

it("never lets tiles of one layer overlap on screen, so merged paths draw in order", () => {
  // 높이 ≤ 간격이면 한 층 안 타일의 면끼리는 겹치지 않는다(§18 스테이지 1) — 칸 16개 모든 쌍의 세 면을 표본 점으로 본다.
  expect(TILE_H).toBeLessThanOrEqual(TILE_GAP);
  const n = mapNode(1, "a");
  const s = layoutCluster(
    toModel({
      nodes: [n],
      vms: Array.from({ length: PER_LAYER }, (_, i) => mapVm(i, `v${i}`, n.id)),
    }),
  );
  const cells = s.nodes[0]!.grid!.cells;
  for (const a of cells) {
    for (const b of cells) {
      if (a === b) {
        continue;
      }
      for (const fa of ["left", "right", "top"] as const) {
        for (const fb of ["left", "right", "top"] as const) {
          const poly = boxFace(b.box, fb);
          expect(samples(boxFace(a.box, fa)).some((p) => inside(p, poly))).toBe(false);
        }
      }
    }
  }
});

function overlaps(a: Box, b: Box): boolean {
  const p = boxBounds(a);
  const q = boxBounds(b);
  return p.minX < q.maxX && q.minX < p.maxX && p.minY < q.maxY && q.minY < p.maxY;
}

it("keeps nodes of one band apart on screen and tiles on their node (property)", () => {
  for (let seed = 1; seed <= 40; seed++) {
    const s = layoutCluster(toModel(randomCluster(seed)));
    // 그리기 차례는 띠 차례
    const bands = s.nodes.map((n) => n.band);
    expect(bands).toEqual([...bands].sort((a, b) => a - b));
    for (const a of s.nodes) {
      const tall = (n: typeof a): Box => ({ ...n.box, h: n.box.h + n.stack + 30 });
      for (const b of s.nodes) {
        if (a !== b && a.band === b.band) {
          expect(overlaps(tall(a), tall(b))).toBe(false);
        }
      }
      if (a.kind !== "node") {
        continue;
      }
      for (const c of a.grid!.cells) {
        // 타일은 노드 윗면 안, 층마다 높이 TILE_H
        expect(c.box.x).toBeGreaterThanOrEqual(a.box.x);
        expect(c.box.x + c.box.w).toBeLessThanOrEqual(a.box.x + a.box.w);
        expect(c.box.y).toBeGreaterThanOrEqual(a.box.y);
        expect(c.box.y + c.box.d).toBeLessThanOrEqual(a.box.y + a.box.d);
        expect(c.box.z).toBe(a.box.z + a.box.h + c.layer * TILE_H);
      }
      expect(a.grid!.cells.length).toBeLessThanOrEqual(MAX_CELLS);
    }
    // 판끼리 겹치지 않는다
    for (const p of s.slabs) {
      for (const q of s.slabs) {
        if (p !== q) {
          const overlapX = p.box.x < q.box.x + q.box.w && q.box.x < p.box.x + p.box.w;
          const overlapY = p.box.y < q.box.y + q.box.d && q.box.y < p.box.y + p.box.d;
          expect(overlapX && overlapY).toBe(false);
        }
      }
    }
  }
});

it("wraps slabs into rows so big clusters stay roughly square", () => {
  const nodes = Array.from({ length: 40 }, (_, i) =>
    mapNode(i, `n${i}`, { labels: { zone: `z${i % 10}` } }),
  );
  const s = layoutCluster(toModel({ nodes, vms: [] }));
  const rows = new Set(s.slabs.map((x) => x.box.y));
  expect(rows.size).toBeGreaterThan(1);
  const w = s.bounds.maxX - s.bounds.minX;
  const h = s.bounds.maxY - s.bounds.minY;
  expect(w / h).toBeLessThan(3);
  expect(h / w).toBeLessThan(3);
});
