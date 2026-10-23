import { Navigate, Outlet, useLocation } from "react-router";

import { useAuth } from "@/auth/AuthProvider";

/**
 * 로그인하지 않았으면 로그인 화면으로 보내고, 로그인한 뒤 돌아올 주소를 넘긴다. 직접 로그아웃했으면
 * 주소를 넘기지 않는다.
 */
export function RequireAuth() {
  const { principal, signedOut } = useAuth();
  const location = useLocation();
  if (principal === undefined) {
    return null;
  }
  if (principal === null) {
    if (signedOut) {
      return <Navigate to="/login" replace />;
    }
    const next = location.pathname + location.search;
    return <Navigate to={`/login?next=${encodeURIComponent(next)}`} replace />;
  }
  return <Outlet />;
}

/**
 * 로그인 뒤 돌아갈 주소. 다른 사이트로 가는 주소(`//evil`, `/\evil`, `https://...`)는 받지 않는다.
 * 브라우저와 같은 규칙으로 읽어 출처가 같을 때만 쓴다.
 */
export function safeNext(next: string | null): string {
  if (!next?.startsWith("/")) {
    return "/";
  }
  try {
    const url = new URL(next, window.location.origin);
    return url.origin === window.location.origin ? url.pathname + url.search + url.hash : "/";
  } catch {
    return "/";
  }
}
