import { useTranslation } from "react-i18next";
import { Navigate, useLocation } from "react-router";

import { useNavUser } from "@/auth/usePermissions";
import { EmptyState } from "@/components/EmptyState";
import { AREA_ICONS, type AreaId } from "@/components/icons";
import { PageHeader } from "@/components/PageHeader";
import { AREAS, lastTab, visibleTabs } from "@/layout/nav";

/**
 * 옛 주소를 새 주소로 바꾼다(`replace`). `<Navigate to="...">`에 글을 주면 쿼리와 해시를 버리므로 지금 것을
 * 그대로 붙인다(`/tokens?x=1#a` → `/settings/tokens?x=1#a`).
 */
export function Redirect({ to }: { to: string }) {
  const { search, hash } = useLocation();
  return <Navigate to={{ pathname: to, search, hash }} replace />;
}

/**
 * 영역 주소(`/infra`·`/protect`·`/activity`)는 그 영역에서 마지막으로 본 탭으로, 없으면 보이는 첫 탭으로 간다.
 * 이 역할에 보이는 탭이 하나도 없으면(`lastTab`이 영역 주소 자신을 준다) 넘기지 않고 빈 화면을 보인다 — 넘기면
 * 같은 주소로 끝없이 돈다.
 */
export function AreaRedirect({ area }: { area: AreaId }) {
  const { t } = useTranslation();
  const me = useNavUser();
  const found = AREAS.find((a) => a.id === area)!;
  if (visibleTabs(found, me.gate).length === 0) {
    return (
      <div className="flex flex-col gap-4">
        <PageHeader title={t(found.key)} />
        <EmptyState icon={AREA_ICONS[area]} title={t("nav.noTabs")} />
      </div>
    );
  }
  return <Navigate to={lastTab(found, me)} replace />;
}
