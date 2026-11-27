import { cn } from "cn";
import { ChevronDown, ChevronsLeft, ChevronsRight } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useLocation } from "react-router";

import { useNavUser } from "@/auth/usePermissions";
import { ACTION_ICONS, GROUP_ICONS, type GroupId, RAIL_ICON_SIZE } from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Kbd } from "@/components/ui/kbd";
import { type Badges, useNavBadges } from "@/layout/badges";
import {
  type Gate,
  itemHome,
  type NavGroup,
  type NavItem,
  navOf,
  readOpenGroups,
  visibleGroups,
  visibleItems,
  writeOpenGroups,
} from "@/layout/nav";
import { readPref, readUserPref, writePref, writeUserPref } from "@/lib/prefs";
import { keyLabel } from "@/lib/shortcuts";

const RAIL_PREF = "nav.rail";
/** 이 폭부터 사이드바를 펼친 채 시작한다. 그 아래는 아이콘 레일 + 펼침 창이다(계획 77 설계). */
const WIDE = "(min-width: 1280px)";

/** 접은 레일인지. 사용자가 고른 것을 기억하고, 고른 적이 없으면 화면 폭으로 정한다. */
export function readRail(): boolean {
  const saved = readPref(RAIL_PREF);
  if (saved === "1" || saved === "0") {
    return saved === "1";
  }
  return !window.matchMedia(WIDE).matches;
}

/** 레일 접기 상태. 머리줄 단축키 `[`와 사이드바 단추가 함께 쓴다. */
export function useRail() {
  const [rail, setRail] = useState(readRail);
  const toggle = () => {
    writePref(RAIL_PREF, rail ? "0" : "1");
    setRail(!rail);
  };
  return { rail, toggle };
}

/**
 * 펼친 묶음. 사용자별로 기억하고(`{user}.nav.open`), 지금 화면의 묶음은 늘 펼친 채 시작한다(기억이 없으면 그것만).
 * 다른 묶음의 화면으로 가면 그 묶음을 펼친다(지금 묶음을 접으면 다시 그 묶음에 들어올 때까지 닫혀 있다).
 */
function useOpenGroups(current: GroupId | undefined) {
  const { user } = useNavUser();
  const [state, setState] = useState(() => {
    const open = readOpenGroups((key) => readUserPref(user, key)) ?? new Set<GroupId>();
    return { open: current ? new Set([...open, current]) : open, current };
  });
  // 다른 묶음의 화면으로 오면 그 묶음을 펼친다(그리는 중에 고친다 — 효과로 하면 한 번 접힌 채 그린다).
  if (state.current !== current) {
    const open =
      current && !state.open.has(current) ? new Set([...state.open, current]) : state.open;
    setState({ open, current });
  }
  const toggle = (id: GroupId) => {
    const next = new Set(state.open);
    if (!next.delete(id)) {
      next.add(id);
    }
    writeOpenGroups((key, value) => writeUserPref(user, key, value), next);
    setState({ open: next, current });
  };
  return { open: state.open, toggle };
}

/** 항목 옆 숫자. 숫자 자체는 화면 읽기 프로그램에 숨기고 뜻을 글로 준다. */
function BadgeCount({ item, badges }: { item: NavItem; badges: Badges }) {
  const { t } = useTranslation();
  const n = item.badge ? badges[item.badge] : 0;
  if (n <= 0) {
    return null;
  }
  return (
    <>
      <span
        aria-hidden
        className={cn(
          "ml-auto flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] leading-none font-semibold tabular-nums",
          item.badge === "alerts"
            ? "bg-danger-fg text-background"
            : "bg-danger-soft text-danger-fg",
        )}
      >
        {n > 99 ? "99+" : n}
      </span>
      <span className="sr-only">
        {" "}
        ({t(item.badge === "alerts" ? "nav.badgeAlerts" : "nav.badgeFailedTasks", { n })})
      </span>
    </>
  );
}

function ItemLink({
  item,
  gate,
  current,
  badges,
  onNavigate,
}: {
  item: NavItem;
  gate: Gate;
  current: boolean;
  badges: Badges;
  onNavigate?(): void;
}) {
  const { t } = useTranslation();
  return (
    <Link
      to={itemHome(item, gate)}
      aria-current={current ? "page" : undefined}
      onClick={onNavigate}
      className={cn(
        "flex min-h-8 items-center gap-2 rounded-md py-1 pr-2 pl-9 text-sm text-sidebar-foreground/80 hover:bg-sidebar-accent hover:text-sidebar-foreground",
        "aria-[current=page]:bg-sidebar-accent aria-[current=page]:font-medium aria-[current=page]:text-brand",
      )}
    >
      <span className="min-w-0">{t(item.key)}</span>
      <BadgeCount item={item} badges={badges} />
    </Link>
  );
}

/**
 * 묶음 제목(아이콘+이름, 누르면 펼치고 접는다)과 그 아래 2단 항목들. 데스크톱 사이드바와 휴대폰 ☰ 서랍이 함께 쓴다.
 * 관리 묶음은 맨 아래에 따로 둔다.
 */
export function NavTree({ onNavigate }: { onNavigate?(): void }) {
  const { t } = useTranslation();
  const { gate } = useNavUser();
  const { pathname } = useLocation();
  const hit = navOf(pathname);
  const { open, toggle } = useOpenGroups(hit?.group.hidden ? undefined : hit?.group.id);
  const badges = useNavBadges();
  const groups = visibleGroups(gate);
  const section = (group: NavGroup) => {
    const Icon = GROUP_ICONS[group.id];
    const expanded = open.has(group.id);
    const listId = `nav-group-${group.id}`;
    const here = hit?.group.id === group.id;
    return (
      <li key={group.id}>
        <button
          type="button"
          aria-expanded={expanded}
          aria-controls={listId}
          onClick={() => toggle(group.id)}
          className={cn(
            "flex h-9 w-full items-center gap-3 rounded-md px-3 text-left text-sm font-medium text-sidebar-foreground hover:bg-sidebar-accent",
            here && !expanded && "text-brand",
          )}
        >
          <Icon className={cn(RAIL_ICON_SIZE, "shrink-0")} aria-hidden />
          <span className="min-w-0 flex-1">{t(group.key)}</span>
          <ChevronDown
            aria-hidden
            className={cn(
              "size-4 shrink-0 opacity-60 transition-transform duration-(--duration-normal)",
              !expanded && "-rotate-90",
            )}
          />
        </button>
        <ul id={listId} hidden={!expanded} className="flex flex-col gap-0.5 pb-1">
          {visibleItems(group, gate).map((item) => (
            <li key={item.id}>
              <ItemLink
                item={item}
                gate={gate}
                current={hit?.item === item}
                badges={badges}
                onNavigate={onNavigate}
              />
            </li>
          ))}
        </ul>
      </li>
    );
  };
  return (
    <nav
      aria-label={t("nav.main")}
      className="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto px-2 pb-2"
    >
      <ul className="flex flex-col gap-0.5">
        {groups.filter((group) => group.id !== "admin").map(section)}
      </ul>
      <ul className="mt-auto flex flex-col gap-0.5 border-t pt-2">
        {groups.filter((group) => group.id === "admin").map(section)}
      </ul>
    </nav>
  );
}

/**
 * 접은 레일: 묶음 아이콘마다 펼침 창(메뉴)이 그 묶음의 항목을 보인다. 메뉴라서 Enter·Space·↓로 열고 ↑·↓로 고르고
 * Esc면 아이콘으로 포커스가 돌아간다.
 */
function RailNav() {
  const { t } = useTranslation();
  const { gate } = useNavUser();
  const { pathname } = useLocation();
  const hit = navOf(pathname);
  const badges = useNavBadges();
  const groups = visibleGroups(gate);
  const icon = (group: NavGroup) => {
    const Icon = GROUP_ICONS[group.id];
    const items = visibleItems(group, gate);
    const here = hit?.group.id === group.id;
    const counted = items.some((item) => item.badge && badges[item.badge] > 0);
    return (
      <li key={group.id}>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              data-flyout-trigger
              data-current={here || undefined}
              aria-label={t("nav.groupMenu", { group: t(group.key) })}
              className={cn(
                "relative flex h-10 w-full items-center justify-center rounded-md text-sidebar-foreground/80 hover:bg-sidebar-accent hover:text-sidebar-foreground",
                "data-[current]:bg-sidebar-accent data-[current]:text-brand data-[state=open]:bg-sidebar-accent",
              )}
            >
              <Icon className={RAIL_ICON_SIZE} aria-hidden />
              {counted && (
                <span
                  aria-hidden
                  className="absolute top-1.5 right-2.5 size-2 rounded-full bg-danger-fg"
                />
              )}
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent side="right" align="start" className="min-w-56">
            <DropdownMenuLabel>{t(group.key)}</DropdownMenuLabel>
            {items.map((item) => (
              <DropdownMenuItem key={item.id} asChild>
                <Link
                  to={itemHome(item, gate)}
                  aria-current={hit?.item === item ? "page" : undefined}
                  className="aria-[current=page]:font-medium aria-[current=page]:text-brand"
                >
                  <span className="min-w-0 flex-1">{t(item.key)}</span>
                  <BadgeCount item={item} badges={badges} />
                </Link>
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      </li>
    );
  };
  return (
    <nav aria-label={t("nav.main")} className="flex min-h-0 flex-1 flex-col gap-1 px-2 pb-2">
      <ul className="flex flex-col gap-1">
        {groups.filter((group) => group.id !== "admin").map(icon)}
      </ul>
      <ul className="mt-auto flex flex-col gap-1 border-t pt-2">
        {groups.filter((group) => group.id === "admin").map(icon)}
      </ul>
    </nav>
  );
}

/** 사이드바 맨 위의 "찾기·명령 ⌘K"(명령 팔레트를 연다). 레일에서는 아이콘 단추다. */
export function SidebarSearch({ rail = false, onOpen }: { rail?: boolean; onOpen(): void }) {
  const { t } = useTranslation();
  const Icon = ACTION_ICONS.search;
  return (
    <div className={cn("px-2 pb-2", rail && "flex justify-center")}>
      <Button
        variant="outline"
        size={rail ? "icon" : "default"}
        onClick={onOpen}
        aria-label={t("palette.open")}
        aria-keyshortcuts="Meta+K Control+K"
        className={cn("text-muted-foreground", !rail && "w-full justify-start")}
      >
        <Icon />
        {!rail && (
          <>
            <span>{t("nav.search")}</span>
            <Kbd className="ml-auto">{keyLabel("mod+k")}</Kbd>
          </>
        )}
      </Button>
    </div>
  );
}

/** 768px 이상에서 보이는 왼쪽 사이드바. 펼치면 240px 2단 트리, 접으면 56px 아이콘 레일 + 펼침 창이다. */
export function Sidebar({
  rail,
  onToggle: toggle,
  onSearch,
}: {
  rail: boolean;
  onToggle(): void;
  onSearch(): void;
}) {
  const { t } = useTranslation();
  const Chevron = rail ? ChevronsRight : ChevronsLeft;
  return (
    <aside
      data-rail={rail || undefined}
      className={cn(
        "sticky top-0 hidden h-svh shrink-0 flex-col border-r bg-sidebar text-sidebar-foreground transition-[width] duration-(--duration-normal) ease-out md:flex",
        rail ? "w-14" : "w-60",
      )}
    >
      <div
        className={cn(
          "flex h-14 shrink-0 items-center px-4 text-lg font-semibold",
          rail && "justify-center px-0",
        )}
      >
        {rail ? (
          <>
            <span aria-hidden>O</span>
            <span className="sr-only">Orbivisor</span>
          </>
        ) : (
          "Orbivisor"
        )}
      </div>
      <SidebarSearch rail={rail} onOpen={onSearch} />
      {rail ? <RailNav /> : <NavTree />}
      <div className={cn("flex shrink-0 px-2 pb-2", rail ? "justify-center" : "justify-end")}>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={rail ? t("nav.expand") : t("nav.collapse")}
          aria-keyshortcuts="["
          onClick={toggle}
        >
          <Chevron />
        </Button>
      </div>
    </aside>
  );
}
