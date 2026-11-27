import { matchRoutes } from "react-router";

import { appRoutes } from "@/App";
import type { Role } from "@/api/types";
import { Permissions } from "@/auth/permissions";
import en from "@/i18n/en.json";
import {
  crumbsOf,
  GROUPS,
  hitVisible,
  isFullWidth,
  itemHome,
  LEGACY_REDIRECTS,
  navOf,
  pageVisible,
  readOpenGroups,
  visibleGroups,
  visibleItems,
  writeOpenGroups,
} from "@/layout/nav";

const gate = (role: Role) => Permissions.fromRole(role);
const FOLDER_ONLY = new Permissions({
  all: false,
  root: [],
  root_contents: [],
  folders: {},
  contents: { "00000000-0000-0000-0000-0000000000f1": ["system.read", "vm.create"] },
  objects: [],
});

/** `nav.group.vms` 같은 키를 영어 글로 바꾼다(i18n을 띄우지 않는 순수 시험). */
function t(key: string): string {
  return key.split(".").reduce<unknown>((v, k) => (v as Record<string, unknown>)[k], en) as string;
}

/** 경로 표에서 맞는 마지막 경로(`*`이면 NotFound) */
function routeOf(pathname: string) {
  return matchRoutes(appRoutes, pathname)?.at(-1)?.route.path;
}

const allItems = GROUPS.flatMap((g) => g.items);

it("has nine sidebar groups plus administration last, and the account group outside the sidebar", () => {
  expect(GROUPS.map((g) => g.id)).toEqual([
    "overview",
    "vms",
    "images",
    "storage",
    "network",
    "cluster",
    "backup",
    "records",
    "admin",
    "account",
  ]);
  expect(visibleGroups(gate("admin")).map((g) => g.id)).not.toContain("account");
  expect(new Set(allItems.map((i) => i.id)).size).toBe(allItems.length);
});

it("has a route, an English label and its own item for every page and folder view", () => {
  for (const group of GROUPS) {
    expect(t(group.key)).toEqual(expect.any(String));
    for (const item of group.items) {
      expect(t(item.key)).toEqual(expect.any(String));
      for (const page of item.pages) {
        expect(t(page.key)).toEqual(expect.any(String));
        expect([page.to, routeOf(page.to)]).not.toEqual([page.to, "*"]);
        expect(navOf(page.to)?.item).toBe(item);
        if (page.keywords) {
          expect(t(page.keywords)).toEqual(expect.any(String));
        }
      }
      if (item.folder) {
        expect(t(item.folder.key)).toEqual(expect.any(String));
        expect(routeOf(item.folder.to)).not.toBe("*");
        expect(navOf(item.folder.to)).toMatchObject({ item, folder: true });
      }
    }
  }
});

// 진단 §1.4 대응표 전부: 지금 경로 → 새 묶음 › 항목
it.each([
  ["/", "overview", "dashboard"],
  ["/cluster/map", "overview", "map"],
  ["/alerts", "overview", "alerts"],
  ["/vms", "vms", "vms"],
  ["/vms/web", "vms", "vms"],
  ["/vms/web/edit", "vms", "vms"],
  ["/vms/web/console", "vms", "vms"],
  ["/vm-folders", "vms", "vms"],
  ["/vm-folders/f1", "vms", "vms"],
  ["/templates", "vms", "templates"],
  ["/vms/-new", "vms", "create"],
  ["/vms/-new/builder", "vms", "create"],
  ["/images", "images", "images"],
  ["/isos", "images", "isos"],
  ["/imports", "images", "imports"],
  ["/imports/esxi-1", "images", "imports"],
  ["/vmware-migrations", "images", "vmware"],
  ["/vmware-migrations/-new", "images", "vmware"],
  ["/vmware-migrations/wave1", "images", "vmware"],
  ["/vmware-sources", "images", "vmware"],
  ["/vm-exports", "images", "exports"],
  ["/storage-pools", "storage", "pools"],
  ["/storage-folders", "storage", "pools"],
  ["/volumes", "storage", "volumes"],
  ["/ceph", "storage", "ceph"],
  ["/networks", "network", "networks"],
  ["/network-folders", "network", "networks"],
  ["/security-groups", "network", "securityGroups"],
  ["/security-groups/web", "network", "securityGroups"],
  ["/routers", "network", "routing"],
  ["/floating-ips", "network", "routing"],
  ["/load-balancers", "network", "lb"],
  ["/sdn", "network", "sdn"],
  ["/networks/topology", "network", "topology"],
  ["/nodes", "cluster", "nodes"],
  ["/nodes/node-a", "cluster", "nodes"],
  ["/cluster", "cluster", "cluster"],
  ["/ha-groups", "cluster", "haGroups"],
  ["/ha-checks", "cluster", "haChecks"],
  ["/placement-rules", "cluster", "placement"],
  ["/placement", "cluster", "placement"],
  ["/backups", "backup", "backups"],
  ["/backups/b1/files", "backup", "backups"],
  ["/backup-jobs", "backup", "backupJobs"],
  ["/backup-stores", "backup", "backupStores"],
  ["/remotes", "backup", "remotes"],
  ["/remotes/dr", "backup", "remotes"],
  ["/remotes/-manage", "backup", "remotes"],
  ["/dr-plans", "backup", "dr"],
  ["/dr-plans/site-b", "backup", "dr"],
  ["/dr-runs/r1", "backup", "dr"],
  ["/tasks", "records", "tasks"],
  ["/history", "records", "history"],
  ["/apply-sets", "records", "applySets"],
  ["/apply-sets/web-prod", "records", "applySets"],
  ["/audit", "records", "audit"],
  ["/settings", "admin", "general"],
  ["/settings/users", "admin", "users"],
  ["/settings/groups", "admin", "users"],
  ["/settings/roles", "admin", "roles"],
  ["/settings/permissions", "admin", "roles"],
  ["/settings/realms", "admin", "realms"],
  ["/settings/security", "admin", "security"],
  ["/settings/certificates", "admin", "security"],
  ["/settings/notifications/", "admin", "notifications"],
  ["/settings/subscription", "admin", "subscription"],
  ["/settings/tokens", "account", "tokens"],
  ["/settings/account", "account", "account"],
])("puts %s in %s › %s", (pathname, groupId, itemId) => {
  const hit = navOf(pathname)!;
  expect(hit.group.id).toBe(groupId);
  expect(hit.item.id).toBe(itemId);
});

it("knows nothing of unknown pages", () => {
  expect(navOf("/somewhere")).toBeNull();
  expect(navOf("/vmsx")).toBeNull();
  expect(crumbsOf("/somewhere", gate("admin"), t)).toEqual([]);
});

it("shows the console full width", () => {
  expect(isFullWidth("/vms/web/console")).toBe(true);
  expect(isFullWidth("/vms/web")).toBe(false);
  expect(isFullWidth("/vms/console")).toBe(false);
});

it("redirects every old address that no longer has a page to a page that exists", () => {
  for (const { from, to } of LEGACY_REDIRECTS) {
    expect(routeOf(to)).not.toBe("*");
    expect(navOf(to)).not.toBeNull();
    // 옛 주소는 넘기기 경로다(화면이 아니다).
    expect(routeOf(from)).toBe(from.slice(1));
  }
  // 원격 클러스터 관리(예전 설정 › 원격 클러스터)는 백업·복구의 원격 항목 하나로 모였다.
  expect(LEGACY_REDIRECTS).toContainEqual({ from: "/settings/remotes", to: "/remotes/-manage" });
  expect(LEGACY_REDIRECTS.map((r) => r.from)).toEqual(
    expect.arrayContaining(["/infra", "/protect", "/activity"]),
  );
});

it("shows every item to admins", () => {
  for (const group of GROUPS) {
    expect(visibleItems(group, gate("admin"))).toEqual(group.items);
  }
});

it.each(["operator", "viewer"] as const)("hides admin-only items from %s", (role) => {
  const items = Object.fromEntries(
    visibleGroups(gate(role)).map((g) => [g.id, visibleItems(g, gate(role)).map((i) => i.id)]),
  );
  expect(items.records).not.toContain("audit");
  expect(items.admin).toEqual(["general", "security", "subscription"]);
  // 원격 클러스터 상태는 누구나 읽고, 관리 탭은 admin만이다.
  const remotes = allItems.find((i) => i.id === "remotes")!;
  expect(itemHome(remotes, gate(role))).toBe("/remotes");
  expect(pageVisible(remotes.pages[1]!, gate(role))).toBe(false);
  // 보안·인증서는 인증서 화면으로 간다.
  expect(
    itemHome(
      allItems.find((i) => i.id === "security")!,
      gate(role),
    ),
  ).toBe("/settings/certificates");
});

it("shows VM create only to users who may create a VM somewhere", () => {
  const create = allItems.find((i) => i.id === "create")!;
  expect(visibleItems(GROUPS[1]!, gate("viewer")).map((i) => i.id)).not.toContain("create");
  expect(visibleItems(GROUPS[1]!, FOLDER_ONLY).map((i) => i.id)).toContain("create");
  expect(hitVisible(navOf("/vms/-new/builder")!, gate("viewer"))).toBe(false);
  expect(create.pages[0]!.anywhere).toBe("vm.create");
});

it("hides root-only pages from folder users", () => {
  const items = Object.fromEntries(
    visibleGroups(FOLDER_ONLY).map((g) => [g.id, visibleItems(g, FOLDER_ONLY).map((i) => i.id)]),
  );
  // 선언 묶음(루트 system.read)·SDN·VMware 원본은 없다.
  expect(items.records).toEqual(["tasks", "history"]);
  expect(items.network).not.toContain("sdn");
  const vmware = allItems.find((i) => i.id === "vmware")!;
  expect(pageVisible(vmware.pages[1]!, FOLDER_ONLY)).toBe(false);
  expect(items.admin).toBeUndefined();
});

it.each([
  ["/", [{ label: "Overview" }, { label: "Dashboard" }]],
  ["/storage-pools", [{ label: "Storage" }, { label: "Storage pools" }]],
  [
    "/storage-folders",
    [
      { label: "Storage" },
      { label: "Storage pools", to: "/storage-pools" },
      { label: "Storage folders" },
    ],
  ],
  ["/cluster", [{ label: "Cluster & availability" }, { label: "Cluster (members, certificates)" }]],
  ["/cluster/map", [{ label: "Overview" }, { label: "Infrastructure map" }]],
  [
    "/vms/web%20db/edit",
    [{ label: "Virtual machines" }, { label: "All VMs", to: "/vms" }, { label: "web db" }],
  ],
  ["/vms/-new", [{ label: "Virtual machines" }, { label: "Create VM" }]],
  ["/vms/-new/builder", [{ label: "Virtual machines" }, { label: "Create VM" }]],
  [
    "/placement",
    [
      { label: "Cluster & availability" },
      { label: "Placement rules & balancing", to: "/placement-rules" },
      { label: "Optimal placement" },
    ],
  ],
  [
    "/vmware-migrations/wave1",
    [
      { label: "Images & migration" },
      { label: "VMware migrations", to: "/vmware-migrations" },
      { label: "wave1" },
    ],
  ],
  ["/settings/tokens", [{ label: "My account" }, { label: "My API tokens" }]],
])("builds the breadcrumb of %s", (pathname, crumbs) => {
  expect(crumbsOf(pathname, gate("admin"), t)).toEqual(crumbs);
});

it("reads and writes open groups, ignoring unknown ids", () => {
  const store = new Map<string, string>();
  expect(readOpenGroups((k) => store.get(k) ?? null)).toBeNull();
  writeOpenGroups((k, v) => store.set(k, v), new Set(["vms", "backup"]));
  expect(store.get("nav.open")).toBe("vms,backup");
  store.set("nav.open", "vms,infra,,records");
  expect([...readOpenGroups((k) => store.get(k) ?? null)!]).toEqual(["vms", "records"]);
  // 모두 접은 것도 기억이다(빈 목록 ≠ 기억 없음).
  store.set("nav.open", "");
  expect(readOpenGroups((k) => store.get(k) ?? null)?.size).toBe(0);
});

it("finds VMware, OVA, drift and ACME words for the palette", () => {
  const words = (id: string, i = 0) => t(allItems.find((x) => x.id === id)!.pages[i]!.keywords!);
  expect(words("vmware")).toContain("vcenter");
  expect(words("exports")).toContain("ova");
  expect(words("history")).toContain("drift");
  expect(words("applySets")).toContain("yaml");
  expect(words("security", 1)).toContain("ACME");
});
