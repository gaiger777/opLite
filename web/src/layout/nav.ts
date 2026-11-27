// 화면 구성의 한 곳(계획 77 설계 "2단 사이드바", 진단 §1.3·§1.4). 사이드바(묶음 → 항목)·화면 안 탭·목록|폴더
// 토글·이동 경로·팔레트 이동 항목·단축키·옛 주소 넘기기가 모두 이 표를 읽는다. 주소는 되도록 그대로 두고 표만 바꾼다.
import type { Kind, Permissions, Privilege } from "@/auth/permissions";
import type { GroupId } from "@/components/icons";

/**
 * 주소가 있는 화면 하나. `kind`가 있으면 그 종류를 읽을 수 있는 사용자에게만, `admin`이면 루트 `cluster.admin`이
 * 있는 사용자에게만, `anywhere`면 그 권한이 어디든(루트·폴더) 있는 사용자에게만 보인다. `key`는 화면 안 탭·팔레트의
 * 이름(번역 키)이다.
 */
export interface NavPage {
  to: string;
  key: string;
  kind?: Kind;
  admin?: boolean;
  anywhere?: Privilege;
  /** 팔레트 이동 항목이 더 찾는 낱말(번역 키 — 쉼표로 나눔) */
  keywords?: string;
}

/**
 * 사이드바의 2단 항목. `pages`의 첫 보이는 화면이 항목 링크이고, 화면이 둘 이상이면 그 목록 화면 위에 화면 안 탭으로
 * 보인다(라우터 | 유동 IP). `folder`는 같은 목록의 폴더 모양(목록 오른쪽 위 아이콘 토글)이고 첫 화면과 같은 권한을 본다.
 * `match`는 이 항목에 속하는 다른 경로 접두사(DR 실행 화면)다. 상세 경로(`/vms/web`)는 그 접두사의 항목에 속한다.
 */
export interface NavItem {
  id: string;
  /** 사이드바 이름(번역 키) */
  key: string;
  pages: NavPage[];
  folder?: { to: string; key: string };
  match?: string[];
  /** 아래 경로가 리소스가 아니라 같은 화면의 다른 모양이다(만들기 화면의 빌더 `/vms/-new/builder`) */
  leaf?: boolean;
  /** 사이드바 항목 옆 숫자 */
  badge?: "alerts" | "failedTasks";
}

export interface NavGroup {
  id: GroupId;
  key: string;
  items: NavItem[];
  /** 사이드바에 없는 묶음(내 계정 — 사용자 메뉴에서 연다) */
  hidden?: boolean;
  /** 팔레트가 이 묶음의 항목마다 더 찾는 낱말(번역 키): 예전 사이드바의 영역 이름(계획 61 — 인프라·보호·활동·설정) */
  keywords?: string;
}

/** 항목이 화면 하나뿐일 때 */
const one = (id: string, key: string, page: Omit<NavPage, "key"> & { key?: string }): NavItem => ({
  id,
  key,
  pages: [{ ...page, key: page.key ?? key }],
});

export const GROUPS: NavGroup[] = [
  {
    id: "overview",
    key: "nav.group.overview",
    items: [
      one("dashboard", "nav.item.dashboard", { to: "/", key: "nav.area.home" }),
      // 2.5D 클러스터 지도(계획 70). `/nodes/map`은 노드 상세 `/nodes/:name`과 겹쳐 `/cluster` 아래에 둔다.
      one("map", "nav.item.map", {
        to: "/cluster/map",
        key: "nav.clusterMap",
        kind: "node",
        keywords: "nav.clusterMapKeywords",
      }),
      {
        ...one("alerts", "nav.item.alerts", { to: "/alerts", key: "nav.alerts" }),
        badge: "alerts",
      },
    ],
  },
  {
    id: "vms",
    key: "nav.group.vms",
    items: [
      {
        ...one("vms", "nav.item.vms", { to: "/vms", key: "nav.vms" }),
        folder: { to: "/vm-folders", key: "nav.vmFolders" },
      },
      one("templates", "nav.templates", { to: "/templates" }),
      // `/vms/-new/builder`도 이 항목이다(가장 긴 접두사).
      {
        ...one("create", "nav.item.createVm", { to: "/vms/-new", anywhere: "vm.create" }),
        leaf: true,
      },
    ],
  },
  {
    id: "images",
    key: "nav.group.images",
    items: [
      one("images", "nav.item.images", { to: "/images", key: "nav.images" }),
      one("isos", "nav.item.isos", { to: "/isos", key: "nav.isos" }),
      one("imports", "nav.item.imports", { to: "/imports", key: "nav.imports" }),
      // VMware 이전과 원본 연결(계획 72)은 한 항목의 화면 안 탭 둘이다.
      {
        id: "vmware",
        key: "nav.item.vmware",
        pages: [
          {
            to: "/vmware-migrations",
            key: "nav.vmwareMigrations",
            kind: "vmware-migration",
            keywords: "nav.vmwareMigrationsKeywords",
          },
          {
            to: "/vmware-sources",
            key: "nav.vmwareSources",
            kind: "vmware-source",
            keywords: "nav.vmwareSourcesKeywords",
          },
        ],
      },
      one("exports", "nav.vmExports", { to: "/vm-exports", keywords: "nav.vmExportsKeywords" }),
    ],
  },
  {
    id: "storage",
    key: "nav.group.storage",
    keywords: "nav.oldArea.infra",
    items: [
      {
        ...one("pools", "nav.pools", { to: "/storage-pools" }),
        folder: { to: "/storage-folders", key: "nav.storageFolders" },
      },
      one("volumes", "nav.item.volumes", { to: "/volumes", key: "nav.volumes" }),
      one("ceph", "nav.ceph", { to: "/ceph", kind: "ceph" }),
    ],
  },
  {
    id: "network",
    key: "nav.group.network",
    keywords: "nav.oldArea.infra",
    items: [
      {
        ...one("networks", "nav.networks", { to: "/networks" }),
        folder: { to: "/network-folders", key: "nav.networkFolders" },
      },
      one("securityGroups", "nav.item.securityGroups", {
        to: "/security-groups",
        key: "nav.securityGroups",
        kind: "security-group",
      }),
      // 라우터·유동 IP(계획 66)는 한 항목의 화면 안 탭 둘이다.
      {
        id: "routing",
        key: "nav.item.routing",
        pages: [
          { to: "/routers", key: "nav.routers" },
          { to: "/floating-ips", key: "nav.floatingIps", keywords: "nav.floatingIpsKeywords" },
        ],
      },
      one("lb", "nav.loadBalancers", { to: "/load-balancers" }),
      // SDN(OVN) 설정·상태(계획 65)
      one("sdn", "nav.item.sdn", { to: "/sdn", key: "nav.sdn", kind: "sdn" }),
      // 2.5D 토폴로지(계획 67)
      one("topology", "nav.item.topology", { to: "/networks/topology", key: "nav.topology" }),
    ],
  },
  {
    id: "cluster",
    key: "nav.group.cluster",
    keywords: "nav.oldArea.infra",
    items: [
      one("nodes", "nav.nodes", { to: "/nodes", kind: "node" }),
      one("cluster", "nav.item.cluster", { to: "/cluster", key: "nav.cluster", kind: "node" }),
      one("haGroups", "nav.item.haGroups", {
        to: "/ha-groups",
        key: "nav.haGroups",
        kind: "ha-group",
      }),
      one("haChecks", "nav.item.haChecks", {
        to: "/ha-checks",
        key: "nav.haChecks",
        kind: "ha-check",
      }),
      {
        id: "placement",
        key: "nav.item.placement",
        pages: [
          { to: "/placement-rules", key: "nav.placementRules", kind: "placement-rule" },
          { to: "/placement", key: "nav.placement", kind: "placement-rule" },
        ],
      },
    ],
  },
  {
    id: "backup",
    key: "nav.group.backup",
    keywords: "nav.oldArea.protect",
    items: [
      one("backups", "nav.backups", { to: "/backups" }),
      one("backupJobs", "nav.item.backupJobs", {
        to: "/backup-jobs",
        key: "nav.backupJobs",
        kind: "backup-job",
      }),
      // 백업 저장소(계획 68 — 중복 제거·암호화·S3)
      one("backupStores", "nav.backupStores", { to: "/backup-stores", kind: "backup-store" }),
      // 원격 클러스터의 상태·복제와 주소·토큰 관리(예전 설정 › 원격 클러스터)는 한 항목의 화면 안 탭 둘이다.
      {
        id: "remotes",
        key: "nav.item.remotes",
        pages: [
          { to: "/remotes", key: "nav.replication", kind: "remote" },
          { to: "/remotes/-manage", key: "nav.remotes", admin: true },
        ],
      },
      // DR 복구 계획·실행(계획 76). 실행 화면(`/dr-runs/<id>`)도 이 항목에 속한다.
      {
        ...one("dr", "nav.item.dr", { to: "/dr-plans", key: "nav.dr", kind: "recovery-plan" }),
        match: ["/dr-runs"],
      },
    ],
  },
  {
    id: "records",
    key: "nav.group.records",
    keywords: "nav.oldArea.activity",
    items: [
      {
        ...one("tasks", "nav.item.tasks", { to: "/tasks", key: "nav.tasks" }),
        badge: "failedTasks",
      },
      // 설정 기록(누구나 — 서버가 읽을 수 있는 리소스의 줄만 준다)과 선언 묶음(루트 `system.read`, 계획 73)
      one("history", "nav.item.history", {
        to: "/history",
        key: "nav.history",
        keywords: "nav.historyKeywords",
      }),
      one("applySets", "nav.item.applySets", {
        to: "/apply-sets",
        key: "nav.applySets",
        kind: "apply-set",
        keywords: "nav.applySetsKeywords",
      }),
      one("audit", "nav.audit", { to: "/audit", admin: true }),
    ],
  },
  {
    id: "admin",
    key: "nav.group.admin",
    keywords: "nav.oldArea.settings",
    items: [
      one("general", "nav.item.general", { to: "/settings", key: "nav.general", kind: "settings" }),
      {
        id: "users",
        key: "nav.item.users",
        pages: [
          { to: "/settings/users", key: "nav.users", kind: "user" },
          { to: "/settings/groups", key: "nav.groups", kind: "group" },
        ],
      },
      // 계획 62: 모든 grant 표·사용자 정의 역할(admin)
      {
        id: "roles",
        key: "nav.item.roles",
        pages: [
          { to: "/settings/roles", key: "nav.roles", admin: true },
          { to: "/settings/permissions", key: "nav.permissions", admin: true },
        ],
      },
      one("realms", "nav.item.realms", {
        to: "/settings/realms",
        key: "nav.realms",
        kind: "realm",
      }),
      // 2단계 인증 정책(admin)과 ACME 인증서(계획 73, 루트 `cluster.read`)
      {
        id: "security",
        key: "nav.item.security",
        pages: [
          { to: "/settings/security", key: "nav.security", admin: true },
          {
            to: "/settings/certificates",
            key: "nav.certificates",
            kind: "certificate",
            keywords: "nav.certificatesKeywords",
          },
        ],
      },
      one("notifications", "nav.item.notifications", {
        to: "/settings/notifications",
        key: "nav.notificationTargets",
        admin: true,
      }),
      one("subscription", "nav.subscription", { to: "/settings/subscription", kind: "settings" }),
    ],
  },
  {
    // 누구나 자기 것을 다룬다. 사이드바가 아니라 오른쪽 위 사용자 메뉴에서 연다.
    id: "account",
    key: "nav.group.account",
    hidden: true,
    items: [
      one("tokens", "nav.item.tokens", { to: "/settings/tokens", key: "nav.tokens" }),
      one("account", "nav.item.account", { to: "/settings/account", key: "nav.account" }),
    ],
  },
];

export interface LegacyRedirect {
  from: string;
  to: string;
  /** 이 묶음들의 보이는 첫 항목으로 간다(없으면 `to`). 예전 영역에 지금은 여러 묶음에 흩어진 화면이 있을 때 */
  groups?: GroupId[];
}

/**
 * 옛 주소 → 새 주소. 쿼리와 해시를 그대로 붙여 `replace`로 넘긴다. 예전 사이드바의 영역 주소(계획 61)는 그 영역의
 * 첫 화면으로 간다(예전 인프라 영역은 노드를 못 읽는 사용자에게 스토리지·네트워크의 보이는 첫 화면).
 */
export const LEGACY_REDIRECTS: LegacyRedirect[] = [
  { from: "/users", to: "/settings/users" },
  { from: "/realms", to: "/settings/realms" },
  { from: "/tokens", to: "/settings/tokens" },
  { from: "/infra", to: "/nodes", groups: ["cluster", "storage", "network"] },
  { from: "/protect", to: "/backups" },
  { from: "/activity", to: "/tasks" },
  { from: "/settings/remotes", to: "/remotes/-manage" },
];

/** 옛 주소가 지금 권한으로 갈 곳 */
export function legacyTarget(redirect: LegacyRedirect, gate: Gate) {
  for (const id of redirect.groups ?? []) {
    const group = GROUPS.find((g) => g.id === id);
    const first = group && visibleItems(group, gate)[0];
    if (first) {
      return itemHome(first, gate);
    }
  }
  return redirect.to;
}

/** 옛 주소(넘기기 경로)인지. 넘기기 전에 "권한 없음"으로 막지 않는다(`/settings/remotes`는 `/settings` 아래다). */
export function isLegacyPath(pathname: string) {
  const path = pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
  return LEGACY_REDIRECTS.some((redirect) => redirect.from === path);
}

/** 사이드바 없이 전체 폭으로 보이는 경로(콘솔) */
export function isFullWidth(pathname: string) {
  return /^\/vms\/[^/]+\/console\/?$/.test(pathname);
}

/** 화면을 보일지 정하는 권한(`usePermissions()`) */
export type Gate = Pick<Permissions, "has" | "reads" | "anywhere">;

export function pageVisible(page: NavPage, gate: Gate) {
  return (
    (!page.kind || gate.reads(page.kind)) &&
    (!page.admin || gate.has("cluster.admin")) &&
    (!page.anywhere || gate.anywhere(page.anywhere))
  );
}

export function visiblePages(item: NavItem, gate: Gate) {
  return item.pages.filter((page) => pageVisible(page, gate));
}

export function itemVisible(item: NavItem, gate: Gate) {
  return visiblePages(item, gate).length > 0;
}

/** 항목 링크가 가는 곳: 보이는 첫 화면 */
export function itemHome(item: NavItem, gate: Gate) {
  return visiblePages(item, gate)[0]?.to ?? item.pages[0]!.to;
}

export function visibleItems(group: NavGroup, gate: Gate) {
  return group.items.filter((item) => itemVisible(item, gate));
}

/** 사이드바 묶음: 보이는 항목이 하나라도 있는 것(숨긴 묶음 빼고) */
export function visibleGroups(gate: Gate) {
  return GROUPS.filter((group) => !group.hidden && visibleItems(group, gate).length > 0);
}

function prefixes(item: NavItem) {
  return [
    ...item.pages.map((page) => page.to),
    ...(item.folder ? [item.folder.to] : []),
    ...(item.match ?? []),
  ];
}

function under(pathname: string, prefix: string) {
  if (prefix === "/") {
    return pathname === "/";
  }
  return pathname === prefix || pathname.startsWith(`${prefix}/`);
}

export interface NavHit {
  group: NavGroup;
  item: NavItem;
  /** 맞은 접두사가 화면이면 그 화면(폴더 모양·`match`이면 없음) */
  page?: NavPage;
  /** 폴더 모양(`item.folder`) 아래 */
  folder: boolean;
  /** 맞은 접두사(그 화면의 목록 주소) */
  prefix: string;
}

/** 경로가 속한 묶음과 항목. 가장 긴 접두사가 이긴다(`/vms/-new`는 모든 VM이 아니라 VM 만들기). */
export function navOf(pathname: string): NavHit | null {
  const path = pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
  let best: NavHit | null = null;
  for (const group of GROUPS) {
    for (const item of group.items) {
      for (const prefix of prefixes(item)) {
        if (under(path, prefix) && (!best || prefix.length > best.prefix.length)) {
          best = {
            group,
            item,
            prefix,
            page: item.pages.find((page) => page.to === prefix),
            folder: prefix === item.folder?.to,
          };
        }
      }
    }
  }
  return best;
}

/** 이 경로를 지금 권한으로 볼 수 있는지(맞은 화면, 없으면 항목의 첫 화면으로 본다) */
export function hitVisible(hit: NavHit, gate: Gate) {
  return pageVisible(hit.page ?? hit.item.pages[0]!, gate);
}

/** 지금 경로가 그 항목의 목록 화면(상세가 아님)인지 */
export function onListPage(pathname: string, hit: NavHit) {
  return (pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname) === hit.prefix;
}

const openKey = "nav.open";

/**
 * 펼친 묶음(사용자별 `{user}.nav.open`, 쉼표로 나눈 id). 기억이 없으면 null — 그러면 모든 묶음을 펼친다.
 * 저장소를 못 써도 null이다(`readPref`가 실패를 삼킨다).
 */
export function readOpenGroups(read: (key: string) => string | null): Set<GroupId> | null {
  const saved = read(openKey);
  if (saved === null) {
    return null;
  }
  const known = new Set(GROUPS.map((group) => group.id));
  return new Set(saved.split(",").filter((id): id is GroupId => known.has(id as GroupId)));
}

export function writeOpenGroups(write: (key: string, value: string) => void, open: Set<GroupId>) {
  write(openKey, [...open].join(","));
}

export interface Crumb {
  label: string;
  /** 없으면 링크가 아니다(묶음 이름, 지금 화면) */
  to?: string;
}

/**
 * 이동 경로 `묶음 › 항목 › 화면·리소스`. 화면 안 탭이 있는 항목은 탭 이름이, 폴더 모양은 폴더 이름(`nav.vmFolders`)이
 * 셋째 칸이다. 리소스 이름은 목록 주소 뒤 첫 경로 조각이다(`label`이 주면 그것으로 바꾼다: 폴더 id → 폴더 이름).
 * `t`는 i18n 함수다.
 */
export function crumbsOf(
  pathname: string,
  gate: Gate,
  t: (key: string) => string,
  label?: (prefix: string, segment: string) => string | undefined,
): Crumb[] {
  const hit = navOf(pathname);
  if (!hit) {
    return [];
  }
  const group = { label: t(hit.group.key) };
  const itemLabel = t(hit.item.key);
  if (hit.item.id === "dashboard") {
    return [group, { label: itemLabel }];
  }
  const path = pathname.replace(/\/+$/, "");
  const rest = path.slice(hit.prefix.length + 1).split("/")[0] ?? "";
  const tabbed = hit.item.pages.length > 1;
  const here = hit.folder
    ? t(hit.item.folder!.key)
    : tabbed && hit.page
      ? t(hit.page.key)
      : undefined;
  const item = { label: itemLabel, to: itemHome(hit.item, gate) };
  if (!rest || hit.item.leaf) {
    return here ? [group, item, { label: here }] : [group, { label: itemLabel }];
  }
  let resource: string;
  try {
    resource = rest === "-new" ? t("nav.new") : decodeURIComponent(rest);
  } catch {
    resource = rest;
  }
  return [
    group,
    { label: here ?? itemLabel, to: hit.prefix },
    { label: label?.(hit.prefix, resource) ?? resource },
  ];
}
