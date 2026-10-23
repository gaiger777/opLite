import { Navigate, Outlet, useLocation } from "react-router";

import { useAuth } from "@/auth/AuthProvider";

/** 로그인하지 않았으면 로그인 화면으로 보내고, 로그인한 뒤 돌아올 주소를 넘긴다. */
export function RequireAuth() {
  const { principal } = useAuth();
  const location = useLocation();
  if (principal === undefined) {
    return null;
  }
  if (principal === null) {
    const next = location.pathname + location.search;
    return <Navigate to={`/login?next=${encodeURIComponent(next)}`} replace />;
  }
  return <Outlet />;
}

/** 로그인 뒤 돌아갈 주소. 다른 사이트로 가는 주소(`//evil`, `https://...`)는 받지 않는다. */
export function safeNext(next: string | null): string {
  return next && next.startsWith("/") && !next.startsWith("//") ? next : "/";
}
