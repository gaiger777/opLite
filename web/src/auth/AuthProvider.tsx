import { useQueryClient } from "@tanstack/react-query";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { toast } from "sonner";

import { api, call, setUnauthorizedHandler } from "@/api/client";
import type { Principal } from "@/api/types";
import { resetTasks } from "@/tasks/tasks";

interface Auth {
  /** 확인 중이면 undefined, 로그인하지 않았으면 null */
  principal: Principal | null | undefined;
  /** 직접 로그아웃했다. 다음 사용자를 앞 사용자의 화면으로 보내지 않도록 돌아올 주소를 남기지 않는다. */
  signedOut: boolean;
  login(username: string, password: string): Promise<void>;
  logout(): Promise<void>;
}

const AuthContext = createContext<Auth | null>(null);

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const queryClient = useQueryClient();
  const [principal, setPrincipal] = useState<Principal | null | undefined>(undefined);
  const [signedOut, setSignedOut] = useState(false);
  // 인증 세대. 로그인·로그아웃·세션 만료마다 늘려, 그 전에 보낸 요청의 결과(느린 whoami, 늦은 401)가
  // 새 로그인을 되돌리지 못하게 한다.
  const epoch = useRef(0);

  // 남은 데이터와 알림을 지우고 로그인하지 않은 상태로 둔다.
  const clear = useCallback(
    (explicit: boolean) => {
      epoch.current += 1;
      queryClient.clear();
      resetTasks();
      toast.dismiss();
      // 로그아웃하는 중에 받은 401은 직접 로그아웃한 것으로 둔다(로그인이 다시 false로 바꾼다).
      setSignedOut((was) => explicit || was);
      setPrincipal(null);
    },
    [queryClient],
  );

  useEffect(() => {
    // 세션이 끝나면(만료, opld 재시작, 비밀번호 변경) 로그인 화면으로 간다.
    setUnauthorizedHandler(
      (sentAt) => {
        if (sentAt === epoch.current) {
          clear(false);
        }
      },
      () => epoch.current,
    );
    let live = true;
    const started = epoch.current;
    call(api.GET("/auth/whoami")).then(
      (p) => live && epoch.current === started && setPrincipal(p),
      () => live && epoch.current === started && setPrincipal(null),
    );
    return () => {
      live = false;
    };
  }, [clear]);

  const login = useCallback(async (username: string, password: string) => {
    const mine = ++epoch.current;
    await call(api.POST("/auth/login", { body: { username, password } }));
    const p = await call(api.GET("/auth/whoami"));
    if (epoch.current === mine) {
      setSignedOut(false);
      setPrincipal(p);
    }
  }, []);

  const logout = useCallback(async () => {
    // 보내기 전에 로그아웃으로 표시하고 세대를 늘린다. 그 사이 도착한 401이 돌아올 주소(`?next=`)를
    // 남기지 못하게 한다.
    epoch.current += 1;
    setSignedOut(true);
    await call(api.POST("/auth/logout")).catch(() => {});
    clear(true);
  }, [clear]);

  const value = useMemo(
    () => ({ principal, signedOut, login, logout }),
    [principal, signedOut, login, logout],
  );
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
