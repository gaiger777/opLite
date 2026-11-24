import { boxFace, iso, topCenter } from "@/components/iso/iso";
import { hitsBox, hitTest } from "@/clusterMap/hit";
import { layoutCluster, PER_LAYER } from "@/clusterMap/layout";
import { toModel } from "@/clusterMap/model";
import { mapNode, mapVm, randomCluster } from "@/test/clusterMap";

it("hits a box anywhere on its three visible faces and nowhere else", () => {
  const b = { x: 0, y: 0, z: 0, w: 10, d: 10, h: 4 };
  expect(hitsBox(topCenter(b), b)).toBe(true);
  // 옆면의 가운데
  expect(hitsBox(iso(5, 10, 2), b)).toBe(true);
  expect(hitsBox(iso(10, 5, 2), b)).toBe(true);
  // 윤곽 밖
  expect(hitsBox(iso(5, 10, 6), { ...b, h: 0 })).toBe(false);
  expect(hitsBox(iso(20, 20, 0), b)).toBe(false);
  expect(hitsBox(iso(-1, -1, 4.5), b)).toBe(false);
});

it("finds every visible tile from the middle of its top (property)", () => {
  for (let seed = 1; seed <= 30; seed++) {
    const scene = layoutCluster(toModel(randomCluster(seed)));
    for (const n of scene.nodes) {
      const grid = n.grid;
      if (!grid) {
        continue;
      }
      for (const cell of grid.cells) {
        if (cell.index + grid.perLayer < grid.cells.length) {
          continue; // 위 층이 덮음
        }
        expect(hitTest(scene, "tiles", topCenter(cell.box))).toEqual({
          kind: "tile",
          key: cell.key,
          node: n.key,
        });
      }
    }
  }
});

it("lets the upper layer win and finds lower tiles by their sides", () => {
  const n = mapNode(1, "a");
  const scene = layoutCluster(
    toModel({
      nodes: [n],
      vms: Array.from({ length: 17 }, (_, i) => mapVm(i, `v${String(i).padStart(2, "0")}`, n.id)),
    }),
  );
  const cells = scene.nodes[0]!.grid!.cells;
  // 칸 0의 윗면 가운데는 그 위(칸 16)의 바닥이다 — 위 층이 보인다
  expect(hitTest(scene, "tiles", topCenter(cells[0]!.box))).toMatchObject({
    key: cells[PER_LAYER]!.key,
  });
  // 아래 층 타일은 옆면으로 맞힌다(높이 = 간격이라 앞 타일이 옆면을 가리지 않는다)
  const side = boxFace(cells[0]!.box, "left");
  const nearBottom = { x: (side[2]!.x + side[3]!.x) / 2, y: (side[2]!.y + side[3]!.y) / 2 - 0.5 };
  expect(hitTest(scene, "tiles", nearBottom)).toMatchObject({ key: cells[0]!.key });
});

it("hits the block, not the tiles, in the summary and nothing on the floor", () => {
  const n = mapNode(1, "a");
  const scene = layoutCluster(toModel({ nodes: [n], vms: [mapVm(1, "x", n.id)] }));
  const item = scene.nodes[0]!;
  const cell = item.grid!.cells[0]!;
  expect(hitTest(scene, "tiles", topCenter(cell.box))).toMatchObject({ kind: "tile" });
  expect(hitTest(scene, "summary", topCenter(cell.box))).toEqual({ kind: "node", key: item.key });
  // 상태 막대 꼭대기(블록 윗면 위)도 노드
  const bar = item.bars[0]!;
  expect(hitTest(scene, "summary", topCenter(bar.box))).toEqual({ kind: "node", key: item.key });
  // 블록 옆면
  expect(hitTest(scene, "tiles", iso(item.box.x + 60, item.box.y + 120, item.box.z + 20))).toEqual({
    kind: "node",
    key: item.key,
  });
  // 판 바닥(블록이 가리지 않는 오른쪽 모서리)·빈 곳
  const slab = scene.slabs[0]!.box;
  expect(hitTest(scene, "tiles", iso(slab.x + slab.w - 5, slab.y + 5, slab.h))).toBeNull();
  expect(hitTest(scene, "tiles", { x: 1e6, y: 1e6 })).toBeNull();
});
