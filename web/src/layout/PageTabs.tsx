import { cn } from "cn";
import { FolderTree, List } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Link, useLocation } from "react-router";

import { usePermissions } from "@/auth/usePermissions";
import { navOf, onListPage, visiblePages } from "@/layout/nav";

/**
 * 화면 안 탭: 사이드바 항목 하나에 화면이 둘 이상일 때(라우터 | 공인 IP) 그 목록 화면 위에 보인다. 경로가 바뀌므로
 * `role=tablist`가 아니라 `<nav>`이고 지금 화면은 `aria-current="page"`다. 상세 화면과 보이는 화면이 하나뿐이면 없다.
 * 밑줄은 목록 안쪽 그림자다(스크롤 상자에 테두리를 겹치면 1px 넘쳐 세로 스크롤바가 생겼다).
 */
export function PageTabs() {
  const { t } = useTranslation();
  const gate = usePermissions();
  const { pathname } = useLocation();
  const hit = navOf(pathname);
  if (!hit?.page || !onListPage(pathname, hit)) {
    return null;
  }
  const pages = visiblePages(hit.item, gate);
  if (pages.length <= 1) {
    return null;
  }
  return (
    <nav aria-label={t("nav.pageTabs", { item: t(hit.item.key) })} className="overflow-x-auto">
      <ul className="flex min-w-max gap-1 shadow-[inset_0_-1px_0_var(--color-border)]">
        {pages.map((page) => (
          <li key={page.to}>
            <Link
              to={page.to}
              aria-current={page === hit.page ? "page" : undefined}
              className={cn(
                "flex h-10 items-center border-b-2 border-transparent px-3 text-sm whitespace-nowrap text-muted-foreground hover:text-foreground",
                "aria-[current=page]:border-brand aria-[current=page]:font-medium aria-[current=page]:text-foreground",
              )}
            >
              {t(page.key)}
            </Link>
          </li>
        ))}
      </ul>
    </nav>
  );
}

/**
 * 같은 목록의 다른 모양(목록 | 폴더). 목록 화면 머리의 오른쪽 위 아이콘 두 개다(VM·스토리지 풀·네트워크). 이 항목에
 * 폴더 모양이 없거나 목록 화면이 아니면 없다.
 */
export function ViewToggle() {
  const { t } = useTranslation();
  const { pathname } = useLocation();
  const hit = navOf(pathname);
  const folder = hit?.item.folder;
  if (!hit || !folder || !onListPage(pathname, hit)) {
    return null;
  }
  const choices = [
    { to: hit.item.pages[0]!.to, label: t("nav.listView"), Icon: List, current: !hit.folder },
    { to: folder.to, label: t("nav.folderView"), Icon: FolderTree, current: hit.folder },
  ];
  return (
    <nav aria-label={t("nav.viewAs")}>
      <ul className="inline-flex rounded-md bg-muted p-0.5">
        {choices.map(({ to, label, Icon, current }) => (
          <li key={to}>
            <Link
              to={to}
              aria-label={label}
              title={label}
              aria-current={current ? "page" : undefined}
              className={cn(
                "flex size-8 items-center justify-center rounded-[calc(var(--radius)-2px)] text-muted-foreground hover:text-foreground",
                "aria-[current=page]:bg-background aria-[current=page]:text-foreground aria-[current=page]:shadow-sm",
              )}
            >
              <Icon className="size-4" aria-hidden />
            </Link>
          </li>
        ))}
      </ul>
    </nav>
  );
}
