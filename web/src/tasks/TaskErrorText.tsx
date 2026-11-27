import { cn } from "cn";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

/** 이보다 길거나 여러 줄이면 2줄로 접고 "전체 보기"를 단다. */
const LONG = 120;

/**
 * Task 오류(성공 Task면 경고) 글(계획 77): 줄바꿈하며 2줄까지 보이고, 길면 "전체 보기"로 펼친다. 잘린 글을 마우스로만
 * 볼 수 있게(`title`) 두지 않는다. 원문 전체·복사는 상세 서랍에 있다. Task 밖의 오류 글(VMware 이전 항목·노드 전원
 * 차단 검사)도 같이 쓴다. `subject`(무엇의 오류인지)를 주면 단추 이름에 넣는다 — 목록에 단추가 여럿이어도 화면
 * 읽기에서 구별된다.
 */
export function TaskErrorText({
  text,
  warning = false,
  subject,
  className,
}: {
  text: string;
  warning?: boolean;
  subject?: string;
  className?: string;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const textId = useId();
  const long = text.length > LONG || text.includes("\n");
  return (
    <div className={cn("flex min-w-0 flex-col items-start gap-0.5 text-xs", className)}>
      <p
        id={textId}
        className={cn(
          "max-w-full break-words whitespace-pre-wrap",
          warning ? "text-warning-fg" : "text-danger-fg",
          long && !open && "line-clamp-2",
        )}
      >
        <span className="sr-only">{warning ? t("tasks.warning") : t("tasks.error")}: </span>
        {text}
      </p>
      {long && (
        <button
          type="button"
          aria-expanded={open}
          aria-controls={textId}
          aria-label={
            subject
              ? t(open ? "tasks.errorLessOf" : "tasks.errorMoreOf", { name: subject })
              : undefined
          }
          className="text-muted-foreground underline-offset-4 hover:underline"
          onClick={() => setOpen(!open)}
        >
          {open ? t("tasks.errorLess") : t("tasks.errorMore")}
        </button>
      )}
    </div>
  );
}
