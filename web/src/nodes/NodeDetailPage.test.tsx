import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { NodeDetailPage } from "@/nodes/NodeDetailPage";
import { history, NODE, NODE_FIELDS, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const at = { path: "/nodes/:name", url: "/nodes/node-a" };

it("shows the node's charts with AVG, MAX, MIN and exact percentiles", async () => {
  const ranges: (string | null)[] = [];
  mockLists({
    node: [NODE],
    vm: [
      vm("web", "running"),
      vm("far", "stopped", { spec: { ...vm("far", "stopped").spec, node: "n2" } }),
    ],
  });
  server.use(
    http.get(apiUrl("/nodes/:key/metrics/history"), ({ request, params }) => {
      expect(params.key).toBe("n1");
      ranges.push(new URL(request.url).searchParams.get("range"));
      return HttpResponse.json(history(NODE_FIELDS));
    }),
  );
  renderPage(<NodeDetailPage />, at);
  expect(await screen.findByRole("heading", { level: 1, name: "node-a" })).toBeInTheDocument();
  const cpu = await screen.findByRole("img", { name: /^CPU, last 1 hour:/ });
  expect(cpu).toHaveAccessibleName("CPU, last 1 hour: CPU average 20 %, max 40 %, min 5 %");
  const legend = screen.getByRole("table", { name: "CPU summary" });
  expect(within(legend).getByRole("columnheader", { name: "p95" })).toBeInTheDocument();
  expect(within(legend).getByRole("row", { name: /CPU/ })).toHaveTextContent(
    "CPU20 %40 %5 %20 %38 %",
  );
  expect(screen.getByRole("table", { name: "Memory summary" })).toHaveTextContent("Used");
  expect(screen.getByRole("table", { name: "Network summary" })).toHaveTextContent("Received");
  // 그 노드의 VM만
  const vms = screen.getByRole("table", { name: "Virtual machines" });
  expect(within(vms).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(within(vms).queryByText("far")).not.toBeInTheDocument();
  expect(ranges[0]).toBe("hour");
});

it("changes the range with the picker and marks approximate percentiles", async () => {
  const ranges: (string | null)[] = [];
  mockLists({ node: [NODE], vm: [] });
  server.use(
    http.get(apiUrl("/nodes/:key/metrics/history"), ({ request }) => {
      const range = new URL(request.url).searchParams.get("range");
      ranges.push(range);
      return HttpResponse.json(history(NODE_FIELDS, { approx: range !== "hour" }));
    }),
  );
  const { router } = renderPage(<NodeDetailPage />, at);
  const picker = await screen.findByRole("radiogroup", { name: "Time range" });
  expect(within(picker).getByRole("radio", { name: "1 hour" })).toBeChecked();
  await userEvent.click(within(picker).getByRole("radio", { name: "1 week" }));
  expect(await screen.findByRole("img", { name: /^CPU, last 1 week:/ })).toBeInTheDocument();
  expect(router.state.location.search).toBe("?range=week");
  expect(ranges).toContain("week");
  const legend = screen.getByRole("table", { name: "CPU summary" });
  expect(within(legend).getByRole("columnheader", { name: "p95 ≈" })).toBeInTheDocument();
  expect(
    screen.getAllByText(/Percentiles over this range are computed from averages/),
  ).not.toHaveLength(0);
  // 화살표 키로 옮긴다.
  await userEvent.keyboard("{ArrowRight}");
  expect(within(picker).getByRole("radio", { name: "1 month" })).toHaveFocus();
});

it("shows the values as a table on request", async () => {
  mockLists({ node: [NODE], vm: [] });
  server.use(
    http.get(apiUrl("/nodes/:key/metrics/history"), () => HttpResponse.json(history(NODE_FIELDS))),
  );
  renderPage(<NodeDetailPage />, at);
  await screen.findByRole("img", { name: /^CPU,/ });
  const cpu = screen.getByRole("region", { name: "CPU" });
  expect(within(cpu).queryByRole("table", { name: "CPU values" })).not.toBeInTheDocument();
  await userEvent.click(within(cpu).getByRole("button", { name: "Show as table" }));
  const table = within(cpu).getByRole("table", { name: "CPU values" });
  // 머리 줄과 점 셋. 모르는 점은 "—"
  expect(within(table).getAllByRole("row")).toHaveLength(4);
  expect(within(table).getAllByRole("row")[2]).toHaveTextContent("—");
  expect(within(cpu).getByRole("button", { name: "Hide table" })).toHaveAttribute(
    "aria-expanded",
    "true",
  );
});

it("says why the history could not be read", async () => {
  mockLists({ node: [NODE], vm: [] });
  server.use(
    http.get(apiUrl("/nodes/:key/metrics/history"), () =>
      HttpResponse.json(
        { code: "cluster_upgrading", message: 'node "node-a" does not keep metrics history yet' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<NodeDetailPage />, at);
  expect(
    await within(await screen.findByRole("region", { name: "CPU" })).findByText(
      'The cluster is being upgraded: node "node-a" does not keep metrics history yet',
    ),
  ).toBeInTheDocument();
});

it("does not ask a witness for history", async () => {
  // 기록을 물으면 처리하지 않은 요청으로 실패한다.
  mockLists({ node: [{ ...NODE, spec: { ...NODE.spec, witness: true } }], vm: [] });
  renderPage(<NodeDetailPage />, at);
  expect(await screen.findByText("Witness nodes keep no metrics history.")).toBeInTheDocument();
  expect(screen.queryByRole("radiogroup")).not.toBeInTheDocument();
});

it("says when the node does not exist", async () => {
  mockLists({ node: [NODE] });
  renderPage(<NodeDetailPage />, { path: "/nodes/:name", url: "/nodes/gone" });
  expect(await screen.findByRole("heading", { name: "Node gone not found" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Back to nodes" })).toHaveAttribute("href", "/nodes");
});

it("opens the node dialogs from the header menu", async () => {
  mockLists({ node: [NODE], vm: [] });
  server.use(
    http.get(apiUrl("/nodes/:key/metrics/history"), () => HttpResponse.json(history(NODE_FIELDS))),
  );
  renderPage(<NodeDetailPage />, at);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: /interfaces/i }));
  expect(
    await screen.findByRole("dialog", { name: "Network interfaces of node-a" }),
  ).toBeInTheDocument();
});
