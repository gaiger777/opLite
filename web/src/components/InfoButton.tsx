import { cn } from "cn";
import { Info } from "lucide-react";
import { useTranslation } from "react-i18next";

import { type HelpTopic, openHelp } from "@/help/help";

/**
 * 칸 이름·화면 제목 옆의 ⓘ. 누르면 오른쪽 도움말 패널(무엇·어떻게 동작·언제 쓰나·그림)이 그 주제로 열린다(진단 §2.0).
 * 이름은 "〈칸〉 도움말"이다.
 */
export function InfoButton({ className, ...topic }: HelpTopic & { className?: string }) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      data-info-button
      aria-label={t("help.open", { name: topic.label })}
      aria-haspopup="dialog"
      onClick={() => openHelp(topic)}
      className={cn(
        "inline-flex size-5 shrink-0 items-center justify-center rounded-full text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
        className,
      )}
    >
      <Info aria-hidden className="size-3.5" />
    </button>
  );
}
