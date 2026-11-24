import { QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse, ws } from "msw";
import { createMemoryRouter, RouterProvider } from "react-router";

import { appRoutes } from "@/App";
import { createQueryClient } from "@/api/queryClient";
import type { Principal } from "@/api/types";
import { AuthProvider } from "@/auth/AuthProvider";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AREAS, LEGACY_REDIRECTS } from "@/layout/nav";
import { DensityProvider } from "@/lib/density";
import { ThemeProvider } from "@/lib/theme";
import { NODE } from "@/test/fixtures";
import { ADMIN, mockLists } from "@/test/render";
import { apiUrl, server } from "@/test/server";

// 새로 배포해 옛 조각이 없어진 것처럼: 구독 탭 화면을 받지 못한다(다른 시험은 이 화면을 열지 않는다).
vi.mock("@/settings/SubscriptionPage", () => ({
  get SubscriptionPage() {
    throw new TypeError(
      "Failed to fetch dynamically imported module: http://localhost/assets/SubscriptionPage-x1.js",
    );
  },
}));

// 감사 화면은 시험이 열어 줄 때까지 받지 못한다(받는 동안의 막대를 보려고).
const auditGate = vi.hoisted(() => {
  let open = () => {};
  const ready = new Promise<void>((resolve) => (open = resolve));
  return { ready, open: () => open() };
});
vi.mock("@/audit/AuditPage", async (importOriginal) => {
  await auditGate.ready;
  return importOriginal();
});

/** 계획 61 전 사이드바(`NAV`)의 26개 주소. 북마크가 깨지지 않아야 한다(설계 §10). */
const OLD_NAV_PATHS = [
  "/",
  "/vms",
  "/templates",
  "/images",
  "/imports",
  "/volumes",
  "/isos",
  "/storage-pools",
  "/ceph",
  "/backups",
  "/backup-jobs",
  "/networks",
  "/security-groups",
  "/nodes",
  "/ha-groups",
  "/ha-checks",
  "/placement-rules",
  "/placement",
  "/tasks",
  "/audit",
  "/users",
  "/realms",
  "/tokens",
  "/cluster",
  "/remotes",
  "/settings",
];

const SETTINGS = {
  id: "00000000-0000-0000-0000-000000000000",
  name: "cluster",
  revision: 1,
  labels: {},
  spec: { updates: { channel: "stable" } },
  status: {},
};

/** 화면들이 읽는 API를 빈 값으로 정한다. */
function mockApi(as: Principal) {
  mockLists({});
  const empty = () => HttpResponse.json({ revision: 1, items: [] });
  server.use(
    http.get(apiUrl("/auth/whoami"), () => HttpResponse.json(as)),
    ws.link(`ws://${window.location.host}/api/v1/watch`).addEventListener("connection", () => {}),
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
    http.get(apiUrl("/settings"), () => HttpResponse.json(SETTINGS)),
    ...["/users", "/realms", "/tokens", "/remotes", "/ha-checks", "/templates", "/alert-rules"].map(
      (path) => http.get(apiUrl(path), empty),
    ),
    http.get(apiUrl("/notification-targets"), empty),
    http.get(apiUrl("/audit"), () => HttpResponse.json({ items: [], next: null })),
    http.get(apiUrl("/placement"), () => HttpResponse.json({ moves: [], nodes: [] })),
    http.get(apiUrl("/join-tokens"), empty),
  );
}

function renderApp(url: string, as: Principal = ADMIN) {
  mockApi(as);
  const router = createMemoryRouter(appRoutes, { initialEntries: [url] });
  render(
    <ThemeProvider>
      <DensityProvider>
        <TooltipProvider>
          <QueryClientProvider client={createQueryClient()}>
            <AuthProvider>
              <RouterProvider router={router} />
            </AuthProvider>
          </QueryClientProvider>
        </TooltipProvider>
      </DensityProvider>
    </ThemeProvider>,
  );
  return router;
}

const where = (router: ReturnType<typeof renderApp>) => {
  const { pathname, search, hash } = router.state.location;
  return `${pathname}${search}${hash}`;
};

afterEach(() => localStorage.clear());

it("shows a thin progress bar while a lazy page loads", async () => {
  const router = renderApp("/");
  await screen.findByRole("heading", { level: 1, name: "Home" });
  void router.navigate("/audit");
  expect(await screen.findByRole("progressbar", { name: "Loading page" })).toBeInTheDocument();
  auditGate.open();
  expect(
    await screen.findByRole("heading", { level: 1, name: "Audit log" }, { timeout: 3000 }),
  ).toBeInTheDocument();
  expect(screen.queryByRole("progressbar", { name: "Loading page" })).not.toBeInTheDocument();
});

it("offers a reload inside the shell when a page chunk is gone after a deploy", async () => {
  const user = userEvent.setup();
  renderApp("/settings");
  const tabs = await screen.findByRole("navigation", { name: "Settings menu" });
  await user.click(within(tabs).getByRole("link", { name: "Subscription" }));
  expect(
    await screen.findByRole("heading", { level: 1, name: "A new version is available" }),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Reload" })).toBeInTheDocument();
  // 셸(사이드바)은 그대로다.
  expect(screen.getByRole("navigation", { name: "Main" })).toBeInTheDocument();
  expect(screen.queryByText(/Unexpected Application Error/)).not.toBeInTheDocument();
});

it("shows an empty area instead of redirecting to itself when no tab is visible", async () => {
  const infra = AREAS.find((a) => a.id === "infra")!;
  const tabs = infra.tabs;
  infra.tabs = tabs.map((tab) => ({ ...tab, admin: true }));
  try {
    const router = renderApp("/infra", { ...ADMIN, role: "viewer" });
    expect(
      await screen.findByText("Nothing in this area is available to your role."),
    ).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 1, name: "Infrastructure" })).toBeInTheDocument();
    expect(where(router)).toBe("/infra");
  } finally {
    infra.tabs = tabs;
  }
});

it.each(LEGACY_REDIRECTS)("moves $from to $to with its query and hash", async ({ from, to }) => {
  const router = renderApp(`${from}?x=1#h`);
  await vi.waitFor(() => expect(where(router)).toBe(`${to}?x=1#h`));
  // 넘기기는 기록을 바꾼다(뒤로 가기가 옛 주소로 돌아가 다시 넘기지 않게).
  expect(router.state.historyAction).toBe("REPLACE");
  expect(await screen.findByRole("heading", { level: 1 })).not.toHaveTextContent("Page not found");
});

it.each(OLD_NAV_PATHS)("still opens %s", async (path) => {
  renderApp(path);
  const heading = await screen.findByRole("heading", { level: 1 }, { timeout: 3000 });
  expect(heading).not.toHaveTextContent("Page not found");
});

it.each([
  ["/infra", "/nodes"],
  ["/protect", "/backups"],
  ["/activity", "/tasks"],
])("opens the first tab of %s without a remembered tab", async (from, to) => {
  const router = renderApp(from);
  await vi.waitFor(() => expect(where(router)).toBe(to));
});

it("opens the remembered tab of an area address", async () => {
  localStorage.setItem("opl.nav.last.infra", "/volumes?q=root");
  localStorage.setItem("opl.nav.last.activity", "/audit");
  const router = renderApp("/infra");
  await vi.waitFor(() => expect(where(router)).toBe("/volumes?q=root"));
});

it("ignores a remembered tab the user may not see", async () => {
  localStorage.setItem("opl.nav.last.activity", "/audit");
  const router = renderApp("/activity", { ...ADMIN, role: "viewer" });
  await vi.waitFor(() => expect(where(router)).toBe("/tasks"));
});

it("returns to the last tab from the sidebar and moves focus to the new page", async () => {
  const user = userEvent.setup();
  const router = renderApp("/storage-pools");
  const main = await screen.findByRole("navigation", { name: "Main" });
  const tabs = await screen.findByRole("navigation", { name: "Infrastructure menu" });
  expect(within(tabs).getByRole("link", { name: "Storage pools" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  expect(within(main).getByRole("link", { name: "Infrastructure" })).toHaveAttribute(
    "aria-current",
    "page",
  );

  await user.click(within(tabs).getByRole("link", { name: "Volumes" }));
  await vi.waitFor(() => expect(where(router)).toBe("/volumes"));
  // 경로가 바뀌면 본문 제목으로 포커스가 간다.
  await vi.waitFor(() =>
    expect(screen.getByRole("heading", { level: 1, name: "Volumes" })).toHaveFocus(),
  );

  await user.click(within(main).getByRole("link", { name: "Home" }));
  await vi.waitFor(() => expect(where(router)).toBe("/"));
  expect(within(main).getByRole("link", { name: "Infrastructure" })).toHaveAttribute(
    "href",
    "/volumes",
  );
  await user.click(within(main).getByRole("link", { name: "Infrastructure" }));
  await vi.waitFor(() => expect(where(router)).toBe("/volumes"));
});

it("opens the cluster map as a lazy view of Infrastructure › Nodes and keeps [ inside the map", async () => {
  const router = renderApp("/cluster/map");
  // 노드 하나(레이블 없음) — 그림이 노드 블록을 그린다
  server.use(http.get(apiUrl("/nodes"), () => HttpResponse.json({ revision: 1, items: [NODE] })));
  expect(
    await screen.findByRole("heading", { level: 1, name: "Map" }, { timeout: 3000 }),
  ).toBeInTheDocument();
  const views = screen.getByRole("navigation", { name: "Nodes views" });
  expect(
    within(views)
      .getAllByRole("link")
      .map((a) => a.textContent),
  ).toEqual(["Nodes", "Map", "Cluster"]);
  expect(within(views).getByRole("link", { name: "Map" })).toHaveAttribute("aria-current", "page");
  const block = await screen.findByRole("button", { name: /^Node node-a, online/ });
  // 그림 밖의 `[`는 사이드바를 접는다
  expect(localStorage.getItem("opl.nav.rail")).toBeNull();
  fireEvent.keyDown(document.body, { key: "[" });
  const rail = localStorage.getItem("opl.nav.rail");
  expect(rail).not.toBeNull();
  // 그림 안의 `[`·`]`는 그림이 받는다(사이드바 그대로)
  block.focus();
  fireEvent.keyDown(block, { key: "]" });
  fireEvent.keyDown(block, { key: "[" });
  expect(localStorage.getItem("opl.nav.rail")).toBe(rail);
  expect(where(router)).toBe("/cluster/map");
});
