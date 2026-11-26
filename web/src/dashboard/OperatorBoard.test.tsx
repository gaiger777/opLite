import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { MapSelection } from "@/dashboard/selection";
import type { NodeTopologyInfo } from "@/numa/numa";
import { DashboardPage } from "@/pages/DashboardPage";
import { expectNoAxeViolations } from "@/test/axe";
import { NODE, vm, volume } from "@/test/fixtures";
import { CAPACITY, LAN, PHYSICAL } from "@/test/physical";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

// 지도(5단계 `clusterMap/InfraMap`)는 가짜: 받은 초점을 글로 보이고, 단추로 고른다.
vi.mock("@/dashboard/HomeMap", () => ({
  HomeMap: ({
    onSelect,
    focus,
    selected,
  }: {
    onSelect(s: MapSelection | null): void;
    focus?: string;
    selected?: MapSelection;
  }) => (
    <div>
      <p>map focus: {focus ?? "none"}</p>
      <p>map selected: {selected ? selected.kind : "none"}</p>
      <button
        type="button"
        onClick={() => onSelect({ kind: "vm", id: "019a0f3c-0000-7000-8000-0000000000db" })}
      >
        map vm db
      </button>
      <button type="button" onClick={() => onSelect({ kind: "rail", id: "vmbr0" })}>
        map rail lan
      </button>
    </div>
  ),
}));

const VIEWER = { ...ADMIN, role: "viewer" as const };

const TOPO: NodeTopologyInfo = {
  node: "node-a",
  host_nodes: [],
  vms: [
    { vm: "db", host_nodes: [1], vcpus: [8, 9], siblings: "", emulator: "", page: "2M", pages: 64 },
  ],
  sets: { reserved: "0", dedicated: "8-15", shared: "1-7" },
  warnings: [],
};

const NODE_B = { ...NODE, id: "n2", name: "node-b" };
const DB = vm("db", "running", {
  spec: {
    ...vm("db", "running").spec,
    disks: [{ volume: "db-root", bus: "virtio-blk" }],
    nics: [{ network: "tenant", model: "virtio" }],
  },
});
const WEB = vm("web", "running", {
  spec: { ...vm("web", "running").spec, nics: [{ network: "lan", model: "virtio" }] },
});

function dashboard({ rules = [] as unknown[] } = {}) {
  const volumes = [volume("db-root", "fast")];
  mockLists({ node: [NODE, NODE_B], vm: [DB, WEB] });
  server.use(
    // VM 판은 디스크의 볼륨만 하나씩 묻는다(목록 전체를 지켜보지 않음).
    http.get(apiUrl("/volumes/:key"), ({ params }) => {
      const v = volumes.find((x) => x.name === params.key);
      return v
        ? HttpResponse.json(v)
        : HttpResponse.json({ code: "not_found", message: "no" }, { status: 404 });
    }),
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
    http.get(apiUrl("/capacity"), () => HttpResponse.json(CAPACITY)),
    http.get(apiUrl("/topology/physical"), () => HttpResponse.json(PHYSICAL)),
    http.get(apiUrl("/nodes/:key/topology"), ({ params }) =>
      HttpResponse.json(
        params.key === "node-a"
          ? TOPO
          : { node: params.key, host_nodes: [], vms: [], warnings: [] },
      ),
    ),
    http.get(apiUrl("/nodes/:key/metrics"), ({ params }) =>
      HttpResponse.json({
        node: params.key,
        samples: [
          { t: 1, cpu: 10 },
          {
            t: 2,
            cpu: 12,
            interfaces: [{ name: "eno2", rx_bps: 2048, tx_bps: 1024, rx_errors: 3, tx_errors: 0 }],
          },
        ],
      }),
    ),
    http.get(apiUrl("/alert-rules"), () => HttpResponse.json({ revision: 1, items: rules })),
  );
}

const regionNames = () =>
  screen.getAllByRole("region").map((r) => {
    const id = r.getAttribute("aria-labelledby");
    return r.getAttribute("aria-label") ?? (id ? document.getElementById(id)?.textContent : "");
  });

it("lays out the map, the details panel and the allocation table on a wide screen", async () => {
  dashboard();
  const { container } = renderPage(<DashboardPage />);
  const table = await screen.findByRole("table", { name: "Allocation and use by node" });
  const names = regionNames();
  // 지도 → 표 → (그 뒤) 지금 위젯
  expect(names.indexOf("Infrastructure map")).toBeLessThan(
    names.indexOf("Allocation and use by node"),
  );
  expect(names.indexOf("Allocation and use by node")).toBeLessThan(names.indexOf("Active alerts"));
  // 지난 홈의 용량·노드 부하 위젯은 표가 대신한다.
  expect(screen.queryByRole("region", { name: "Capacity" })).not.toBeInTheDocument();
  expect(screen.queryByRole("table", { name: "Node load" })).not.toBeInTheDocument();

  // 고른 것 없음: 클러스터 합계와 장애 목록
  const panel = screen.getByRole("complementary", { name: "Details" });
  expect(within(panel).getByRole("heading", { name: "Cluster" })).toBeInTheDocument();
  expect(within(panel).getByRole("meter", { name: "Memory" })).toHaveAttribute(
    "aria-valuetext",
    "allocated 60 GiB / 96 GiB · used 36 GiB · ×0.6",
  );
  expect(await within(panel).findByRole("button", { name: "eno1 on node-a" })).toBeInTheDocument();

  // 행: 막대(넘침 꼬리), 오버커밋, 문제
  const a = within(table).getByRole("row", { name: /node-a/ });
  const cpu = within(a).getByRole("meter", { name: "CPU of node-a" });
  // 공유 할당과 전용 할당을 따로, 사용률은 노드 전체(공유 풀 막대에 얹지 않음)
  expect(cpu).toHaveAttribute(
    "aria-valuetext",
    "14 shared vCPU / 7 threads · ×2.0 · dedicated 2 / 8 · node use 40 %",
  );
  expect(cpu.querySelector("[data-part=overflow]")).not.toBeNull();
  expect(
    within(a)
      .getByRole("meter", { name: "Memory of node-a" })
      .querySelector("[data-part=overflow]"),
  ).toBeNull();
  expect(within(a).getByRole("meter", { name: "Storage of node-a" })).toHaveAttribute(
    "aria-valuetext",
    "provisioned 150 GiB / 100 GiB · used 95 GiB",
  );
  // 공유 vCPU ×2는 문제가 아니다(×4 넘음부터).
  expect(a).not.toHaveTextContent("overcommitted");
  expect(a).toHaveTextContent("over 90 %");
  expect(a).toHaveTextContent("1 link fault");
  expect(within(table).getByRole("row", { name: /node-b/ })).toHaveTextContent("1 pool not ready");
  expect(
    screen.getByText(
      /^Shared pools: ceph used 100 GiB \/ 1000 GiB, provisioned 300 GiB \(2\/3 nodes\)/,
    ),
  ).toBeInTheDocument();
  await expectNoAxeViolations(container);
});

it("says the physical state is unknown instead of 'no faults' when it cannot be read", async () => {
  dashboard();
  let fail = true;
  server.use(
    http.get(apiUrl("/topology/physical"), () =>
      fail
        ? HttpResponse.json({ code: "internal", message: "leader changed" }, { status: 500 })
        : HttpResponse.json(PHYSICAL),
    ),
  );
  const user = userEvent.setup();
  renderPage(<DashboardPage />);
  const panel = await screen.findByRole("complementary", { name: "Details" });
  expect(
    await within(panel).findByText(/Physical link state unknown/, undefined, { timeout: 5000 }),
  ).toBeInTheDocument();
  expect(within(panel).queryByText("No link or bond faults.")).not.toBeInTheDocument();
  const table = screen.getByRole("table", { name: "Allocation and use by node" });
  expect(within(table).getByRole("row", { name: /node-c/ })).toHaveTextContent(
    "link state unknown",
  );
  // 배지는 "?"이고 누르면 다시 묻는다.
  fail = false;
  await user.click(screen.getByRole("button", { name: /Physical state unknown/ }));
  expect(await within(panel).findByRole("button", { name: "eno1 on node-a" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "1 physical fault" })).toBeInTheDocument();
});

it("sorts the table and filters it to the nodes with problems", async () => {
  dashboard();
  const user = userEvent.setup();
  renderPage(<DashboardPage />);
  const table = await screen.findByRole("table", { name: "Allocation and use by node" });
  const order = () =>
    within(table)
      .getAllByRole("row")
      .slice(1)
      .map((r) => within(r).getAllByRole("button")[0]!.textContent);
  expect(order()).toEqual(["node-a", "node-b", "node-c"]);
  const memory = within(table).getByRole("columnheader", { name: "Memory" });
  await user.click(within(memory).getByRole("button"));
  // 수치 열은 큰 것부터
  expect(memory).toHaveAttribute("aria-sort", "descending");
  expect(order()).toEqual(["node-b", "node-a", "node-c"]);
  await user.click(within(memory).getByRole("button"));
  expect(order()).toEqual(["node-c", "node-a", "node-b"]);
  await user.click(screen.getByRole("checkbox", { name: "Problems only" }));
  expect(order()).toEqual(["node-a", "node-b"]);
});

it("opens a node in four sections with the thread grid and the network chain", async () => {
  dashboard();
  const user = userEvent.setup();
  const { container } = renderPage(<DashboardPage />);
  const table = await screen.findByRole("table", { name: "Allocation and use by node" });
  await user.click(within(table).getByRole("button", { name: "node-a" }));
  const panel = screen.getByRole("complementary", { name: "Details" });
  expect(within(panel).getByRole("heading", { name: "Node node-a" })).toBeInTheDocument();
  expect(within(table).getByRole("row", { name: /node-a/ })).toHaveAttribute(
    "aria-current",
    "true",
  );
  for (const name of ["CPU", "Memory", "Storage", "Network"]) {
    expect(within(panel).getByRole("group", { name })).toBeInTheDocument();
  }

  // CPU: NUMA 둘, 예약 0, 전용 8-15 가운데 db가 8·9에 고정
  const cpu = within(panel).getByRole("group", { name: "CPU" });
  const grid = await within(cpu).findByRole("img", {
    name: /^16 threads: 1 reserved, 8 dedicated/,
  });
  expect(within(cpu).getByText("NUMA node 0")).toBeInTheDocument();
  expect(within(cpu).getByText("NUMA node 1")).toBeInTheDocument();
  const cell = (n: number) => grid.querySelector(`[data-cpu="${n}"]`)!;
  expect(cell(0)).toHaveAttribute("data-kind", "reserved");
  await waitFor(() => expect(cell(8)).toHaveAttribute("title", "CPU 8 — dedicated (db)"));
  expect(cell(1)).toHaveAttribute("data-kind", "shared");
  expect(
    within(cpu).getByText("14 shared vCPUs on 7 threads — overcommit ×2.0"),
  ).toBeInTheDocument();
  // VM에 올리면 그 VM의 칸: 고정이면 그 스레드, 공유면 공유 칸 전체
  await user.hover(within(cpu).getByRole("button", { name: "db" }));
  expect(grid.querySelectorAll("[data-lit]")).toHaveLength(2);
  expect(cell(8)).toHaveAttribute("data-lit");
  await user.hover(within(cpu).getByRole("button", { name: "web" }));
  expect([...grid.querySelectorAll("[data-lit]")].map((c) => c.getAttribute("data-kind"))).toEqual(
    Array(7).fill("shared"),
  );

  // 메모리: NUMA별 묶임과 hugepages
  const memory = within(panel).getByRole("group", { name: "Memory" });
  expect(within(memory).getByRole("meter", { name: "NUMA node 1" })).toBeInTheDocument();
  expect(
    within(memory).getByText(/^Hugepages \(size used\/total\): 2M 128\/1024/),
  ).toBeInTheDocument();

  // 스토리지: 씬 비율, 공유 풀
  const storage = within(panel).getByRole("group", { name: "Storage" });
  expect(within(storage).getByRole("meter", { name: "Pool fast" })).toHaveAttribute(
    "aria-valuetext",
    "provisioned 150 GiB / 100 GiB · used 95 GiB · thin ×1.50",
  );
  expect(within(storage).getByText("2/3 nodes")).toBeInTheDocument();

  // 네트워크: 장애 포트 eno1에서 본딩·VLAN·브리지·네트워크까지 저하(빗금)
  const network = within(panel).getByRole("group", { name: "Network" });
  const chain = within(network).getByRole("img", { name: /^Network chain with faults:/ });
  expect(chain.getAttribute("aria-label")).toContain("eno1 (degraded)");
  expect(chain.getAttribute("aria-label")).toContain("lan (degraded)");
  const box = (id: string) => chain.querySelector(`[data-item="${id}"]`)!;
  expect(box("i:eno1")).toHaveAttribute("data-fault", "degraded");
  expect(box("i:eno1")).toHaveTextContent("× eno1");
  expect(box("i:eno2")).not.toHaveAttribute("data-fault");
  expect(box(`n:${LAN}`)).toHaveTextContent("1 VM");
  // 포트 표: LLDP·속도·실시간 표본
  const ports = within(network).getByRole("table", { name: "Ports" });
  expect(within(ports).getByRole("row", { name: /^eno1/ })).toHaveTextContent("tor-1 / Gi1/0/1");
  expect(within(ports).getByRole("row", { name: /^eno1/ })).toHaveTextContent("no carrier");
  expect(
    await within(ports).findByText(
      (_, el) => el?.textContent === "↓ 2 KiB/s ↑ 1 KiB/s3 errors/drops",
    ),
  ).toBeInTheDocument();
  expect(within(network).getByText("1 unused port not shown.")).toBeInTheDocument();
  await expectNoAxeViolations(container);
});

it("falls back to one thread group without NUMA", async () => {
  dashboard();
  const user = userEvent.setup();
  renderPage(<DashboardPage />);
  const table = await screen.findByRole("table", { name: "Allocation and use by node" });
  await user.click(within(table).getByRole("button", { name: "node-b" }));
  const cpu = within(screen.getByRole("complementary", { name: "Details" })).getByRole("group", {
    name: "CPU",
  });
  const grid = within(cpu).getByRole("img", { name: /^8 threads:/ });
  expect(grid.querySelectorAll("[data-kind=shared]")).toHaveLength(8);
  expect(within(cpu).queryByText(/^NUMA node/)).not.toBeInTheDocument();
});

it("jumps from the fault badge to the port and focuses the map on it", async () => {
  dashboard();
  const user = userEvent.setup();
  renderPage(<DashboardPage />);
  await user.click(await screen.findByRole("button", { name: "1 physical fault" }));
  const panel = screen.getByRole("complementary", { name: "Details" });
  expect(within(panel).getByRole("heading", { name: "eno1 on node-a" })).toBeInTheDocument();
  // 포트의 노드는 id다(지도 `focus`는 이름·id 둘 다 받는다).
  expect(screen.getByText("map focus: port:n1/eno1")).toBeInTheDocument();
  // 포트만: 네트워크 칸과 그 포트 강조
  expect(within(panel).queryByRole("group", { name: "CPU" })).not.toBeInTheDocument();
  const chain = within(panel).getByRole("img", { name: /^Network chain/ });
  expect(chain.querySelector('[data-item="i:eno1"]')).toHaveAttribute("data-highlight", "true");
  expect(
    within(within(panel).getByRole("table", { name: "Ports" })).getByRole("row", { name: /^eno1/ }),
  ).toHaveAttribute("aria-current", "true");
  await user.click(within(panel).getByRole("button", { name: "Whole node" }));
  expect(within(panel).getByRole("heading", { name: "Node node-a" })).toBeInTheDocument();
});

it("shows a VM with its path and a network with its nodes", async () => {
  dashboard();
  const user = userEvent.setup();
  renderPage(<DashboardPage />);
  await user.click(await screen.findByRole("button", { name: "map vm db" }));
  const panel = screen.getByRole("complementary", { name: "Details" });
  // 지금 VmPeek
  expect(within(panel).getByRole("heading", { name: "db" })).toBeInTheDocument();
  const path = within(panel).getByRole("region", { name: "Path of this VM" });
  expect(await within(path).findByText("2 vCPUs pinned to 8, 9")).toBeInTheDocument();
  expect(within(path).getByText(/NUMA 1/)).toBeInTheDocument();
  expect(within(path).getByText("db-root → fast")).toBeInTheDocument();
  expect(
    within(path).getByText(/^tenant → vmbr100 → × eno1 \(down\), eno2 \(up\) · degraded$/),
  ).toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "map rail lan" }));
  // 레일 id는 운반자(브리지 vmbr0) — 그 위의 네트워크 lan
  expect(within(panel).getByRole("heading", { name: "Rail vmbr0" })).toBeInTheDocument();
  const nodes = within(panel).getByRole("list", { name: "Nodes on lan" });
  expect(
    within(nodes)
      .getAllByRole("listitem")
      .map((li) => li.textContent),
  ).toEqual(["node-adegraded", "node-bup"]);
  await user.click(within(nodes).getByRole("button", { name: "node-b" }));
  expect(within(panel).getByRole("heading", { name: "Node node-b" })).toBeInTheDocument();
});

it("stacks health, alerts, map and table cards on a phone and opens details in a sheet", async () => {
  const media = vi.spyOn(window, "matchMedia").mockImplementation(
    (query: string) =>
      ({
        matches: query === "(max-width: 767px)",
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      }) as unknown as MediaQueryList,
  );
  try {
    dashboard();
    const user = userEvent.setup();
    renderPage(<DashboardPage />);
    const cards = await screen.findByRole("list", { name: "Allocation and use by node" });
    expect(regionNames().slice(0, 4)).toEqual([
      "Cluster health",
      "Active alerts",
      "Infrastructure map",
      "Allocation and use by node",
    ]);
    expect(screen.queryByRole("complementary", { name: "Details" })).not.toBeInTheDocument();
    await user.click(within(cards).getByRole("button", { name: "node-b" }));
    const sheet = await screen.findByRole("dialog", { name: "Details" });
    expect(within(sheet).getByRole("heading", { name: "Node node-b" })).toBeInTheDocument();
  } finally {
    media.mockRestore();
  }
});

it("offers to turn on link alerts until a link rule exists", async () => {
  dashboard();
  const sent: unknown[] = [];
  server.use(
    http.get(apiUrl("/notification-targets"), () =>
      HttpResponse.json({
        revision: 1,
        items: [
          {
            id: "019a0f3c-0000-7000-8011-000000000001",
            name: "ops",
            revision: 1,
            enabled: true,
            target: {
              kind: "webhook",
              url: "https://h/…",
              format: "json",
              header_names: [],
              has_token: false,
            },
          },
        ],
      }),
    ),
    http.post(apiUrl("/alert-rules/link-defaults"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json({ created: [], existing: ["node-link-down", "bond-degraded"] });
    }),
  );
  const user = userEvent.setup();
  const { unmount } = renderPage(<DashboardPage />);
  const card = await screen.findByRole("region", { name: "Link alerts" });
  await user.click(within(card).getByRole("button", { name: "Turn on link alerts" }));
  const dialog = await screen.findByRole("dialog", { name: "Turn on link alerts" });
  expect(
    within(dialog)
      .getAllByRole("listitem")
      .map((li) => li.textContent),
  ).toEqual([
    expect.stringMatching(/Critical · for 30 s$/),
    expect.stringMatching(/Warning · for 60 s$/),
  ]);
  await user.click(within(dialog).getByLabelText("ops"));
  await user.click(within(dialog).getByRole("button", { name: "Create 2 rules" }));
  await waitFor(() => expect(sent).toEqual([{ targets: ["ops"] }]));
  unmount();

  // 링크 규칙이 하나라도 있으면, 또는 규칙을 바꾸지 못하면 숨는다.
  dashboard({
    rules: [
      {
        id: "019a0f3c-0000-7000-8010-000000000001",
        name: "mine",
        revision: 1,
        spec: {
          condition: { kind: "bond_degraded" },
          enabled: true,
          for_secs: 0,
          severity: "warning",
          targets: ["ops"],
          repeat_secs: 14400,
          send_resolved: true,
        },
        status: {},
      },
    ],
  });
  const second = renderPage(<DashboardPage />);
  await screen.findByRole("table", { name: "Allocation and use by node" });
  await screen.findByRole("button", { name: "1 physical fault" });
  expect(screen.queryByRole("region", { name: "Link alerts" })).not.toBeInTheDocument();
  second.unmount();
  dashboard();
  renderPage(<DashboardPage />, { as: VIEWER });
  await screen.findByRole("table", { name: "Allocation and use by node" });
  expect(screen.queryByRole("region", { name: "Link alerts" })).not.toBeInTheDocument();
});

it("does not ask for the dashboard without cluster read", async () => {
  const asked: string[] = [];
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: [],
        root_contents: [],
        folders: {},
        contents: {},
        objects: [],
      }),
    ),
    http.get(apiUrl("/capacity"), () => {
      asked.push("capacity");
      return HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 });
    }),
    http.get(apiUrl("/topology/physical"), () => {
      asked.push("physical");
      return HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 });
    }),
  );
  mockLists({ vm: [vm("web", "running")] });
  renderPage(<DashboardPage />, { as: { ...ADMIN, role: "none" } });
  expect(
    await screen.findByText(
      "Nodes, capacity and cluster charts need permission to read the cluster.",
    ),
  ).toBeInTheDocument();
  expect(screen.queryByRole("region", { name: "Infrastructure map" })).not.toBeInTheDocument();
  expect(asked).toEqual([]);
});
