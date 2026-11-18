import { cn } from "cn";
import { ChevronRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Link, useLocation } from "react-router";

import { usePrincipal } from "@/auth/AuthProvider";
import { crumbsOf } from "@/layout/nav";

/** 머리줄 왼쪽의 `영역 › 탭 › 리소스`. 768px 아래에서는 마지막 둘만 보인다. */
export function Breadcrumbs() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const { pathname } = useLocation();
  const crumbs = crumbsOf(pathname, me.role, t);
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
              i < crumbs.length - 2 && "hidden md:flex",
            )}
          >
            {i > 0 && (
              <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
            )}
            {crumb.to ? (
              <Link
                to={crumb.to}
                className="truncate text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
              >
                {crumb.label}
              </Link>
            ) : (
              <span aria-current="page" className="truncate font-medium">
                {crumb.label}
              </span>
            )}
          </li>
        ))}
      </ol>
    </nav>
  );
}
