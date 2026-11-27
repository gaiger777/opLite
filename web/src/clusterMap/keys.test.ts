import { layoutCluster } from "@/clusterMap/layout";
import { type Focus, isNavKey, type NavKey, neighbor, readingOrder } from "@/clusterMap/keys";
import { toModel } from "@/clusterMap/model";
import { mapNode, mapVm, nodeId, randomCluster, vmId } from "@/test/clusterMap";

const node = (n: number): Focus => ({ kind: "node", key: `node:${nodeId(n)}` });
const tile = (n: number, v: number): Focus => ({
  kind: "tile",
  node: `node:${nodeId(n)}`,
  key: `vm:${vmId(v)}`,
});

// 판 x: a(0,0) b(0,1) / c(1,0), 판 y: d, 판 x 뒤에 증인 w. 배치 안 됨 판(앞)에 VM 하나.
function scene() {
  const nodes = [
    mapNode(1, "a", { labels: { zone: "x" } }),
    mapNode(2, "b", { labels: { zone: "x" } }),
    mapNode(3, "c", { labels: { zone: "x" } }),
    mapNode(4, "d", { labels: { zone: "y" } }),
    mapNode(5, "w", { labels: { zone: "x" }, spec: { witness: true } }),
  ];
  const vms = [
    ...Array.from({ length: 20 }, (_, i) => mapVm(i, `v${String(i).padStart(2, "0")}`, nodeId(1))),
    mapVm(50, "loose", null, "stopped"),
  ];
  return layoutCluster(toModel({ nodes, vms }));
}

it("moves between nodes along the slab grid and across slabs", () => {
  const s = scene();
  const go = (f: Focus, k: NavKey) => neighbor(s, f, k);
  expect(go(node(1), "ArrowRight")).toEqual(node(2));
  expect(go(node(2), "ArrowLeft")).toEqual(node(1));
  // 판 끝: 같은 판 줄의 옆 판(마주 보는 가장자리의 가장 가까운 노드)
  expect(go(node(2), "ArrowRight")).toEqual(node(4));
  expect(go(node(4), "ArrowLeft")).toEqual(node(2));
  expect(go(node(1), "ArrowLeft")).toBeNull();
  expect(go(node(4), "ArrowRight")).toBeNull();
  // 줄: 아래 줄의 x가 가까운 노드, 위로는 증인 줄
  expect(go(node(2), "ArrowDown")).toEqual(node(3));
  expect(go(node(3), "ArrowUp")).toEqual(node(1));
  expect(go(node(1), "ArrowUp")).toEqual(node(5));
  expect(go(node(5), "ArrowDown")).toEqual(node(1));
  expect(go(node(5), "ArrowUp")).toBeNull();
  // 판 줄 끝: 앞 판 줄(배치 안 됨 판)
  expect(go(node(3), "ArrowDown")).toEqual({ kind: "node", key: "unplaced" });
  expect(go({ kind: "node", key: "unplaced" }, "ArrowUp")).not.toBeNull();
  // Home·End: 읽는 차례의 처음·끝
  expect(go(node(3), "Home")).toEqual(node(5));
  expect(go(node(3), "End")).toEqual({ kind: "node", key: "unplaced" });
  expect(go(node(5), "Home")).toBeNull();
  expect(readingOrder(s).map((n) => n.name)).toEqual(["w", "a", "b", "c", "d", ""]);
  // 초점이 없거나 사라졌으면 첫 노드
  expect(neighbor(s, null, "ArrowDown")).toEqual(node(5));
  expect(neighbor(s, { kind: "node", key: "node:gone" }, "ArrowDown")).toEqual(node(5));
  // 노드 단계의 [·Esc는 아무것도 하지 않는다(전역 단축키에 맡기지 않음은 화면이 정한다)
  expect(go(node(1), "[")).toBeNull();
  expect(go(node(1), "Escape")).toBeNull();
});

it("enters a node's tiles with ] and leaves with [ or Esc", () => {
  const s = scene();
  const go = (f: Focus, k: NavKey) => neighbor(s, f, k);
  expect(go(node(1), "]")).toEqual(tile(1, 0));
  // VM이 없으면 들어가지 않는다(증인도)
  expect(go(node(2), "]")).toBeNull();
  expect(go(node(5), "]")).toBeNull();
  expect(go(tile(1, 3), "[")).toEqual(node(1));
  expect(go(tile(1, 3), "Escape")).toEqual(node(1));
  // 타일 단계의 ←·→는 이름 순 앞뒤, ↑·↓는 위·아래 층의 같은 자리
  expect(go(tile(1, 0), "ArrowRight")).toEqual(tile(1, 1));
  expect(go(tile(1, 0), "ArrowLeft")).toBeNull();
  expect(go(tile(1, 15), "ArrowRight")).toEqual(tile(1, 16));
  expect(go(tile(1, 2), "ArrowUp")).toEqual(tile(1, 18));
  expect(go(tile(1, 18), "ArrowDown")).toEqual(tile(1, 2));
  // 위 층이 덜 찼으면 그 층의 끝 칸, 맨 위 층에서는 멈춤
  expect(go(tile(1, 9), "ArrowUp")).toEqual(tile(1, 19));
  expect(go(tile(1, 18), "ArrowUp")).toBeNull();
  expect(go(tile(1, 0), "ArrowDown")).toBeNull();
  expect(go(tile(1, 5), "Home")).toEqual(tile(1, 0));
  expect(go(tile(1, 5), "End")).toEqual(tile(1, 19));
  expect(go(tile(1, 5), "]")).toBeNull();
  // 사라진 타일이면 그 노드로
  expect(go({ kind: "tile", node: node(1).key, key: "vm:gone" }, "ArrowRight")).toEqual(node(1));
  // 배치 안 됨 판의 타일도
  expect(go({ kind: "node", key: "unplaced" }, "]")).toEqual({
    kind: "tile",
    node: "unplaced",
    key: `vm:${vmId(50)}`,
  });
});

it("skips ghost tiles of VMs migrating in", () => {
  const a = mapNode(1, "a");
  const b = mapNode(2, "b");
  const s = layoutCluster(
    toModel({
      nodes: [a, b],
      vms: [
        mapVm(1, "web", a.id, "migrating", {
          status: { node: a.id, migration: { node: b.id, task: "t" } },
        }),
      ],
    }),
  );
  expect(s.byKey.get(`node:${b.id}`)!.grid!.cells).toHaveLength(1);
  expect(neighbor(s, node(2), "]")).toBeNull();
});

it("moves ↑·↓ by rows on a busy node's dense grid, down to the stopped-VM band", () => {
  const a = mapNode(1, "a");
  const vms = Array.from({ length: 131 }, (_, i) =>
    mapVm(i, `vm${String(i).padStart(3, "0")}`, a.id, i % 2 === 0 ? "stopped" : "running"),
  );
  const s = layoutCluster(toModel({ nodes: [a], vms }));
  const grid = s.nodes[0]!.grid!;
  expect(grid.size).toBeLessThan(20);
  const at = (key: string) => grid.cells.find((c) => c.key === key)!;
  const go = (key: string, k: NavKey) => {
    const f = neighbor(s, { kind: "tile", node: `node:${a.id}`, key }, k);
    return f?.kind === "tile" ? at(f.key) : null;
  };
  const vmCells = grid.cells.filter((c) => c.kind === "vm");
  const first = vmCells[2]!;
  // 아래 줄 같은 열, 위로는 맨 뒤 줄에서 멈춤
  expect(go(first.key, "ArrowDown")).toMatchObject({ row: 1, col: 2 });
  expect(go(go(first.key, "ArrowDown")!.key, "ArrowUp")).toBe(first);
  expect(go(first.key, "ArrowUp")).toBeNull();
  // 맨 앞 줄에서 ↓는 꺼진 VM 띠, 띠에서 ↑는 맨 앞 줄의 첫 열
  const last = vmCells.at(-1)!;
  const fold = go(last.key, "ArrowDown")!;
  expect(fold.kind).toBe("fold");
  expect(go(fold.key, "ArrowDown")).toBeNull();
  expect(go(fold.key, "ArrowUp")).toMatchObject({ row: last.row, col: 0 });
});

it("reaches every node with arrows and every tile from ] (property)", () => {
  const keys: NavKey[] = ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"];
  for (let seed = 1; seed <= 40; seed++) {
    const s = layoutCluster(toModel(randomCluster(seed)));
    const start = neighbor(s, null, "ArrowRight");
    if (!start) {
      expect(s.nodes).toEqual([]);
      continue;
    }
    const seen = new Set<string>([start.key]);
    const queue: Focus[] = [start];
    while (queue.length > 0) {
      const f = queue.shift()!;
      for (const k of keys) {
        const next = neighbor(s, f, k);
        if (next && !seen.has(next.key)) {
          expect(next.kind).toBe("node");
          seen.add(next.key);
          queue.push(next);
        }
      }
    }
    expect([...seen].sort()).toEqual(s.nodes.map((n) => n.key).sort());
    for (const n of s.nodes) {
      const cells = (n.grid?.cells ?? []).filter((c) => c.kind !== "ghost");
      let f = neighbor(s, { kind: "node", key: n.key }, "]");
      const walked: string[] = [];
      while (f) {
        walked.push(f.key);
        f = neighbor(s, f, "ArrowRight");
      }
      expect(walked).toEqual(cells.map((c) => c.key));
    }
  }
});

it("recognises the keys the map handles", () => {
  expect(isNavKey("]")).toBe(true);
  expect(isNavKey("ArrowUp")).toBe(true);
  expect(isNavKey("Enter")).toBe(false);
  expect(isNavKey("a")).toBe(false);
});
