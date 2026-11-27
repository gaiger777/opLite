import { useState } from "react";
import { useTranslation } from "react-i18next";

import { usePermissions } from "@/auth/usePermissions";
import { useCeph } from "@/ceph/ceph";
import { CephStatusView } from "@/ceph/CephStatusView";
import { CephWizard } from "@/ceph/CephWizard";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";

/**
 * Orbivisor가 관리하는 Ceph. 없으면 admin에게 만들기 마법사를, 있으면 상태를 보인다. 마법사로 만들기 시작하면 끝내기
 * 전까지 마법사를 그대로 둔다(만든 뒤에도 mon·OSD·풀 단계가 이어진다).
 */
export function CephPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const ceph = useCeph(true);
  const [inWizard, setInWizard] = useState(false);
  // 빈 상태의 주 단추를 눌러 마법사를 열었다.
  const [opened, setOpened] = useState(false);
  const admin = perms.can("write", "ceph");

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title={t("nav.ceph")} help="ceph" intro={t("help.ceph.intro")} />
      {ceph.data === undefined ? (
        <ListPending error={ceph.error} />
      ) : inWizard || (ceph.data === null && opened && admin) ? (
        <CephWizard
          ceph={ceph.data}
          onStarted={() => setInWizard(true)}
          onDone={() => setInWizard(false)}
        />
      ) : ceph.data === null ? (
        <EmptyState
          diagram="storageSharing"
          help="ceph.page"
          title={t("ceph.empty.title")}
          description={t(admin ? "ceph.empty.admin" : "ceph.none")}
          action={admin && <Button onClick={() => setOpened(true)}>{t("ceph.empty.start")}</Button>}
        />
      ) : (
        <CephStatusView ceph={ceph.data} />
      )}
    </div>
  );
}
