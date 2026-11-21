import { layoutTopology, MAX_VMS_PER_SLAB, neighbor, related } from "@/topology/layout";
import { countReads, sampleGraph, topoId as id } from "@/test/topology";
import { type Topology, withoutNicless } from "@/topology/topology";

it("lays out the same picture for the same input, whatever the order", () => {
  const a = layoutTopology(sampleGraph());
  const shuffled = sampleGraph();
  shuffled.networks.reverse();
  shuffled.vms.reverse();
  const b = layoutTopology(shuffled);
  expect(b).toEqual(a);
});

it("puts providers at the back, routers above their networks and simple networks on the side", () => {
  const scene = layoutTopology(sampleGraph());
  const at = (key: string) => scene.items.find((it) => it.key.startsWith(key))!;
  const ext = at(`net:${id("001", 1)}`);
  const t1 = at(`net:${id("001", 2)}`);
  const t2 = at(`net:${id("001", 3)}`);
  const lan = at(`net:${id("001", 5)}`);
  const r1 = at("router:");
  expect(ext.kind).toBe("provider");
  expect(ext.box.y).toBeLessThan(r1.box.y);
  expect(r1.box.y).toBeLessThan(t1.box.y);
  // 라우터의 네트워크는 이름 순으로 앞(y)으로 펼친다
  expect(t1.box.y).toBeLessThan(t2.box.y);
  expect(t1.box.x).toBe(t2.box.x);
  // 단순 네트워크는 옆 열
  expect(lan.kind).toBe("simple");
  expect(lan.box.x).toBeGreaterThan(t1.box.x);
  expect(r1.detail).toBe("node-a");
  // 깊이 순: 바닥 판이 모두 먼저, 같은 층은 x + y가 작은 것부터
  const layers = scene.items.map((it) => it.layer);
  expect(layers).toEqual([...layers].sort());
  // VM은 첫 NIC의 판에 한 번, 다른 NIC로는 선
  expect(scene.items.filter((it) => it.kind === "vm" && it.id === id("002", 1))).toHaveLength(1);
  expect(scene.links.some((l) => l.kind === "nic")).toBe(true);
  // 꼬리표(유동 IP·바깥 VIP)와 그 선, 옮기기 화살표와 진행 수
  expect(scene.items.filter((it) => it.kind === "fip" || it.kind === "vip")).toHaveLength(2);
  expect(scene.links.find((l) => l.kind === "fip")?.dashed).toBe(false);
  expect(scene.links.find((l) => l.kind === "migration")?.label).toBe("1/2");
  expect(scene.links.find((l) => l.kind === "member")?.tone).toBe("online");
});

it("hides simple networks or VMs when asked", () => {
  const scene = layoutTopology(sampleGraph(), { hideSimple: true, hideVms: true });
  expect(scene.items.some((it) => it.kind === "simple")).toBe(false);
  expect(scene.items.some((it) => it.kind === "vm")).toBe(false);
  expect(scene.links.some((l) => l.kind === "migration")).toBe(false);
});

it("folds VMs past the slab limit into one +N block", () => {
  const graph = sampleGraph();
  const T3 = id("001", 4);
  for (let i = 0; i < 40; i++) {
    graph.vms.push({
      id: id("003", i),
      name: `vm${String(i).padStart(2, "0")}`,
      power: "stopped",
      nics: [{ network: T3, ips: [] }],
    });
  }
  const scene = layoutTopology(graph);
  const onT3 = scene.items.filter((it) => it.parent === `net:${T3}`);
  expect(onT3).toHaveLength(MAX_VMS_PER_SLAB);
  const more = onT3.find((it) => it.kind === "more")!;
  expect(more.label).toBe(`+${40 - (MAX_VMS_PER_SLAB - 1)}`);
  expect(scene.folded[`net:${T3}`]).toHaveLength(40 - (MAX_VMS_PER_SLAB - 1));
});

it("finds keyboard neighbours within and between rows", () => {
  const scene = layoutTopology(sampleGraph());
  const r1 = scene.items.find((it) => it.kind === "router")!.key;
  // 위는 꼬리표 줄, 아래는 네트워크 줄
  expect(scene.items.find((it) => it.key === neighbor(scene.items, r1, "up"))?.row).toBe(1);
  expect(scene.items.find((it) => it.key === neighbor(scene.items, r1, "down"))?.row).toBe(3);
  // 라우터는 하나라 옆이 없다
  expect(neighbor(scene.items, r1, "left")).toBeNull();
  const nets = scene.items.filter((it) => it.row === 3).sort((a, b) => a.center.x - b.center.x);
  expect(neighbor(scene.items, nets[0]!.key, "right")).toBe(nets[1]!.key);
});

it("lights the path from a VM up to the physical network", () => {
  const scene = layoutTopology(sampleGraph());
  const db = scene.items.find((it) => it.kind === "vm" && it.label === "db")!;
  const lit = related(scene, db.key);
  expect(lit.has(`net:${id("001", 2)}`)).toBe(true);
  expect(lit.has(`router:${id("00a", 1)}`)).toBe(true);
  expect(lit.has(`net:${id("001", 1)}`)).toBe(true);
  // 그 VM에 붙은 유동 IP도, 이웃 VM은 아니다
  expect(lit.has(`fip:${id("00b", 1)}`)).toBe(true);
  expect([...lit].some((k) => k.includes(id("002", 3)))).toBe(false);
});

/** §12.1의 큰 클러스터: 네트워크 200, 라우터 50, VM `vms` */
function bigGraph(vms: number): Topology {
  const graph: Topology = {
    networks: [],
    routers: [],
    load_balancers: [],
    floating_ips: [],
    vms: [],
    hidden: { networks: 0, routers: 0, load_balancers: 0, floating_ips: 0, vms: 0 },
  };
  for (let n = 0; n < 200; n++) {
    graph.networks.push({
      id: id("004", n),
      name: `net${n}`,
      kind: n % 4 === 0 ? "bridge" : "sdn",
      cidrs: [`10.${n}.0.0/24`],
    });
  }
  for (let r = 0; r < 50; r++) {
    graph.routers.push({
      id: id("005", r),
      name: `r${r}`,
      interfaces: [1, 2, 3].map((k) => id("004", r * 4 + k)),
      gateway: { state: "unknown" },
    });
  }
  for (let v = 0; v < vms; v++) {
    graph.vms.push({
      id: id("006", v),
      name: `vm${v}`,
      power: "running",
      nics: [{ network: id("004", v % 200), ips: [] }],
    });
  }
  return graph;
}

/** 배치가 VM 목록을 몇 번 읽는지(벽시계 대신 — 리뷰 2 웹 #4) */
function layoutReads(vms: number): number {
  const graph = bigGraph(vms);
  const counted = countReads(graph.vms);
  layoutTopology({ ...graph, vms: counted.items });
  return counted.reads();
}

it(
  "lays out a large cluster in work linear in the VMs (§12.1: 200 networks, 50 routers, 2000 VMs)",
  { timeout: 30_000 },
  () => {
    // VM 목록을 VM마다 다시 훑지 않는다(설계 목표 50 ms — M 계열 Mac에서 13 ms, §18). 시간은 시험 기계마다 다르므로 읽기
    // 수를 센다.
    const big = layoutReads(2000);
    expect(big).toBeGreaterThanOrEqual(2000);
    expect(big).toBeLessThanOrEqual(4 * 2000);
    expect(big / layoutReads(1000)).toBeLessThan(2.5);
    const scene = layoutTopology(bigGraph(2000));
    // SVG 원소: 원소마다 <g> + 면(블록 3, 판 3~5, 기둥 4~5) + 글 — 대략 5개(2250개 → 약 11,400, §18)
    const elements = scene.items.length * 5 + scene.links.length;
    expect(elements).toBeLessThan(15_000);
  },
);

it("drops VMs without a shown NIC and counts them (review 2 m7)", () => {
  const graph = sampleGraph();
  expect(withoutNicless(graph)).toEqual({ graph, nicless: 0 });
  const ghost = { id: id("006", 1), name: "ghost", power: "running" as const, nics: [] };
  const out = withoutNicless({ ...graph, vms: [...graph.vms, ghost] });
  expect(out.nicless).toBe(1);
  expect(out.graph.vms).toEqual(graph.vms);
});
