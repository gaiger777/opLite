import { boxesPath } from "@/components/iso/iso";
import { layoutCluster, PER_LAYER } from "@/clusterMap/layout";
import { toModel } from "@/clusterMap/model";
import {
  elementCount,
  FRAME_ELEMENTS,
  nodeElements,
  nodeMarks,
  scenePaths,
} from "@/clusterMap/paths";
import type { StoragePool } from "@/api/types";
import { mapNode, mapVm, randomCluster } from "@/test/clusterMap";

const subpaths = (d: string) => (d.match(/M/g) ?? []).length;

it("draws one path per band, layer, colour and face", () => {
  const n = mapNode(1, "a");
  // 20대: 0층 16칸(켜짐 12 + 실패 4), 1층 4칸(켜짐), HA 하나
  const vms = Array.from({ length: 20 }, (_, i) =>
    mapVm(i, `v${String(i).padStart(2, "0")}`, n.id, i >= 4 && i < 8 ? "failed" : "running", {
      spec: { ha: i === 18 },
    }),
  );
  const scene = layoutCluster(toModel({ nodes: [n], vms }));
  const [band] = scenePaths(scene, "tiles");
  expect(band!.nodes.map((x) => x.key)).toEqual([`node:${n.id}`]);
  const pieces = band!.pieces.map((p) => [p.layer, p.kind, p.tone, p.face, p.parts]);
  expect(pieces).toEqual([
    // 0층: 옆면 둘(색마다) → 오른쪽 면 그늘(색과 상관없이) → 윗면(위 층이 덮지 않은 칸만 — 4..15 중 위가 빈 12칸)
    [0, "tile", "running", "left", 24],
    [0, "tile", "failed", "left", 8],
    [0, "shade", null, "right", 16],
    [0, "tile", "failed", "top", 4],
    [0, "tile", "running", "top", 8],
    [0, "cross", null, null, 4],
    [1, "tile", "running", "left", 8],
    [1, "shade", null, "right", 4],
    [1, "tile", "running", "top", 4],
    [1, "ha", null, "top", 1],
  ]);
  for (const p of band!.pieces) {
    expect(subpaths(p.d)).toBe(p.kind === "cross" ? 2 * p.parts : p.parts);
  }
  const cells = scene.nodes[0]!.grid!.cells;
  expect(band!.pieces.find((p) => p.layer === 1 && p.face === "top")!.d).toBe(
    boxesPath(
      cells.slice(PER_LAYER).map((c) => c.box),
      "top",
    ),
  );
  expect(new Set(band!.pieces.map((p) => p.key)).size).toBe(band!.pieces.length);
});

it("merges tiles of every node in a band and keeps bands in draw order (property)", () => {
  for (let seed = 1; seed <= 30; seed++) {
    const scene = layoutCluster(toModel(randomCluster(seed)));
    const paths = scenePaths(scene, "tiles");
    expect(paths.flatMap((b) => b.nodes)).toEqual(scene.nodes);
    expect(paths.map((b) => b.band)).toEqual([...new Set(scene.nodes.map((n) => n.band))]);
    let sides = 0;
    let shaded = 0;
    let tops = 0;
    let cells = 0;
    let visible = 0;
    for (const b of paths) {
      for (const n of b.nodes) {
        const grid = n.grid;
        cells += grid?.cells.length ?? 0;
        visible +=
          grid?.cells.filter((c) => c.index + grid.perLayer >= grid.cells.length).length ?? 0;
      }
      // 층 차례, 층 안에서는 옆면이 윗면보다 먼저
      const rank = (p: (typeof b.pieces)[number]) =>
        p.kind === "tile" ? (p.face === "top" ? 3 : 1) : p.kind === "shade" ? 2 : 4;
      const order = b.pieces.map((p) => p.layer * 10 + rank(p));
      expect(order).toEqual([...order].sort((x, y) => x - y));
      for (const p of b.pieces) {
        if (p.kind === "tile" && p.face !== "top") {
          sides += p.parts;
        } else if (p.kind === "tile") {
          tops += p.parts;
        } else if (p.kind === "shade") {
          shaded += p.parts;
        }
      }
    }
    // 경로 조각 수 = 칸 수(옆면 둘, 그늘 하나), 윗면은 보이는 칸만
    expect(sides).toBe(cells * 2);
    expect(shaded).toBe(cells);
    expect(tops).toBe(visible);
  }
});

it("keeps tiles of offline nodes in their own hatched paths", () => {
  const on = mapNode(1, "a");
  const off = mapNode(2, "b", { status: { online: false } });
  const scene = layoutCluster(
    toModel({ nodes: [on, off], vms: [mapVm(1, "x", on.id), mapVm(2, "y", off.id)] }),
  );
  const pieces = scenePaths(scene, "tiles").flatMap((b) => b.pieces);
  expect(pieces.filter((p) => p.face === "top").map((p) => [p.tone, p.stale])).toEqual([
    ["running", false],
    ["running", true],
  ]);
});

it("draws summary bars per band, state and face", () => {
  const nodes = [mapNode(1, "a"), mapNode(2, "b"), mapNode(3, "c"), mapNode(4, "d")];
  // 2 × 2 격자: b(줄 0, 열 1)와 c(줄 1, 열 0)가 같은 띠(띠 안은 열 차례)
  const vms = [
    mapVm(1, "x", nodes[1]!.id),
    mapVm(2, "y", nodes[2]!.id),
    mapVm(3, "z", nodes[2]!.id, "failed"),
  ];
  const scene = layoutCluster(toModel({ nodes, vms }));
  const paths = scenePaths(scene, "summary");
  expect(paths.map((b) => b.nodes.map((n) => n.name))).toEqual([["a"], ["c", "b"], ["d"]]);
  expect(paths[1]!.pieces.map((p) => [p.kind, p.tone, p.face, p.parts])).toEqual([
    ["bar", "running", "left", 2],
    ["bar", "running", "right", 2],
    ["bar", "running", "top", 2],
    ["bar", "failed", "left", 1],
    ["bar", "failed", "right", 1],
    ["bar", "failed", "top", 1],
  ]);
  expect(paths[0]!.pieces).toEqual([]);
});

it("counts the SVG elements a picture would make", () => {
  const n = mapNode(1, "a", { spec: { maintenance: true } });
  const scene = layoutCluster(toModel({ nodes: [n], vms: [mapVm(1, "x", n.id)] }));
  const tiles = scenePaths(scene, "tiles");
  const F = FRAME_ELEMENTS;
  // 틀 + 판 4 + 노드(g + 글 층 g + 이름 + 윗면 + 채움 면 둘 × 다각형 둘 + 유지보수 꼬리표 × 2 + 줄무늬 띠) 11 + 경로 셋(옆면·그늘·윗면)
  expect(elementCount(scene, tiles, "tiles")).toBe(F + 4 + 11 + 3);
  // 요약: g + 글 층 g + 이름 + 블록 면 셋 + 꼬리표 + 띠, 막대 면 셋
  expect(elementCount(scene, scenePaths(scene, "summary"), "summary")).toBe(F + 4 + 9 + 3);
  // 자세히: 부하 글 + 타일 이름 하나
  expect(elementCount(scene, tiles, "detail")).toBe(F + 4 + 12 + 3 + 1);
  // 레이어 자료가 없는 노드(풀·인터페이스·버전 없음)에는 레이어가 더하는 것이 없다
  expect(elementCount(scene, tiles, "tiles", ["load", "storage", "network", "ha"])).toBe(
    F + 4 + 11 + 3,
  );
});

it("counts what each layer adds to a node", () => {
  const n = mapNode(1, "a", {
    spec: { sdn: { gateway: 1 } },
    status: {
      version: "1.0",
      pools: {
        local: { available_bytes: 0, total_bytes: 10, ready: false, error: "x", pool_id: "p" },
        nfs: { available_bytes: 0, total_bytes: 0, ready: false, error: "y", pool_id: "q" },
      },
      interfaces: [{ name: "eth0", kind: "ethernet", state: "up", carrier: true, master: "br0" }],
      fence_check: { at: 1, by: "b", error: "ipmi: timeout" },
    },
  });
  const pools = [
    {
      id: "p",
      name: "local",
      revision: 1,
      labels: {},
      spec: { backend: { kind: "dir", path: "/x" } },
    },
    {
      id: "q",
      name: "nfs",
      revision: 1,
      labels: {},
      spec: { backend: { kind: "dir", path: "/y" }, shared: true },
    },
  ] as unknown as StoragePool[];
  const scene = layoutCluster(toModel({ nodes: [n], vms: [], pools }));
  const item = scene.nodes[0]!;
  const marks = nodeMarks(item, new Set(["storage", "network", "ha"]));
  expect(marks).toMatchObject({
    tags: ["gateway", "fence"],
    pedestal: true,
    poolBad: true,
    sharedTick: true,
    uplink: true,
    ring: true,
  });
  expect(nodeMarks(item, new Set(["load"]))).toMatchObject({ tags: [], pedestal: false });
  // g + 글 층 g + 이름 + 윗면·채움 5 + 꼬리표 둘 × 2 + 받침·빗금·눈금·띠·둘레
  expect(nodeElements(item, "tiles", marks)).toBe(3 + 5 + 4 + 5);
  // 공유 풀 띠는 판마다 둘
  expect(elementCount(scene, [], "tiles", ["storage"]) - elementCount(scene, [], "tiles")).toBe(
    2 + 3,
  );
});
