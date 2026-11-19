// 설정 › 2단계 인증(누구나 자기 것)과 설정 › 보안(2FA 정책, admin) — 계획 62 설계 §10·§13 웹.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Trash2 } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import {
  PasskeySetup,
  type Proof,
  type Prove,
  RecoveryCodes,
  TotpSetup,
} from "@/accounts/TwoFactorSetup";
import { passkeysSupported } from "@/accounts/webauthn";
import { api, call } from "@/api/client";
import { useSettings } from "@/api/live";
import type { Role } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { formatDate } from "@/lib/format";
import { patchSettings, settingsError } from "@/settings/save";

const TWO_FACTOR_KEY = ["2fa"] as const;

/**
 * 본인 확인: 인증 앱·복구 코드, 또는 로컬 비밀번호(외부 사용자는 코드만). 빼기·다시 만들기와, 요소가 있을 때의
 * 요소 더하기가 쓴다.
 * `run`이 실패하면 창에 오류를 보인다.
 */
function ProofDialog({
  title,
  submitLabel,
  destructive,
  onClose,
  run,
}: {
  title: string;
  submitLabel: string;
  destructive?: boolean;
  onClose(): void;
  run(proof: Proof): Promise<void>;
}) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const local = !me.name.includes("@");
  const [code, setCode] = useState("");
  const [password, setPassword] = useState("");
  return (
    <FormDialog
      title={title}
      description={t(local ? "twoFactor.proofHint" : "twoFactor.proofHintCode")}
      submitLabel={submitLabel}
      destructive={destructive}
      submitDisabled={!code.trim() && !password}
      onClose={onClose}
      onSubmit={() => run(code.trim() ? { code: code.trim() } : { password })}
    >
      <Field id="proof-code" label={t("twoFactor.codeOrRecovery")}>
        <Input
          id="proof-code"
          autoComplete="one-time-code"
          value={code}
          onChange={(e) => setCode(e.target.value)}
        />
      </Field>
      {local && (
        <Field id="proof-password" label={t("twoFactor.orPassword")}>
          <Input
            id="proof-password"
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </Field>
      )}
    </FormDialog>
  );
}

type Dialog =
  | { kind: "disable" }
  | { kind: "regenerate" }
  | { kind: "passkey"; id: string; name: string }
  | { kind: "add"; title: string; send(proof: Proof): Promise<void> };

/** 설정 › 2단계 인증: 인증 앱·복구 코드·패스키 */
export function AccountPage() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const status = useQuery({
    queryKey: TWO_FACTOR_KEY,
    queryFn: () => call(api.GET("/auth/2fa")),
  });
  const [codes, setCodes] = useState<string[] | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const titleId = useId();
  const changed = () => queryClient.invalidateQueries({ queryKey: TWO_FACTOR_KEY });
  const gotCodes = async (got: string[]) => {
    await changed();
    if (got.length > 0) {
      setCodes(got);
    } else {
      toast.success(t("twoFactor.saved"));
    }
  };

  if (!status.data) {
    return <ListPending error={status.error} />;
  }
  const s = status.data;
  // 이미 요소가 있으면 더하기 전에 본인 확인(서버 `proof_required`)
  const prove: Prove | undefined =
    s.totp || s.passkeys.length > 0
      ? (title, send) => setDialog({ kind: "add", title, send })
      : undefined;
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id={titleId}
        title={t("nav.account")}
        description={t("twoFactor.pageHint")}
        actions={s.required && <Badge variant="secondary">{t("twoFactor.required")}</Badge>}
      />
      {!s.available && (
        <p role="status" className="text-sm text-warning-fg">
          {t("twoFactor.unavailable")}
        </p>
      )}
      {codes && (
        <Card>
          <CardContent className="pt-6">
            <RecoveryCodes codes={codes} onDone={() => setCodes(null)} />
          </CardContent>
        </Card>
      )}
      <Card>
        <CardHeader>
          <CardTitle>
            <h2>{t("twoFactor.app")}</h2>
          </CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          <p className="text-sm">{t(s.totp ? "twoFactor.appOn" : "twoFactor.appOff")}</p>
          {s.available && !s.totp && (
            <TotpSetup onDone={(got) => void gotCodes(got)} prove={prove} />
          )}
          {s.available && s.totp && (
            <div>
              <Button variant="outline" onClick={() => setDialog({ kind: "disable" })}>
                {t("twoFactor.disableApp")}
              </Button>
            </div>
          )}
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>
            <h2>{t("twoFactor.recoveryTitle")}</h2>
          </CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          <p className="text-sm">{t("twoFactor.recoveryLeft", { count: s.recovery_codes })}</p>
          {s.available && (s.totp || s.passkeys.length > 0) && (
            <div>
              <Button variant="outline" onClick={() => setDialog({ kind: "regenerate" })}>
                {t("twoFactor.regenerate")}
              </Button>
            </div>
          )}
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>
            <h2>{t("twoFactor.passkeys")}</h2>
          </CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          {s.passkeys.length > 0 && (
            <ul className="flex flex-col gap-1 text-sm">
              {s.passkeys.map((p) => (
                <li key={p.id} className="flex items-center justify-between gap-2">
                  <span>
                    <span className="font-medium">{p.name}</span>
                    <span className="ml-2 text-xs text-muted-foreground">
                      {p.rp_id} · {formatDate(new Date(p.created_at * 1000), i18n.language)}
                    </span>
                  </span>
                  {s.available && (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: p.name })}
                      onClick={() => setDialog({ kind: "passkey", id: p.id, name: p.name })}
                    >
                      <Trash2 />
                    </Button>
                  )}
                </li>
              ))}
            </ul>
          )}
          {!s.rp_id ? (
            // IP 주소 접속에서는 브라우저가 WebAuthn을 막는다(설계 §10.1).
            <p className="text-sm text-muted-foreground">{t("twoFactor.passkeyIp")}</p>
          ) : !passkeysSupported() ? (
            <p className="text-sm text-muted-foreground">{t("twoFactor.passkeyBrowser")}</p>
          ) : (
            s.available && <PasskeySetup onDone={(got) => void gotCodes(got)} prove={prove} />
          )}
        </CardContent>
      </Card>
      {dialog?.kind === "disable" && (
        <ProofDialog
          title={t("twoFactor.disableApp")}
          submitLabel={t("twoFactor.disable")}
          destructive
          onClose={() => setDialog(null)}
          run={async (proof) => {
            await call(api.DELETE("/auth/2fa/totp", { body: proof }));
            toast.success(t("twoFactor.appDisabled"));
            await changed();
          }}
        />
      )}
      {dialog?.kind === "add" && (
        <ProofDialog
          title={dialog.title}
          submitLabel={t("twoFactor.confirm")}
          onClose={() => setDialog(null)}
          run={dialog.send}
        />
      )}
      {dialog?.kind === "regenerate" && (
        <ProofDialog
          title={t("twoFactor.regenerate")}
          submitLabel={t("twoFactor.regenerate")}
          onClose={() => setDialog(null)}
          run={async (proof) => {
            const result = await call(api.POST("/auth/2fa/recovery", { body: proof }));
            await gotCodes(result.codes);
          }}
        />
      )}
      {dialog?.kind === "passkey" && (
        <ProofDialog
          title={t("twoFactor.removePasskey", { name: dialog.name })}
          submitLabel={t("common.delete")}
          destructive
          onClose={() => setDialog(null)}
          run={async (proof) => {
            await call(
              api.DELETE("/auth/2fa/webauthn/{id}", {
                params: { path: { id: dialog.id } },
                body: proof,
              }),
            );
            toast.success(t("twoFactor.passkeyRemoved", { name: dialog.name }));
            await changed();
          }}
        />
      )}
    </div>
  );
}

const POLICY_ROLES: readonly Role[] = ["admin", "operator", "viewer", "none"];

/** 설정 › 보안: 2단계 인증 정책(전역 역할·영역, 패스키 RP ID) */
export function SecurityPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const admin = perms.has("cluster.admin");
  const settings = useSettings();
  if (!admin) {
    return (
      <div className="flex flex-col gap-4">
        <PageHeader title={t("nav.security")} />
        <p className="text-sm text-muted-foreground">{t("users.adminOnly")}</p>
      </div>
    );
  }
  if (!settings.data) {
    return <ListPending error={settings.error} />;
  }
  // 저장된 정책이 바뀌면(watch) 폼을 새로 연다. 다른 설정(원격 측정·HA …)이 바뀐 것으로는 고치던 값을 버리지 않는다.
  return (
    <PolicyForm
      key={JSON.stringify(settings.data.spec.two_factor ?? null)}
      revision={settings.data.revision}
      policy={settings.data.spec.two_factor ?? null}
    />
  );
}

function PolicyForm({
  revision,
  policy,
}: {
  revision: number;
  policy: { roles?: Role[]; realms?: string[]; rp_id?: string } | null;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const realms = useQuery({
    queryKey: ["realms"],
    queryFn: () => call(api.GET("/realms")),
    select: (list) => list.items,
  });
  const [roles, setRoles] = useState<ReadonlySet<string>>(() => new Set(policy?.roles ?? []));
  const [realmSet, setRealmSet] = useState<ReadonlySet<string>>(
    () => new Set(policy?.realms ?? []),
  );
  const [rpId, setRpId] = useState(policy?.rp_id ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // OIDC 영역은 IdP가 MFA를 맡는다(서버도 400).
  const realmNames = [
    "local",
    ...(realms.data ?? []).filter((r) => r.backend.kind === "ldap").map((r) => r.name),
  ];
  const flip = (set: ReadonlySet<string>, value: string, on: boolean) => {
    const next = new Set(set);
    if (on) {
      next.add(value);
    } else {
      next.delete(value);
    }
    return next;
  };

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await patchSettings(queryClient, revision, {
        two_factor: {
          roles: POLICY_ROLES.filter((r) => roles.has(r)),
          realms: [...realmSet],
          ...(rpId.trim() && { rp_id: rpId.trim() }),
        },
      });
      toast.success(t("settings.saved"));
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={(e) => void save(e)} className="flex flex-col gap-4">
      <PageHeader title={t("nav.security")} description={t("twoFactor.policyHint")} />
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1 text-sm font-medium">{t("twoFactor.policyRoles")}</legend>
        {POLICY_ROLES.map((role) => (
          <div key={role} className="flex items-center gap-2">
            <Checkbox
              id={`policy-role-${role}`}
              checked={roles.has(role)}
              onCheckedChange={(v) => setRoles(flip(roles, role, v === true))}
            />
            <Label htmlFor={`policy-role-${role}`}>{t(`roles.${role}`)}</Label>
          </div>
        ))}
      </fieldset>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1 text-sm font-medium">{t("twoFactor.policyRealms")}</legend>
        {realmNames.map((realm) => (
          <div key={realm} className="flex items-center gap-2">
            <Checkbox
              id={`policy-realm-${realm}`}
              checked={realmSet.has(realm)}
              onCheckedChange={(v) => setRealmSet(flip(realmSet, realm, v === true))}
            />
            <Label htmlFor={`policy-realm-${realm}`}>
              {realm === "local" ? t("login.localRealm") : realm}
            </Label>
          </div>
        ))}
      </fieldset>
      <Field id="policy-rp" label={t("twoFactor.rpId")} hint={t("twoFactor.rpIdHint")}>
        {(a11y) => (
          <Input
            id="policy-rp"
            className="w-72"
            placeholder="opl.example.com"
            value={rpId}
            onChange={(e) => setRpId(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <div>
        <Button type="submit" disabled={busy}>
          {t("common.save")}
        </Button>
      </div>
    </form>
  );
}
