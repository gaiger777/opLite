// 클러스터 지도의 끌어 옮기기(계획 71 설계 §10): 켜진 VM 타일 → 노드로 끌면 놓을 수 있는 노드를 밝히고, 놓으면 대상을
// 고른 옮기기 대화상자가 열린다(바로 옮기지 않음). 막힌 노드·빈 곳·원래 노드는 열지 않고 까닭을 읽는다. 타일 밖·꺼진 VM·
// 권한 없는 VM은 보기 움직이기 그대로, 터치는 길게 누르기, 키보드는 `m` → 화살표 → Enter·Esc.
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { ClusterMapPage } from "@/clusterMap/ClusterMapPage";
// 판정 함수는 서랍 조각과 함께 그림을 그린 뒤 온다 — 미리 읽어 두어 그림이 뜨면 끌 수 있게
import "@/clusterMap/MapSheet";
import { layoutCluster, nodeTop } from "@/clusterMap/layout";
import { toModel } from "@/clusterMap/model";
import { topCenter } from "@/components/iso";
import { LONG_PRESS_MS } from "@/dnd/core";
import { expectNoAxeViolations } from "@/test/axe";
import { load, mapNode, mapVm, placement, stubSvgMatrix } from "@/test/clusterMap";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const node = (n: number, name: string, spec = {}) =>
  mapNode(n, name, { labels: { zone: "a" }, spec, status: { arch: "aarch64" } });
const N1 = node(1, "n1");
const N2 = node(2, "n2");
const N3 = node(3, "n3", { maintenance: true });
const NODES = [N1, N2, N3];
const WEB = mapVm(1, "web", N1.id, "running");
const OFF = mapVm(2, "off", N1.id, "stopped");
const VMS = [WEB, OFF];

/** 클러스터·VM을 읽되 옮기기(`vm.migrate`)는 없는 사용자 */
const READER = {
  all: false,
  root: ["cluster.read", "system.read"],
  root_contents: ["system.read"],
  folders: {},
  contents: {},
  objects: [],
};

function world({ reader = false } = {}) {
  mockLists({ node: NODES, vm: VMS });
  if (reader) {
    server.use(http.get(apiUrl("/permissions/mine"), () => HttpResponse.json(READER)));
  }
  server.use(
    http.get(apiUrl("/placement"), () =>
      HttpResponse.json(
        placement([
          load(N1, { mem_total_mib: 16384, mem_allocated_mib: 4096 }),
          load(N2, { mem_total_mib: 16384, mem_allocated_mib: 4096 }),
          load(N3),
        ]),
      ),
    ),
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        node: N1.id,
        leader: N1.id,
        members: [],
      }),
    ),
    http.get(apiUrl("/alerts"), () =>
      HttpResponse.json({ alerts: [], unreachable: [], unsupported: [] }),
    ),
  );
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
}

// 그림과 같은 배치(같은 노드·VM)에서 타일·노드의 화면 점 — 시험의 SVG 행렬은 같은 좌표다.
const scene = layoutCluster(toModel({ nodes: NODES, vms: VMS }));
const tileAt = (id: string) => topCenter(scene.tiles.get(`vm:${id}`)!.cell.box);
const nodeAt = (id: string) => nodeTop(scene.byKey.get(`node:${id}`)!);

let restore: () => void;
beforeEach(() => {
  restore = stubSvgMatrix();
});
afterEach(() => {
  restore();
  localStorage.clear();
});

async function picture() {
  const map = await screen.findByRole("group", { name: "Cluster map" });
  await within(map).findByRole("button", { name: /^Node n1,/ });
  // 타일 자리가 위 배치와 같아질 때까지(목록이 다 옴)
  await waitFor(() => expect(map.querySelectorAll("path").length).toBeGreaterThan(4));
  return map;
}
const live = () => document.querySelector<HTMLElement>('div[role="status"][aria-live="polite"]')!;
const css = (map: HTMLElement) =>
  [...map.querySelectorAll("style")].map((s) => s.textContent).join("");
const at = (p: { x: number; y: number }, pointerType = "mouse", pointerId = 1) => ({
  pointerId,
  pointerType,
  button: 0,
  isPrimary: true,
  clientX: p.x,
  clientY: p.y,
});

it("drags a running VM tile onto a node, lights the nodes that can take it and opens the migrate dialog with that node", async () => {
  world();
  const map = await picture();
  fireEvent.pointerDown(map, at(tileAt(WEB.id)));
  // 누르기만으로는 아무것도 들지 않는다
  expect(css(map)).not.toContain("var(--brand)");
  fireEvent.pointerMove(map, at(nodeAt(N3.id)));
  // 막힌 노드 위: 까닭(알림 줄·카드), 빗금 흐림
  expect(live()).toHaveTextContent("n3. Cannot drop here: n3 is in maintenance");
  expect(map.parentElement!.querySelector("[data-map-card]")).toHaveTextContent(
    "n3 is in maintenance",
  );
  const style = css(map);
  expect(style).toContain(`g[data-key="node:${N2.id}"] .cm-top{stroke:var(--brand)`);
  expect(style).toMatch(new RegExp(`g\\[data-key="node:${N3.id}"\\]\\{opacity:\\.45\\}`));
  // 원래 노드는 "here" 꼬리표만
  expect(within(map).getByText("here")).toBeInTheDocument();
  expect(style).not.toContain(`g[data-key="node:${N1.id}"]{opacity`);
  await expectNoAxeViolations(document.body, { disable: ["region"] });
  fireEvent.pointerMove(map, at(nodeAt(N2.id)));
  expect(live()).toHaveTextContent("n2. Can drop here.");
  expect(screen.queryByRole("dialog")).toBeNull();
  fireEvent.pointerUp(map, at(nodeAt(N2.id)));
  fireEvent.click(map, at(nodeAt(N2.id)));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await waitFor(() => expect(within(dialog).getByLabelText("Target node")).toHaveValue(N2.id));
  // 끌기 뒤의 누르기는 서랍을 열지 않았다(대화상자 하나)
  expect(screen.getAllByRole("dialog")).toHaveLength(1);
  expect(css(map)).not.toContain("var(--brand)");
});

it("opens nothing when dropped on a blocked node, the VM's own node or empty space, and says why", async () => {
  world();
  const map = await picture();
  for (const [p, said] of [
    [nodeAt(N3.id), "Cannot drop here: n3 is in maintenance"],
    [nodeAt(N1.id), "Cannot drop here: the VM is already here"],
    [{ x: 5000, y: 5000 }, "Cancelled."],
  ] as const) {
    fireEvent.pointerDown(map, at(tileAt(WEB.id)));
    fireEvent.pointerMove(map, at({ x: tileAt(WEB.id).x + 20, y: tileAt(WEB.id).y + 20 }));
    fireEvent.pointerMove(map, at(p));
    fireEvent.pointerUp(map, at(p));
    expect(live()).toHaveTextContent(said);
  }
  await act(() => new Promise((r) => setTimeout(r, 50)));
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("pans from a node or a stopped VM's tile and opens the drawer on a plain click of a tile", async () => {
  world();
  const map = await picture();
  for (const from of [nodeAt(N2.id), tileAt(OFF.id)]) {
    const before = map.getAttribute("viewBox");
    fireEvent.pointerDown(map, at(from));
    fireEvent.pointerMove(map, at({ x: from.x + 40, y: from.y + 30 }));
    fireEvent.pointerUp(map, at({ x: from.x + 40, y: from.y + 30 }));
    expect(map.getAttribute("viewBox")).not.toBe(before);
    expect(live()).toHaveTextContent("");
  }
  fireEvent.click(map, at({ x: 0, y: 0 }));
  fireEvent.pointerDown(map, at(tileAt(WEB.id)));
  fireEvent.pointerUp(map, at(tileAt(WEB.id)));
  fireEvent.click(map, at(tileAt(WEB.id)));
  const sheet = await screen.findByRole("dialog");
  expect(within(sheet).getByRole("heading", { name: "web" })).toBeVisible();
  // 누르기는 들기가 아니다(알림 없음)
  expect(live()).not.toHaveTextContent("Picked up");
});

it("drags on touch only after a long press; moving before it pans", async () => {
  world();
  const map = await picture();
  const before = map.getAttribute("viewBox");
  fireEvent.pointerDown(map, at(tileAt(WEB.id), "touch"));
  fireEvent.pointerMove(map, at({ x: tileAt(WEB.id).x + 30, y: tileAt(WEB.id).y }, "touch"));
  fireEvent.pointerMove(map, at({ x: tileAt(WEB.id).x + 60, y: tileAt(WEB.id).y }, "touch"));
  fireEvent.pointerUp(map, at({ x: tileAt(WEB.id).x + 60, y: tileAt(WEB.id).y }, "touch"));
  expect(map.getAttribute("viewBox")).not.toBe(before);
  expect(live()).toHaveTextContent("");

  fireEvent.pointerDown(map, at(tileAt(WEB.id), "touch", 2));
  await act(() => new Promise((r) => setTimeout(r, LONG_PRESS_MS + 50)));
  const held = map.getAttribute("viewBox");
  fireEvent.pointerMove(map, at(nodeAt(N2.id), "touch", 2));
  expect(live()).toHaveTextContent("n2. Can drop here.");
  // 끄는 손가락은 보기를 움직이지 않는다
  expect(map.getAttribute("viewBox")).toBe(held);
  fireEvent.pointerUp(map, at(nodeAt(N2.id), "touch", 2));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await waitFor(() => expect(within(dialog).getByLabelText("Target node")).toHaveValue(N2.id));
});

it("moves with m, arrows and Enter, reads each node and cancels with Escape", async () => {
  world();
  const map = await picture();
  const n1 = within(map).getByRole("button", { name: /^Node n1,/ });
  n1.focus();
  fireEvent.keyDown(n1, { key: "]" });
  // 첫 타일은 이름 순 off(꺼짐): 까닭만 읽는다
  await screen.findByText(/^VM off, /);
  expect(fireEvent.keyDown(n1, { key: "m" })).toBe(false);
  expect(live()).toHaveTextContent("Stopped VMs change node in Edit.");
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  await screen.findByText(/^VM web, /);
  fireEvent.keyDown(n1, { key: "m" });
  expect(live()).toHaveTextContent(
    "Moving web. Arrows choose a node, Enter drops, Escape cancels. 1 node can take it.",
  );
  // 옮기는 동안 전역 단축키는 쉰다
  expect(document.documentElement).toHaveAttribute("data-dnd-held");
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  const n2 = within(map).getByRole("button", { name: /^Node n2,/ });
  await waitFor(() => expect(document.activeElement).toBe(n2));
  expect(live()).toHaveTextContent("n2. Can drop here. 12 GiB memory free.");
  expect(css(map)).toContain(`g[data-key="node:${N2.id}"] .cm-top{stroke-width:5}`);
  fireEvent.keyDown(n2, { key: "End" });
  const n3 = within(map).getByRole("button", { name: /^Node n3,/ });
  await waitFor(() => expect(document.activeElement).toBe(n3));
  expect(live()).toHaveTextContent("n3. Cannot drop here: n3 is in maintenance");
  // 막힌 노드에서 Enter: 까닭을 다시 읽고 열지 않는다
  fireEvent.keyDown(n3, { key: "Enter" });
  expect(screen.queryByRole("dialog")).toBeNull();
  fireEvent.keyDown(n3, { key: "Escape" });
  expect(live()).toHaveTextContent("Cancelled.");
  await waitFor(() => expect(document.activeElement).toBe(n1));
  expect(document.documentElement).not.toHaveAttribute("data-dnd-held");
  // Esc는 VM 타일로 돌아간다 — 다시 m
  fireEvent.keyDown(n1, { key: "m" });
  expect(live()).toHaveTextContent("Moving web.");
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  await waitFor(() => expect(document.activeElement).toBe(n2));
  fireEvent.keyDown(n2, { key: "Enter" });
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await waitFor(() => expect(within(dialog).getByLabelText("Target node")).toHaveValue(N2.id));
});

it("does not let a user without vm.migrate drag the VM, and m says why", async () => {
  world({ reader: true });
  const map = await picture();
  const before = map.getAttribute("viewBox");
  fireEvent.pointerDown(map, at(tileAt(WEB.id)));
  fireEvent.pointerMove(map, at(nodeAt(N2.id)));
  fireEvent.pointerUp(map, at(nodeAt(N2.id)));
  expect(map.getAttribute("viewBox")).not.toBe(before);
  expect(live()).toHaveTextContent("");
  const n1 = within(map).getByRole("button", { name: /^Node n1,/ });
  n1.focus();
  fireEvent.keyDown(n1, { key: "]" });
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  await screen.findByText(/^VM web, /);
  fireEvent.keyDown(n1, { key: "m" });
  expect(live()).toHaveTextContent("You may not migrate web.");
});

describe("edge of the picture", () => {
  let rect: ReturnType<typeof vi.spyOn>;
  beforeEach(() => {
    rect = vi
      .spyOn(SVGSVGElement.prototype, "getBoundingClientRect")
      .mockReturnValue(new DOMRect(0, 0, 960, 560));
  });
  afterEach(() => rect.mockRestore());

  const drag = async (map: HTMLElement) => {
    fireEvent.pointerDown(map, at(tileAt(WEB.id)));
    fireEvent.pointerMove(map, at({ x: tileAt(WEB.id).x + 20, y: tileAt(WEB.id).y }));
    fireEvent.pointerMove(map, at({ x: 4, y: 280 }));
    const x0 = Number(map.getAttribute("viewBox")!.split(" ")[0]);
    await waitFor(() =>
      expect(Number(map.getAttribute("viewBox")!.split(" ")[0])).toBeLessThan(x0),
    );
    fireEvent.keyDown(window, { key: "Escape" });
    return x0 - Number(map.getAttribute("viewBox")!.split(" ")[0]);
  };

  it("slides the view toward the edge the VM is dragged to", async () => {
    world();
    const moved = await drag(await picture());
    expect(moved).toBeGreaterThan(0);
  });

  it("jumps instead of sliding when the user asks for reduced motion", async () => {
    const real = window.matchMedia;
    window.matchMedia = (query: string) =>
      ({ ...real(query), matches: query.includes("reduce") }) as MediaQueryList;
    try {
      world();
      const map = await picture();
      const w = Number(map.getAttribute("viewBox")!.split(" ")[2]);
      const moved = await drag(map);
      // 한 번에 보기 너비의 1/3
      expect(moved).toBeCloseTo(w / 3, 0);
    } finally {
      window.matchMedia = real;
    }
  });
});
