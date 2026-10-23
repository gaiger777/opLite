import { useQueryClient } from "@tanstack/react-query";
import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";

import { api, call, setUnauthorizedHandler } from "@/api/client";
import type { Principal } from "@/api/types";

interface Auth {
  /** 확인 중이면 undefined, 로그인하지 않았으면 null */
  principal: Principal | null | undefined;
  login(username: string, password: string): Promise<void>;
  logout(): Promise<void>;
}

const AuthContext = createContext<Auth | null>(null);

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const queryClient = useQueryClient();
  const [principal, setPrincipal] = useState<Principal | null | undefined>(undefined);

  useEffect(() => {
    // 세션이 끝나면(만료, opld 재시작, 비밀번호 변경) 남은 데이터를 지우고 로그인 화면으로 간다.
    setUnauthorizedHandler(() => {
      queryClient.clear();
      setPrincipal(null);
    });
    let live = true;
    call(api.GET("/auth/whoami")).then(
      (p) => live && setPrincipal(p),
      () => live && setPrincipal(null),
    );
    return () => {
      live = false;
    };
  }, [queryClient]);

  const login = useCallback(async (username: string, password: string) => {
    await call(api.POST("/auth/login", { body: { username, password } }));
    setPrincipal(await call(api.GET("/auth/whoami")));
  }, []);

  const logout = useCallback(async () => {
    await call(api.POST("/auth/logout")).catch(() => {});
    queryClient.clear();
    setPrincipal(null);
  }, [queryClient]);

  const value = useMemo(() => ({ principal, login, logout }), [principal, login, logout]);
  return <AuthContext value={value}>{children}</AuthContext>;
}

export function useAuth(): Auth {
  const auth = useContext(AuthContext);
  if (!auth) {
    throw new Error("useAuth outside AuthProvider");
  }
  return auth;
}

/** 로그인한 사용자. 로그인이 필요한 화면 안에서만 쓴다. */
export function usePrincipal(): Principal {
  const { principal } = useAuth();
  if (!principal) {
    throw new Error("usePrincipal outside RequireAuth");
  }
  return principal;
}
