import { Suspense } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";

import { useList } from "@/api/live";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { errorMessage } from "@/i18n/errors";
import { VmConsole } from "@/vms/lazyConsole";
import { useVmByName } from "@/vms/VmDetailPage";

/** 콘솔만 크게 보는 화면(새 탭으로도 연다) */
export function ConsolePage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const me = usePrincipal();
  const vms = useList("vm");
  const vm = useVmByName(name);
  // 콘솔은 VM 동작 권한이 있어야 연다(서버도 막는다).
  if (!can(me.role, "operate", "vm")) {
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
