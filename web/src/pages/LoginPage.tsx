import { useQuery } from "@tanstack/react-query";
import { lazy, Suspense, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Navigate, useSearchParams } from "react-router";

import { getAssertion, passkeysSupported } from "@/accounts/webauthn";
import { api, ApiError, call } from "@/api/client";
import type { SecondFactorPrompt } from "@/api/types";
import { isUnknownPrompt, useAuth } from "@/auth/AuthProvider";
import { safeNext } from "@/auth/RequireAuth";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { codeMessage, errorMessage } from "@/i18n/errors";

// 강제 등록 화면(QR 포함)은 그때만 내려받는다.
const EnrollStep = lazy(async () => ({
  default: (await import("@/accounts/TwoFactorSetup")).EnrollStep,
}));

/** SSO 콜백이 `/login?error=<code>`로 돌려보내는 코드(`oidc_login`). 모르는 코드는 `sso_failed`로 보인다. */
const SSO_ERRORS = ["sso_failed", "no_role", "realm_unavailable", "too_many_attempts"];

export function LoginPage() {
  const { t } = useTranslation();
  const { principal, login, pending, finishEnrollment, logout } = useAuth();
  const [params, setParams] = useSearchParams();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [realm, setRealm] = useState("local");
  const [error, setError] = useState<string | null>(null);
  // SSO가 실패해 돌아왔으면 그 까닭을 처음부터 보인다(비밀번호 입력의 오류가 아니다).
  const [ssoError, setSsoError] = useState<string | null>(() => {
    const code = params.get("error");
    if (code === null) {
      return null;
    }
    const known = SSO_ERRORS.includes(code) ? code : "sso_failed";
    return codeMessage(t, known, "", "login.sso");
  });
  const [busy, setBusy] = useState(false);
  // 서버 버전은 로그인 없이 읽는다. 읽지 못하면(옛 서버) 보이지 않는다.
  const version = useQuery({
    queryKey: ["version"],
    queryFn: () => call(api.GET("/version")),
    staleTime: Infinity,
    retry: false,
  });
  // 고를 영역도 로그인 없이 읽는다. 읽지 못하면(옛 서버) 로컬로만 로그인한다.
  const realms = useQuery({
    queryKey: ["login-realms"],
    queryFn: () => call(api.GET("/auth/realms")),
    retry: false,
  });
  // 비밀번호로 로그인하는 영역은 고르고, OIDC 영역은 IdP로 가는 버튼이다.
  const choices = (realms.data ?? []).filter((r) => r.kind !== "oidc");
  const sso = (realms.data ?? []).filter((r) => r.kind === "oidc");

  // 읽은 오류 코드는 주소에서 지운다(새로 고쳐도 다시 보이지 않게). `next` 같은 다른 값은 둔다.
  useEffect(() => {
    if (params.has("error")) {
      setParams(
        (p) => {
          p.delete("error");
          return p;
        },
        { replace: true },
      );
    }
  }, [params, setParams]);

  // 세션을 확인하는 동안에는 폼을 그리지 않는다(로그인돼 있으면 잠깐 보였다 사라진다).
  if (principal === undefined) {
    return null;
  }
  if (principal) {
    return <Navigate to={safeNext(params.get("next"))} replace />;
  }
  // 비밀번호는 맞았고 둘째 단계(또는 강제 등록)가 남았다.
  if (pending) {
    return (
      <LoginCard>
        {pending.enroll ? (
          <Suspense fallback={null}>
            <EnrollStep onDone={() => void finishEnrollment()} onCancel={() => void logout()} />
          </Suspense>
        ) : (
          <SecondFactorStep prompt={pending} />
        )}
      </LoginCard>
    );
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setSsoError(null);
    try {
      await login(username, password, realm === "local" ? undefined : realm);
    } catch (err) {
      // 로그인 전용 문구(`login.<code>`, 예: 업그레이드 중이라 2단계 인증 등록을 받을 수 없다)를 먼저 찾는다.
      setError(
        err instanceof ApiError
          ? codeMessage(t, err.code, err.message, "login")
          : errorMessage(t, err),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex min-h-svh flex-col items-center justify-center gap-4 p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle className="text-xl">
            <h1>Orbivisor</h1>
          </CardTitle>
        </CardHeader>
        <CardContent>
          {ssoError && (
            <p role="alert" className="mb-4 text-sm text-destructive">
              {ssoError}
            </p>
          )}
          <form onSubmit={submit} className="flex flex-col gap-4">
            {/* 영역이 로컬뿐이면 고르지 않는다. */}
            {choices.length > 1 && (
              <div className="flex flex-col gap-2">
                <Label htmlFor="realm">{t("login.realm")}</Label>
                <NativeSelect
                  id="realm"
                  className="w-full"
                  value={realm}
                  onChange={(e) => setRealm(e.target.value)}
                >
                  {choices.map((r) => (
                    <NativeSelectOption key={r.name} value={r.name}>
                      {r.kind === "local" ? t("login.localRealm") : (r.display ?? r.name)}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </div>
            )}
            <div className="flex flex-col gap-2">
              <Label htmlFor="username">{t("login.username")}</Label>
              <Input
                id="username"
                autoComplete="username"
                required
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              />
            </div>
            <div className="flex flex-col gap-2">
              <Label htmlFor="password">{t("login.password")}</Label>
              <Input
                id="password"
                type="password"
                autoComplete="current-password"
                required
                aria-invalid={error ? true : undefined}
                aria-describedby={error ? "login-error" : undefined}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </div>
            {error && (
              <p id="login-error" role="alert" className="text-sm text-destructive">
                {error}
              </p>
            )}
            <Button type="submit" disabled={busy}>
              {t("login.submit")}
            </Button>
          </form>
          {sso.length > 0 && (
            <div className="mt-4 flex flex-col gap-4">
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                <span className="h-px flex-1 bg-border" />
                {t("login.or")}
                <span className="h-px flex-1 bg-border" />
              </div>
              {/* 전체 페이지로 이동한다(IdP에 갔다가 콜백이 `/`로 돌려보낸다). */}
              {sso.map((r) => (
                <Button key={r.name} variant="outline" asChild>
                  <a href={`/api/v1/auth/oidc/${encodeURIComponent(r.name)}/start`}>
                    {t("login.sso.signIn", { name: r.display ?? r.name })}
                  </a>
                </Button>
              ))}
            </div>
          )}
        </CardContent>
      </Card>
      {version.data && (
        <p className="text-xs text-muted-foreground">
          {t("login.version", { version: version.data.version })}
        </p>
      )}
    </div>
  );
}

function LoginCard({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex min-h-svh flex-col items-center justify-center gap-4 p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle className="text-xl">
            <h1>Orbivisor</h1>
          </CardTitle>
        </CardHeader>
        <CardContent>{children}</CardContent>
      </Card>
    </div>
  );
}

/**
 * 로그인 둘째 단계(계획 62 설계 §10.2): 인증 앱 코드, "복구 코드 쓰기", 패스키가 있으면 "패스키 사용". 강제 등록
 * 세션이면(403 `enrollment_required`) 등록 화면으로 바꾸고, 대기 세션이 끝났으면(401) 처음으로 돌아간다.
 */
function SecondFactorStep({ prompt }: { prompt: SecondFactorPrompt }) {
  const { t } = useTranslation();
  const { secondFactor, needEnrollment, finishEnrollment, logout } = useAuth();
  const [recovery, setRecovery] = useState(!prompt.methods.includes("totp"));
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const passkey = prompt.methods.includes("webauthn") && prompt.webauthn;
  // 새로 고침 뒤에는 둘째 단계의 방법(패스키·강제 등록)을 모른다. 다시 로그인하면 진짜 안내를 받는다.
  const unknown = isUnknownPrompt(prompt);
  // 인증 앱도 패스키도 묻지 않고 복구 코드만 물으면 요소는 패스키뿐인데 이 주소(IP)에서는 쓸 수 없다.
  const passkeyElsewhere =
    !unknown && !prompt.methods.includes("totp") && !prompt.methods.includes("webauthn");

  async function send(body: Parameters<typeof secondFactor>[0]) {
    setBusy(true);
    setError(null);
    try {
      await secondFactor(body);
    } catch (err) {
      if (err instanceof ApiError && err.code === "enrollment_required") {
        needEnrollment();
      } else if (err instanceof ApiError && err.code === "already_signed_in") {
        // 다른 탭에서 둘째 단계를 이미 마쳤다(같은 쿠키가 정식 세션이다).
        await finishEnrollment();
      } else if (
        err instanceof ApiError &&
        err.status === 401 &&
        err.code !== "invalid_second_factor"
      ) {
        // 대기 세션이 끝났다(5분). 비밀번호부터 다시 한다.
        await logout();
      } else {
        // 시도 제한(429)은 둘째 단계 전용 문구로 기다리라고 알린다.
        setError(
          err instanceof ApiError
            ? codeMessage(t, err.code, err.message, "login.secondFactor")
            : errorMessage(t, err),
        );
      }
    } finally {
      setBusy(false);
    }
  }

  async function signInWithPasskey() {
    if (!prompt.webauthn) {
      return;
    }
    // 브라우저 패스키 창이 열린 동안 코드 폼을 내지 못하게 한다.
    setBusy(true);
    setError(null);
    let credential;
    try {
      credential = await getAssertion(prompt.webauthn);
    } catch (err) {
      setError(errorMessage(t, err));
      return;
    } finally {
      setBusy(false);
    }
    if (credential) {
      await send({ webauthn: credential });
    }
  }

  return (
    <div className="flex flex-col gap-4">
      {unknown && (
        <p role="status" className="text-sm">
          {t("login.secondFactor.reloaded")}
        </p>
      )}
      <p className="text-sm text-muted-foreground">
        {t(recovery ? "login.secondFactor.recoveryHint" : "login.secondFactor.hint")}
      </p>
      {passkeyElsewhere && (
        <p className="text-sm text-muted-foreground">{t("login.secondFactor.passkeyDomain")}</p>
      )}
      <form
        onSubmit={(e) => {
          e.preventDefault();
          const value = code.trim();
          void send(recovery ? { recovery: value } : { totp: value.replace(/\s/g, "") });
        }}
        className="flex flex-col gap-4"
      >
        <div className="flex flex-col gap-2">
          <Label htmlFor="second-factor">
            {t(recovery ? "login.secondFactor.recovery" : "login.secondFactor.code")}
          </Label>
          <Input
            id="second-factor"
            autoComplete="one-time-code"
            inputMode={recovery ? "text" : "numeric"}
            autoFocus
            required
            aria-invalid={error ? true : undefined}
            aria-describedby={error ? "second-factor-error" : undefined}
            value={code}
            onChange={(e) => setCode(e.target.value)}
          />
        </div>
        {error && (
          <p id="second-factor-error" role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        <Button type="submit" disabled={busy}>
          {t("login.secondFactor.verify")}
        </Button>
      </form>
      {passkey &&
        (passkeysSupported() ? (
          <Button variant="outline" disabled={busy} onClick={() => void signInWithPasskey()}>
            {t("login.secondFactor.passkey")}
          </Button>
        ) : (
          <p className="text-sm text-muted-foreground">{t("twoFactor.passkeyBrowser")}</p>
        ))}
      <div className="flex flex-wrap justify-between gap-2">
        {prompt.methods.includes("recovery") && prompt.methods.includes("totp") && (
          <Button
            variant="link"
            className="px-0"
            onClick={() => {
              setRecovery(!recovery);
              setCode("");
              setError(null);
            }}
          >
            {t(recovery ? "login.secondFactor.useCode" : "login.secondFactor.useRecovery")}
          </Button>
        )}
        <Button
          variant={unknown ? "default" : "link"}
          className={unknown ? undefined : "px-0"}
          onClick={() => void logout()}
        >
          {t("twoFactor.backToLogin")}
        </Button>
      </div>
    </div>
  );
}
