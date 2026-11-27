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
import { LEGACY_REDIRECTS } from "@/layout/nav";
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
  await user.click(await screen.findByRole("button", { name: "Administration menu" }));
  await user.click(await screen.findByRole("menuitem", { name: "Subscription" }));
  expect(
    await screen.findByRole("heading", { level: 1, name: "A new version is available" }),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Reload" })).toBeInTheDocument();
  // 셸(사이드바)은 그대로다.
  expect(screen.getByRole("navigation", { name: "Main" })).toBeInTheDocument();
  expect(screen.queryByText(/Unexpected Application Error/)).not.toBeInTheDocument();
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

// VMware 이전·원본·내보내기(계획 72): 늦게 받는 화면과 문구로 열린다. 이전·원본은 VMware 이전 항목의 화면 안 탭 둘이다.
it.each([
  ["/vmware-migrations", "VMware migrations"],
  ["/vmware-migrations/-new", "New VMware migration"],
  ["/vmware-sources", "Connected vCenter & ESXi"],
  ["/vm-exports", "Exports"],
])("opens %s lazily", async (path, title) => {
  server.use(
    ...["/vmware-migrations", "/vmware-sources", "/vm-exports"].map((p) =>
      http.get(apiUrl(p), () => HttpResponse.json({ revision: 1, items: [] })),
    ),
  );
  renderApp(path);
  expect(
    await screen.findByRole("heading", { level: 1, name: title }, { timeout: 3000 }),
  ).toBeInTheDocument();
  // 상세(새 계획)와 내보내기에는 화면 안 탭이 없다
  const tabs = screen.queryByRole("navigation", { name: "VMware migration pages" });
  if (path.endsWith("-new") || path === "/vm-exports") {
    expect(tabs).not.toBeInTheDocument();
    return;
  }
  expect(
    within(tabs!)
      .getAllByRole("link")
      .map((l) => l.textContent),
  ).toEqual(["VMware migrations", "VMware sources"]);
});

// 설정 기록·선언 묶음(계획 73)은 기록 묶음의 항목, 인증서는 관리 › 보안·인증서의 화면 안 탭으로 늦게 받는 화면과 문구로 열린다.
it.each([
  ["/history", "Config history", null],
  ["/apply-sets", "Apply sets", null],
  ["/settings/certificates", "Certificates", ["Security", "Certificates"]],
] as const)("opens %s lazily", async (path, title, tabs) => {
  server.use(
    http.get(apiUrl("/history"), () => HttpResponse.json({ items: [] })),
    ...["/apply-sets", "/acme-accounts", "/acme-dns", "/certificates"].map((p) =>
      http.get(apiUrl(p), () => HttpResponse.json({ revision: 1, items: [] })),
    ),
  );
  renderApp(path);
  expect(
    await screen.findByRole("heading", { level: 1, name: title }, { timeout: 3000 }),
  ).toBeInTheDocument();
  const nav = screen.queryByRole("navigation", { name: / pages$/ });
  if (!tabs) {
    expect(nav).not.toBeInTheDocument();
    return;
  }
  expect(
    within(nav!)
      .getAllByRole("link")
      .map((l) => l.textContent),
  ).toEqual(tabs);
});

// DR(계획 76)은 백업·복구 › DR(재해 복구)이다.
it("opens /dr-plans under Backup & recovery", async () => {
  server.use(http.get(apiUrl("/dr-plans"), () => HttpResponse.json({ revision: 1, items: [] })));
  renderApp("/dr-plans");
  expect(
    await screen.findByRole("heading", { level: 1, name: "DR" }, { timeout: 3000 }),
  ).toBeInTheDocument();
  const crumbs = screen.getByRole("navigation", { name: "Breadcrumb" });
  expect(within(crumbs).getByText("Backup & recovery")).toBeInTheDocument();
  expect(within(crumbs).getByText("DR (disaster recovery)")).toHaveAttribute(
    "aria-current",
    "page",
  );
  expect(screen.getByRole("button", { name: "Backup & recovery menu" })).toHaveAttribute(
    "data-current",
  );
});

it("merges remote clusters into one item with the manage tab for admins", async () => {
  renderApp("/remotes/-manage");
  expect(
    await screen.findByRole("heading", { level: 1, name: "Remote clusters" }, { timeout: 3000 }),
  ).toBeInTheDocument();
  const tabs = screen.getByRole("navigation", { name: "Remote clusters & replication pages" });
  expect(
    within(tabs)
      .getAllByRole("link")
      .map((l) => [l.textContent, l.getAttribute("href")]),
  ).toEqual([
    ["Remote replication", "/remotes"],
    ["Remote clusters", "/remotes/-manage"],
  ]);
});

it("goes to a group with g and a key, and focuses the new page", async () => {
  const router = renderApp("/");
  await screen.findByRole("heading", { level: 1, name: "Home" });
  fireEvent.keyDown(document.body, { key: "g" });
  fireEvent.keyDown(document.body, { key: "d" });
  await vi.waitFor(() => expect(where(router)).toBe("/storage-pools"));
  await vi.waitFor(() =>
    expect(screen.getByRole("heading", { level: 1, name: "Storage pools" })).toHaveFocus(),
  );
  fireEvent.keyDown(document.body, { key: "g" });
  fireEvent.keyDown(document.body, { key: "a" });
  await vi.waitFor(() => expect(where(router)).toBe("/tasks"));
});

it("opens an item from a rail flyout", async () => {
  const user = userEvent.setup();
  const router = renderApp("/storage-pools");
  await user.click(await screen.findByRole("button", { name: "Storage menu" }));
  await user.click(await screen.findByRole("menuitem", { name: "Disks (volumes)" }));
  await vi.waitFor(() => expect(where(router)).toBe("/volumes"));
});

it("opens the VM builder as a lazy view of the create page with its strings", async () => {
  renderApp("/vms/-new/builder");
  expect(
    await screen.findByRole(
      "button",
      { name: "Start with a medium Linux server" },
      { timeout: 3000 },
    ),
  ).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Builder" })).toHaveAttribute("aria-current", "page");
});

it("opens the cluster map lazily under Overview and keeps [ inside the map", async () => {
  const router = renderApp("/cluster/map");
  // 노드 하나(레이블 없음) — 그림이 노드 블록을 그린다
  server.use(http.get(apiUrl("/nodes"), () => HttpResponse.json({ revision: 1, items: [NODE] })));
  expect(
    await screen.findByRole("heading", { level: 1, name: "Map" }, { timeout: 3000 }),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Overview menu" })).toHaveAttribute("data-current");
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
