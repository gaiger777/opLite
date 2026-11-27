import { Navigate, useLocation } from "react-router";

import { useNavUser, usePermissionsReady } from "@/auth/usePermissions";
import { legacyTarget, type LegacyRedirect } from "@/layout/nav";

/**
 * 옛 주소를 새 주소로 바꾼다(`replace`). `<Navigate to="...">`에 글을 주면 쿼리와 해시를 버리므로 지금 것을
 * 그대로 붙인다(`/tokens?x=1#a` → `/settings/tokens?x=1#a`). 갈 곳이 권한에 따라 다르면(`groups`) 권한을 받은 뒤 넘긴다.
 */
export function Redirect({ redirect }: { redirect: LegacyRedirect }) {
  const { search, hash } = useLocation();
  const { gate } = useNavUser();
  const ready = usePermissionsReady();
  if (redirect.groups && !ready) {
    return null;
  }
  return <Navigate to={{ pathname: legacyTarget(redirect, gate), search, hash }} replace />;
}
