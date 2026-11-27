import { X } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { useOptionalPrincipal } from "@/auth/AuthProvider";
import { InfoButton } from "@/components/InfoButton";
import { readPref, readUserPref, writePref, writeUserPref } from "@/lib/prefs";

/** 화면 소개를 접었는지(사용자별 — 한 브라우저를 여럿이 써도 남의 것이 넘어가지 않게) */
function useIntroClosed(help: string | undefined) {
  const user = useOptionalPrincipal()?.user;
  const key = `intro.${help}`;
  const [closed, setClosed] = useState(
    () => help !== undefined && (user ? readUserPref(user, key) : readPref(key)) === "closed",
  );
  const close = () => {
    setClosed(true);
    if (user) {
      writeUserPref(user, key, "closed");
    } else {
      writePref(key, "closed");
    }
  };
  return [closed, close] as const;
}

/**
 * 화면 머리: 제목(h1, 20px)과 오른쪽 동작 한 줄, 밑에 짧은 설명. 영역 탭·이동 경로는 셸이 그리므로 화면은
 * 이것만 둔다(설계 §6·§8.2 5). `help`(화면 이름)를 주면 제목 옆 ⓘ가 "이 화면은?" 도움말(`helpInfo.<help>.page`)을
 * 열고, `intro`는 처음에 펼친 소개 한 단락이다 — 접으면 기억한다(계획 77-4).
 */
export function PageHeader({
  title,
  description,
  actions,
  id,
  help,
  intro,
}: {
  title: React.ReactNode;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  id?: string;
  /** 화면 이름(도움말 주제 `<help>.page`, 소개 접기 기억의 키) */
  help?: string;
  /** "이 화면은?" 소개 한 단락: 무엇을 하는 화면이고 언제 쓰나 */
  intro?: string;
}) {
  const { t } = useTranslation();
  const [closed, close] = useIntroClosed(help);
  return (
    <div className="flex flex-col gap-1">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-1.5">
          <h1 id={id} className="text-xl font-semibold">
            {title}
          </h1>
          {help && (
            <InfoButton
              topic={`${help}.page`}
              label={
                typeof title === "string"
                  ? t("help.aboutPageOf", { name: title })
                  : t("help.aboutPage")
              }
              line={intro}
            />
          )}
        </div>
        {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
      </div>
      {description && <p className="text-sm text-muted-foreground">{description}</p>}
      {intro && !closed && (
        <div
          data-page-intro
          className="mt-1 flex items-start gap-2 rounded-md border border-info/30 bg-info-soft px-3 py-2 text-sm text-info-fg"
        >
          <p className="min-w-0 flex-1">{intro}</p>
          {help && (
            <button
              type="button"
              aria-label={t("help.closeIntro")}
              onClick={close}
              className="-m-1 rounded p-1 outline-none hover:bg-info/10 focus-visible:ring-2 focus-visible:ring-ring"
            >
              <X aria-hidden className="size-3.5" />
            </button>
          )}
        </div>
      )}
    </div>
  );
}
