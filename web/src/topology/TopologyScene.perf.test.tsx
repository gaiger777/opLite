import { fireEvent, render, screen, within } from "@testing-library/react";

import i18n from "@/i18n";
import { topoId as id } from "@/test/topology";
import { layoutTopology } from "@/topology/layout";
import type { Topology } from "@/topology/topology";
import { itemLabels } from "@/topology/TopologyPage";
import { TopologyScene } from "@/topology/TopologyScene";

// 블록(VM·라우터·"+N")을 몇 번 그렸는지 센다(리뷰 1 I5 — 가리키기·끌기가 모든 원소를 다시 그리지 않는다).
const drawn = vi.hoisted(() => ({ blocks: 0 }));
vi.mock("@/components/iso", async (importOriginal) => {
  const real = await importOriginal<typeof import("@/components/iso")>();
  return {
    ...real,
    IsoBlock: (props: Parameters<typeof real.IsoBlock>[0]) => {
      drawn.blocks += 1;
      return real.IsoBlock(props);
    },
  };
});

/** §12.1의 큰 클러스터: 네트워크 200, 라우터 50, VM 2000 */
function bigGraph(): Topology {
  const graph: Topology = {
    networks: [],
    routers: [],
    load_balancers: [],
    floating_ips: [],
    vms: [],
    hidden: { networks: 0, routers: 0, load_balancers: 0, floating_ips: 0, vms: 0 },
  };
  for (let n = 0; n < 200; n++) {
    graph.networks.push({ id: id("004", n), name: `net${n}`, kind: "sdn", cidrs: [] });
  }
  for (let r = 0; r < 50; r++) {
    graph.routers.push({
      id: id("005", r),
      name: `r${r}`,
      interfaces: [1, 2, 3].map((k) => id("004", r * 4 + k)),
      gateway: { state: "unknown" },
    });
  }
  for (let v = 0; v < 2000; v++) {
    graph.vms.push({
      id: id("006", v),
      name: `vm${v}`,
      power: "running",
      nics: [{ network: id("004", v % 200), ips: [] }],
    });
  }
  return graph;
}

it("hovering and dragging a 2000-VM picture redraws only the items that change", () => {
  const graph = bigGraph();
  const scene = layoutTopology(graph);
  const started = performance.now();
  const labels = itemLabels(i18n.t, graph, scene);
  // 이름은 장면마다 한 번(원소마다 VM을 훑지 않음)
  expect(performance.now() - started).toBeLessThan(500);
  render(<TopologyScene scene={scene} labels={labels} onOpen={() => {}} showVmNames={false} />);
  const picture = screen.getByRole("group", { name: "Network topology" });
  const first = drawn.blocks;
  expect(first).toBeGreaterThan(1000);

  const vm = within(picture).getByRole("button", { name: "VM vm0, Running, —" });
  drawn.blocks = 0;
  fireEvent.pointerEnter(vm);
  // 밝힌 VM 하나만(굵게) 다시 그린다. 옅게 하기는 `<style>` 한 줄이다.
  expect(drawn.blocks).toBeLessThanOrEqual(2);
  const style = picture.querySelector("style")!.textContent!;
  expect(style).toContain(`g[data-key="vm:${id("004", 0)}:${id("006", 0)}"]`);
  drawn.blocks = 0;
  fireEvent.pointerLeave(vm);
  expect(drawn.blocks).toBeLessThanOrEqual(2);
  expect(picture.querySelector("style")!.textContent).toBe("");

  // 끌기(viewBox만 바뀜)도 원소를 다시 그리지 않는다.
  drawn.blocks = 0;
  fireEvent.pointerDown(picture, { pointerId: 1, clientX: 10, clientY: 10, button: 0 });
  fireEvent.pointerMove(picture, { pointerId: 1, clientX: 60, clientY: 40 });
  fireEvent.pointerUp(picture, { pointerId: 1, clientX: 60, clientY: 40 });
  expect(drawn.blocks).toBe(0);
});
