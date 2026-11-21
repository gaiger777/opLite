import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { expectNoAxeViolations } from "@/test/axe";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { sampleGraph } from "@/test/topology";
import { TopologyPage } from "@/topology/TopologyPage";

// 보기(그림·목록)는 브라우저에 기억한다.
afterEach(() => localStorage.clear());

const LAN = "019a0f3c-0000-7000-8001-000000000009";
const WEB = "019a0f3c-0000-7000-8002-000000000009";

/** SDN을 세우기 전 클러스터(svc01과 같은 모양): 단순 네트워크 `lan`과 VM 하나. 빈 배열·`hidden`은 늘 온다. */
const SIMPLE_ONLY = {
  networks: [{ id: LAN, name: "lan", kind: "bridge", cidrs: [] }],
  routers: [],
  load_balancers: [],
  floating_ips: [],
  vms: [
    { id: WEB, name: "web", power: "running", node: "node-a", nics: [{ network: LAN, ips: [] }] },
  ],
  hidden: { networks: 0, routers: 0, load_balancers: 0, floating_ips: 0, vms: 2 },
};

function topology(body: object, seen: URL[] = []) {
  server.use(
    http.get(apiUrl("/network-topology"), ({ request }) => {
      seen.push(new URL(request.url));
      return HttpResponse.json(body);
    }),
  );
}

it("draws a cluster with only simple networks and switches to the list view", async () => {
  mockLists({});
  const seen: URL[] = [];
  topology(SIMPLE_ONLY, seen);
  const user = userEvent.setup();
  const { container } = renderPage(<TopologyPage />, { path: "/networks/topology" });
  const picture = await screen.findByRole("group", { name: "Network topology" });
  const lan = within(picture).getByRole("button", { name: "Simple network lan, bridge, 1 VM" });
  expect(lan).toHaveAttribute("tabindex", "0");
  const web = within(picture).getByRole("button", { name: "VM web, Running, —" });
  expect(web).toHaveAttribute("tabindex", "-1");
  // 살아 있는 상태도 읽는다(`live=true`)
  expect(seen[0]?.searchParams.get("live")).toBe("true");
  expect(screen.getByText("2 items you can't see are left out.")).toBeVisible();
  await expectNoAxeViolations(container, { disable: ["region"] });

  await user.click(screen.getByRole("radio", { name: "List" }));
  const list = screen.getByRole("region", { name: "Network topology as a list" });
  expect(within(list).getByRole("heading", { name: "Simple networks" })).toBeVisible();
  expect(within(list).getByRole("button", { name: "web" })).toBeVisible();
  await expectNoAxeViolations(container, { disable: ["region"] });
});

it("moves between items with the arrow keys and opens them with Enter", async () => {
  mockLists({});
  topology(sampleGraph());
  const { router } = renderPage(<TopologyPage />, { path: "/networks/topology" });
  const picture = await screen.findByRole("group", { name: "Network topology" });
  const r1 = within(picture).getByRole("button", {
    name: "Router r1, 2 interfaces, active gateway node-a",
  });
  r1.focus();
  fireEvent.keyDown(r1, { key: "ArrowDown" });
  // 아래 층(네트워크)으로 초점이 옮겨 가고 그것만 탭 차례다
  await waitFor(() =>
    expect(document.activeElement?.getAttribute("aria-label")).toMatch(/^SDN network/),
  );
  expect(document.activeElement).toHaveAttribute("tabindex", "0");
  expect(r1).toHaveAttribute("tabindex", "-1");

  fireEvent.keyDown(r1, { key: "Enter" });
  await waitFor(() => expect(router.state.location.pathname).toBe("/routers"));
  expect(router.state.location.search).toBe("?open=019a0f3c-0000-7000-800a-000000000001");
});

it("filters by network folder and hides VMs", async () => {
  mockLists({
    folder: [
      {
        id: "019a0f3c-0000-7000-8009-000000000001",
        name: "tenant-a",
        revision: 1,
        spec: { kind: "network", name: "tenant-a" },
        status: {},
      },
    ],
  });
  const seen: URL[] = [];
  topology(sampleGraph(), seen);
  const user = userEvent.setup();
  renderPage(<TopologyPage />, { path: "/networks/topology" });
  const picture = await screen.findByRole("group", { name: "Network topology" });
  expect(within(picture).getAllByRole("button", { name: /^VM / })).toHaveLength(3);
  await user.click(screen.getByLabelText("Hide VMs"));
  expect(within(picture).queryAllByRole("button", { name: /^VM / })).toHaveLength(0);
  await user.selectOptions(screen.getByLabelText("Network folder"), "/tenant-a");
  await waitFor(() =>
    expect(seen.at(-1)?.searchParams.get("folder")).toBe("019a0f3c-0000-7000-8009-000000000001"),
  );
});
