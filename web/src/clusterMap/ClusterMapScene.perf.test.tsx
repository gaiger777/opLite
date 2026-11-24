// 큰 클러스터의 예산(계획 70 §9.2): 100 노드 × 50 VM(5000 VM, 판 10개)의 배치 + 경로 시간과 단계마다 SVG 원소 수.
// 원소 수는 그리기 없이 장면으로 센다(`elementCount`) — 화면(스테이지 2)이 같은 셈을 따르고 그린 DOM으로도 잰다.
import { layoutCluster } from "@/clusterMap/layout";
import { toModel } from "@/clusterMap/model";
import { elementCount, scenePaths } from "@/clusterMap/paths";
import { bigCluster } from "@/test/clusterMap";

/** 이 Mac 기록의 예산(ms). CI 기계는 느리고 다른 일과 나누므로 넉넉히(벽시계 시험의 흔들림). */
const BUDGET_MS = import.meta.env.CI ? 150 : 30;

function build(input: ReturnType<typeof bigCluster>) {
  const scene = layoutCluster(toModel(input));
  return { scene, tiles: scenePaths(scene, "tiles") };
}

it("lays out 100 nodes × 50 VMs with merged paths within the time budget", () => {
  const input = bigCluster(100, 50, 10);
  build(input); // 데우기(JIT)
  let best = Infinity;
  for (let i = 0; i < 5; i++) {
    const t0 = performance.now();
    build(input);
    best = Math.min(best, performance.now() - t0);
  }
  expect(best).toBeLessThan(BUDGET_MS);
});

it("keeps the 100 × 50 picture within the element budget at every level of detail", () => {
  const { scene, tiles } = build(bigCluster(100, 50, 10));
  expect(scene.slabs).toHaveLength(10);
  expect(scene.tiles.size).toBe(100 * 50);
  const summary = elementCount(scene, scenePaths(scene, "summary"), "summary");
  const tiled = elementCount(scene, tiles, "tiles");
  const all = elementCount(scene, tiles, "tiles", [
    "load",
    "storage",
    "network",
    "ha",
    "recommendations",
  ]);
  expect(summary).toBeLessThan(1500);
  expect(tiled).toBeLessThan(3000);
  expect(all).toBeLessThan(4000);
  // 견줌: 타일마다 <g> + 다각형 셋이면 타일만 2만
  expect(tiled).toBeLessThan((100 * 50 * 4) / 6);
});

it("keeps a 20 × 20 picture within the detail budget", () => {
  const { scene, tiles } = build(bigCluster(20, 20, 4));
  expect(elementCount(scene, tiles, "detail")).toBeLessThan(2500);
});
