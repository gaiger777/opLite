import { Suspense } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";

import { useList } from "@/api/live";
import { objectTarget } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { errorMessage } from "@/i18n/errors";
import { VmConsole } from "@/vms/lazyConsole";
import { useVmByName } from "@/vms/VmDetailPage";

/** 콘솔만 크게 보는 화면(새 탭으로도 연다) */
export function ConsolePage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const perms = usePermissions();
  const vms = useList("vm");
  const vm = useVmByName(name);
  // 콘솔은 그 VM의 콘솔 권한이 있어야 연다(서버도 막는다).
  if (vm ? !perms.has("vm.console", objectTarget("vm", vm)) : !perms.anywhere("vm.console")) {
    return (
      <p role="alert" className="text-destructive">
        {t("console.forbidden")}
      </p>
    );
  }
  if (vm === undefined) {
    return vms.isError ? (
      <p role="alert" className="text-destructive">
        {errorMessage(t, vms.error)}
      </p>
    ) : (
      <p role="status" className="text-muted-foreground">
        {t("common.loading")}
      </p>
    );
  }
  if (vm === null) {
    return <h1 className="text-xl font-semibold">{t("vms.notFound", { name })}</h1>;
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
      {/* 콘솔은 넓은 화면·키보드가 있어야 쓰기 좋다(설계 §5.9). */}
      <p className="text-sm text-muted-foreground md:hidden">{t("common.desktopOnly")}</p>
      <Suspense
        fallback={
          <p role="status" className="text-muted-foreground">
            {t("common.loading")}
          </p>
        }
      >
        <VmConsole vm={vm} />
      </Suspense>
    </div>
  );
}
