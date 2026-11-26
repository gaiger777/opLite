import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { useState } from "react";

import type { AlertView } from "@/alerts/alerts";
import { listKey } from "@/api/live";
import type { Task } from "@/api/types";
import { ClusterMapPage } from "@/clusterMap/ClusterMapPage";
import InfraMap, { type MapSelection } from "@/clusterMap/InfraMap";
import { AppLayout } from "@/layout/AppLayout";
import { PLACEMENT_KEY } from "@/placement/balance";
import { expectNoAxeViolations } from "@/test/axe";
import {
  layerStatus,
  load,
  mapNode,
  mapPool,
  mapVm,
  physicalTopology,
  placement,
  vmId,
} from "@/test/clusterMap";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const A = mapNode(1, "n1", {
  labels: { zone: "a" },
  spec: { sdn: { gateway: 1 } },
  status: layerStatus(40),
});
const B = mapNode(2, "n2", { labels: { zone: "a" }, spec: { maintenance: true } });
const C = mapNode(3, "n3", { labels: { zone: "b" }, status: { online: false } });
const W = mapNode(4, "w1", { labels: { zone: "b" }, spec: { witness: true } });
const TASK = "019a0f3c-0000-7000-8d00-000000000001";
const REC = "019a0f3c-0000-7000-8e00-000000000001";

const VMS = [
  mapVm(1, "web", A.id, "running", { spec: { ha: true } }),
  mapVm(2, "db", A.id, "failed"),
  mapVm(3, "job", B.id, "stopped"),
  mapVm(4, "mig", A.id, "migrating", {
    spec: { power: "running" },
    status: { node: A.id, migration: { node: B.id, task: TASK } },
  }),
  mapVm(5, "loose", null, "stopped"),
];

const ALERT: AlertView = {
  rule: "r",
  rule_name: "cpu",
  condition: "c",
  severity: "critical",
  since: 0,
  state: "firing",
  subject: { kind: "node", id: A.id, name: "n1" },
};

const MIGRATION: Task = {
  id: TASK,
  name: TASK,
  revision: 1,
  spec: { op: { kind: "vm-migrate" }, target: vmId(4), node: A.id },
  status: {
    state: "running",
    error: null,
    progress: { stage: "copying-disks", done_bytes: 62, total_bytes: 100 },
  },
} as Task;

function placementBody(mode: "recommend" | "off" = "recommend") {
  return placement(
    [
      load(A, { cpu_use_pct: 724, mem_use_pct: 380, vms: 2 }),
      load(B, { cpu_use_pct: 100, mem_use_pct: 100, vms: 1 }),
    ],
    {
      settings: { mode, policy: "balance", threshold: 15 } as never,
      recommendations: [
        {
          id: REC,
          vm: vmId(3),
          vm_name: "job",
          from: B.id,
          from_name: "n2",
          to: A.id,
          to_name: "n1",
          gain_pct: 120,
          cost_mib: 2048,
          reason: "balance",
        },
      ],
    },
  );
}

/** 클러스터·노드를 읽고 VM은 한 폴더 안만 읽는 사용자(옮기기·노드 쓰기 없음) */
const LIMITED = {
  all: false,
  root: ["cluster.read", "alert.read"],
  root_contents: [],
  folders: {},
  contents: { "019a0f3c-0000-7000-8f00-000000000001": ["system.read"] },
  objects: [],
};

function mockCluster({
  placementStatus = 200,
  clusterStatus = 200,
  mode = "recommend",
  stage = "copying-disks",
  vms = VMS,
  limited = false,
  physicalStatus = 200,
}: {
  placementStatus?: number;
  clusterStatus?: number;
  mode?: "recommend" | "off";
  stage?: string;
  vms?: typeof VMS;
  limited?: boolean;
  /** `GET /topology/physical`(계획 75): 200이면 n1의 eno1이 끊긴 자료, 아니면 그 상태(409 `cluster_upgrading`·403) */
  physicalStatus?: number;
} = {}) {
  mockLists({
    node: [A, B, C, W],
    vm: vms,
    "storage-pool": [mapPool(1, "local"), mapPool(2, "nfs", true)],
    task: [
      {
        ...MIGRATION,
        status: { ...MIGRATION.status, progress: { ...MIGRATION.status.progress!, stage } },
      },
    ],
  });
  if (limited) {
    server.use(http.get(apiUrl("/permissions/mine"), () => HttpResponse.json(LIMITED)));
  }
  server.use(
    http.get(apiUrl("/placement"), () =>
      placementStatus === 200
        ? HttpResponse.json(placementBody(mode))
        : HttpResponse.json(
            { code: "placement_unavailable", message: "leader unreachable" },
            { status: placementStatus },
          ),
    ),
    http.get(apiUrl("/cluster"), () =>
      clusterStatus === 200
        ? HttpResponse.json({
            name: "lab",
            ca_fingerprint: "x",
            node: A.id,
            leader: A.id,
            members: [],
          })
        : HttpResponse.json({ code: "forbidden", message: "no" }, { status: clusterStatus }),
    ),
    http.get(apiUrl("/alerts"), () =>
      HttpResponse.json({ alerts: [ALERT], unreachable: [], unsupported: [] }),
    ),
    http.get(apiUrl("/topology/physical"), () => {
      physicalCalls += 1;
      return physicalStatus === 200
        ? HttpResponse.json(physicalTopology([A, B, C, W], { n1: ["eno1"] }))
        : HttpResponse.json(
            {
              code: physicalStatus === 409 ? "cluster_upgrading" : "forbidden",
              message: "no",
            },
            { status: physicalStatus },
          );
    }),
  );
}

let physicalCalls = 0;
beforeEach(() => {
  physicalCalls = 0;
});

const pref = (key: string) => localStorage.getItem(`opl.${ADMIN.user}.clusterMap.${key}`);

afterEach(() => {
  localStorage.clear();
  document.documentElement.classList.remove("dark");
});

async function picture() {
  return screen.findByRole("group", { name: "Cluster map" });
}

it("draws node blocks with their state and the VMs on them", async () => {
  mockCluster();
  const { container } = renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  const n1 = await within(map).findByRole("button", {
    name: /^Node n1, group a, online, leader, CPU 72%, memory 38%, 3 VMs .*, 1 alert$/,
  });
  expect(n1).toHaveAttribute("tabindex", "0");
  expect(
    within(map).getByRole("button", { name: /^Node n2, group a, online, maintenance/ }),
  ).toHaveAttribute("tabindex", "-1");
  expect(
    within(map).getByRole("button", { name: /^Node n3, group b, offline, load unknown/ }),
  ).toBeVisible();
  expect(
    within(map).getByRole("button", { name: "Witness node w1, group b, online" }),
  ).toBeVisible();
  expect(within(map).getByRole("button", { name: "Not placed, 1 VM" })).toBeVisible();
  // 꼬리표는 모양 + 글(색만이 아님)
  expect(within(map).getByText("maintenance")).toBeInTheDocument();
  expect(within(map).getByText("offline")).toBeInTheDocument();
  expect(within(map).getByText("★ leader")).toBeInTheDocument();
  expect(within(map).getByText("⚠ 1")).toBeInTheDocument();
  // 진행 중 마이그레이션 화살표와 진행률
  expect(await within(map).findByText("mig → n2 · 62%")).toBeInTheDocument();
  // 판 이름
  expect(within(map).getByText("a · 2 nodes")).toBeInTheDocument();
  expect(screen.getByRole("list", { name: "Legend" })).toBeVisible();
  await expectNoAxeViolations(container, { disable: ["region"] });
});

it("shows unknown load and blocks recommendations when /placement is unavailable", async () => {
  mockCluster({ placementStatus: 503, clusterStatus: 403 });
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  expect(
    await screen.findByText("Load unknown: the leader's placement summary can't be read."),
  ).toBeVisible();
  expect(
    within(map).getByRole("button", { name: /^Node n1, group a, online, load unknown/ }),
  ).toBeVisible();
  // `/cluster` 403 → 리더 꼬리표 없음
  expect(within(map).queryByText("★ leader")).toBeNull();
  const recs = screen.getByRole("checkbox", { name: "Recommendations" });
  expect(recs).toBeDisabled();
  expect(screen.getByText("(placement summary unavailable)")).toBeVisible();
});

it("blocks the recommendations layer when the balancer is off", async () => {
  mockCluster({ mode: "off" });
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  await picture();
  await waitFor(() =>
    expect(screen.getByRole("checkbox", { name: "Recommendations" })).toBeDisabled(),
  );
  expect(screen.getByText("(the balancer is off)")).toBeVisible();
});

it("opens the node drawer with its VMs and node actions, then a VM drawer with Migrate", async () => {
  mockCluster();
  const user = userEvent.setup();
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  const n1 = await within(map).findByRole("button", { name: /^Node n1,/ });
  await user.click(n1);
  const sheet = await screen.findByRole("dialog");
  expect(within(sheet).getByRole("heading", { name: "n1" })).toBeVisible();
  expect(within(sheet).getByRole("button", { name: "Node actions" })).toBeVisible();
  expect(within(sheet).getByRole("link", { name: "View in network topology" })).toHaveAttribute(
    "href",
    "/networks/topology",
  );
  // 실패한 VM이 먼저
  const names = within(sheet)
    .getAllByRole("button")
    .map((b) => b.textContent)
    .filter((n) => ["web", "db", "mig"].includes(n ?? ""));
  expect(names).toEqual(["db", "mig", "web"]);
  await user.click(within(sheet).getByRole("button", { name: "web" }));
  const vmSheet = await screen.findByRole("dialog");
  expect(within(vmSheet).getByRole("heading", { name: "web" })).toBeVisible();
  await user.click(within(vmSheet).getByRole("button", { name: "Migrate…" }));
  expect(await screen.findByRole("dialog", { name: "Migrate web" })).toBeVisible();
});

it("moves with the keyboard: arrows between nodes, ] into tiles, Enter opens, [ back, Shift+Enter detail", async () => {
  mockCluster();
  const { router } = renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  const n1 = await within(map).findByRole("button", { name: /^Node n1,/ });
  n1.focus();
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  await waitFor(() =>
    expect(document.activeElement?.getAttribute("aria-label")).toMatch(/^Node n2,/),
  );
  expect(n1).toHaveAttribute("tabindex", "-1");
  const n2 = document.activeElement as HTMLElement;
  fireEvent.keyDown(n2, { key: "ArrowLeft" });
  await waitFor(() => expect(document.activeElement).toBe(n1));
  // `]`: 첫 VM 타일(이름 순 db) — 화면 초점은 노드에 남고 이름을 읽는다
  const into = fireEvent.keyDown(n1, { key: "]" });
  expect(into).toBe(false); // 막음(전역 `[`·`]`로 가지 않음)
  expect(await screen.findByText(/^VM db, Failed, node n1/)).toBeInTheDocument();
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  expect(
    await screen.findByText(/^VM mig, Migrating, node n1, moving to n2 62%/),
  ).toBeInTheDocument();
  fireEvent.keyDown(n1, { key: "Enter" });
  const sheet = await screen.findByRole("dialog");
  expect(within(sheet).getByRole("heading", { name: "mig" })).toBeVisible();
  fireEvent.keyDown(sheet, { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  // 서랍을 닫으면 그 노드로 초점이 돌아온다
  await waitFor(() => expect(document.activeElement).toBe(n1));
  fireEvent.keyDown(n1, { key: "[" });
  expect(await screen.findByText(/^Node n1, group a/, { selector: "p" })).toBeInTheDocument();
  fireEvent.keyDown(n1, { key: "Enter", shiftKey: true });
  await waitFor(() => expect(router.state.location.pathname).toBe("/nodes/n1"));
});

it("focuses the element named in ?focus=", async () => {
  mockCluster();
  renderPage(<ClusterMapPage />, { path: "/cluster/map", url: "/cluster/map?focus=vm:web" });
  const map = await picture();
  const n1 = await within(map).findByRole("button", { name: /^Node n1,/ });
  await waitFor(() => expect(document.activeElement).toBe(n1));
  expect(n1).toHaveAttribute("tabindex", "0");
  // 서랍은 열지 않는다
  expect(screen.queryByRole("dialog")).toBeNull();
  fireEvent.keyDown(n1, { key: "Enter" });
  const sheet = await screen.findByRole("dialog");
  expect(within(sheet).getByRole("heading", { name: "web" })).toBeVisible();
});

it("switches to the list view and remembers the view, layers and grouping per user", async () => {
  mockCluster();
  const user = userEvent.setup();
  const { container, unmount } = renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  await picture();
  await user.click(screen.getByRole("radio", { name: "List" }));
  const list = screen.getByRole("region", { name: "Cluster map as a list" });
  expect(within(list).getByRole("heading", { name: "a · 2 nodes" })).toBeVisible();
  expect(within(list).getByRole("button", { name: "web" })).toBeVisible();
  expect(within(list).getByRole("heading", { name: "Not placed" })).toBeVisible();
  expect(within(list).getByRole("button", { name: "loose" })).toBeVisible();
  await expectNoAxeViolations(container, { disable: ["region"] });
  await user.click(within(list).getByRole("button", { name: "n1" }));
  expect(await screen.findByRole("dialog")).toBeVisible();
  await user.keyboard("{Escape}");
  expect(pref("mode")).toBe("list");

  await user.click(screen.getByRole("checkbox", { name: "Storage" }));
  await user.selectOptions(screen.getByLabelText("Group by"), "none");
  expect(pref("layers")).toBe("load,storage");
  expect(pref("group")).toBe("none");
  unmount();

  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  expect(await screen.findByRole("region", { name: "Cluster map as a list" })).toBeVisible();
  expect(screen.getByRole("checkbox", { name: "Storage" })).toBeChecked();
  expect(screen.getByLabelText("Group by")).toHaveValue("none");
  await user.click(screen.getByRole("radio", { name: "Picture" }));
  const map = await picture();
  // 묶지 않으면 판 하나(클러스터 이름), 스토리지 레이어는 공유 풀 띠
  expect(within(map).getByText("lab · 4 nodes")).toBeInTheDocument();
  expect(within(map).getByText("Shared: nfs")).toBeInTheDocument();
});

it("applies a placement recommendation from its arrow after confirming", async () => {
  mockCluster();
  localStorage.setItem(`opl.${ADMIN.user}.clusterMap.layers`, "load,recommendations");
  const applied: string[] = [];
  server.use(
    http.post(apiUrl("/placement/recommendations/:id/apply"), ({ params }) => {
      applied.push(String(params.id));
      return HttpResponse.json({ ...MIGRATION, id: "t2", name: "t2" });
    }),
  );
  const user = userEvent.setup();
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  const arrow = await within(map).findByText("Recommended: job n2 → n1");
  await user.click(arrow);
  const sheet = await screen.findByRole("dialog", { name: "Recommended move of job" });
  expect(within(sheet).getByText("2 GiB")).toBeVisible();
  await user.click(within(sheet).getByRole("button", { name: "Apply" }));
  const confirm = await screen.findByRole("alertdialog", { name: "Move job?" });
  await user.click(within(confirm).getByRole("button", { name: "Apply" }));
  await waitFor(() => expect(applied).toEqual([REC]));
});

it("stops the migration animation when the user asks for reduced motion", async () => {
  const real = window.matchMedia;
  window.matchMedia = (query: string) =>
    ({ ...real(query), matches: query.includes("reduce") }) as MediaQueryList;
  try {
    mockCluster();
    renderPage(<ClusterMapPage />, { path: "/cluster/map" });
    const map = await picture();
    await within(map).findByText("mig → n2 · 62%");
    const css = [...map.querySelectorAll("style")].map((s) => s.textContent).join("");
    expect(css).toContain("cm-flow");
    expect(css).not.toContain("@keyframes");
  } finally {
    window.matchMedia = real;
  }
});

it("animates the migration arrow otherwise", async () => {
  mockCluster();
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  await within(map).findByText("mig → n2 · 62%");
  const css = [...map.querySelectorAll("style")].map((s) => s.textContent).join("");
  expect(css).toContain("@keyframes cm-flow");
  expect(css).toContain("prefers-reduced-motion");
});

it("folds the toolbar into one View settings sheet on a phone", async () => {
  const real = window.matchMedia;
  window.matchMedia = (query: string) =>
    ({ ...real(query), matches: query.includes("max-width: 767px") }) as MediaQueryList;
  try {
    mockCluster();
    const user = userEvent.setup();
    renderPage(<ClusterMapPage />, { path: "/cluster/map" });
    await picture();
    expect(screen.queryByLabelText("Group by")).toBeNull();
    await user.click(screen.getByRole("button", { name: "View settings" }));
    const sheet = await screen.findByRole("dialog", { name: "View settings" });
    expect(within(sheet).getByLabelText("Group by")).toBeVisible();
    expect(within(sheet).getByRole("checkbox", { name: "Network" })).toBeVisible();
  } finally {
    window.matchMedia = real;
  }
});

it.each(["light", "dark"])(
  "has no axe violations in the %s theme with every layer on",
  async (theme) => {
    localStorage.setItem("opl.theme", theme);
    localStorage.setItem(
      `opl.${ADMIN.user}.clusterMap.layers`,
      "load,storage,network,ha,recommendations",
    );
    mockCluster();
    const user = userEvent.setup();
    const { container } = renderPage(<ClusterMapPage />, { path: "/cluster/map" });
    const map = await picture();
    await within(map).findByText("mig → n2 · 62%");
    expect(document.documentElement.classList.contains("dark")).toBe(theme === "dark");
    expect(within(map).getByText("GW")).toBeInTheDocument();
    await expectNoAxeViolations(container, { disable: ["region"] });
    await user.click(within(map).getByRole("button", { name: /^Node n1,/ }));
    const sheet = await screen.findByRole("dialog");
    await expectNoAxeViolations(sheet);
  },
);

it("shows the memory stage, not 100%, once the disks are copied", async () => {
  mockCluster({ stage: "migrating-memory" });
  const user = userEvent.setup();
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  // 서버는 메모리 단계에 끝난 디스크 복사의 바이트(62/100)를 그대로 둔다 — 퍼센트가 아니라 번역한 단계
  const arrow = await within(map).findByText("mig → n2 · Migrating memory");
  expect(within(map).queryByText(/mig → n2 · \d+%/)).toBeNull();
  await user.click(arrow);
  const sheet = await screen.findByRole("dialog", { name: "Migration of mig" });
  expect(within(sheet).getByText("Migrating memory")).toBeVisible();
});

it("counts unreadable VMs only for users whose VM read is limited", async () => {
  // 관리자: `/placement`가 n2에 켜 둔 VM 하나를 세지만(꺼진 `job`만 보임) 가짜 "볼 수 없음"이 없다
  mockCluster();
  const { unmount } = renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  let map = await picture();
  const n2 = await within(map).findByRole("button", { name: /^Node n2, .*CPU 10%/ });
  expect(n2.getAttribute("aria-label")).not.toMatch(/can't see/);
  unmount();

  mockCluster({ limited: true });
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  map = await picture();
  expect(
    await within(map).findByRole("button", { name: /^Node n2, .*, 1 you can't see$/ }),
  ).toBeVisible();
});

it("hides Apply and node actions from a user who may not use them", async () => {
  mockCluster({ limited: true });
  localStorage.setItem(`opl.${ADMIN.user}.clusterMap.layers`, "load,recommendations");
  const user = userEvent.setup();
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  await user.click(await within(map).findByText("Recommended: job n2 → n1"));
  const sheet = await screen.findByRole("dialog", { name: "Recommended move of job" });
  expect(within(sheet).getByText("2 GiB")).toBeVisible();
  // VM `job`을 옮길 수 없다(`vm.migrate` 없음) — 서버의 403을 보지 않게 숨긴다
  expect(within(sheet).queryByRole("button", { name: "Apply" })).toBeNull();
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await user.click(within(map).getByRole("button", { name: /^Node n1,/ }));
  const node = await screen.findByRole("dialog");
  expect(within(node).getByRole("heading", { name: "n1" })).toBeVisible();
  expect(within(node).queryByRole("button", { name: "Node actions" })).toBeNull();
});

it("shows Apply to a folder operator who may migrate the VM in its folder", async () => {
  // 루트에는 `vm.migrate`가 없고 `job`의 폴더 안에만 있다 — 화면은 VM마다(그 폴더로) 묻는다
  const folder = "019a0f3c-0000-7000-8f00-000000000001";
  mockCluster({
    limited: true,
    vms: VMS.map((vm) => (vm.name === "job" ? { ...vm, spec: { ...vm.spec, folder } } : vm)),
  });
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({ ...LIMITED, contents: { [folder]: ["system.read", "vm.migrate"] } }),
    ),
  );
  localStorage.setItem(`opl.${ADMIN.user}.clusterMap.layers`, "load,recommendations");
  const user = userEvent.setup();
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  await user.click(await within(map).findByText("Recommended: job n2 → n1"));
  const sheet = await screen.findByRole("dialog", { name: "Recommended move of job" });
  expect(await within(sheet).findByRole("button", { name: "Apply" })).toBeVisible();
});

it("leaves Ctrl/⌘ with + - 0 to the browser's page zoom", async () => {
  mockCluster();
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  const n1 = await within(map).findByRole("button", { name: /^Node n1,/ });
  n1.focus();
  for (const key of ["-", "+", "=", "0"]) {
    expect(fireEvent.keyDown(n1, { key, ctrlKey: true })).toBe(true);
    expect(fireEvent.keyDown(n1, { key, metaKey: true })).toBe(true);
  }
  // 그냥 누르면 지도의 확대·축소·맞추기
  expect(fireEvent.keyDown(n1, { key: "-" })).toBe(false);
  expect(fireEvent.keyDown(n1, { key: "0" })).toBe(false);
});

it("reaches arrows without a mouse: the node drawer and the list view list them with Apply", async () => {
  mockCluster();
  localStorage.setItem(`opl.${ADMIN.user}.clusterMap.layers`, "load,recommendations");
  const applied: string[] = [];
  server.use(
    http.post(apiUrl("/placement/recommendations/:id/apply"), ({ params }) => {
      applied.push(String(params.id));
      return HttpResponse.json({ ...MIGRATION, id: "t2", name: "t2" });
    }),
  );
  const user = userEvent.setup();
  const { container, queryClient } = renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  // 그림: n2에 초점 → Enter → 노드 서랍의 "마이그레이션과 추천" → 추천 서랍 → 적용
  const n1 = await within(map).findByRole("button", { name: /^Node n1,/ });
  n1.focus();
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  await waitFor(() =>
    expect(document.activeElement?.getAttribute("aria-label")).toMatch(/^Node n2,/),
  );
  const n2 = document.activeElement as HTMLElement;
  fireEvent.keyDown(n2, { key: "Enter" });
  const sheet = await screen.findByRole("dialog");
  expect(
    within(sheet).getByRole("heading", { name: "Migrations and recommendations" }),
  ).toBeVisible();
  expect(within(sheet).getByRole("button", { name: "mig → n2 · 62%" })).toBeVisible();
  await user.click(within(sheet).getByRole("button", { name: "Recommended: job n2 → n1" }));
  const rec = await screen.findByRole("dialog", { name: "Recommended move of job" });
  await user.click(within(rec).getByRole("button", { name: "Apply" }));
  const confirm = await screen.findByRole("alertdialog", { name: "Move job?" });
  await user.click(within(confirm).getByRole("button", { name: "Apply" }));
  await waitFor(() => expect(applied).toEqual([REC]));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await waitFor(() => expect(document.activeElement).toBe(n2));

  // 목록 보기: 같은 화살표가 단추, 누르면 같은 서랍
  await user.click(screen.getByRole("radio", { name: "List" }));
  const list = screen.getByRole("region", { name: "Cluster map as a list" });
  expect(
    within(list).getByRole("heading", { name: "Migrations and recommendations" }),
  ).toBeVisible();
  await expectNoAxeViolations(container, { disable: ["region"] });
  const mig = within(list).getByRole("button", { name: "mig → n2 · 62%" });
  await user.click(mig);
  const task = await screen.findByRole("dialog", { name: "Migration of mig" });
  expect(within(task).getByRole("link", { name: "Open tasks" })).toBeVisible();
  await user.keyboard("{Escape}");
  await waitFor(() => expect(document.activeElement).toBe(mig));

  // 목록에서 적용: 서버가 추천에 Task를 달고 `job`이 옮겨 가기 시작한다 → 추천 단추가 사라지고 초점은 `job`의 새
  // 화살표로(사라진 단추나 `<body>`가 아니라)
  const moving = VMS.map((vm) =>
    vm.name === "job"
      ? mapVm(3, "job", B.id, "migrating", {
          spec: { power: "running" },
          status: { node: B.id, migration: { node: A.id, task: "t2" } },
        })
      : vm,
  );
  server.use(
    http.post(apiUrl("/placement/recommendations/:id/apply"), async () => {
      mockCluster({ vms: moving });
      const body = placementBody();
      server.use(
        http.get(apiUrl("/placement"), () =>
          HttpResponse.json({
            ...body,
            recommendations: body.recommendations.map((r) => ({ ...r, task: "t2" })),
          }),
        ),
      );
      await queryClient.refetchQueries({ queryKey: listKey("vm") });
      return HttpResponse.json({ ...MIGRATION, id: "t2", name: "t2" });
    }),
  );
  await user.click(within(list).getByRole("button", { name: "Recommended: job n2 → n1" }));
  const rec2 = await screen.findByRole("dialog", { name: "Recommended move of job" });
  await user.click(within(rec2).getByRole("button", { name: "Apply" }));
  const confirm2 = await screen.findByRole("alertdialog", { name: "Move job?" });
  await user.click(within(confirm2).getByRole("button", { name: "Apply" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await waitFor(
    () => expect(document.activeElement?.getAttribute("data-key")).toBe(`move:${vmId(3)}`),
    { timeout: 3000 },
  );
  expect(within(list).queryByRole("button", { name: /^Recommended:/ })).toBeNull();

  // 서랍을 연 채 마이그레이션이 끝난다 → 화살표와 서랍이 사라지고 초점은 "마이그레이션과 추천" 머리글로
  await user.click(within(list).getByRole("button", { name: "mig → n2 · 62%" }));
  await screen.findByRole("dialog", { name: "Migration of mig" });
  mockCluster({
    vms: moving.map((vm) => (vm.name === "mig" ? mapVm(4, "mig", B.id, "running") : vm)),
  });
  await queryClient.refetchQueries({ queryKey: listKey("vm") });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull(), { timeout: 3000 });
  await waitFor(() =>
    expect(document.activeElement).toBe(
      within(list).getByRole("heading", { name: "Migrations and recommendations" }),
    ),
  );
});

it("does not list recommendations in the list view while their layer is off", async () => {
  mockCluster();
  const user = userEvent.setup();
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  await picture();
  await user.click(screen.getByRole("radio", { name: "List" }));
  const list = screen.getByRole("region", { name: "Cluster map as a list" });
  expect(await within(list).findByRole("button", { name: "mig → n2 · 62%" })).toBeVisible();
  expect(within(list).queryByRole("button", { name: /^Recommended:/ })).toBeNull();
});

it("returns focus to the right node after Shift+Enter on a +N hidden tile", async () => {
  mockCluster({ limited: true });
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  const n1 = await within(map).findByRole("button", { name: /^Node n1,/ });
  // 먼저 n1의 서랍을 열고 닫는다(돌려줄 초점이 n1로 남음)
  n1.focus();
  fireEvent.keyDown(n1, { key: "Enter" });
  fireEvent.keyDown(await screen.findByRole("dialog"), { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await waitFor(() => expect(document.activeElement).toBe(n1));
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  await waitFor(() =>
    expect(document.activeElement?.getAttribute("aria-label")).toMatch(/^Node n2,/),
  );
  const n2 = document.activeElement as HTMLElement;
  fireEvent.keyDown(n2, { key: "]" });
  fireEvent.keyDown(n2, { key: "End" });
  expect((await screen.findAllByText("1 VM on n2 you can't see")).length).toBeGreaterThan(0);
  fireEvent.keyDown(n2, { key: "Enter", shiftKey: true });
  const sheet = await screen.findByRole("dialog", { name: "VMs on n2" });
  fireEvent.keyDown(sheet, { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await waitFor(() => expect(document.activeElement).toBe(n2));
});

it("outlines the Not placed tray when it has focus", async () => {
  mockCluster();
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  const n1 = await within(map).findByRole("button", { name: /^Node n1,/ });
  n1.focus();
  fireEvent.keyDown(n1, { key: "End" });
  const tray = within(map).getByRole("button", { name: "Not placed, 1 VM" });
  await waitFor(() => expect(document.activeElement).toBe(tray));
  const outline = map.querySelector('path[pointer-events="none"]');
  expect(outline?.getAttribute("d")).not.toBe("");
});

it("drops a remembered label grouping or group that no longer exists", async () => {
  mockCluster();
  localStorage.setItem(`opl.${ADMIN.user}.clusterMap.group`, "label:gone");
  localStorage.setItem(
    `opl.${ADMIN.user}.clusterMap.filter`,
    JSON.stringify({ slab: "slab:gone", hideStopped: false, problems: false }),
  );
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  await picture();
  await waitFor(() => expect(screen.getByLabelText("Group by")).toHaveValue("zone"));
  expect(screen.getByLabelText("Show")).toHaveValue("");
  expect(pref("group")).toBe("zone");
  expect(JSON.parse(pref("filter")!)).toMatchObject({ slab: "" });
});

it("lists a VM in the drawer of the node the picture draws it on", async () => {
  // `status.node`가 지운 노드를 가리키면 그림은 `spec.node`(n2)에 그린다 — 서랍도 같게
  const stray = mapVm(6, "stray", B.id, "stopped", { status: { node: "gone" } });
  mockCluster({ vms: [...VMS, stray] });
  const user = userEvent.setup();
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  await user.click(await within(map).findByRole("button", { name: /^Node n2,/ }));
  const sheet = await screen.findByRole("dialog");
  expect(within(sheet).getByRole("button", { name: "stray" })).toBeVisible();
  expect(within(sheet).getByRole("button", { name: "job" })).toBeVisible();
});

it("drops stale load when a later /placement read fails", async () => {
  mockCluster();
  const { queryClient } = renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  expect(
    await within(map).findByRole("button", { name: /^Node n1, .*CPU 72%, memory 38%/ }),
  ).toBeVisible();
  server.use(
    http.get(apiUrl("/placement"), () =>
      HttpResponse.json(
        { code: "placement_unavailable", message: "leader unreachable" },
        { status: 503 },
      ),
    ),
  );
  await queryClient.refetchQueries({ queryKey: PLACEMENT_KEY });
  expect(
    await screen.findByText("Load unknown: the leader's placement summary can't be read."),
  ).toBeVisible();
  expect(
    await within(map).findByRole(
      "button",
      { name: /^Node n1, group a, online, leader, load unknown/ },
      { timeout: 3000 },
    ),
  ).toBeVisible();
});

it("keeps [ for the map while it has focus and for the sidebar elsewhere", async () => {
  mockCluster();
  renderPage(
    <>
      <AppLayout />
      <ClusterMapPage />
    </>,
    { path: "*", url: "/cluster/map" },
  );
  const map = await picture();
  // jsdom은 좁은 화면이라 레일(접힘)로 시작한다
  expect(await screen.findByRole("button", { name: "Expand sidebar" })).toBeInTheDocument();
  // 그림 밖: 전역 `[`가 사이드바를 펴고 접는다
  fireEvent.keyDown(document.body, { key: "[" });
  expect(await screen.findByRole("button", { name: "Collapse sidebar" })).toBeInTheDocument();
  // 그림의 노드에 초점: `[`는 그림이 받는다(사이드바 그대로)
  const n1 = await within(map).findByRole("button", { name: /^Node n1,/ });
  n1.focus();
  fireEvent.keyDown(n1, { key: "]" });
  fireEvent.keyDown(n1, { key: "[" });
  expect(await screen.findByText(/^Node n1, group a/, { selector: "p" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Collapse sidebar" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Expand sidebar" })).toBeNull();
});

it("reads the physical topology only with the network layer on", async () => {
  mockCluster();
  const { unmount } = renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const map = await picture();
  await within(map).findByRole("button", { name: /^Node n1,/ });
  expect(physicalCalls).toBe(0);
  expect(within(map).queryByText(/^vmbr0 · /)).toBeNull();
  unmount();

  localStorage.setItem(`opl.${ADMIN.user}.clusterMap.layers`, "load,network");
  renderPage(<ClusterMapPage />, { path: "/cluster/map" });
  const net = await picture();
  // 판마다 레일(브리지 vmbr0의 lan·v100, underlay), 장애 꼬리표, 범례의 포트·레일
  expect(await within(net).findAllByText("vmbr0 · 2 networks")).toHaveLength(2);
  expect(within(net).getAllByText("Underlay (VXLAN) · 1 network")).toHaveLength(2);
  expect(within(net).getByText("link ✕ 1")).toBeInTheDocument();
  expect(
    within(net).getByRole("button", { name: /^Node n1, .*, 1 link fault$/ }),
  ).toBeInTheDocument();
  expect(screen.getByRole("list", { name: "Legend" })).toHaveTextContent("Network rail");
});

it("focuses the port named in ?focus=port: and opens its sheet with the impact", async () => {
  mockCluster();
  renderPage(<ClusterMapPage />, {
    path: "/cluster/map",
    url: "/cluster/map?focus=port:n1/eno1",
  });
  const map = await picture();
  const n1 = await within(map).findByRole("button", { name: /^Node n1,/ });
  // 네트워크 레이어를 (기억하지 않고) 켜고 물리 관계가 온 뒤 그 포트로
  await waitFor(() => expect(document.activeElement).toBe(n1));
  const card = await screen.findByText("Port eno1 on n1");
  expect(card.closest("[data-map-card]")).toHaveTextContent(
    "Affected: no redundancy for networks 2, VMs 3",
  );
  expect(pref("layers")).toBeNull();
  expect(screen.queryByRole("dialog")).toBeNull();
  fireEvent.keyDown(n1, { key: "Enter" });
  const sheet = await screen.findByRole("dialog", { name: "Port eno1 on n1" });
  expect(within(sheet).getByText("Switch: sw1 · Gi1/0/1")).toBeVisible();
  expect(within(sheet).getByText("Bridges: vmbr0")).toBeVisible();
  expect(within(sheet).getByText("lan, v100")).toBeVisible();
  // 다음 포트 eno2는 LLDP가 없다 — lldpd 안내
  fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await waitFor(() => expect(document.activeElement).toBe(n1));
  fireEvent.keyDown(n1, { key: "ArrowRight" });
  fireEvent.keyDown(n1, { key: "Enter" });
  const next = await screen.findByRole("dialog", { name: "Port eno2 on n1" });
  expect(within(next).getByText("Install lldpd to see the switch port.")).toBeVisible();
});

it.each([409, 403])(
  "keeps the single uplink band when the physical topology answers %i",
  async (status) => {
    localStorage.setItem(`opl.${ADMIN.user}.clusterMap.layers`, "load,network");
    mockCluster({ physicalStatus: status });
    renderPage(<ClusterMapPage />, { path: "/cluster/map" });
    const map = await picture();
    await within(map).findByRole("button", { name: /^Node n1,/ });
    await waitFor(() => expect(physicalCalls).toBe(1));
    expect(within(map).queryByText(/networks?$/)).toBeNull();
    expect(screen.getByRole("list", { name: "Legend" })).toHaveTextContent("Uplinks up");
    expect(screen.getByRole("list", { name: "Legend" })).not.toHaveTextContent("Network rail");
    // 다시 묻지 않는다(403·409)
    await new Promise((r) => setTimeout(r, 50));
    expect(physicalCalls).toBe(1);
  },
);

describe("home variant", () => {
  it("reports selections instead of opening a sheet, with the legend folded", async () => {
    mockCluster();
    const onSelect = vi.fn();
    renderPage(<InfraMap variant="home" onSelect={onSelect} />, { path: "/" });
    const map = await picture();
    // 홈은 쪽 머리글 없이, 기본 레이어에 네트워크(물리 관계를 읽음)
    expect(screen.queryByRole("heading", { name: "Map" })).toBeNull();
    expect(await within(map).findAllByText("vmbr0 · 2 networks")).toHaveLength(2);
    expect(map.getAttribute("class")).toContain("h-[26rem]");
    const legend = screen.getByText("Legend", { selector: "summary" });
    expect(legend.closest("details")).not.toHaveAttribute("open");
    // 도구 띠는 보기 설정 단추 하나로 접힘
    expect(screen.queryByLabelText("Group by")).toBeNull();
    expect(screen.getByRole("button", { name: "View settings" })).toBeVisible();

    const n1 = within(map).getByRole("button", { name: /^Node n1,/ });
    fireEvent.click(n1);
    expect(onSelect).toHaveBeenLastCalledWith({ kind: "node", id: A.id });
    act(() => n1.focus());
    fireEvent.keyDown(n1, { key: "p" });
    fireEvent.keyDown(n1, { key: "Enter" });
    expect(onSelect).toHaveBeenLastCalledWith({ kind: "port", node: A.id, port: "eno1" });
    fireEvent.keyDown(n1, { key: "ArrowDown" });
    fireEvent.keyDown(n1, { key: "Enter" });
    expect(onSelect).toHaveBeenLastCalledWith({ kind: "rail", id: "vmbr0" });
    fireEvent.keyDown(n1, { key: "Escape" });
    fireEvent.keyDown(n1, { key: "]" });
    fireEvent.keyDown(n1, { key: "Enter" });
    expect(onSelect).toHaveBeenLastCalledWith({ kind: "vm", id: vmId(2) });
    // 빈 곳을 누르면 고른 것을 푼다
    fireEvent.click(map);
    expect(onSelect).toHaveBeenLastCalledWith(null);
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("lights the selected element and moves to the focus without taking the page's focus", async () => {
    mockCluster();
    function Harness() {
      const [selected, setSelected] = useState<MapSelection | undefined>({
        kind: "node",
        id: B.id,
      });
      const [focus, setFocus] = useState<string | undefined>();
      return (
        <>
          <button onClick={() => setSelected({ kind: "rail", id: "vmbr0" })}>rail</button>
          <button
            onClick={() => {
              setSelected(undefined);
              setFocus("port:n1/eno1");
            }}
          >
            port
          </button>
          <InfraMap variant="home" selected={selected} focus={focus} />
        </>
      );
    }
    renderPage(<Harness />, { path: "/" });
    const map = await picture();
    await within(map).findByRole("button", { name: /^Node n2,/ });
    const css = () => map.querySelector("style")!.textContent!;
    expect(css()).toContain(`g[data-key="node:${B.id}"]`);
    expect(css()).toContain("{opacity:.35}");
    // 레일을 고르면 그 레일을 쓰는 노드(증인 아님)를 밝힌다
    fireEvent.click(screen.getByRole("button", { name: "rail" }));
    await waitFor(() => expect(css()).toContain(`g[data-key="node:${C.id}"]`));
    expect(css()).not.toContain(`g[data-key="node:${W.id}"]`);
    const port = screen.getByRole("button", { name: "port" });
    act(() => port.focus());
    fireEvent.click(port);
    expect(await screen.findByText("Port eno1 on n1")).toBeInTheDocument();
    expect(document.activeElement).toBe(port);
  });

  it("has no axe violations in the home variant", async () => {
    mockCluster();
    const { container } = renderPage(<InfraMap variant="home" onSelect={() => {}} />, {
      path: "/",
    });
    const map = await picture();
    await within(map).findAllByText("vmbr0 · 2 networks");
    await expectNoAxeViolations(container, { disable: ["region"] });
  });
});
