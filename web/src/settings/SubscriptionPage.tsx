import { useTranslation } from "react-i18next";

import { usePermissions } from "@/auth/usePermissions";
import { PageHeader } from "@/components/PageHeader";
import { SubscriptionSection } from "@/settings/SubscriptionSection";

/** 설정 › 구독. 누구나 상태를 읽고 admin만 키를 넣고 지운다. */
export function SubscriptionPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  return (
    <div className="flex max-w-xl flex-col gap-6">
      <PageHeader
        title={t("nav.subscription")}
        help="subscription"
        intro={t("help.subscription.intro")}
      />
      <SubscriptionSection write={perms.can("write", "settings")} />
    </div>
  );
}
