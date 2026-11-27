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
import type { components } from "@/api/schema";
import type { Principal, SecondFactorPrompt } from "@/api/types";
import { forgetPlanEdits } from "@/dr/edits";
import { resetChunked } from "@/storage/chunked";
import { resetUploads } from "@/storage/upload";
import { resetTasks } from "@/tasks/tasks";

interface Auth {
  /** 확인 중이면 undefined, 로그인하지 않았으면 null */
  principal: Principal | null | undefined;
  /** 직접 로그아웃했다. 다음 사용자를 앞 사용자의 화면으로 보내지 않도록 돌아올 주소를 남기지 않는다. */
  signedOut: boolean;
  /**
   * 로그인 둘째 단계(계획 62): 비밀번호는 맞았고 코드·패스키·복구 코드가 남았다(`enroll`이면 강제 등록). 다른 요청이
   * 401 `second_factor_required`를 받아도 여기로 돌아온다.
   */
  pending: SecondFactorPrompt | null;
  /** `realm`이 없으면 로컬 사용자로 로그인한다. 둘째 단계가 남으면 `pending`을 두고 끝난다. */
  login(username: string, password: string, realm?: string): Promise<void>;
  /** 둘째 단계를 낸다. 틀리면 던진다(`invalid_second_factor` 등). */
  secondFactor(body: components["schemas"]["SecondFactorRequest"]): Promise<void>;
  /** 강제 등록을 마쳤다(같은 쿠키가 정식 세션이 됐다). 로그인을 끝낸다. */
  finishEnrollment(): Promise<void>;
  /** 둘째 단계를 강제 등록으로 바꾼다(서버가 403 `enrollment_required`를 줬다). */
  needEnrollment(): void;
  logout(): Promise<void>;
}

/** 둘째 단계를 모를 때(새로 고침 뒤 401 `second_factor_required`)의 기본 */
const UNKNOWN_PROMPT: SecondFactorPrompt = {
  methods: ["totp", "recovery"],
  enroll: false,
  expires_at: 0,
};

/** 새로 고침 뒤라 진짜 둘째 단계 안내(패스키·강제 등록)를 모른다 */
export function isUnknownPrompt(prompt: SecondFactorPrompt): boolean {
  return prompt === UNKNOWN_PROMPT;
}

const AuthContext = createContext<Auth | null>(null);

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const queryClient = useQueryClient();
  const [principal, setPrincipal] = useState<Principal | null | undefined>(undefined);
  const [signedOut, setSignedOut] = useState(false);
  const [pending, setPending] = useState<SecondFactorPrompt | null>(null);
  // 인증 세대. 로그인·로그아웃·세션 만료마다 늘려, 그 전에 보낸 요청의 결과(느린 whoami, 늦은 401)가
  // 새 로그인을 되돌리지 못하게 한다.
  const epoch = useRef(0);

  // 남은 데이터와 알림을 지우고 로그인하지 않은 상태로 둔다.
  const clear = useCallback(
    (explicit: boolean) => {
      epoch.current += 1;
      queryClient.clear();
      resetTasks();
      resetUploads();
      resetChunked();
      forgetPlanEdits();
      toast.dismiss();
      // 로그아웃하는 중에 받은 401은 직접 로그아웃한 것으로 둔다(로그인이 다시 false로 바꾼다).
      setSignedOut((was) => explicit || was);
      setPrincipal(null);
      if (explicit) {
        setPending(null);
      }
    },
    [queryClient],
  );

  useEffect(() => {
    // 세션이 끝나면(만료, opld 재시작, 비밀번호 변경) 로그인 화면으로 간다.
    setUnauthorizedHandler(
      (sentAt, code) => {
        if (sentAt === epoch.current) {
          // 대기 세션(비밀번호만 맞음)이면 로그인 둘째 단계로 돌아간다.
          if (code === "second_factor_required") {
            setPending((prev) => prev ?? UNKNOWN_PROMPT);
          } else {
            setPending(null);
          }
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

  // 로그인이 끝났다: 사용자를 다시 읽는다.
  const finish = useCallback(async (mine: number) => {
    const p = await call(api.GET("/auth/whoami"));
    if (epoch.current === mine) {
      setSignedOut(false);
      setPending(null);
      setPrincipal(p);
    }
  }, []);

  const login = useCallback(
    async (username: string, password: string, realm?: string) => {
      const mine = ++epoch.current;
      const result = await call(
        api.POST("/auth/login", { body: { username, password, ...(realm && { realm }) } }),
      );
      if (result.second_factor) {
        if (epoch.current === mine) {
          setSignedOut(false);
          setPending(result.second_factor);
        }
        return;
      }
      await finish(mine);
    },
    [finish],
  );

  const secondFactor = useCallback(
    async (body: components["schemas"]["SecondFactorRequest"]) => {
      const mine = ++epoch.current;
      await call(api.POST("/auth/login/second-factor", { body }));
      await finish(mine);
    },
    [finish],
  );

  const finishEnrollment = useCallback(() => finish(++epoch.current), [finish]);
  const needEnrollment = useCallback(
    () => setPending((prev) => ({ ...(prev ?? UNKNOWN_PROMPT), enroll: true, methods: [] })),
    [],
  );

  const logout = useCallback(async () => {
    // 보내기 전에 로그아웃으로 표시하고 세대를 늘린다. 그 사이 도착한 401이 돌아올 주소(`?next=`)를
    // 남기지 못하게 한다.
    epoch.current += 1;
    setSignedOut(true);
    await call(api.POST("/auth/logout")).catch(() => {});
    clear(true);
  }, [clear]);

  const value = useMemo(
    () => ({
      principal,
      signedOut,
      pending,
      login,
      secondFactor,
      finishEnrollment,
      needEnrollment,
      logout,
    }),
    [principal, signedOut, pending, login, secondFactor, finishEnrollment, needEnrollment, logout],
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

/** 로그인한 사용자. 로그인 밖이거나 `AuthProvider` 밖이면 null이다(실시간 연결·목록이 함께 쓴다). */
export function useOptionalPrincipal(): Principal | null {
  return useContext(AuthContext)?.principal ?? null;
}

/** 로그인한 사용자. 로그인이 필요한 화면 안에서만 쓴다. */
export function usePrincipal(): Principal {
  const { principal } = useAuth();
  if (!principal) {
    throw new Error("usePrincipal outside RequireAuth");
  }
  return principal;
}
