import { Navigate, useLocation } from "react-router";

import { usePrincipal } from "@/auth/AuthProvider";
import type { AreaId } from "@/components/icons";
import { AREAS, lastTab } from "@/layout/nav";

/**
 * 옛 주소를 새 주소로 바꾼다(`replace`). `<Navigate to="...">`에 글을 주면 쿼리와 해시를 버리므로 지금 것을
 * 그대로 붙인다(`/tokens?x=1#a` → `/settings/tokens?x=1#a`).
 */
export function Redirect({ to }: { to: string }) {
  const { search, hash } = useLocation();
  return <Navigate to={{ pathname: to, search, hash }} replace />;
}

/** 영역 주소(`/infra`·`/protect`·`/activity`)는 그 영역에서 마지막으로 본 탭으로, 없으면 보이는 첫 탭으로 간다. */
export function AreaRedirect({ area }: { area: AreaId }) {
  const me = usePrincipal();
  const found = AREAS.find((a) => a.id === area)!;
  return <Navigate to={lastTab(found, me.role)} replace />;
}
