import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { DashboardPage } from "@/pages/DashboardPage";
import { NODE } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const GIB = 1024 ** 3;

const DIGEST = {
  tier: "raw5s",
  step_ms: 30_000,
  start: 1_800_000_000_000,
  columns: {
    cpu: { avg: [10, 30, null] },
    mem_used: { avg: [4096, 4096, 4096] },
    mem_total: { avg: [16384, 16384, 16384] },
  },
  nodes: [
    {
      node: "n1",
      name: "node-a",
      cpus: 8,
      avg: {
        cpu: 25.5,
        mem_used: 4096,
        mem_total: 16384,
        disk_read: 1024,
        disk_write: 1024,
        net_rx: null,
        net_tx: null,
      },
      cpu: [20, 30, null],
    },
  ],
  node_count: 30,
  top: [{ vm: "v1", name: "web", node: "node-a", cpu: 87.25, mem_rss: 2048 }],
  pools: [
    { pool: "p1", name: "fast", shared: false, used_bytes: 90 * GIB, total_bytes: 100 * GIB },
    { pool: "p2", name: "bulk", shared: true, used_bytes: 10 * GIB, total_bytes: 100 * GIB },
  ],
  unreachable: ["node-c"],
  unsupported: [],
  skewed: ["node-d"],
};

const ALERT = {
  rule: "r1",
  rule_name: "cpu-hot",
  severity: "critical",
  condition: "node_metric",
  subject: { kind: "node", id: "n1", name: "node-a", node: "node-a" },
  state: "firing",
  since: 1_800_000_000_000,
  value: 97.123,
  threshold: 90,
};

function cluster() {
  return http.get(apiUrl("/cluster"), () =>
    HttpResponse.json({ name: "lab", ca_fingerprint: "x", leader: "n1", node: "n1", members: [] }),
  );
}

it("shows cluster charts, node load, top VMs, pools and alerts", async () => {
  const ranges: (string | null)[] = [];
  mockLists({ node: [NODE], vm: [] });
  server.use(
    cluster(),
    http.get(apiUrl("/metrics/cluster"), ({ request }) => {
      ranges.push(new URL(request.url).searchParams.get("range"));
      return HttpResponse.json(DIGEST);
    }),
    http.get(apiUrl("/alerts"), () =>
      HttpResponse.json({
        alerts: [
          ALERT,
          {
            ...ALERT,
            rule_name: "vm-down",
            severity: "warning",
            condition: "vm_failed",
            subject: { kind: "vm", id: "v1", name: "web", node: "node-a" },
            value: undefined,
            threshold: undefined,
            clear_since: 1_800_000_060_000,
          },
        ],
        unreachable: [],
        unsupported: [],
      }),
    ),
  );
  renderPage(<DashboardPage />);

  // 활성 경보: 심각도 칩, 대상, 경보 화면 링크
  const alerts = await screen.findByRole("region", { name: "Active alerts" });
  const list = await within(alerts).findByRole("list", { name: "Active alerts" });
  const items = within(list).getAllByRole("listitem");
  expect(items[0]).toHaveTextContent("Critical");
  expect(items[0]).toHaveTextContent("cpu-hot");
  expect(items[1]).toHaveTextContent("vm/web@node-a");
  expect(items[1]).toHaveTextContent("Clearing");
  expect(within(alerts).getByRole("link", { name: "View all alerts" })).toHaveAttribute(
    "href",
    "/alerts",
  );

  // 풀: 85 %를 넘으면 경고
  const pools = screen.getByRole("region", { name: "Storage pool usage" });
  const fast = await within(pools).findByRole("meter", { name: "fast" });
  expect(fast).toHaveAttribute("aria-valuenow", "90");
  expect(fast).toHaveAttribute("aria-valuetext", "90 %, nearly full");
  expect(within(pools).getByRole("meter", { name: "bulk" })).toHaveAttribute(
    "aria-valuetext",
    "10 %",
  );

  // 클러스터 차트: AVG 열에서 셈한 요약
  const load = screen.getByRole("region", { name: "Cluster load" });
  expect(
    within(load).getByRole("img", {
      name: "CPU, last 1 hour: CPU average 20 %, max 30 %, min 10 %",
    }),
  ).toBeInTheDocument();
  expect(
    within(load).getByText("Not included: node-c (did not answer), node-d (clock is off)."),
  ).toBeInTheDocument();

  // 노드 줄: 상세 링크, 20개만 보였다는 안내
  const nodes = screen.getByRole("table", { name: "Node load" });
  expect(within(nodes).getByRole("link", { name: "node-a" })).toHaveAttribute(
    "href",
    "/nodes/node-a",
  );
  expect(within(nodes).getByRole("row", { name: /node-a/ })).toHaveTextContent(
    "25.5 %8 cores4 GiB / 16 GiB2 KiB/s—",
  );
  expect(screen.getByText("Showing the 1 busiest of 30 nodes.")).toBeInTheDocument();

  // 상위 VM: VM 지표 탭 링크
  const top = screen.getByRole("table", { name: "Top VMs by CPU" });
  expect(within(top).getByRole("link", { name: "web" })).toHaveAttribute(
    "href",
    "/vms/web?tab=metrics",
  );
  expect(within(top).getByRole("row", { name: /web/ })).toHaveTextContent("87.3 %");

  // 범위를 바꾸면 위젯들이 같은 범위로 다시 읽는다.
  await userEvent.click(within(load).getByRole("radio", { name: "1 month" }));
  expect(await within(load).findByRole("img", { name: /^CPU, last 1 month:/ })).toBeInTheDocument();
  expect(ranges).toEqual(["hour", "month"]);
});

it("does not raise alerts as toasts", async () => {
  mockLists({ node: [NODE], vm: [] });
  server.use(
    cluster(),
    http.get(apiUrl("/alerts"), () =>
      HttpResponse.json({ alerts: [ALERT], unreachable: [], unsupported: [] }),
    ),
  );
  renderPage(<DashboardPage />);
  const alerts = await screen.findByRole("region", { name: "Active alerts" });
  expect(await within(alerts).findByText("cpu-hot")).toBeInTheDocument();
  // sonner 알림이 없다(종 서랍이 맡는다).
  expect(document.querySelector("[data-sonner-toast]")).toBeNull();
});

it("says when there are no alerts, pools or history", async () => {
  mockLists({ node: [NODE], vm: [] });
  server.use(cluster());
  renderPage(<DashboardPage />);
  expect(await screen.findByText("No active alerts.")).toBeInTheDocument();
  expect(await screen.findByText("No storage pools.")).toBeInTheDocument();
  expect(screen.getByText("No node has metrics history yet.")).toBeInTheDocument();
  expect(screen.getByText("No VM history in this range.")).toBeInTheDocument();
});
