import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen, within } from "@testing-library/react";
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
import { ADMIN, mockLists } from "@/test/render";
import { apiUrl, server } from "@/test/server";

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
