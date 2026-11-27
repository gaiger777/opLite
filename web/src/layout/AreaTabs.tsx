import { cn } from "cn";
import { useTranslation } from "react-i18next";
import { Link, useLocation } from "react-router";

import { usePermissions } from "@/auth/usePermissions";
import { areaOf, tabHome, viewVisible, visibleTabs } from "@/layout/nav";

/**
 * 영역 탭: 본문 위의 링크 목록이다. 경로가 바뀌므로 `role=tablist`가 아니라 `<nav>`이고 지금 탭은
 * `aria-current="page"`다. 좁은 화면에서는 가로로 스크롤한다. 보이는 탭이 하나뿐이면 그리지 않는다.
 * 밑줄은 `<nav>`의 테두리가 아니라 목록 안쪽 그림자다: 스크롤 상자(`overflow-x-auto`는 세로도 auto)에서 탭이 `-mb-px`로
 * 테두리에 겹치면 1px 넘쳐 탭 줄 옆에 세로 스크롤바가 생겼다.
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
      className="-mx-1 overflow-x-auto"
    >
      <ul className="flex min-w-max gap-1 px-1 shadow-[inset_0_-1px_0_var(--color-border)]">
        {tabs.map((tab) => (
          <li key={tab.to}>
            <Link
              to={tabHome(tab, gate)}
              aria-current={tab === hit.tab ? "page" : undefined}
              className={cn(
                "flex h-10 items-center border-b-2 border-transparent px-3 text-sm whitespace-nowrap text-muted-foreground hover:text-foreground",
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
  const gate = usePermissions();
  const { pathname } = useLocation();
  const hit = areaOf(pathname);
  const views = hit?.tab.views?.filter((view) => viewVisible(view, gate));
  if (!hit || !views || !hit.view || pathname.replace(/\/+$/, "") !== hit.prefix) {
    return null;
  }
  return (
    // 보기가 많으면(가져오기 탭 넷, 네트워크 탭 일곱) 휴대폰 폭에서 문서가 아니라 이 줄만 가로로 넘긴다(계획 72).
    <nav
      aria-label={t("nav.viewMenu", { tab: t(hit.tab.key) })}
      className="max-w-full overflow-x-auto"
    >
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
