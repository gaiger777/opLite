import { fireEvent, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import i18n from "@/i18n";
import { AppLayout } from "@/layout/AppLayout";
import { expectNoAxeViolations } from "@/test/axe";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const original = window.matchMedia;

beforeEach(() =>
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
  ),
);
afterEach(() => localStorage.clear());

/** 셸을 `url`에서 그린다(본문은 비어 있다). */
function shell(url = "/", as = ADMIN) {
  return renderPage(<AppLayout />, { path: "*", url, as });
}

const links = (nav: HTMLElement) =>
  within(nav)
    .getAllByRole("link")
    .map((a) => [a.textContent, a.getAttribute("href")]);

it("has only the six top-level areas in the sidebar, settings last", async () => {
  const { container } = shell();
  const nav = await screen.findByRole("navigation", { name: "Main" });
  expect(links(nav)).toEqual([
    ["Home", "/"],
    ["Virtual machines", "/vms"],
    ["Infrastructure", "/nodes"],
    ["Protection", "/backups"],
    ["Activity", "/tasks"],
    ["Settings", "/settings"],
  ]);
  // 둘째 단계가 없다.
  expect(nav.querySelector("ul ul")).toBeNull();
  expect(within(nav).getByRole("link", { name: "Home" })).toHaveAttribute("aria-current", "page");
  // 영역 탭은 홈에 없다.
  expect(container.querySelector("nav[aria-label$=' menu']")).toBeNull();
});

it("shows the tabs of the current area as links", async () => {
  shell("/storage-pools");
  const tabs = await screen.findByRole("navigation", { name: "Infrastructure menu" });
  expect(links(tabs)).toEqual([
    ["Nodes", "/nodes"],
    ["Storage pools", "/storage-pools"],
    ["Volumes", "/volumes"],
    ["Ceph", "/ceph"],
    ["Networks", "/networks"],
    ["Security groups", "/security-groups"],
  ]);
  expect(within(tabs).getByRole("link", { name: "Storage pools" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  expect(
    within(screen.getByRole("navigation", { name: "Main" })).getByRole("link", {
      name: "Infrastructure",
    }),
  ).toHaveAttribute("aria-current", "page");
  const crumbs = screen.getByRole("navigation", { name: "Breadcrumb" });
  expect(within(crumbs).getByRole("link", { name: "Infrastructure" })).toHaveAttribute(
    "href",
    "/nodes",
  );
  expect(within(crumbs).getByText("Storage pools")).toHaveAttribute("aria-current", "page");
  // 풀 탭은 목록 | 폴더 보기 전환이 있다(계획 62).
  expect(links(screen.getByRole("navigation", { name: "Storage pools views" }))).toEqual([
    ["Storage pools", "/storage-pools"],
    ["Storage folders", "/storage-folders"],
  ]);
});

it("switches views inside a tab", async () => {
  shell("/cluster");
  const views = await screen.findByRole("navigation", { name: "Nodes views" });
  expect(links(views)).toEqual([
    ["Nodes", "/nodes"],
    ["Cluster", "/cluster"],
  ]);
  expect(within(views).getByRole("link", { name: "Cluster" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  const tabs = screen.getByRole("navigation", { name: "Infrastructure menu" });
  expect(within(tabs).getByRole("link", { name: "Nodes" })).toHaveAttribute("aria-current", "page");
});

it("keeps the tab of a detail page and names the resource", async () => {
  shell("/vms/web");
  const tabs = await screen.findByRole("navigation", { name: "Virtual machines menu" });
  expect(within(tabs).getByRole("link", { name: "VMs" })).toHaveAttribute("aria-current", "page");
  const crumbs = screen.getByRole("navigation", { name: "Breadcrumb" });
  expect(within(crumbs).getByRole("link", { name: "VMs" })).toHaveAttribute("href", "/vms");
  expect(within(crumbs).getByText("web")).toHaveAttribute("aria-current", "page");
});

it("shows the console without area tabs", async () => {
  shell("/vms/web/console");
  await screen.findByRole("navigation", { name: "Breadcrumb" });
  expect(
    screen.queryByRole("navigation", { name: "Virtual machines menu" }),
  ).not.toBeInTheDocument();
});

it("hides account and admin settings tabs from operators", async () => {
  shell("/settings/tokens", { ...ADMIN, role: "operator" });
  const tabs = await screen.findByRole("navigation", { name: "Settings menu" });
  expect(links(tabs)).toEqual([
    ["General", "/settings"],
    ["API tokens", "/settings/tokens"],
    ["Two-step verification", "/settings/account"],
    ["Subscription", "/settings/subscription"],
  ]);
});

it("shows every settings tab to admins", async () => {
  shell("/settings");
  const tabs = await screen.findByRole("navigation", { name: "Settings menu" });
  expect(links(tabs).map(([name]) => name)).toEqual([
    "General",
    "Users",
    "Realms",
    "Permissions",
    "Roles",
    "Groups",
    "Security",
    "API tokens",
    "Two-step verification",
    "Remote clusters",
    "Notification targets",
    "Subscription",
  ]);
});

it("hides the audit log from operators", async () => {
  shell("/tasks", { ...ADMIN, role: "operator" });
  const tabs = await screen.findByRole("navigation", { name: "Activity menu" });
  expect(links(tabs)).toEqual([
    ["Tasks", "/tasks"],
    ["Alerts", "/alerts"],
  ]);
});

it("links an area to the tab seen last", async () => {
  localStorage.setItem("opl.nav.last.protect", "/ha-groups");
  shell("/volumes");
  const nav = await screen.findByRole("navigation", { name: "Main" });
  expect(within(nav).getByRole("link", { name: "Protection" })).toHaveAttribute(
    "href",
    "/ha-groups",
  );
  await vi.waitFor(() =>
    expect(localStorage.getItem(`opl.${ADMIN.user}.nav.last.infra`)).toBe("/volumes"),
  );
});

it("collapses the sidebar to an icon rail and remembers it", async () => {
  const user = userEvent.setup();
  const wide = vi
    .spyOn(window, "matchMedia")
    .mockImplementation((query) => ({ ...original(query), matches: query.includes("min-width") }));
  shell();
  // 넓은 화면은 펼친 채 시작한다.
  await user.click(await screen.findByRole("button", { name: "Collapse sidebar" }));
  expect(localStorage.getItem("opl.nav.rail")).toBe("1");
  // 접어도 링크 이름은 그대로 읽히고, 포커스하면 툴팁으로 보인다.
  const nav = screen.getByRole("navigation", { name: "Main" });
  const home = within(nav).getByRole("link", { name: "Home" });
  home.focus();
  expect(await screen.findByRole("tooltip")).toHaveTextContent("Home");
  await user.click(screen.getByRole("button", { name: "Expand sidebar" }));
  expect(localStorage.getItem("opl.nav.rail")).toBe("0");
  wide.mockRestore();
});

it("starts as a rail on narrow screens", async () => {
  shell();
  expect(await screen.findByRole("button", { name: "Expand sidebar" })).toBeInTheDocument();
});

it("opens the areas in a sheet from the menu button", async () => {
  const user = userEvent.setup();
  shell("/vms");
  await user.click(await screen.findByRole("button", { name: "Open menu" }));
  const sheet = await screen.findByRole("dialog", { name: "Orbivisor" });
  const nav = within(sheet).getByRole("navigation", { name: "Main" });
  expect(within(nav).getAllByRole("link")).toHaveLength(6);
  await user.click(within(nav).getByRole("link", { name: "Activity" }));
  await vi.waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
});

it("switches density, language and theme from the account menu", async () => {
  const user = userEvent.setup();
  const wide = vi
    .spyOn(window, "matchMedia")
    .mockImplementation((query) => ({ ...original(query), matches: query.includes("min-width") }));
  shell();
  await user.click(await screen.findByRole("button", { name: "Account (admin)" }));
  const group = screen.getByRole("group", { name: "Density" });
  expect(within(group).getByRole("menuitemradio", { name: "Comfortable" })).toBeChecked();
  await user.click(within(group).getByRole("menuitemradio", { name: "Compact" }));
  expect(document.documentElement.dataset.density).toBe("compact");
  expect(localStorage.getItem("opl.density")).toBe("compact");
  wide.mockRestore();

  await user.click(screen.getByRole("button", { name: "Account (admin)" }));
  await user.click(await screen.findByRole("menuitem", { name: "Theme" }));
  // 포인터를 옮기면 하위 메뉴가 닫히므로 항목만 누른다.
  fireEvent.click(await screen.findByRole("menuitemradio", { name: "Dark" }));
  await vi.waitFor(() => expect(document.documentElement).toHaveClass("dark"));
  expect(localStorage.getItem("opl.theme")).toBe("dark");

  await user.click(screen.getByRole("button", { name: "Account (admin)" }));
  await user.click(await screen.findByRole("menuitem", { name: "Language" }));
  fireEvent.click(await screen.findByRole("menuitemradio", { name: "한국어" }));
  expect(i18n.language).toBe("ko");
  // 머리줄에는 언어·테마 단추가 따로 없다.
  expect(screen.queryByRole("button", { name: /^(언어|테마|Language|Theme)$/ })).toBeNull();
  document.documentElement.classList.remove("dark");
  document.documentElement.removeAttribute("data-density");
});

it("has a skip link to the content", async () => {
  shell();
  const skip = await screen.findByRole("link", { name: "Skip to content" });
  expect(skip).toHaveAttribute("href", "#main");
  expect(screen.getByRole("main")).toHaveAttribute("id", "main");
});

it("has no accessibility violations in the shell", async () => {
  const { container } = shell("/storage-pools");
  await screen.findByRole("navigation", { name: "Infrastructure menu" });
  await expectNoAxeViolations(container);
});
