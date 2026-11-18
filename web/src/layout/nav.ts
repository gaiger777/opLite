// 화면 구성의 한 곳(설계 §4). 사이드바·영역 탭·보기 전환·이동 경로·팔레트 이동 항목·넘기기 시험이 모두
// 이 표를 읽는다. 사이드바에는 영역만 두고(둘째 단계 없음) 하위 화면은 영역 안 탭과 보기 전환으로 고른다.
import type { Principal, Role } from "@/api/types";
import { can, type Kind } from "@/auth/permissions";
import type { AreaId } from "@/components/icons";
import { readUserPref, writeUserPref } from "@/lib/prefs";

/** 탭 안의 보기(segmented). 이것도 경로 링크다. */
export interface NavView {
  to: string;
  key: string;
}

/**
 * 영역 탭. `kind`가 있으면 그 종류를 읽을 수 있는 역할에만, `admin`이면 admin에게만 보인다. `match`는 이 탭에
 * 속하는 경로 접두사이고 주지 않으면 `to`와 보기들의 `to`다(상세 경로 `/vms/web`은 `/vms`에 속한다).
 */
export interface NavTab {
  to: string;
  key: string;
  kind?: Kind;
  admin?: boolean;
  views?: NavView[];
  match?: string[];
}

export interface NavArea {
  id: AreaId;
  key: string;
  /** 영역 주소. 사이드바는 기억한 탭으로 가고, 이 주소를 열어도 기억한 탭으로 넘긴다(`/vms`·`/`·`/settings`는 첫 탭 자체다). */
  to: string;
  tabs: NavTab[];
}

export const AREAS: NavArea[] = [
  { id: "home", key: "nav.area.home", to: "/", tabs: [{ to: "/", key: "nav.area.home" }] },
  {
    id: "vms",
    key: "nav.area.vms",
    to: "/vms",
    tabs: [
      { to: "/vms", key: "nav.vmList" },
      { to: "/templates", key: "nav.templates" },
      {
        to: "/images",
        key: "nav.imagesIsos",
        views: [
          { to: "/images", key: "nav.images" },
          { to: "/isos", key: "nav.isos" },
        ],
      },
      { to: "/imports", key: "nav.imports" },
    ],
  },
  {
    id: "infra",
    key: "nav.area.infra",
    to: "/infra",
    tabs: [
      {
        to: "/nodes",
        key: "nav.nodes",
        views: [
          { to: "/nodes", key: "nav.nodes" },
          { to: "/cluster", key: "nav.cluster" },
        ],
      },
      { to: "/storage-pools", key: "nav.pools" },
      { to: "/volumes", key: "nav.volumes" },
      { to: "/ceph", key: "nav.ceph" },
      { to: "/networks", key: "nav.networks" },
      { to: "/security-groups", key: "nav.securityGroups" },
    ],
  },
  {
    id: "protect",
    key: "nav.area.protect",
    to: "/protect",
    tabs: [
      { to: "/backups", key: "nav.backups" },
      { to: "/backup-jobs", key: "nav.backupJobs" },
      { to: "/remotes", key: "nav.replication", kind: "remote" },
      { to: "/ha-groups", key: "nav.haGroups" },
      { to: "/ha-checks", key: "nav.haChecks" },
      {
        to: "/placement-rules",
        key: "nav.placementTab",
        views: [
          { to: "/placement-rules", key: "nav.placementRules" },
          { to: "/placement", key: "nav.placement" },
        ],
      },
    ],
  },
  {
    id: "activity",
    key: "nav.area.activity",
    to: "/activity",
    tabs: [
      { to: "/tasks", key: "nav.tasks" },
      { to: "/alerts", key: "nav.alerts" },
      { to: "/audit", key: "nav.audit", admin: true },
    ],
  },
  {
    id: "settings",
    key: "nav.area.settings",
    to: "/settings",
    tabs: [
      { to: "/settings", key: "nav.general" },
      { to: "/settings/users", key: "nav.users", kind: "user" },
      { to: "/settings/realms", key: "nav.realms", kind: "realm" },
      // 토큰은 누구나 자기 것을 다룬다.
      { to: "/settings/tokens", key: "nav.tokens" },
      { to: "/settings/remotes", key: "nav.remotes", admin: true },
      { to: "/settings/notifications", key: "nav.notificationTargets", admin: true },
      { to: "/settings/subscription", key: "nav.subscription" },
    ],
  },
];

/** 옛 주소 → 새 주소. 쿼리와 해시를 그대로 붙여 `replace`로 넘긴다(설계 §4.3). */
export const LEGACY_REDIRECTS: { from: string; to: string }[] = [
  { from: "/users", to: "/settings/users" },
  { from: "/realms", to: "/settings/realms" },
  { from: "/tokens", to: "/settings/tokens" },
];

/** 영역 탭 없이 전체 폭으로 보이는 경로(콘솔) */
export function isFullWidth(pathname: string) {
  return /^\/vms\/[^/]+\/console\/?$/.test(pathname);
}

export function tabVisible(tab: NavTab, role: Role) {
  return (!tab.kind || can(role, "read", tab.kind)) && (!tab.admin || role === "admin");
}

export function visibleTabs(area: NavArea, role: Role) {
  return area.tabs.filter((tab) => tabVisible(tab, role));
}

/** 보이는 탭이 하나라도 있는 영역 */
export function visibleAreas(role: Role) {
  return AREAS.filter((area) => visibleTabs(area, role).length > 0);
}

function prefixes(tab: NavTab) {
  return tab.match ?? [tab.to, ...(tab.views ?? []).map((view) => view.to)];
}

function under(pathname: string, prefix: string) {
  if (prefix === "/") {
    return pathname === "/";
  }
  return pathname === prefix || pathname.startsWith(`${prefix}/`);
}

export interface NavHit {
  area: NavArea;
  tab: NavTab;
  /** 탭에 보기가 있으면 지금 보기 */
  view?: NavView;
  /** 맞은 접두사(그 탭의 목록 주소) */
  prefix: string;
}

/** 경로가 속한 영역과 탭. 가장 긴 접두사가 이긴다(`/settings/users`는 일반이 아니라 사용자 탭). */
export function areaOf(pathname: string): NavHit | null {
  const path = pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
  let best: NavHit | null = null;
  for (const area of AREAS) {
    for (const tab of area.tabs) {
      for (const prefix of prefixes(tab)) {
        if (under(path, prefix) && (!best || prefix.length > best.prefix.length)) {
          best = { area, tab, prefix, view: tab.views?.find((view) => view.to === prefix) };
        }
      }
    }
  }
  return best;
}

const lastKey = (area: AreaId) => `nav.last.${area}`;

/** 마지막 탭을 사용자별로 읽고 쓰는 데 필요한 것 */
export type NavUser = Pick<Principal, "user" | "role">;

/**
 * 영역에서 마지막으로 본 탭(경로와 쿼리, 사용자별 `{user}.nav.last.{area}`). 기억이 없거나, 그 영역의 탭이
 * 아니거나, 지금 권한으로 보이지 않으면 보이는 첫 탭이다. 보이는 탭이 없으면 영역 주소다.
 */
export function lastTab(area: NavArea, { user, role }: NavUser): string {
  const saved = readUserPref(user, lastKey(area.id));
  if (saved) {
    const pathname = saved.split(/[?#]/)[0] ?? "";
    const hit = areaOf(pathname);
    if (hit && hit.area.id === area.id && hit.prefix === pathname && tabVisible(hit.tab, role)) {
      return saved;
    }
  }
  return visibleTabs(area, role)[0]?.to ?? area.to;
}

/** 지금 화면을 그 영역의 마지막 탭으로 기억한다. 상세 경로는 그 탭의 목록을 기억한다. */
export function rememberTab(user: string, pathname: string, search: string) {
  const hit = areaOf(pathname);
  if (!hit) {
    return;
  }
  const path = pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
  writeUserPref(user, lastKey(hit.area.id), path === hit.prefix ? `${path}${search}` : hit.prefix);
}

export interface Crumb {
  label: string;
  /** 없으면 지금 화면 */
  to?: string;
}

/**
 * 이동 경로 `영역 › 탭(보기) › 리소스`. 리소스 이름은 탭 목록 주소 뒤 첫 경로 조각이다. `t`는 i18n 함수다.
 */
export function crumbsOf(pathname: string, me: NavUser, t: (key: string) => string): Crumb[] {
  const hit = areaOf(pathname);
  if (!hit) {
    return [];
  }
  const area = { label: t(hit.area.key), to: lastTab(hit.area, me) };
  if (hit.area.id === "home") {
    return [{ label: area.label }];
  }
  const path = pathname.replace(/\/+$/, "");
  const rest = path.slice(hit.prefix.length + 1).split("/")[0] ?? "";
  const tabLabel = t(hit.view?.key ?? hit.tab.key);
  if (!rest) {
    return [area, { label: tabLabel }];
  }
  let resource: string;
  try {
    resource = rest === "-new" ? t("nav.new") : decodeURIComponent(rest);
  } catch {
    resource = rest;
  }
  return [area, { label: tabLabel, to: hit.prefix }, { label: resource }];
}
