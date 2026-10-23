import { lazy, Suspense } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";

import { useVmByName } from "@/vms/VmDetailPage";

// noVNC는 콘솔을 열 때만 내려받는다.
export const VmConsole = lazy(() => import("@/vms/VmConsole"));

/** 콘솔만 크게 보는 화면(새 탭으로도 연다) */
export function ConsolePage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const vm = useVmByName(name);
  if (vm === undefined) {
    return <p className="text-muted-foreground">{t("common.loading")}</p>;
  }
  if (vm === null) {
    return <h1 className="text-2xl font-semibold">{t("vms.notFound", { name })}</h1>;
  }
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-3">
        <h1 className="text-xl font-semibold">
          <Link to={`/vms/${encodeURIComponent(vm.name)}`} className="hover:underline">
            {vm.name}
          </Link>{" "}
          · {t("vms.action.console")}
        </h1>
      </div>
      <Suspense fallback={<p className="text-muted-foreground">{t("common.loading")}</p>}>
        <VmConsole vm={vm} />
      </Suspense>
    </div>
  );
}
