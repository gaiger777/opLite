import { useState } from "react";
import { useTranslation } from "react-i18next";

import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { useCeph } from "@/ceph/ceph";
import { CephStatusView } from "@/ceph/CephStatusView";
import { CephWizard } from "@/ceph/CephWizard";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";

/**
 * Orbivisor가 관리하는 Ceph. 없으면 admin에게 만들기 마법사를, 있으면 상태를 보인다. 마법사로 만들기 시작하면 끝내기
 * 전까지 마법사를 그대로 둔다(만든 뒤에도 mon·OSD·풀 단계가 이어진다).
 */
export function CephPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const ceph = useCeph(true);
  const [inWizard, setInWizard] = useState(false);
  const admin = can(me.role, "write", "ceph");

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title={t("nav.ceph")} description={t("ceph.pageHint")} />
      {ceph.data === undefined ? (
        <ListPending error={ceph.error} />
      ) : ceph.data === null || inWizard ? (
        admin ? (
          <CephWizard
            ceph={ceph.data}
            onStarted={() => setInWizard(true)}
            onDone={() => setInWizard(false)}
          />
        ) : (
          <p className="text-sm text-muted-foreground">{t("ceph.none")}</p>
        )
      ) : (
        <CephStatusView ceph={ceph.data} />
      )}
    </div>
  );
}
