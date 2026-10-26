import { useQuery } from "@tanstack/react-query";
import {
  Cpu,
  Database,
  Languages,
  LayoutDashboard,
  ListChecks,
  LogOut,
  Monitor,
  Moon,
  Network,
  Server,
  Sun,
  User,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { NavLink, Outlet } from "react-router";

import { api, call } from "@/api/client";
import { useLiveStatus } from "@/api/LiveProvider";
import { useAuth, usePrincipal } from "@/auth/AuthProvider";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { type Language, setLanguage } from "@/i18n";
import { type Theme, useTheme } from "@/lib/theme";

const NAV = [
  { to: "/", key: "nav.dashboard", icon: LayoutDashboard, end: true },
  { to: "/vms", key: "nav.vms", icon: Server, end: false },
  { to: "/storage-pools", key: "nav.pools", icon: Database, end: false },
  { to: "/networks", key: "nav.networks", icon: Network, end: false },
  { to: "/nodes", key: "nav.nodes", icon: Cpu, end: false },
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
] as const;

export function AppLayout() {
  const { t } = useTranslation();
  const cluster = useQuery({
    queryKey: ["cluster"],
    queryFn: () => call(api.GET("/cluster")),
  });
  return (
    <div className="flex min-h-svh flex-col md:flex-row">
      <aside className="border-b bg-sidebar text-sidebar-foreground md:w-56 md:border-r md:border-b-0">
        <div className="px-4 py-3 text-lg font-semibold">opLite</div>
        <nav className="flex gap-1 overflow-x-auto px-2 pb-2 md:flex-col">
          {NAV.map(({ to, key, icon: Icon, end }) => (
            <NavLink
              key={to}
              to={to}
              end={end}
              className={({ isActive }) =>
                `flex items-center gap-2 rounded-md px-3 py-2 text-sm whitespace-nowrap hover:bg-sidebar-accent ${
                  isActive ? "bg-sidebar-accent font-medium" : ""
                }`
              }
            >
              <Icon className="size-4" />
              {t(key)}
            </NavLink>
          ))}
        </nav>
      </aside>
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 items-center justify-between gap-2 border-b px-4">
          <span className="truncate text-sm text-muted-foreground">{cluster.data?.name}</span>
          <div className="flex items-center gap-1">
            <LiveBadge />
            <LanguageMenu />
            <ThemeMenu />
            <UserMenu />
          </div>
        </header>
        <main className="min-w-0 flex-1 p-4 md:p-6">
          <Outlet />
        </main>
      </div>
    </div>
  );
}

/** 실시간 연결이 끊겼으면 화면의 내용이 오래됐을 수 있다고 알린다. */
function LiveBadge() {
  const { t } = useTranslation();
  const status = useLiveStatus();
  const color =
    status === "live" ? "bg-emerald-500" : status === "offline" ? "bg-destructive" : "bg-amber-500";
  return (
    <span role="status" className="flex items-center gap-2 px-2 text-xs text-muted-foreground">
      <span className={`size-2 rounded-full ${color}`} />
      <span className={status === "live" ? "sr-only sm:not-sr-only" : ""}>
        {t(`live.${status}`)}
      </span>
    </span>
  );
}

function LanguageMenu() {
  const { t, i18n } = useTranslation();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={t("header.language")}>
          <Languages />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuRadioGroup
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
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function ThemeMenu() {
  const { t } = useTranslation();
  const { theme, setTheme } = useTheme();
  const Icon = theme === "dark" ? Moon : theme === "light" ? Sun : Monitor;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={t("header.theme")}>
          <Icon />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuRadioGroup value={theme} onValueChange={(v) => setTheme(v as Theme)}>
          <DropdownMenuRadioItem value="system">{t("header.themeSystem")}</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="light">{t("header.themeLight")}</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="dark">{t("header.themeDark")}</DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function UserMenu() {
  const { t } = useTranslation();
  const { logout } = useAuth();
  const me = usePrincipal();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" aria-label={`${t("header.account")} (${me.name})`}>
          <User />
          <span className="hidden sm:inline">{me.name}</span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>
          {me.name} · {t(`roles.${me.role}`)}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={() => void logout()}>
          <LogOut />
          {t("header.logout")}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
