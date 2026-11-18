import { cn } from "cn";
import { useTranslation } from "react-i18next";
import { Link, useLocation } from "react-router";

import { usePermissions } from "@/auth/usePermissions";
import { areaOf, visibleTabs } from "@/layout/nav";

/**
 * 영역 탭: 본문 위의 링크 목록이다. 경로가 바뀌므로 `role=tablist`가 아니라 `<nav>`이고 지금 탭은
 * `aria-current="page"`다. 좁은 화면에서는 가로로 스크롤한다. 보이는 탭이 하나뿐이면 그리지 않는다.
 */
export function AreaTabs() {
  const { t } = useTranslation();
  const gate = usePermissions();
  const { pathname } = useLocation();
  const hit = areaOf(pathname);
  if (!hit) {
    return null;
  }
  const tabs = visibleTabs(hit.area, gate);
  if (tabs.length <= 1) {
    return null;
  }
  return (
    <nav
      aria-label={t("nav.areaMenu", { area: t(hit.area.key) })}
      className="-mx-1 overflow-x-auto border-b"
    >
      <ul className="flex min-w-max gap-1 px-1">
        {tabs.map((tab) => (
          <li key={tab.to}>
            <Link
              to={tab.to}
              aria-current={tab === hit.tab ? "page" : undefined}
              className={cn(
                "-mb-px flex h-10 items-center border-b-2 border-transparent px-3 text-sm whitespace-nowrap text-muted-foreground hover:text-foreground",
                "aria-[current=page]:border-brand aria-[current=page]:font-medium aria-[current=page]:text-foreground",
              )}
            >
              {t(tab.key)}
            </Link>
          </li>
        ))}
      </ul>
    </nav>
  );
}

/** 탭 안의 보기 전환(segmented). 보기가 있는 탭의 목록 화면에서만 보인다(상세 화면에는 없다). */
export function ViewSwitch() {
  const { t } = useTranslation();
  const { pathname } = useLocation();
  const hit = areaOf(pathname);
  const views = hit?.tab.views;
  if (!hit || !views || !hit.view || pathname.replace(/\/+$/, "") !== hit.prefix) {
    return null;
  }
  return (
    <nav aria-label={t("nav.viewMenu", { tab: t(hit.tab.key) })}>
      <ul className="inline-flex rounded-md bg-muted p-0.5">
        {views.map((view) => (
          <li key={view.to}>
            <Link
              to={view.to}
              aria-current={view === hit.view ? "page" : undefined}
              className={cn(
                "flex h-8 items-center rounded-[calc(var(--radius)-2px)] px-3 text-sm whitespace-nowrap text-muted-foreground hover:text-foreground",
                "aria-[current=page]:bg-background aria-[current=page]:font-medium aria-[current=page]:text-foreground aria-[current=page]:shadow-sm",
              )}
            >
              {t(view.key)}
            </Link>
          </li>
        ))}
      </ul>
    </nav>
  );
}
