import { Lock, Menu, Monitor, Moon, Sun, User, WifiOff } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Outlet, useLocation, useNavigate } from "react-router";

import { ChangePasswordDialog } from "@/accounts/ChangePasswordDialog";
import { useLiveStatus } from "@/api/LiveProvider";
import { useAuth, usePrincipal } from "@/auth/AuthProvider";
import { ACTION_ICONS } from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Sheet, SheetContent, SheetTitle, SheetTrigger } from "@/components/ui/sheet";
import { type Language, setLanguage } from "@/i18n";
import { AreaTabs, ViewSwitch } from "@/layout/AreaTabs";
import { Breadcrumbs } from "@/layout/Breadcrumbs";
import { ClusterSwitch, CreateMenu, SearchButton } from "@/layout/HeaderTools";
import { AREAS, crumbsOf, isFullWidth, lastTab, rememberTab } from "@/layout/nav";
import { NavProgress } from "@/layout/RouteError";
import { NotificationBell } from "@/layout/NotificationBell";
import { ShortcutsHelp } from "@/layout/ShortcutsHelp";
import { AreaLinks, Sidebar, useRail } from "@/layout/Sidebar";
import { type Density, useDensity } from "@/lib/density";
import { type ShortcutId, useShortcuts } from "@/lib/shortcuts";
import { type Theme, useTheme } from "@/lib/theme";
import { CommandPalette } from "@/palette/CommandPalette";
import { setCenterOpen } from "@/tasks/center";

/**
 * 앱의 틀: 왼쪽 영역 사이드바(설계 §4), 머리줄(☰·이동 경로·검색·+ 만들기·클러스터 전환·종·실시간·사용자 메뉴), 본문 위
 * 영역 탭과 보기 전환, ⌘K 팔레트와 단축키(§5.2·§5.7). 경로가 바뀌면 그 영역의 마지막 탭을 기억하고 본문 제목으로
 * 포커스를 옮긴다.
 */
export function AppLayout() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const navigate = useNavigate();
  const { pathname, search } = useLocation();
  const main = useRef<HTMLElement>(null);
  const [announce, setAnnounce] = useState("");
  const full = isFullWidth(pathname);
  const { rail, toggle: toggleRail } = useRail();
  const [palette, setPalette] = useState(false);
  const [help, setHelp] = useState(false);
  const [creating, setCreating] = useState(false);
  // 팔레트를 연 곳(닫으면 돌아간다)
  const opener = useRef<HTMLElement | null>(null);
  const openPalette = () => {
    if (!palette) {
      opener.current =
        document.activeElement instanceof HTMLElement ? document.activeElement : null;
    }
    setPalette(!palette);
  };
  const goTo = Object.fromEntries(
    AREAS.map((area) => [`go.${area.id}`, () => void navigate(lastTab(area, me))]),
  ) as Partial<Record<ShortcutId, () => void>>;
  // 콘솔 화면에서는 키를 모두 게스트에 넘긴다.
  useShortcuts(
    {
      ...goTo,
      palette: openPalette,
      help: () => setHelp(true),
      rail: toggleRail,
      create: () => setCreating(true),
      search: () => main.current?.querySelector<HTMLElement>("[data-list-search]")?.focus(),
      favorite: () => main.current?.querySelector<HTMLElement>("[data-pin-toggle]")?.click(),
      center: () => setCenterOpen(true),
    },
    !full,
  );

  useEffect(() => rememberTab(me.user, pathname, search), [me.user, pathname, search]);

  // 첫 화면은 그대로 두고, 화면이 바뀔 때만 본문 제목(없으면 본문)으로 포커스를 옮기고 화면 이름을 읽어 준다.
  const shown = useRef(pathname);
  useEffect(() => {
    if (shown.current === pathname) {
      return;
    }
    shown.current = pathname;
    const target = main.current?.querySelector("h1") ?? main.current;
    if (target) {
      target.setAttribute("tabindex", "-1");
      target.focus({ preventScroll: true });
    }
    setAnnounce(
      crumbsOf(pathname, me, t)
        .map((crumb) => crumb.label)
        .join(" › "),
    );
    // 화면 이름은 경로가 바뀔 때만 읽는다(언어를 바꿨다고 다시 읽지 않는다).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pathname]);

  return (
    <div className="flex min-h-svh">
      <NavProgress />
      <a
        href="#main"
        className="sr-only z-50 rounded-md bg-background px-3 py-2 text-sm focus:not-sr-only focus:fixed focus:top-2 focus:left-2 focus:ring-2 focus:ring-ring"
      >
        {t("nav.skipToContent")}
      </a>
      <Sidebar rail={rail} onToggle={toggleRail} />
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 items-center justify-between gap-2 border-b px-4">
          <div className="flex min-w-0 items-center gap-2">
            <MobileNav />
            <Breadcrumbs />
          </div>
          <div className="flex shrink-0 items-center gap-1">
            <SearchButton onOpen={openPalette} />
            <CreateMenu open={creating} onOpenChange={setCreating} />
            <ClusterSwitch />
            <NotificationBell />
            <LiveBadge />
            <UserMenu onHelp={() => setHelp(true)} />
          </div>
        </header>
        <main
          id="main"
          ref={main}
          tabIndex={-1}
          className="flex min-w-0 flex-1 flex-col gap-(--gap) p-4 outline-none md:p-6"
        >
          <OfflineBanner />
          {!full && <AreaTabs />}
          {!full && <ViewSwitch />}
          <div className="min-w-0">
            <Outlet />
          </div>
        </main>
        <div aria-live="polite" className="sr-only">
          {announce}
        </div>
        <CommandPalette
          open={palette}
          onOpenChange={setPalette}
          returnFocus={opener}
          onHelp={() => setHelp(true)}
        />
        <ShortcutsHelp open={help} onOpenChange={setHelp} />
      </div>
    </div>
  );
}

/** 768px 아래: 사이드바 대신 머리줄의 ☰가 영역 링크 서랍을 연다. */
function MobileNav() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <Sheet open={open} onOpenChange={setOpen}>
      <SheetTrigger asChild>
        <Button variant="ghost" size="icon" className="md:hidden" aria-label={t("nav.openMenu")}>
          <Menu />
        </Button>
      </SheetTrigger>
      <SheetContent side="left" className="w-64 gap-0 p-0" aria-describedby={undefined}>
        <SheetTitle className="flex h-14 items-center px-4 text-lg">Orbivisor</SheetTitle>
        <AreaLinks onNavigate={() => setOpen(false)} />
      </SheetContent>
    </Sheet>
  );
}

/**
 * 실시간 연결이 끊긴 동안 본문 위의 띠(설계 §5.5). 화면 읽기 프로그램에는 머리줄 `LiveBadge`(role=status)가
 * 이미 알리므로 여기는 역할을 두지 않는다.
 */
function OfflineBanner() {
  const { t } = useTranslation();
  if (useLiveStatus() !== "offline") {
    return null;
  }
  return (
    <div
      data-offline-banner
      className="flex items-center gap-2 rounded-md bg-warning-soft px-3 py-2 text-sm text-warning-fg"
    >
      <WifiOff aria-hidden className="size-4 shrink-0" />
      {t("live.banner")}
    </div>
  );
}

/** 실시간 연결이 끊겼으면 화면의 내용이 오래됐을 수 있다고 알린다. */
function LiveBadge() {
  const { t } = useTranslation();
  const status = useLiveStatus();
  const color =
    status === "live" ? "bg-success" : status === "offline" ? "bg-danger" : "bg-warning";
  return (
    <span role="status" className="flex items-center gap-2 px-2 text-xs text-muted-foreground">
      <span className={`size-2 rounded-full ${color}`} />
      <span className={status === "live" ? "sr-only sm:not-sr-only" : ""}>
        {t(`live.${status}`)}
      </span>
    </span>
  );
}

function UserMenu({ onHelp }: { onHelp(): void }) {
  const { t } = useTranslation();
  const { logout } = useAuth();
  const me = usePrincipal();
  const { density, setDensity } = useDensity();
  const [changing, setChanging] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  return (
    <>
      {/* 메뉴에서 대화상자를 열므로 모달로 두지 않는다(닫힌 메뉴가 화면 클릭을 막지 않게). */}
      <DropdownMenu modal={false}>
        <DropdownMenuTrigger asChild>
          <Button ref={trigger} variant="ghost" aria-label={`${t("header.account")} (${me.name})`}>
            <User />
            <span className="hidden sm:inline">{me.name}</span>
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuLabel>
            {me.name} · {t(`roles.${me.role}`)}
          </DropdownMenuLabel>
          <DropdownMenuSeparator />
          <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
            {t("header.density")}
          </DropdownMenuLabel>
          <DropdownMenuRadioGroup
            aria-label={t("header.density")}
            value={density}
            onValueChange={(v) => setDensity(v as Density)}
          >
            <DropdownMenuRadioItem value="comfortable">
              {t("header.densityComfortable")}
            </DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="compact">
              {t("header.densityCompact")}
            </DropdownMenuRadioItem>
          </DropdownMenuRadioGroup>
          <LanguageItems />
          <ThemeItems />
          <DropdownMenuItem onSelect={onHelp}>
            <ACTION_ICONS.shortcuts />
            {t("shortcuts.title")}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          {/* 외부 사용자(이름에 `@<영역>`)의 비밀번호는 영역의 디렉터리가 맡는다. */}
          {!me.name.includes("@") && (
            <DropdownMenuItem onSelect={() => setChanging(true)}>
              <Lock />
              {t("accounts.changePassword")}
            </DropdownMenuItem>
          )}
          <DropdownMenuItem onSelect={() => void logout()}>
            <ACTION_ICONS.logout />
            {t("header.logout")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {changing && (
        <ChangePasswordDialog returnFocus={trigger} onClose={() => setChanging(false)} />
      )}
    </>
  );
}

/** 사용자 메뉴의 언어 고르기 */
function LanguageItems() {
  const { t, i18n } = useTranslation();
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        <ACTION_ICONS.language />
        {t("header.language")}
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent>
        <DropdownMenuRadioGroup
          aria-label={t("header.language")}
          value={i18n.language}
          onValueChange={(v) => setLanguage(v as Language)}
        >
          <DropdownMenuRadioItem value="ko" lang="ko">
            한국어
          </DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="en" lang="en">
            English
          </DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}

/** 사용자 메뉴의 테마 고르기 */
function ThemeItems() {
  const { t } = useTranslation();
  const { theme, setTheme } = useTheme();
  const Icon = theme === "dark" ? Moon : theme === "light" ? Sun : Monitor;
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        <Icon />
        {t("header.theme")}
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent>
        <DropdownMenuRadioGroup
          aria-label={t("header.theme")}
          value={theme}
          onValueChange={(v) => setTheme(v as Theme)}
        >
          <DropdownMenuRadioItem value="system">{t("header.themeSystem")}</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="light">{t("header.themeLight")}</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="dark">{t("header.themeDark")}</DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}
