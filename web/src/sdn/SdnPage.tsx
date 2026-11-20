import { Network } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { usePermissions } from "@/auth/usePermissions";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import { useSdn } from "@/sdn/sdn";
import { SdnStatusView } from "@/sdn/SdnStatusView";
import { SdnWizard } from "@/sdn/SdnWizard";

/**
 * 인프라 › 네트워크 › SDN(계획 65): Orbivisor가 설치하는 OVN. 설정 전이면 admin에게 마법사를, 설정 뒤에는 상태(중앙·
 * Raft 역할·northd·동기화·chassis)를 보인다. 마법사로 시작하면 끝내기 전까지 마법사를 그대로 둔다(Ceph와 같다).
 */
export function SdnPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const sdn = useSdn(true);
  const [inWizard, setInWizard] = useState(false);
  const [opened, setOpened] = useState(false);
  const admin = perms.can("write", "sdn");
  const unset = sdn.data !== undefined && sdn.data.phase === undefined;

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title={t("nav.sdn")} description={t("sdn.pageHint")} />
      {sdn.data === undefined ? (
        <ListPending error={sdn.error} />
      ) : inWizard || (unset && opened && admin) ? (
        <SdnWizard
          sdn={sdn.data}
          onStarted={() => setInWizard(true)}
          onDone={() => setInWizard(false)}
        />
      ) : unset ? (
        <EmptyState
          icon={Network}
          title={t("sdn.empty.title")}
          description={t(admin ? "sdn.empty.admin" : "sdn.none")}
          action={admin && <Button onClick={() => setOpened(true)}>{t("sdn.empty.start")}</Button>}
        />
      ) : (
        <SdnStatusView sdn={sdn.data} admin={admin} />
      )}
    </div>
  );
}
