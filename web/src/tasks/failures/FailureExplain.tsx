import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import type { Task } from "@/api/types";
import { ACTION_ICONS } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { type FailureText, failureLinks } from "@/tasks/failures/failure";
import { retryLabel } from "@/tasks/retry";
import { useRetry } from "@/tasks/useRetry";

/**
 * 실패한 Task의 "무엇이 잘못됐나 / 왜 / 이렇게 하세요"(계획 77-2, 진단 §3.4): 사전 문구, 관련 화면 링크(원인 값으로), 다시 할 수
 * 있는 Task면 "다시 하기"(서버가 먼저 고쳐야 한다고 했으면 "고친 뒤 다시 하기"). 원문은 부르는 쪽(상세 서랍)이 아래 "자세히(원문)"에 접어 둔다.
 */
export function FailureExplain({ task, text }: { task: Task; text: FailureText }) {
  const { t } = useTranslation();
  const failure = task.status.failure!;
  const links = failureLinks(failure);
  const { of, retry, busy } = useRetry();
  const again = of(task);
  return (
    <section
      aria-labelledby="task-failure"
      className="flex flex-col gap-3 rounded-md border border-danger-fg/40 bg-danger-soft p-3 text-sm"
    >
      <div className="flex flex-col gap-1">
        <h3 id="task-failure" className="text-xs font-medium text-muted-foreground">
          {t("failureUi.what")}
        </h3>
        <p className="font-medium break-words text-danger-fg">{text.title}</p>
        {/* 단계가 제목에 이미 들어간 코드(적용)는 원문 단계를 다시 쓰지 않는다. */}
        {failure.step && !failure.params?.step && (
          <p className="text-xs text-muted-foreground">
            {t("failureUi.step")}: <span className="break-words">{failure.step}</span>
          </p>
        )}
      </div>
      <div className="flex flex-col gap-1">
        <h4 className="text-xs font-medium text-muted-foreground">{t("failureUi.why")}</h4>
        <p className="break-words">{text.cause}</p>
      </div>
      <div className="flex flex-col gap-1">
        <h4 className="text-xs font-medium text-muted-foreground">{t("failureUi.fix")}</h4>
        {text.fix.length > 1 ? (
          <ol className="list-decimal pl-5">
            {text.fix.map((line) => (
              <li key={line} className="break-words">
                {line}
              </li>
            ))}
          </ol>
        ) : (
          <p className="break-words">{text.fix[0]}</p>
        )}
      </div>
      {links.length > 0 && (
        <nav aria-label={t("failureUi.links")} className="flex flex-wrap gap-x-3 gap-y-1 text-sm">
          {links.map((l) => (
            <Link
              key={l.href}
              to={l.href}
              className="text-brand underline-offset-4 hover:underline"
            >
              {t(`tasks.drawer.kinds.${l.kind}`, { defaultValue: l.kind })}
              {l.name ? ` ${l.name}` : ""}
            </Link>
          ))}
        </nav>
      )}
      {again && (
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={again.reason !== undefined || busy.has(task.id)}
            onClick={(e) => void retry(task, again, e.currentTarget)}
          >
            <ACTION_ICONS.retry />
            {retryLabel(again, t)}
          </Button>
          {/* 먼저 고칠 실패는 "이렇게 하세요"로, 잠깐의 문제일 수 있으면 바로 다시 해 보라고(리뷰 I7) */}
          <span className="text-xs text-muted-foreground">
            {again.reason
              ? t(again.reason)
              : again.fixFirst
                ? t("failureUi.fixFirstHint")
                : t("failureUi.retryHint")}
          </span>
        </div>
      )}
    </section>
  );
}
