import { fireEvent, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import i18n from "@/i18n";
import type { AlertView } from "@/alerts/alerts";
import type { Task } from "@/api/types";
import { AppLayout } from "@/layout/AppLayout";
import { ViewToggle } from "@/layout/PageTabs";
import { expectNoAxeViolations } from "@/test/axe";
import { task } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const original = window.matchMedia;

beforeEach(() =>
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
  ),
);
afterEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});

/** 셸을 `url`에서 그린다(본문은 비어 있다). */
function shell(url = "/", as = ADMIN) {
  return renderPage(<AppLayout />, { path: "*", url, as });
}

const links = (nav: HTMLElement) =>
  within(nav)
    .getAllByRole("link")
    .map((a) => [a.textContent, a.getAttribute("href")]);

/** 1280px 이상(펼친 2단 사이드바) */
const wideScreen = () =>
  vi
    .spyOn(window, "matchMedia")
    .mockImplementation((query) => ({ ...original(query), matches: query.includes("min-width") }));

const groupButtons = (nav: HTMLElement) =>
  within(nav)
    .getAllByRole("button")
    .map((b) => [b.textContent, b.getAttribute("aria-expanded")]);

const group = (nav: HTMLElement, name: string) =>
  within(nav).getByRole("button", { name, expanded: true }).nextElementSibling as HTMLElement;

it("has nine groups plus administration, all open on a first sign-in", async () => {
  const wide = wideScreen();
  const { container } = shell();
  const nav = await screen.findByRole("navigation", { name: "Main" });
  // 기억이 없으면(처음 로그인) 모든 묶음을 펼쳐 무엇이 있는지 보인다(설계 "실패해도 기본 펼침").
  expect(groupButtons(nav)).toEqual([
    ["Overview", "true"],
    ["Virtual machines", "true"],
    ["Images & migration", "true"],
    ["Storage", "true"],
    ["Networking", "true"],
    ["Cluster & availability", "true"],
    ["Backup & recovery", "true"],
    ["Records", "true"],
    ["Administration", "true"],
  ]);
  expect(links(group(nav, "Overview"))).toEqual([
    ["Dashboard", "/"],
    ["Infrastructure map", "/cluster/map"],
    ["Alerts", "/alerts"],
  ]);
  expect(within(nav).getByRole("link", { name: "Dashboard" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  // 영역 탭·보기 전환이 없다.
  expect(container.querySelector("nav[aria-label$=' menu'], nav[aria-label$=' views']")).toBeNull();
  // 찾기·명령은 사이드바 맨 위에 있다.
  expect(screen.getAllByRole("button", { name: "Search or run a command" })[0]).toHaveTextContent(
    "Find or run a command",
  );
  wide.mockRestore();
});

it("lists every group's items for admins", async () => {
  const wide = wideScreen();
  localStorage.setItem(
    `opl.${ADMIN.user}.nav.open`,
    "overview,vms,images,storage,network,cluster,backup,records,admin",
  );
  shell();
  const nav = await screen.findByRole("navigation", { name: "Main" });
  const names = (name: string) => links(group(nav, name)).map(([label]) => label);
  expect(names("Virtual machines")).toEqual(["All VMs", "Templates", "Create VM"]);
  expect(names("Images & migration")).toEqual([
    "OS images",
    "ISO files",
    "Imports (OVA, ESXi)",
    "VMware migration (copy while running)",
    "Exports",
  ]);
  expect(names("Storage")).toEqual(["Storage pools", "Disks (volumes)", "Ceph"]);
  expect(names("Networking")).toEqual([
    "Networks",
    "Firewall (security groups)",
    "Routers & floating IPs",
    "Load balancers",
    "SDN (virtual networks)",
    "Topology diagram",
  ]);
  expect(names("Cluster & availability")).toEqual([
    "Nodes",
    "Cluster (members, CA)",
    "Automatic recovery (HA)",
    "Failure detection (HA checks)",
    "Placement rules & balancing",
  ]);
  expect(names("Backup & recovery")).toEqual([
    "Backups",
    "Backup schedules",
    "Backup stores",
    "Remote clusters & replication",
    "DR (disaster recovery)",
  ]);
  expect(names("Records")).toEqual([
    "Task history",
    "Change history",
    "Managed as code (YAML)",
    "Audit log",
  ]);
  expect(names("Administration")).toEqual([
    "General & updates",
    "Users & groups",
    "Roles & permissions",
    "Sign-in methods (realms)",
    "Security & certificates",
    "Where to send alerts",
    "Subscription",
  ]);
  // 내 토큰·2단계 인증은 사이드바에 없다(사용자 메뉴).
  expect(within(nav).queryByRole("link", { name: /API tokens|two-step/i })).toBeNull();
  wide.mockRestore();
});

it("remembers the groups a user closed and opens the group of a page it goes to", async () => {
  const user = userEvent.setup();
  const wide = wideScreen();
  const first = shell("/volumes");
  const nav = await screen.findByRole("navigation", { name: "Main" });
  await user.click(within(nav).getByRole("button", { name: "Backup & recovery" }));
  await user.click(within(nav).getByRole("button", { name: "Storage" }));
  expect(within(nav).getByRole("button", { name: "Storage" })).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  expect(localStorage.getItem(`opl.${ADMIN.user}.nav.open`)).toBe(
    "overview,vms,images,network,cluster,records,admin",
  );
  first.unmount();
  // 다시 열면 접은 묶음은 접힌 채이고, 지금 화면의 묶음(백업·복구)만은 펼친다.
  shell("/backups");
  const again = await screen.findByRole("navigation", { name: "Main" });
  expect(
    groupButtons(again)
      .filter(([, expanded]) => expanded === "false")
      .map(([name]) => name),
  ).toEqual(["Storage"]);
  wide.mockRestore();
});

it("opens the group of a page in another group and marks its item", async () => {
  const wide = wideScreen();
  shell("/floating-ips");
  const nav = await screen.findByRole("navigation", { name: "Main" });
  const item = within(group(nav, "Networking")).getByRole("link", {
    name: "Routers & floating IPs",
  });
  expect(item).toHaveAttribute("aria-current", "page");
  expect(item).toHaveAttribute("href", "/routers");
  // 화면 안 탭(라우터 | 유동 IP)
  const tabs = screen.getByRole("navigation", { name: "Routers & floating IPs pages" });
  expect(links(tabs)).toEqual([
    ["Routers", "/routers"],
    ["Floating IPs", "/floating-ips"],
  ]);
  expect(within(tabs).getByRole("link", { name: "Floating IPs" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  // 이동 경로: 묶음(링크 아님) › 항목 › 탭
  const crumbs = screen.getByRole("navigation", { name: "Breadcrumb" });
  expect(within(crumbs).queryByRole("link", { name: "Networking" })).toBeNull();
  expect(within(crumbs).getByText("Networking")).toBeInTheDocument();
  expect(within(crumbs).getByRole("link", { name: "Routers & floating IPs" })).toHaveAttribute(
    "href",
    "/routers",
  );
  expect(within(crumbs).getByText("Floating IPs")).toHaveAttribute("aria-current", "page");
  wide.mockRestore();
});

it("keeps the item of a detail page and names the resource", async () => {
  shell("/vms/web");
  const crumbs = await screen.findByRole("navigation", { name: "Breadcrumb" });
  expect(within(crumbs).getByRole("link", { name: "All VMs" })).toHaveAttribute("href", "/vms");
  expect(within(crumbs).getByText("web")).toHaveAttribute("aria-current", "page");
  expect(screen.queryByRole("navigation", { name: / pages$/ })).not.toBeInTheDocument();
  // 휴대폰은 지금 화면 이름 하나만 보이므로 위 단계(목록)로 가는 단추를 둔다(넓은 화면에서는 숨김).
  const back = within(crumbs).getByRole("link", { name: "Back to All VMs" });
  expect(back).toHaveAttribute("href", "/vms");
  expect(back).toHaveClass("md:hidden");
});

it("puts the create page and the builder under Create VM", async () => {
  const wide = wideScreen();
  shell("/vms/-new/builder");
  const nav = await screen.findByRole("navigation", { name: "Main" });
  expect(within(nav).getByRole("link", { name: "Create VM" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  expect(within(nav).getByRole("link", { name: "All VMs" })).not.toHaveAttribute("aria-current");
  wide.mockRestore();
});

it("shows the console without page tabs", async () => {
  shell("/vms/web/console");
  await screen.findByRole("navigation", { name: "Breadcrumb" });
  expect(screen.queryByRole("navigation", { name: / pages$/ })).not.toBeInTheDocument();
});

it("shows operators only the administration items they can read", async () => {
  const wide = wideScreen();
  shell("/settings", { ...ADMIN, role: "operator" });
  const nav = await screen.findByRole("navigation", { name: "Main" });
  expect(links(group(nav, "Administration"))).toEqual([
    ["General & updates", "/settings"],
    // 보안·인증서는 인증서 화면으로 간다(2단계 인증 정책은 admin).
    ["Security & certificates", "/settings/certificates"],
    ["Subscription", "/settings/subscription"],
  ]);
  wide.mockRestore();
});

it("hides the audit log from operators but keeps the rest of the records", async () => {
  const wide = wideScreen();
  shell("/tasks", { ...ADMIN, role: "operator" });
  const nav = await screen.findByRole("navigation", { name: "Main" });
  expect(links(group(nav, "Records")).map(([name]) => name)).not.toContain("Audit log");
  expect(links(group(nav, "Records"))[0]).toEqual(["Task history", "/tasks"]);
  wide.mockRestore();
});

it("does not carry the task drawer query into menu links (77-1 review)", async () => {
  const wide = wideScreen();
  shell("/tasks?task=abc");
  // 서랍(모달)이 열려 있어 사이드바는 접근성 트리에서 숨는다 — 숨은 것까지 본다.
  const nav = await screen.findByRole("navigation", { name: "Main", hidden: true });
  const hrefs = within(nav)
    .getAllByRole("link", { hidden: true })
    .map((a) => a.getAttribute("href"));
  expect(hrefs).toContain("/tasks");
  for (const href of hrefs) {
    expect(href).not.toContain("task=");
  }
  // 기억하는 것은 펼친 묶음뿐이다(주소·쿼리를 기억하지 않는다).
  expect(Object.keys(localStorage).filter((key) => key.includes("nav.last"))).toEqual([]);
  wide.mockRestore();
});

it("counts ringing alerts and recent failed tasks beside their items", async () => {
  const wide = wideScreen();
  const ringing: AlertView = {
    rule: "r",
    rule_name: "cpu",
    condition: "c",
    severity: "critical",
    since: 0,
    state: "firing",
    subject: { kind: "node", id: "n", name: "n1" },
  };
  const failed = (id: string, ago: number): Task => {
    const t = task(id, "vm-start", "x");
    return { ...t, status: { state: "failed", error: "boom", finished_at: Date.now() - ago } };
  };
  server.use(
    http.get(apiUrl("/alerts"), () =>
      HttpResponse.json({ alerts: [ringing], unreachable: [], unsupported: [] }),
    ),
  );
  mockLists({ task: [failed("a", 60_000), failed("b", 2 * 86_400_000)] });
  localStorage.setItem(`opl.${ADMIN.user}.nav.open`, "overview,records");
  shell();
  const nav = await screen.findByRole("navigation", { name: "Main" });
  expect(
    await within(nav).findByRole("link", { name: /^Alerts\W+1 alerts ringing/ }),
  ).toBeInTheDocument();
  // 하루가 지난 실패는 세지 않는다.
  expect(
    await within(nav).findByRole("link", {
      name: /^Task history\W+1 tasks failed in the last 24 hours/,
    }),
  ).toBeInTheDocument();
  wide.mockRestore();
});

it("opens a group's items from the rail with the keyboard and returns focus", async () => {
  const user = userEvent.setup();
  shell("/volumes");
  const trigger = await screen.findByRole("button", { name: "Storage menu" });
  expect(trigger).toHaveAttribute("data-current");
  trigger.focus();
  await user.keyboard("{Enter}");
  const menu = await screen.findByRole("menu");
  expect(
    within(menu)
      .getAllByRole("menuitem")
      .map((a) => [a.textContent, a.getAttribute("href")]),
  ).toEqual([
    ["Storage pools", "/storage-pools"],
    ["Disks (volumes)", "/volumes"],
    ["Ceph", "/ceph"],
  ]);
  expect(within(menu).getByRole("menuitem", { name: "Disks (volumes)" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  await user.keyboard("{Escape}");
  await vi.waitFor(() => expect(screen.queryByRole("menu")).not.toBeInTheDocument());
  expect(trigger).toHaveFocus();
});

it("names each rail icon in a tooltip on hover and keyboard focus (77-3 review)", async () => {
  const user = userEvent.setup();
  server.use(
    http.get(apiUrl("/alerts"), () =>
      HttpResponse.json({
        alerts: [
          {
            rule: "r",
            rule_name: "cpu",
            condition: "c",
            severity: "critical",
            since: 0,
            state: "firing",
            subject: { kind: "node", id: "n", name: "n1" },
          },
        ],
        unreachable: [],
        unsupported: [],
      }),
    ),
  );
  shell("/volumes");
  const storage = await screen.findByRole("button", { name: "Storage menu" });
  // 지금 화면의 묶음과 경보 수는 설명으로 읽어 준다(이름은 그대로).
  expect(storage).toHaveAccessibleDescription("current page");
  const overview = screen.getByRole("button", { name: "Overview menu" });
  await vi.waitFor(() => expect(overview).toHaveAccessibleDescription("1 alerts ringing"));
  await user.hover(overview);
  expect(await screen.findByRole("tooltip")).toHaveTextContent("Overview");
  await user.keyboard("{Escape}");
  await vi.waitFor(() => expect(screen.queryByRole("tooltip")).not.toBeInTheDocument());
  // 키보드로 와도 보이고, 메뉴가 열린 동안은 숨는다.
  storage.focus();
  expect(await screen.findByRole("tooltip")).toHaveTextContent("Storage");
  await user.keyboard("{Enter}");
  await screen.findByRole("menu");
  expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
});

it("keeps the sidebar and the menu sheet in step when a group is closed", async () => {
  const user = userEvent.setup();
  const wide = wideScreen();
  shell("/vms");
  await user.click(await screen.findByRole("button", { name: "Open menu" }));
  const sheet = await screen.findByRole("dialog", { name: "Orbivisor" });
  await user.click(within(sheet).getByRole("button", { name: "Storage" }));
  await user.keyboard("{Escape}");
  await vi.waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  const nav = screen.getAllByRole("navigation", { name: "Main" })[0]!;
  expect(within(nav).getByRole("button", { name: "Storage" })).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  wide.mockRestore();
});

it("collapses the sidebar to an icon rail and remembers it", async () => {
  const user = userEvent.setup();
  const wide = wideScreen();
  shell();
  // 넓은 화면은 펼친 채 시작한다.
  await user.click(await screen.findByRole("button", { name: "Collapse sidebar" }));
  expect(localStorage.getItem(`opl.${ADMIN.user}.nav.rail`)).toBe("1");
  expect(screen.getByRole("button", { name: "Overview menu" })).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Expand sidebar" }));
  expect(localStorage.getItem(`opl.${ADMIN.user}.nav.rail`)).toBe("0");
  expect(screen.getByRole("button", { name: "Overview" })).toBeInTheDocument();
  wide.mockRestore();
});

it("starts as a rail on narrow screens", async () => {
  shell();
  expect(await screen.findByRole("button", { name: "Expand sidebar" })).toBeInTheDocument();
});

it("opens the two-level menu in a sheet from the menu button", async () => {
  const user = userEvent.setup();
  shell("/vms");
  await user.click(await screen.findByRole("button", { name: "Open menu" }));
  const sheet = await screen.findByRole("dialog", { name: "Orbivisor" });
  const nav = within(sheet).getByRole("navigation", { name: "Main" });
  expect(within(nav).getByRole("button", { name: "Virtual machines" })).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await user.click(within(nav).getByRole("link", { name: "Templates" }));
  await vi.waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
});

it("opens my API tokens and two-step verification from the account menu", async () => {
  const user = userEvent.setup();
  shell("/");
  await user.click(await screen.findByRole("button", { name: "Account (admin)" }));
  expect(screen.getByRole("menuitem", { name: "My two-step verification" })).toBeInTheDocument();
  await user.click(screen.getByRole("menuitem", { name: "My API tokens" }));
  const crumbs = await screen.findByRole("navigation", { name: "Breadcrumb" });
  await vi.waitFor(() =>
    expect(within(crumbs).getByText("My API tokens")).toHaveAttribute("aria-current", "page"),
  );
  expect(within(crumbs).getByText("My account")).toBeInTheDocument();
});

it.each([
  ["/vms", "Show as a list"],
  ["/vm-folders", "Show by folder"],
  ["/storage-folders", "Show by folder"],
])("toggles list and folder views on %s", async (url, current) => {
  renderPage(<ViewToggle />, { path: "*", url });
  const toggle = await screen.findByRole("navigation", { name: "Show as" });
  expect(within(toggle).getByRole("link", { name: current })).toHaveAttribute(
    "aria-current",
    "page",
  );
  expect(within(toggle).getAllByRole("link")).toHaveLength(2);
});

it("has no list and folder toggle on detail pages or lists without folders", async () => {
  renderPage(<ViewToggle />, { path: "*", url: "/vms/web" });
  expect(screen.queryByRole("navigation", { name: "Show as" })).not.toBeInTheDocument();
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
  // 다른 언어의 문구는 바꿀 때 받으므로 바뀌기까지 기다린다.
  await vi.waitFor(() => expect(i18n.language).toBe("ko"));
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

it("has no accessibility violations in the shell, wide and as a rail", async () => {
  const wide = wideScreen();
  const first = shell("/routers");
  await screen.findByRole("navigation", { name: "Routers & floating IPs pages" });
  await expectNoAxeViolations(first.container);
  first.unmount();
  wide.mockRestore();
  const { container } = shell("/routers");
  await screen.findByRole("button", { name: "Networking menu" });
  await expectNoAxeViolations(container);
});

it("says a hidden tab is not permitted instead of loading forever", async () => {
  // 폴더 사용자가 숨긴 탭의 주소(/nodes)를 바로 연다(계획 62 리뷰 1).
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
  );
  renderPage(<AppLayout />, {
    path: "*",
    url: "/nodes",
    as: { ...ADMIN, name: "alice", role: "none" },
  });
  expect(
    await screen.findByText("You do not have permission to view this page."),
  ).toBeInTheDocument();
  expect(screen.getByRole("heading", { level: 1, name: "Nodes" })).toBeInTheDocument();
  // 클러스터 전환(이름은 cluster.read가 있어야 읽는다)도 없다.
  expect(screen.queryByRole("button", { name: /^Switch cluster/ })).not.toBeInTheDocument();
});
