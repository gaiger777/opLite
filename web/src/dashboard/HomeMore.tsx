import { ChevronDown } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import { usePrincipal } from "@/auth/AuthProvider";
import { Button } from "@/components/ui/button";
import { readUserPref, writeUserPref } from "@/lib/prefs";

/**
 * 홈의 "위젯 더 보기"(계획 77-6 — 홈을 한 화면에): 지도·자세히 판·건강 띠 밑의 나머지 위젯을 접는다. 처음엔 접혀 있고, 연 것은
 * 사용자별로 기억한다(저장소를 쓸 수 없으면 접힌 채 시작). 접힌 동안은 안의 위젯을 그리지 않는다(차트·표 요청도 없음).
 */
export function HomeMore({ names, children }: { names: string[]; children: React.ReactNode }) {
  const { t } = useTranslation();
  const { user } = usePrincipal();
  const [open, setOpen] = useState(() => readUserPref(user, "home.more") === "1");
  const id = useId();
  return (
    <section aria-label={t("dashboard.more.label")} className="flex flex-col gap-(--gap)">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <Button
          variant="outline"
          size="sm"
          aria-expanded={open}
          aria-controls={id}
          onClick={() => {
            setOpen(!open);
            writeUserPref(user, "home.more", open ? "0" : "1");
          }}
        >
          <ChevronDown aria-hidden className={open ? "rotate-180" : undefined} />
          {open ? t("dashboard.more.hide") : t("dashboard.more.show")}
        </Button>
        {!open && <span className="text-xs text-muted-foreground">{names.join(" · ")}</span>}
      </div>
      <div id={id} className="flex flex-col gap-(--gap)" hidden={!open}>
        {open && children}
      </div>
    </section>
  );
}
