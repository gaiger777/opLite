import { matchRoutes } from "react-router";

import { appRoutes } from "@/App";
import type { Role } from "@/api/types";
import { Permissions } from "@/auth/permissions";
import en from "@/i18n/en.json";
import {
  AREAS,
  areaOf,
  crumbsOf,
  isFullWidth,
  LEGACY_REDIRECTS,
  lastTab,
  rememberTab,
  visibleAreas,
  visibleTabs,
} from "@/layout/nav";

const area = (id: string) => AREAS.find((a) => a.id === id)!;
const U1 = "00000000-0000-0000-0000-0000000000a1";
const U2 = "00000000-0000-0000-0000-0000000000a2";
const as = (role: Role, user = U1) => ({ user, gate: Permissions.fromRole(role) });

/** `nav.area.home` 같은 키를 영어 글로 바꾼다(i18n을 띄우지 않는 순수 시험). */
function t(key: string): string {
  return key.split(".").reduce<unknown>((v, k) => (v as Record<string, unknown>)[k], en) as string;
}

/** 경로 표에서 맞는 마지막 경로(`*`이면 NotFound) */
function routeOf(pathname: string) {
  return matchRoutes(appRoutes, pathname)?.at(-1)?.route.path;
}

afterEach(() => localStorage.clear());

it("keeps the sidebar to six top-level areas with settings last", () => {
  expect(AREAS.map((a) => a.id)).toEqual([
    "home",
    "vms",
    "infra",
    "protect",
    "activity",
    "settings",
  ]);
  // 영역 정의에는 탭만 있다(사이드바에 둘째 단계가 없다).
  for (const a of AREAS) {
    expect(Object.keys(a).sort()).toEqual(["id", "key", "tabs", "to"]);
  }
});

it("has a route and an English label for every tab, view and area", () => {
  for (const a of AREAS) {
    expect(t(a.key)).toEqual(expect.any(String));
    for (const tab of a.tabs) {
      expect(t(tab.key)).toEqual(expect.any(String));
      for (const to of [tab.to, ...(tab.views ?? []).map((v) => v.to)]) {
        expect([to, routeOf(to)]).not.toEqual([to, "*"]);
        expect(areaOf(to)?.area.id).toBe(a.id);
      }
      for (const view of tab.views ?? []) {
        expect(t(view.key)).toEqual(expect.any(String));
      }
    }
  }
});

it.each([
  ["/", "home", "/"],
  ["/vms", "vms", "/vms"],
  ["/vms/web", "vms", "/vms"],
  ["/vms/-new", "vms", "/vms"],
  ["/vms/web/edit", "vms", "/vms"],
  ["/vms/web/console", "vms", "/vms"],
  ["/isos", "vms", "/images"],
  ["/imports/esxi-1", "vms", "/imports"],
  ["/nodes/node-a", "infra", "/nodes"],
  ["/cluster", "infra", "/nodes"],
  ["/security-groups/web", "infra", "/security-groups"],
  ["/remotes/dr", "protect", "/remotes"],
  ["/placement", "protect", "/placement-rules"],
  ["/alerts", "activity", "/alerts"],
  ["/settings", "settings", "/settings"],
  ["/settings/users", "settings", "/settings/users"],
  ["/settings/remotes", "settings", "/settings/remotes"],
  ["/settings/notifications/", "settings", "/settings/notifications"],
])("puts %s in area %s, tab %s", (pathname, areaId, tab) => {
  const hit = areaOf(pathname)!;
  expect(hit.area.id).toBe(areaId);
  expect(hit.tab.to).toBe(tab);
});

it("knows nothing of unknown pages", () => {
  expect(areaOf("/somewhere")).toBeNull();
  expect(areaOf("/vmsx")).toBeNull();
  expect(crumbsOf("/somewhere", as("admin"), t)).toEqual([]);
});

it("shows the console full width", () => {
  expect(isFullWidth("/vms/web/console")).toBe(true);
  expect(isFullWidth("/vms/web")).toBe(false);
  expect(isFullWidth("/vms/console")).toBe(false);
});

const visible = (role: Role) =>
  Object.fromEntries(
    visibleAreas(Permissions.fromRole(role)).map((a) => [
      a.id,
      visibleTabs(a, Permissions.fromRole(role)).map((tab) => tab.to),
    ]),
  );

it("shows every area and tab to admins", () => {
  expect(visible("admin")).toEqual(
    Object.fromEntries(AREAS.map((a) => [a.id, a.tabs.map((tab) => tab.to)])),
  );
});

it.each(["operator", "viewer"] as const)("hides account and admin tabs from %s", (role) => {
  const tabs = visible(role);
  expect(Object.keys(tabs)).toEqual(["home", "vms", "infra", "protect", "activity", "settings"]);
  expect(tabs.activity).toEqual(["/tasks", "/alerts"]);
  expect(tabs.settings).toEqual([
    "/settings",
    "/settings/tokens",
    "/settings/account",
    "/settings/subscription",
  ]);
  // 원격 클러스터 상태는 누구나 읽는다.
  expect(tabs.protect).toContain("/remotes");
});

it("remembers the list of the last tab, not a detail page", () => {
  rememberTab(U1, "/volumes", "?q=root");
  expect(lastTab(area("infra"), as("admin"))).toBe("/volumes?q=root");
  rememberTab(U1, "/nodes/node-a", "?tab=metrics");
  expect(lastTab(area("infra"), as("admin"))).toBe("/nodes");
  rememberTab(U1, "/cluster", "");
  expect(lastTab(area("infra"), as("admin"))).toBe("/cluster");
  // 다른 영역의 기억은 그대로다.
  rememberTab(U1, "/backups", "");
  expect(lastTab(area("infra"), as("admin"))).toBe("/cluster");
  expect(lastTab(area("protect"), as("admin"))).toBe("/backups");
  rememberTab(U1, "/somewhere", "");
});

it("falls back to the first visible tab", () => {
  expect(lastTab(area("activity"), as("viewer"))).toBe("/tasks");
  rememberTab(U1, "/audit", "");
  expect(lastTab(area("activity"), as("admin"))).toBe("/audit");
  // 권한 밖의 기억은 쓰지 않는다.
  expect(lastTab(area("activity"), as("viewer"))).toBe("/tasks");
  // 다른 영역의 경로나 상세 경로를 손으로 넣어도 쓰지 않는다.
  localStorage.setItem(`opl.${U1}.nav.last.activity`, "/vms");
  expect(lastTab(area("activity"), as("admin"))).toBe("/tasks");
  localStorage.setItem(`opl.${U1}.nav.last.infra`, "/nodes/node-a");
  expect(lastTab(area("infra"), as("admin"))).toBe("/nodes");
});

it("remembers the last tab per user and moves an old shared memory to the first user once", () => {
  rememberTab(U1, "/volumes", "?q=secret-vm");
  // 같은 브라우저의 다른 사용자에게 앞 사용자의 검색이 넘어가지 않는다.
  expect(lastTab(area("infra"), as("admin", U2))).toBe("/nodes");
  expect(localStorage.getItem(`opl.${U1}.nav.last.infra`)).toBe("/volumes?q=secret-vm");

  localStorage.setItem("opl.nav.last.protect", "/ha-groups");
  expect(lastTab(area("protect"), as("admin", U2))).toBe("/ha-groups");
  expect(localStorage.getItem(`opl.${U2}.nav.last.protect`)).toBe("/ha-groups");
  expect(localStorage.getItem("opl.nav.last.protect")).toBeNull();
  expect(lastTab(area("protect"), as("admin", U1))).toBe("/backups");
});

it("works when the browser storage throws", () => {
  const get = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
    throw new Error("denied");
  });
  const set = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
    throw new Error("denied");
  });
  expect(() => rememberTab(U1, "/volumes", "")).not.toThrow();
  expect(lastTab(area("infra"), as("admin"))).toBe("/nodes");
  get.mockRestore();
  set.mockRestore();
});

it.each([
  ["/", [{ label: "Home" }]],
  ["/storage-pools", [{ label: "Infrastructure", to: "/nodes" }, { label: "Storage pools" }]],
  ["/cluster", [{ label: "Infrastructure", to: "/nodes" }, { label: "Cluster" }]],
  [
    "/vms/web%20db/edit",
    [{ label: "Virtual machines", to: "/vms" }, { label: "VMs", to: "/vms" }, { label: "web db" }],
  ],
  [
    "/vms/-new",
    [{ label: "Virtual machines", to: "/vms" }, { label: "VMs", to: "/vms" }, { label: "New" }],
  ],
  ["/settings/tokens", [{ label: "Settings", to: "/settings" }, { label: "API tokens" }]],
])("builds the breadcrumb of %s", (pathname, crumbs) => {
  expect(crumbsOf(pathname, as("admin"), t)).toEqual(crumbs);
});

it("redirects only addresses that no longer have a page", () => {
  for (const { from, to } of LEGACY_REDIRECTS) {
    expect(routeOf(to)).not.toBe("*");
    expect(areaOf(to)?.area.id).toBe("settings");
    expect(areaOf(from)).toBeNull();
  }
});
