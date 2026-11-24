// 그린 지도(진짜 DOM)의 원소 수와 다시 그리기(계획 70 §9.2): 그린 SVG 원소 수가 그리기 없는 셈(`elementCount`)과 꼭 같고
// 예산 안이며, 가리키기·초점 이동·끌기가 노드 블록을 다시 그리지 않는다(블록은 `memo`, 밝히기는 `<style>` 한 줄).
import { act, fireEvent, render, screen, within } from "@testing-library/react";

import { layoutCluster } from "@/clusterMap/layout";
import { mapLabels } from "@/clusterMap/labels";
import type { Lod } from "@/clusterMap/lod";
import { MapScene } from "@/clusterMap/MapScene";
import { type ClusterModel, toModel } from "@/clusterMap/model";
import { elementCount, type MapLayer, scenePaths } from "@/clusterMap/paths";
import i18n from "@/i18n";
import { bigCluster, layerStatus, mapNode, mapVm } from "@/test/clusterMap";

// 노드 블록(`NodeShape`)을 몇 번 그렸는지: 그릴 때마다 덧표시를 한 번 셈한다.
const drawn = vi.hoisted(() => ({ blocks: 0 }));
vi.mock("@/clusterMap/paths", async (importOriginal) => {
  const real = await importOriginal<typeof import("@/clusterMap/paths")>();
  return {
    ...real,
    nodeMarks: (...args: Parameters<typeof real.nodeMarks>) => {
      drawn.blocks += 1;
      return real.nodeMarks(...args);
    },
  };
});

const ALL: MapLayer[] = ["load", "storage", "network", "ha", "recommendations"];

function draw(
  model: ClusterModel,
  layers: MapLayer[] = ALL,
  onSummaryEnter: (key: string) => void = () => {},
) {
  const scene = layoutCluster(model);
  const labels = mapLabels(i18n.t, model, scene);
  const lod = { current: null as Lod | null };
  const view = render(
    <MapScene
      scene={scene}
      nodes={new Map(model.nodes.map((n) => [`node:${n.id}`, n]))}
      labels={labels}
      vmNames={new Map(model.vms.map((v) => [v.id, v.name]))}
      layers={layers}
      slabName={(s) => s.name ?? "—"}
      onOpen={() => {}}
      onDetail={() => {}}
      onSummaryEnter={onSummaryEnter}
      onLod={(l) => {
        lod.current = l;
      }}
      renderCard={(key) => labels.get(key)}
    />,
  );
  const svg = screen.getByRole("group", { name: "Cluster map" });
  const count = () => svg.querySelectorAll("*").length + 1;
  const expected = () => elementCount(scene, scenePaths(scene, lod.current!), lod.current!, layers);
  return { ...view, scene, svg, lod, count, expected };
}

it("draws exactly the counted elements for 100 nodes × 50 VMs within the summary budget", () => {
  const { svg, lod, count, expected, unmount } = draw(toModel(bigCluster(100, 50, 10)), ["load"]);
  // 보이는 VM 5000 > 3000이면 늘 요약
  expect(lod.current).toBe("summary");
  expect(count()).toBe(expected());
  expect(count()).toBeLessThan(1500);
  expect(svg.querySelectorAll("g[role=button]")).toHaveLength(100);
  unmount();
  // 레이어를 모두 켜도(예산 4000) 셈과 같다
  const all = draw(toModel(bigCluster(100, 50, 10)));
  expect(all.count()).toBe(all.expected());
  expect(all.count()).toBeLessThan(4000);
});

it("draws exactly the counted elements at the tile level after zooming in", () => {
  const { lod, count, expected } = draw(toModel(bigCluster(40, 50, 4)));
  for (let i = 0; i < 12 && lod.current !== "tiles"; i++) {
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
  }
  expect(lod.current).toBe("tiles");
  expect(count()).toBe(expected());
  expect(count()).toBeLessThan(3000);
});

it("draws exactly the counted elements in the detailed view with every mark", () => {
  const nodes = [
    mapNode(1, "a", {
      labels: { zone: "z" },
      spec: { maintenance: true, sdn: { gateway: 1 } },
      status: { ...layerStatus(95), fence_check: { at: 1, by: "b", error: "ipmi: timeout" } },
    }),
    mapNode(2, "b", { labels: { zone: "z" }, status: { online: false, version: "1" } }),
    mapNode(3, "w", { labels: { zone: "z" }, spec: { witness: true } }),
  ];
  const vms = [
    mapVm(1, "web", nodes[0]!.id, "running", { status: { rule_violations: ["apart"] } }),
    mapVm(2, "db", nodes[0]!.id, "failed", { spec: { ha: true } }),
    mapVm(3, "mig", nodes[0]!.id, "migrating", {
      spec: { power: "running" },
      status: { migration: { node: nodes[1]!.id, task: "t" } },
    }),
    mapVm(4, "old", nodes[1]!.id, "running"),
    mapVm(5, "new", null, "stopped"),
  ];
  const { lod, count, expected } = draw(toModel({ nodes, vms }));
  expect(lod.current).toBe("detail");
  expect(count()).toBe(expected());
});

it("does not redraw node blocks on hover, keyboard moves or drags", () => {
  drawn.blocks = 0;
  const { svg, scene } = draw(toModel(bigCluster(40, 20, 4)));
  const first = within(svg).getAllByRole("button")[0]!;
  expect(drawn.blocks).toBeGreaterThanOrEqual(40);
  drawn.blocks = 0;
  for (const g of within(svg).getAllByRole("button").slice(0, 10)) {
    fireEvent.pointerMove(g, { pointerType: "mouse" });
  }
  // 밝히기는 `<style>` 한 줄
  expect(svg.querySelector("style")!.textContent).toContain("opacity:.35");
  fireEvent.pointerLeave(svg);
  act(() => first.focus());
  for (let i = 0; i < 10; i++) {
    fireEvent.keyDown(document.activeElement!, { key: i % 2 === 0 ? "ArrowRight" : "ArrowDown" });
  }
  expect(document.activeElement).not.toBe(first);
  fireEvent.pointerDown(svg, { pointerId: 1, clientX: 10, clientY: 10, button: 0 });
  fireEvent.pointerMove(svg, { pointerId: 1, clientX: 60, clientY: 40 });
  fireEvent.pointerUp(svg, { pointerId: 1, clientX: 60, clientY: 40 });
  expect(drawn.blocks).toBe(0);
  expect(scene.nodes.length).toBe(40);
});

it("opens the node's VM list with ] in the summary view instead of entering tiles", () => {
  const entered: string[] = [];
  const { svg, lod } = draw(toModel(bigCluster(100, 50, 10)), ["load"], (key) => entered.push(key));
  expect(lod.current).toBe("summary");
  const first = within(svg).getAllByRole("button")[0]!;
  act(() => first.focus());
  expect(fireEvent.keyDown(first, { key: "]" })).toBe(false);
  expect(entered).toEqual([first.getAttribute("data-key")]);
});
