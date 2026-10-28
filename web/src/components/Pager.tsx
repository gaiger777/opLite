import { cn } from "cn";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";

/**
 * `items`를 `pageSize`개씩 나눈다. `page`는 0부터 센다. 항목이 줄어 지금 페이지가 없어지면 마지막
 * 페이지로 옮긴다.
 */
export function usePage<T>(items: readonly T[], pageSize: number) {
  const [page, setPage] = useState(0);
  const pageCount = Math.ceil(items.length / pageSize);
  const last = Math.max(pageCount - 1, 0);
  if (page > last) {
    // 렌더 중에 상태를 맞춘다(React가 바로 다시 그린다).
    setPage(last);
  }
  const current = Math.min(page, last);
  return {
    page: current,
    pageItems: items.slice(current * pageSize, (current + 1) * pageSize),
    pageCount,
    setPage,
  };
}

/**
 * "이전/다음" 버튼과 지금 보이는 범위. 항목이 없으면 그리지 않는다. 끝에 닿은 버튼은 `disabled` 대신
 * `aria-disabled`로 막아 누른 버튼이 포커스를 잃지 않게 한다.
 */
export function Pager({
  page,
  pageCount,
  setPage,
  pageSize,
  total,
}: {
  page: number;
  pageCount: number;
  setPage(page: number): void;
  pageSize: number;
  total: number;
}) {
  const { t } = useTranslation();
  if (total === 0) {
    return null;
  }
  const from = page * pageSize + 1;
  const to = Math.min((page + 1) * pageSize, total);
  const first = page === 0;
  const last = page >= pageCount - 1;
  return (
    <nav aria-label={t("pager.label")} className="flex items-center justify-end gap-2">
      <span className="text-sm text-muted-foreground">{t("pager.range", { from, to, total })}</span>
      <Button
        variant="outline"
        size="sm"
        aria-disabled={first || undefined}
        className={cn(first && "opacity-50")}
        onClick={() => !first && setPage(page - 1)}
      >
        {t("pager.previous")}
      </Button>
      <Button
        variant="outline"
        size="sm"
        aria-disabled={last || undefined}
        className={cn(last && "opacity-50")}
        onClick={() => !last && setPage(page + 1)}
      >
        {t("pager.next")}
      </Button>
    </nav>
  );
}
