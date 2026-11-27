import { useQuery } from "@tanstack/react-query";
import { cn } from "cn";
import { ChevronRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Link, useLocation } from "react-router";

import { fetchList, listKey, useList } from "@/api/live";
import { useNavUser } from "@/auth/usePermissions";
import { FOLDER_PATHS } from "@/folders/folders";
import { crumbsOf } from "@/layout/nav";

/** 머리줄 왼쪽의 `묶음 › 항목 › 리소스`. 묶음 이름은 링크가 아니다. 768px 아래에서는 마지막 둘만 보인다. */
export function Breadcrumbs() {
  const { t } = useTranslation();
  const me = useNavUser();
  const { pathname } = useLocation();
  const folders = useList("folder");
  // 백업 파일 찾아보기(계획 69)의 백업 id는 백업 이름으로 보인다. 그 화면에서만, 백업을 읽을 수 있을 때만 백업 목록을 읽는다
  // (화면이 이미 읽는다). 읽을 수 없으면 id 그대로다.
  const onBackup = pathname.startsWith("/backups/") && me.gate.reads("backup");
  const backups = useQuery({
    queryKey: listKey("backup"),
    queryFn: () => fetchList("backup"),
    staleTime: Infinity,
    select: (list) => list.items,
    enabled: onBackup,
  });
  // 폴더 상세 주소의 id는 폴더 이름으로 보인다.
  const crumbs = crumbsOf(pathname, me.gate, t, (prefix, segment) =>
    Object.values(FOLDER_PATHS).includes(prefix)
      ? folders.data?.find((f) => f.id === segment)?.spec.name
      : prefix === "/backups"
        ? backups.data?.find((b) => b.id === segment)?.name
        : undefined,
  );
  if (crumbs.length === 0) {
    return null;
  }
  return (
    <nav aria-label={t("nav.breadcrumb")} className="min-w-0">
      <ol className="flex min-w-0 items-center gap-1 text-sm">
        {crumbs.map((crumb, i) => (
          <li
            key={i}
            className={cn(
              "flex min-w-0 items-center gap-1",
              // 휴대폰은 지금 화면 이름 하나만(계획 77-6 — "Settin… › Gene…"처럼 둘 다 잘리던 것)
              i < crumbs.length - 1 && "hidden md:flex",
            )}
          >
            {i > 0 && (
              <ChevronRight
                className={cn(
                  "size-3.5 shrink-0 text-muted-foreground",
                  i === crumbs.length - 1 && "max-md:hidden",
                )}
                aria-hidden
              />
            )}
            {crumb.to ? (
              <Link
                to={crumb.to}
                className="truncate text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
              >
                {crumb.label}
              </Link>
            ) : i === crumbs.length - 1 ? (
              <span aria-current="page" className="truncate font-medium">
                {crumb.label}
              </span>
            ) : (
              <span className="truncate text-muted-foreground">{crumb.label}</span>
            )}
          </li>
        ))}
      </ol>
    </nav>
  );
}
