// 그린 지도(진짜 DOM)의 원소 수와 다시 그리기(계획 70 §9.2): 그린 SVG 원소 수가 그리기 없는 셈(`elementCount`)과 꼭 같고
// 예산 안이며, 가리키기·초점 이동·끌기가 노드 블록을 다시 그리지 않는다(블록은 `memo`, 밝히기는 `<style>` 한 줄).
import { act, fireEvent, render, screen, within } from "@testing-library/react";

import { layoutCluster, nodeTop } from "@/clusterMap/layout";
import { mapLabels } from "@/clusterMap/labels";
import type { Lod } from "@/clusterMap/lod";
import { MapScene, pillWidth } from "@/clusterMap/MapScene";
import { type ClusterModel, toModel } from "@/clusterMap/model";
import { elementCount, type MapLayer, scenePaths } from "@/clusterMap/paths";
import { topCenter } from "@/components/iso";
import { DndProvider } from "@/dnd/DndProvider";
import i18n from "@/i18n";
import {
  bigCluster,
  layerStatus,
  mapNode,
  mapVm,
  physicalTopology,
  stubSvgMatrix,
} from "@/test/clusterMap";

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
  onOpen: (key: string) => void = () => {},
) {
  const scene = layoutCluster(model, { rails: layers.includes("network") });
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
      onOpen={(o) => onOpen(o.key)}
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

it("sizes tag pills for wide Hangul glyphs as well as Latin ones", () => {
  expect(pillWidth("offline")).toBe(7 * 6 + 12);
  // 한글은 글자마다 약 10px(10px 굵은 글) — 알약 밖으로 넘치지 않게
  expect(pillWidth("오프라인")).toBe(4 * 10 + 12);
  expect(pillWidth("펜스 확인 실패")).toBe(6 * 10 + 2 * 6 + 12);
  expect(pillWidth("비우는 중 2/5")).toBeGreaterThanOrEqual(4 * 10 + 5 * 6 + 12);
});

it("does not redraw node blocks while a VM tile is dragged across nodes", () => {
  const restore = stubSvgMatrix();
  try {
    const model = toModel(bigCluster(12, 4, 3));
    const scene = layoutCluster(model);
    const labels = mapLabels(i18n.t, model, scene);
    const nodes = scene.nodes.filter((n) => n.kind === "node");
    render(
      <DndProvider
        accepts={(_, slot) =>
          nodes.findIndex((n) => n.key === slot) % 2 === 0
            ? { ok: true }
            : { ok: false, reason: { key: "clusterMap.drag.here" } }
        }
        onDrop={() => {}}
      >
        <MapScene
          scene={scene}
          nodes={new Map(model.nodes.map((n) => [`node:${n.id}`, n]))}
          labels={labels}
          vmNames={new Map(model.vms.map((v) => [v.id, v.name]))}
          layers={["load"]}
          slabName={(s) => s.name ?? "—"}
          onOpen={() => {}}
          onDetail={() => {}}
          onSummaryEnter={() => {}}
          renderCard={(key) => labels.get(key)}
          drag={{ item: (key) => ({ id: key, label: key }), free: () => null, onHold: () => {} }}
        />
      </DndProvider>,
    );
    const svg = screen.getByRole("group", { name: "Cluster map" });
    const [, first] = [...scene.tiles].find(([, t]) => t.cell.kind === "vm")!;
    const start = topCenter(first.cell.box);
    const pointer = (p: { x: number; y: number }) => ({
      pointerId: 1,
      pointerType: "mouse",
      button: 0,
      isPrimary: true,
      clientX: p.x,
      clientY: p.y,
    });
    drawn.blocks = 0;
    fireEvent.pointerDown(svg, pointer(start));
    for (const n of nodes.slice(0, 10)) {
      fireEvent.pointerMove(svg, pointer(nodeTop(n)));
    }
    const css = [...svg.querySelectorAll("style")].map((el) => el.textContent).join("");
    expect(css).toContain("stroke:var(--brand)");
    expect(css).toContain("opacity:.45");
    fireEvent.pointerUp(svg, pointer(nodeTop(nodes[0]!)));
    expect(drawn.blocks).toBe(0);
  } finally {
    restore();
  }
});

/** 네트워크 층(계획 75)이 있는 작은 클러스터: n1은 bond0의 eno1과 underlay eno3이 끊김 */
function netModel() {
  const nodes = [
    mapNode(1, "n1", { labels: { zone: "z" }, status: layerStatus(40) }),
    mapNode(2, "n2", { labels: { zone: "z" }, status: layerStatus(40) }),
    mapNode(3, "w", { labels: { zone: "z" }, spec: { witness: true } }),
  ];
  const vms = [mapVm(1, "web", nodes[0]!.id), mapVm(2, "db", nodes[1]!.id)];
  return {
    nodes,
    model: toModel({ nodes, vms, physical: physicalTopology(nodes, { n1: ["eno1", "eno3"] }, 7) }),
  };
}

it("draws exactly the counted elements with the network layer's ports, rails and faults", async () => {
  const { model } = netModel();
  const { svg, lod, count, expected, scene, unmount } = draw(model);
  expect(lod.current).toBe("detail");
  // 네트워크 층은 늦게 받는 조각 — 오면 셈과 꼭 같다
  await within(svg).findByText("vmbr0");
  expect(count()).toBe(expected());
  // 포트 줄(8개 + "+2"), 레일 이름, 장애 꼬리표, 업링크 띠 대신 포트
  expect(scene.byKey.get(`node:${model.nodes[0]!.id}`)!.net!.more).toBe(2);
  expect(within(svg).getAllByText("+2")).toHaveLength(2);
  expect(within(svg).getAllByText("vmbr0")).toHaveLength(1);
  expect(within(svg).getAllByText("underlay")).toHaveLength(1);
  expect(within(svg).getByText("link ✕ 2")).toBeInTheDocument();
  unmount();
  // 네트워크 레이어를 끄면 포트·레일이 없다
  const off = draw(model, ["load"]);
  expect(off.count()).toBe(off.expected());
  expect(within(off.svg).queryByText("vmbr0")).toBeNull();
  expect(within(off.svg).queryByText("link ✕ 2")).toBeNull();
});

it("counts the network layer exactly in the summary and tile views of a big cluster", async () => {
  const input = bigCluster(100, 50, 10);
  const model = toModel({ ...input, physical: physicalTopology(input.nodes, { n3: ["eno1"] }) });
  const big = draw(model);
  expect(big.lod.current).toBe("summary");
  await within(big.svg).findAllByText("vmbr0");
  expect(big.count()).toBe(big.expected());
  expect(big.count()).toBeLessThan(4500);
  big.unmount();
  const mid = toModel({
    ...bigCluster(40, 50, 4),
    physical: physicalTopology(bigCluster(40, 50, 4).nodes, { n3: ["eno1", "eno2"] }),
  });
  const { lod, count, expected, svg } = draw(mid);
  await within(svg).findAllByText("vmbr0");
  for (let i = 0; i < 12 && lod.current !== "tiles"; i++) {
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
  }
  expect(lod.current).toBe("tiles");
  expect(count()).toBe(expected());
});

it("walks into a node's ports with p, to the port's rail with ↓ and back out", () => {
  const { model } = netModel();
  const { svg, scene, unmount } = draw(model);
  const n1 = within(svg).getByRole("button", { name: /^Node n1,/ });
  expect(n1.getAttribute("aria-label")).toContain("2 link faults");
  act(() => n1.focus());
  const live = () => svg.parentElement!.querySelector("[aria-live]")!.textContent;
  drawn.blocks = 0;
  expect(fireEvent.keyDown(n1, { key: "p" })).toBe(false);
  expect(live()).toBe(
    "Port eno1 on n1, down, bond bond0, 10 Gb/s, switch sw1 port Gi1/0/1, " +
      "no redundancy for networks 2, VMs 3",
  );
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  expect(live()).toMatch(/^Port eno2 on n1, up, bond bond0/);
  fireEvent.keyDown(n1, { key: "ArrowDown" });
  const vmbr0 = [...scene.rails.values()].find((r) => r.carrier === "vmbr0")!;
  expect(live()).toBe("Rail vmbr0, 2 networks, 2 nodes, 1 degraded");
  fireEvent.keyDown(n1, { key: "ArrowLeft" });
  expect(live()).toBe("Rail Underlay (VXLAN), 1 network, 2 nodes, 1 down");
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  expect(live()).toContain("Rail vmbr0");
  fireEvent.keyDown(n1, { key: "ArrowUp" });
  expect(live()).toMatch(/^Port eno1 on n1/);
  fireEvent.keyDown(n1, { key: "Escape" });
  expect(live()).toMatch(/^Node n1,/);
  // 화면 초점은 늘 노드 하나(한 탭 정지), 블록은 다시 그리지 않음
  expect(document.activeElement).toBe(n1);
  expect(drawn.blocks).toBe(0);
  expect(vmbr0.nodes).toBe(2);
  // 포트 줄을 그리지 않은(레이어 끔) 그림에서는 `p`를 받지 않는다
  unmount();
  const off = draw(model, ["load"]);
  const m1 = within(off.svg).getByRole("button", { name: /^Node n1,/ });
  act(() => m1.focus());
  expect(fireEvent.keyDown(m1, { key: "p" })).toBe(true);
});

it("hits ports and rails under the pointer and lights the rail's nodes", () => {
  const restore = stubSvgMatrix();
  try {
    const { model } = netModel();
    const opened: string[] = [];
    const { svg, scene } = draw(model, ALL, undefined, (key) => opened.push(key));
    const port = scene.ports.get(`port:${model.nodes[0]!.id}/eno3`)!;
    const at = (p: { x: number; y: number }) => ({
      pointerType: "mouse",
      clientX: p.x,
      clientY: p.y,
    });
    drawn.blocks = 0;
    fireEvent.pointerMove(svg, at(topCenter(port.box)));
    expect(svg.parentElement!.querySelector("[data-map-card]")!.textContent).toContain(
      "Port eno3 on n1",
    );
    fireEvent.click(svg, at(topCenter(port.box)));
    const rail = [...scene.rails.values()].find((r) => r.carrier === "underlay")!;
    const mid = topCenter({ ...rail.box, x: rail.box.x + rail.box.w / 2, w: 1 });
    fireEvent.pointerMove(svg, at(mid));
    const css = svg.querySelector("style")!.textContent!;
    // 그 레일을 쓰는 노드 둘은 밝고 나머지(증인)는 흐림
    for (const n of model.nodes.filter((x) => !x.witness)) {
      expect(css).toContain(`g[data-key="node:${n.id}"]`);
    }
    fireEvent.click(svg, at(mid));
    expect(opened).toEqual([port.key, rail.key]);
    expect(drawn.blocks).toBe(0);
  } finally {
    restore();
  }
});

it("draws every name and tag above all blocks, tiles and ports so no picture covers a label", () => {
  const nodes = [
    mapNode(1, "front", { labels: { zone: "z" }, spec: { maintenance: true } }),
    mapNode(2, "back", { labels: { zone: "z" } }),
  ];
  const vms = Array.from({ length: 12 }, (_, i) =>
    mapVm(i + 1, `vm-${i}`, nodes[i % 2]!.id, "running"),
  );
  const { svg } = draw(toModel({ nodes, vms }));
  const shapes = [...svg.querySelectorAll("polygon, path")].filter(
    // 밝힌 선·초점 테두리·화살표 끝은 그림 맨 위의 덧그림이다
    (el) => !el.closest("marker, pattern") && el.getAttribute("fill") !== "none",
  );
  const last = shapes[shapes.length - 1]!;
  for (const name of ["front", "back", "z"]) {
    const text = [...svg.querySelectorAll("text")].find((t) => t.textContent?.startsWith(name))!;
    expect(text, name).toBeDefined();
    // 문서 차례가 그리는 차례: 글이 마지막 그림보다 뒤
    expect(last.compareDocumentPosition(text) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // 글 층은 포인터를 받지 않는다(누르기·끌기는 밑의 블록·타일로)
    expect(text.closest('g[pointer-events="none"]')).not.toBeNull();
  }
  // 노드 글은 노드 블록처럼 흐려지도록 그 노드 열쇠를 단다(열쇠로 찾으면 블록이 먼저)
  const keyed = svg.querySelectorAll(`g[data-key="node:${nodes[0]!.id}"]`);
  expect(keyed).toHaveLength(2);
  expect(keyed[0]).toHaveAttribute("role", "button");
  expect(keyed[1]!.textContent).toContain("front");
});
