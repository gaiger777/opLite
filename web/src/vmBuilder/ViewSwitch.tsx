// 만들기 화면의 보기 전환 "폼 | 빌더"(계획 71 설계 §8.1): 두 링크(지금 쪽 `aria-current="page"`), 쿼리(`?folder=`)를
// 지키고, 누르면 지금 값을 초안으로 넘기고(`onLeave`) 고른 보기를 기억한다. 폼 조각과 빌더 조각이 함께 쓴다.
import { useTranslation } from "react-i18next";
import { Link, useSearchParams } from "react-router";

import { usePrincipal } from "@/auth/AuthProvider";
import { cn } from "@/lib/utils";
import { type CreateMode, writeMode } from "@/vmBuilder/draft";

const TO: Record<CreateMode, string> = { form: "/vms/-new", builder: "/vms/-new/builder" };

export function ViewSwitch({ mode, onLeave }: { mode: CreateMode; onLeave(): void }) {
  const { t } = useTranslation();
  const { user } = usePrincipal();
  const [params] = useSearchParams();
  const query = params.toString();
  return (
    <nav aria-label={t("vmForm.view")} className="inline-flex rounded-lg border p-0.5 text-sm">
      {(["form", "builder"] as const).map((m) => (
        <Link
          key={m}
          to={query ? `${TO[m]}?${query}` : TO[m]}
          aria-current={m === mode ? "page" : undefined}
          className={cn(
            "rounded-md px-3 py-1",
            m === mode ? "bg-muted font-medium" : "text-muted-foreground hover:text-foreground",
          )}
          onClick={(e) => {
            // 새 탭·창으로 여는 누르기(⌘·Ctrl·Shift·Alt·가운데 단추)는 이 탭에 초안·보기를 남기지 않는다.
            const plain = e.button === 0 && !e.metaKey && !e.ctrlKey && !e.shiftKey && !e.altKey;
            if (plain && m !== mode) {
              onLeave();
              writeMode(user, m);
            }
          }}
        >
          {t(m === "form" ? "vmForm.viewForm" : "vmForm.viewBuilder")}
        </Link>
      ))}
    </nav>
  );
}
