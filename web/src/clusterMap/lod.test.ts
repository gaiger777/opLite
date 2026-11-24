import { layoutCluster, TILE } from "@/clusterMap/layout";
import { type Lod, lodFor, tilePx, tilesInView } from "@/clusterMap/lod";
import { toModel } from "@/clusterMap/model";
import { mapNode, mapVm } from "@/test/clusterMap";
import { boxBounds } from "@/components/iso/iso";

const at = (tilePx: number, inView = 100, vms = 100) => ({ tilePx, inView, vms });

it("picks summary, tiles or detail by on-screen tile size", () => {
  expect(lodFor(at(5.9))).toBe("summary");
  expect(lodFor(at(6))).toBe("tiles");
  expect(lodFor(at(17.9))).toBe("tiles");
  expect(lodFor(at(18))).toBe("detail");
  // 자세히는 화면 안 타일이 300 밑일 때만
  expect(lodFor(at(30, 300))).toBe("tiles");
  expect(lodFor(at(30, 299))).toBe("detail");
  // VM이 3000을 넘으면 늘 요약
  expect(lodFor(at(40, 10, 3001))).toBe("summary");
  expect(lodFor(at(40, 10, 3000))).toBe("detail");
});

it("does not flicker around the thresholds (15% hysteresis)", () => {
  // 요약에서 타일로는 6 × 1.15 넘어야, 타일에서 요약으로는 6 × 0.85 밑이어야
  expect(lodFor(at(6.5), "summary")).toBe("summary");
  expect(lodFor(at(6.9), "summary")).toBe("tiles");
  expect(lodFor(at(5.2), "tiles")).toBe("tiles");
  expect(lodFor(at(5.0), "tiles")).toBe("summary");
  expect(lodFor(at(19), "tiles")).toBe("tiles");
  expect(lodFor(at(20.8), "tiles")).toBe("detail");
  expect(lodFor(at(15.4), "detail")).toBe("detail");
  expect(lodFor(at(15.2), "detail")).toBe("tiles");
  expect(lodFor(at(30, 340), "detail")).toBe("detail");
  expect(lodFor(at(30, 350), "detail")).toBe("tiles");
  expect(lodFor(at(30, 260), "tiles")).toBe("tiles");
  expect(lodFor(at(30, 250), "tiles")).toBe("detail");
  // 성질: 확대를 천천히 올렸다 내려도 단계가 바뀌는 횟수는 문턱마다 한 번
  let lod: Lod | undefined;
  let changes = 0;
  const steps = [
    ...Array.from({ length: 400 }, (_, i) => 2 + i * 0.07),
    ...Array.from({ length: 400 }, (_, i) => 30 - i * 0.07),
  ];
  for (const px of steps) {
    // 문턱 둘레의 떨림
    for (const jitter of [0, 0.3, -0.3, 0]) {
      const next = lodFor(at(px + jitter), lod);
      if (lod !== undefined && next !== lod) {
        changes += 1;
      }
      lod = next;
    }
  }
  expect(changes).toBe(4);
});

it("measures tile size on screen and counts tiles in view", () => {
  expect(tilePx({ x: 0, y: 0, w: 1000, h: 500 }, { width: 500, height: 500 })).toBe(0.5 * TILE);
  expect(tilePx({ x: 0, y: 0, w: 100, h: 100 }, { width: 400, height: 200 })).toBe(2 * TILE);
  expect(tilePx({ x: 0, y: 0, w: 100, h: 100 }, { width: 0, height: 0 })).toBe(0);
  const a = mapNode(1, "a");
  const b = mapNode(2, "b");
  const s = layoutCluster(
    toModel({
      nodes: [a, b],
      vms: [mapVm(1, "x", a.id), mapVm(2, "y", a.id), mapVm(3, "z", b.id)],
    }),
  );
  const all = s.bounds;
  expect(
    tilesInView(s, { x: all.minX, y: all.minY, w: all.maxX - all.minX, h: all.maxY - all.minY }),
  ).toBe(3);
  const nb = boxBounds(s.byKey.get(`node:${a.id}`)!.box);
  expect(tilesInView(s, { x: nb.minX, y: nb.minY, w: 1, h: 1 })).toBe(2);
  expect(tilesInView(s, { x: all.maxX + 100, y: 0, w: 10, h: 10 })).toBe(0);
});
