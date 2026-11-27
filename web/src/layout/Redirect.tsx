import { Navigate, useLocation } from "react-router";

/**
 * 옛 주소를 새 주소로 바꾼다(`replace`). `<Navigate to="...">`에 글을 주면 쿼리와 해시를 버리므로 지금 것을
 * 그대로 붙인다(`/tokens?x=1#a` → `/settings/tokens?x=1#a`).
 */
export function Redirect({ to }: { to: string }) {
  const { search, hash } = useLocation();
  return <Navigate to={{ pathname: to, search, hash }} replace />;
}
