import { cn } from "cn";
import type { LucideIcon } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import { Diagram } from "@/help/Diagram";
import type { DiagramId } from "@/help/diagramIds";
import { openHelp } from "@/help/help";

/**
 * 목록이 비었을 때: 그림(또는 아이콘), 한 문장 설명, 주 단추, "자세히"(도움말 패널)·안내서 링크(설계 §5.6, 계획 77-4).
 * 단추는 부르는 쪽이 권한(`can()`)을 보고 넘긴다. "아직 없음"에만 쓴다 — 거름 결과 0은 표가 따로 알린다.
 */
export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
  docsHref,
  diagram,
  help,
  className,
}: {
  icon?: LucideIcon;
  title: string;
  description?: string;
  /** 주 단추(예: 만들기). 권한이 없으면 넘기지 않는다. */
  action?: React.ReactNode;
  /** `docs/guide`의 해당 절 주소 */
  docsHref?: string;
  /** 무엇을 하는 것인지 보이는 작은 그림(늦게 읽기) */
  diagram?: DiagramId;
  /** "자세히"가 열 도움말 주제(`helpInfo.<help>` — 보통 그 화면의 `<화면>.page`) */
  help?: string;
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
      {diagram ? (
        <Diagram id={diagram} className="flex w-full max-w-sm justify-center" />
      ) : (
        Icon && (
          <span className="flex size-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
            <Icon aria-hidden className="size-5" />
          </span>
        )
      )}
      <div className="flex max-w-prose flex-col gap-1">
        <p className="font-medium">{title}</p>
        {description && <p className="text-muted-foreground">{description}</p>}
      </div>
      {action}
      {help && (
        <Button
          variant="link"
          size="sm"
          aria-haspopup="dialog"
          onClick={() => openHelp({ topic: help, label: t("help.aboutPage") })}
        >
          {t("help.more")}
        </Button>
      )}
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
