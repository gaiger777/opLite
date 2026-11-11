import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Navigate, useSearchParams } from "react-router";

import { api, call } from "@/api/client";
import { useAuth } from "@/auth/AuthProvider";
import { safeNext } from "@/auth/RequireAuth";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { codeMessage, errorMessage } from "@/i18n/errors";

/** SSO 콜백이 `/login?error=<code>`로 돌려보내는 코드(`oidc_login`). 모르는 코드는 `sso_failed`로 보인다. */
const SSO_ERRORS = ["sso_failed", "no_role", "realm_unavailable", "too_many_attempts"];

export function LoginPage() {
  const { t } = useTranslation();
  const { principal, login } = useAuth();
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

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setSsoError(null);
    try {
      await login(username, password, realm === "local" ? undefined : realm);
    } catch (err) {
      setError(errorMessage(t, err));
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
