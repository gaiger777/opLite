import { lazy, Suspense } from "react";
import { useTranslation } from "react-i18next";

import { useIsPhone } from "@/components/data-table/DataTable";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { closeHelp, takeHelpOpener, useHelpTopic } from "@/help/help";
import { loadHelpStrings } from "@/help/strings";

// 패널 본문과 그 문구는 처음 열 때 받는다.
const HelpBody = lazy(async () => {
  const [module] = await Promise.all([import("@/help/HelpBody"), loadHelpStrings()]);
  return module;
});

/**
 * 오른쪽 도움말 패널(계획 77-4, 진단 §2.0·§5 Cloudscape help panel) — 셸에 하나. ⓘ·"자세히"가 연 주제를 보인다.
 * 휴대폰에서는 아래에서 올라오는 시트다. Esc·닫기 단추로 닫고, 닫으면 연 단추로 포커스가 돌아간다(대화상자 안에서
 * 열어도 그 위에 겹친다).
 */
export function HelpPanel() {
  const { t } = useTranslation();
  const topic = useHelpTopic();
  const phone = useIsPhone();
  return (
    <Sheet open={topic !== null} onOpenChange={(open) => !open && closeHelp()}>
      <SheetContent
        side={phone ? "bottom" : "right"}
        data-help-panel
        // 패널 뒤의 칸·대화상자를 읽으며 볼 수 있게 흐리게 하지 않는다.
        overlayClassName="bg-transparent supports-backdrop-filter:backdrop-blur-none"
        aria-describedby={undefined}
        onCloseAutoFocus={(event) => {
          // 연 단추(ⓘ·"자세히")로 돌아간다. 그 단추가 없어졌으면(대화상자가 닫힘) 브라우저에 맡긴다.
          const back = takeHelpOpener();
          if (back?.isConnected) {
            event.preventDefault();
            back.focus();
          }
        }}
        className={
          phone
            ? "max-h-[85svh] gap-0 overflow-y-auto p-0"
            : "w-full gap-0 overflow-y-auto p-0 sm:max-w-md"
        }
      >
        <SheetTitle className="border-b px-4 py-3 pr-12 text-base">
          {topic?.label ?? t("help.title")}
        </SheetTitle>
        {topic && (
          <Suspense fallback={<p className="p-4 text-muted-foreground">{t("common.loading")}</p>}>
            <HelpBody topic={topic} />
          </Suspense>
        )}
      </SheetContent>
    </Sheet>
  );
}
