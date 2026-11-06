import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
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
import { errorMessage } from "@/i18n/errors";

export function LoginPage() {
  const { t } = useTranslation();
  const { principal, login } = useAuth();
  const [params] = useSearchParams();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [realm, setRealm] = useState("local");
  const [error, setError] = useState<string | null>(null);
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
  const choices = realms.data ?? [];

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
            <h1>opLite</h1>
          </CardTitle>
        </CardHeader>
        <CardContent>
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
