import { cn } from "cn";
import { ChevronsLeft, ChevronsRight } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useLocation } from "react-router";

import { usePrincipal } from "@/auth/AuthProvider";
import { AREA_ICONS, RAIL_ICON_SIZE } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { areaOf, lastTab, type NavArea, visibleAreas } from "@/layout/nav";
import { readPref, writePref } from "@/lib/prefs";

const RAIL_PREF = "nav.rail";
/** 이 폭부터 사이드바를 펼친 채 시작한다(설계 §5.9). 그 아래는 아이콘 레일이다. */
const WIDE = "(min-width: 1280px)";

/** 접은 레일인지. 사용자가 고른 것을 기억하고, 고른 적이 없으면 화면 폭으로 정한다. */
export function readRail(): boolean {
  const saved = readPref(RAIL_PREF);
  if (saved === "1" || saved === "0") {
    return saved === "1";
  }
  return !window.matchMedia(WIDE).matches;
}

/** 영역 링크들. 데스크톱 사이드바와 휴대폰 ☰ 서랍이 함께 쓴다. 설정은 맨 아래에 고정한다. */
export function AreaLinks({ rail = false, onNavigate }: { rail?: boolean; onNavigate?(): void }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const { pathname } = useLocation();
  const current = areaOf(pathname)?.area.id;
  const areas = visibleAreas(me.role);
  const item = (area: NavArea) => {
    const Icon = AREA_ICONS[area.id];
    const label = t(area.key);
    const link = (
      <Link
        to={lastTab(area, me)}
        aria-current={current === area.id ? "page" : undefined}
        onClick={onNavigate}
        className={cn(
          "flex h-10 items-center gap-3 rounded-md px-3 text-sm whitespace-nowrap text-sidebar-foreground/80 hover:bg-sidebar-accent hover:text-sidebar-foreground",
          "aria-[current=page]:bg-sidebar-accent aria-[current=page]:font-medium aria-[current=page]:text-brand",
          rail && "justify-center px-0",
        )}
      >
        <Icon className={RAIL_ICON_SIZE} aria-hidden />
        <span className={rail ? "sr-only" : undefined}>{label}</span>
      </Link>
    );
    return (
      <li key={area.id}>
        {rail ? (
          <Tooltip>
            <TooltipTrigger asChild>{link}</TooltipTrigger>
            <TooltipContent side="right">{label}</TooltipContent>
          </Tooltip>
        ) : (
          link
        )}
      </li>
    );
  };
  return (
    <nav aria-label={t("nav.main")} className="flex min-h-0 flex-1 flex-col gap-1 px-2 pb-2">
      <ul className="flex flex-col gap-1">
        {areas.filter((area) => area.id !== "settings").map(item)}
      </ul>
      <ul className="mt-auto flex flex-col gap-1 border-t pt-2">
        {areas.filter((area) => area.id === "settings").map(item)}
      </ul>
    </nav>
  );
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

/** 768px 이상에서 보이는 왼쪽 사이드바. 펼치면 224px, 접으면 56px 아이콘 레일이다. */
export function Sidebar({ rail, onToggle: toggle }: { rail: boolean; onToggle(): void }) {
  const { t } = useTranslation();
  const Chevron = rail ? ChevronsRight : ChevronsLeft;
  return (
    <aside
      data-rail={rail || undefined}
      className={cn(
        "sticky top-0 hidden h-svh shrink-0 flex-col border-r bg-sidebar text-sidebar-foreground transition-[width] duration-(--duration-normal) ease-out md:flex",
        rail ? "w-14" : "w-56",
      )}
    >
      <div
        className={cn(
          "flex h-14 items-center px-4 text-lg font-semibold",
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
      <AreaLinks rail={rail} />
      <div className={cn("flex px-2 pb-2", rail ? "justify-center" : "justify-end")}>
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
