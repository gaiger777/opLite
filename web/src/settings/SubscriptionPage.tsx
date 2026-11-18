import { useTranslation } from "react-i18next";

import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { PageHeader } from "@/components/PageHeader";
import { SubscriptionSection } from "@/settings/SubscriptionSection";

/** 설정 › 구독. 누구나 상태를 읽고 admin만 키를 넣고 지운다. */
export function SubscriptionPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  return (
    <div className="flex max-w-xl flex-col gap-6">
      <PageHeader title={t("nav.subscription")} />
      <SubscriptionSection write={can(me.role, "write", "settings")} />
    </div>
  );
}
