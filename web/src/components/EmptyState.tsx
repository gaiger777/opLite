import { cn } from "cn";
import type { LucideIcon } from "lucide-react";
import { useTranslation } from "react-i18next";

/**
 * 목록이 비었을 때: 아이콘, 한 줄 설명, 주 단추, 안내서 링크(설계 §5.6). 단추는 부르는 쪽이 권한(`can()`)을
 * 보고 넘긴다.
 */
export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
  docsHref,
  className,
}: {
  icon?: LucideIcon;
  title: string;
  description?: string;
  /** 주 단추(예: 만들기). 권한이 없으면 넘기지 않는다. */
  action?: React.ReactNode;
  /** `docs/guide`의 해당 절 주소 */
  docsHref?: string;
  className?: string;
}) {
  const { t } = useTranslation();
  return (
    <div
      data-slot="empty-state"
      className={cn(
        "flex flex-col items-center gap-3 rounded-lg border border-dashed px-6 py-10 text-center",
        className,
      )}
    >
      {Icon && (
        <span className="flex size-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
          <Icon aria-hidden className="size-5" />
        </span>
      )}
      <div className="flex flex-col gap-1">
        <p className="font-medium">{title}</p>
        {description && <p className="text-muted-foreground">{description}</p>}
      </div>
      {action}
      {docsHref && (
        <a
          href={docsHref}
          target="_blank"
          rel="noreferrer"
          className="text-xs text-brand underline-offset-4 hover:underline"
        >
          {t("empty.docs")}
        </a>
      )}
    </div>
  );
}
