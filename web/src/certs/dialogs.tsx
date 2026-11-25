import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import {
  type AcmeAccount,
  type AcmeDns,
  DIRECTORIES,
  DNS_KINDS,
  type DnsKind,
  type DnsProvider,
  http01Missing,
  refreshCerts,
  splitNames,
} from "@/certs/api";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";

const TEXTAREA =
  "w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 font-mono text-xs break-all outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30";

const NAME = /^[a-z0-9][a-z0-9-]{0,62}$/;

function Check({
  id,
  checked,
  onChange,
  children,
}: {
  id: string;
  checked: boolean;
  onChange(v: boolean): void;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center gap-2">
      <Checkbox id={id} checked={checked} onCheckedChange={(v) => onChange(v === true)} />
      <Label htmlFor={id} className="font-normal">
        {children}
      </Label>
    </div>
  );
}

/**
 * ACME 계정 만들기(계획 73 §11.1): 디렉터리(별명 또는 URL)·연락처·약관 동의(필수)·EAB(ZeroSSL·Google·사내 CA)·사내 CA 루트.
 * 서버가 곧 CA에 등록한다(키는 서버가 만든다). EAB HMAC 키는 저장 뒤 다시 보이지 않는다.
 */
export function AccountDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [directory, setDirectory] = useState<string>("letsencrypt");
  const [url, setUrl] = useState("");
  const [email, setEmail] = useState("");
  const [tos, setTos] = useState(false);
  const [kid, setKid] = useState("");
  const [hmac, setHmac] = useState("");
  const [ca, setCa] = useState("");
  const [tried, setTried] = useState(false);
  const custom = directory === "custom";
  const errors = {
    name: NAME.test(name) ? undefined : "certs.errors.name",
    url: !custom || /^https:\/\/\S+$/.test(url.trim()) ? undefined : "certs.errors.url",
    tos: tos ? undefined : "certs.errors.tos",
    eab: (kid.trim() === "") === (hmac.trim() === "") ? undefined : "certs.errors.eab",
  };
  const shown = (k: keyof typeof errors) => (tried ? errors[k] : undefined);
  return (
    <FormDialog
      title={t("certs.account.title")}
      description={t("certs.account.hint")}
      submitLabel={t("certs.account.submit")}
      onClose={onClose}
      onSubmit={async () => {
        setTried(true);
        if (Object.values(errors).some(Boolean)) {
          return false;
        }
        await call(
          api.POST("/acme-accounts", {
            body: {
              name,
              directory: custom ? url.trim() : directory,
              agree_tos: true,
              ...(email.trim() && { email: email.trim() }),
              ...(kid.trim() && { eab: { kid: kid.trim(), hmac_key: hmac.trim() } }),
              ...(ca.trim() && { ca_bundle: ca.trim() }),
            },
          }),
        );
        await refreshCerts(queryClient);
      }}
    >
      <Field id="account-name" label={t("common.name")} error={shown("name")}>
        {(a11y) => (
          <Input
            id="account-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="account-directory" label={t("certs.account.directory")}>
        <NativeSelect
          id="account-directory"
          value={directory}
          onChange={(e) => setDirectory(e.target.value)}
        >
          {DIRECTORIES.map((d) => (
            <NativeSelectOption key={d} value={d}>
              {t(`certs.directories.${d}`)}
            </NativeSelectOption>
          ))}
          <NativeSelectOption value="custom">{t("certs.directories.custom")}</NativeSelectOption>
        </NativeSelect>
      </Field>
      {custom && (
        <Field id="account-url" label={t("certs.account.url")} error={shown("url")}>
          {(a11y) => (
            <Input
              id="account-url"
              placeholder="https://ca.example.com/acme/directory"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      )}
      <Field id="account-email" label={t("certs.account.email")}>
        <Input
          id="account-email"
          type="email"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
      </Field>
      <div className="flex flex-col gap-1">
        <Check id="account-tos" checked={tos} onChange={setTos}>
          {t("certs.account.tos")}
        </Check>
        {shown("tos") && <p className="text-xs text-destructive">{t(errors.tos!)}</p>}
      </div>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1 text-sm font-medium">{t("certs.account.eab")}</legend>
        <p className="text-xs text-muted-foreground">{t("certs.account.eabHint")}</p>
        <div className="grid gap-2 sm:grid-cols-2">
          <Field id="account-kid" label={t("certs.account.kid")} error={shown("eab")}>
            {(a11y) => (
              <Input
                id="account-kid"
                autoComplete="off"
                value={kid}
                onChange={(e) => setKid(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field id="account-hmac" label={t("certs.account.hmac")} hint={t("certs.secretOnce")}>
            {(a11y) => (
              <Input
                id="account-hmac"
                type="password"
                autoComplete="off"
                value={hmac}
                onChange={(e) => setHmac(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </div>
      </fieldset>
      <Field id="account-ca" label={t("certs.account.ca")} hint={t("certs.account.caHint")}>
        {(a11y) => (
          <textarea
            id="account-ca"
            rows={3}
            spellCheck={false}
            autoComplete="off"
            value={ca}
            onChange={(e) => setCa(e.target.value)}
            className={TEXTAREA}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}

/** 공급자 종류마다 칸(이름 → 비밀인지) */
const DNS_FIELDS: Record<DnsKind, { key: string; secret?: boolean; optional?: boolean }[]> = {
  cloudflare: [
    { key: "api_token", secret: true },
    { key: "zone_id", optional: true },
  ],
  route53: [
    { key: "access_key_id" },
    { key: "secret_access_key", secret: true },
    { key: "region", optional: true },
    { key: "hosted_zone_id", optional: true },
  ],
  rfc2136: [
    { key: "server" },
    { key: "key_name" },
    { key: "secret", secret: true },
    { key: "zone", optional: true },
  ],
  power_dns: [
    { key: "url" },
    { key: "api_key", secret: true },
    { key: "server_id", optional: true },
  ],
  hook: [{ key: "path" }],
};

/**
 * DNS 공급자 만들기(계획 73 §11.4): 종류(Cloudflare·Route 53·RFC 2136·PowerDNS·훅)마다 칸, 맡는 영역. 비밀 칸은 저장한 뒤
 * 다시 보이지 않는다(보기·watch·감사에서 빠진다).
 */
export function DnsDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [kind, setKind] = useState<DnsKind>("cloudflare");
  const [values, setValues] = useState<Record<string, string>>({});
  const [zones, setZones] = useState("");
  const [tried, setTried] = useState(false);
  const fields = DNS_FIELDS[kind];
  const missing = fields.filter((f) => !f.optional && !(values[f.key] ?? "").trim());
  const hookBad = kind === "hook" && !(values.path ?? "").trim().startsWith("/etc/opl/acme-hooks/");
  return (
    <FormDialog
      title={t("certs.dns.title")}
      description={t("certs.dns.hint")}
      submitLabel={t("certs.dns.submit")}
      onClose={onClose}
      onSubmit={async () => {
        setTried(true);
        if (!NAME.test(name) || missing.length > 0 || hookBad) {
          return false;
        }
        const provider = Object.fromEntries([
          ["kind", kind],
          ...fields
            .map((f) => [f.key, (values[f.key] ?? "").trim()] as const)
            .filter(([, v]) => v !== ""),
        ]) as DnsProvider;
        await call(
          api.POST("/acme-dns", {
            body: { name, provider, ...(splitNames(zones).length && { zones: splitNames(zones) }) },
          }),
        );
        await refreshCerts(queryClient);
      }}
    >
      <Field
        id="dns-name"
        label={t("common.name")}
        error={tried && !NAME.test(name) ? "certs.errors.name" : undefined}
      >
        {(a11y) => (
          <Input id="dns-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      <Field id="dns-kind" label={t("certs.dns.kind")}>
        <NativeSelect
          id="dns-kind"
          value={kind}
          onChange={(e) => {
            setKind(e.target.value as DnsKind);
            setValues({});
          }}
        >
          {DNS_KINDS.map((k) => (
            <NativeSelectOption key={k} value={k}>
              {t(`certs.dnsKinds.${k}`)}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      </Field>
      {fields.map((f) => (
        <Field
          key={`${kind}-${f.key}`}
          id={`dns-${f.key}`}
          label={t(`certs.dnsFields.${f.key}`)}
          hint={
            f.secret
              ? t("certs.secretOnce")
              : f.key === "path"
                ? t("certs.dns.hookHint")
                : undefined
          }
          error={
            tried && missing.includes(f)
              ? "certs.errors.required"
              : tried && f.key === "path" && hookBad
                ? "certs.errors.hook"
                : undefined
          }
        >
          {(a11y) => (
            <Input
              id={`dns-${f.key}`}
              type={f.secret ? "password" : "text"}
              autoComplete="off"
              value={values[f.key] ?? ""}
              onChange={(e) => setValues((v) => ({ ...v, [f.key]: e.target.value }))}
              {...a11y}
            />
          )}
        </Field>
      ))}
      <Field id="dns-zones" label={t("certs.dns.zones")} hint={t("certs.dns.zonesHint")}>
        {(a11y) => (
          <Input
            id="dns-zones"
            placeholder="example.com"
            value={zones}
            onChange={(e) => setZones(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}

/**
 * 인증서 만들기(계획 73 §11.2): 이름들(`{node}`·`*.`), 계정, 노드마다/공유, 도전(HTTP-01 — 포트 80 리스너가 없는 노드가 있으면
 * 경고, DNS-01 — 공급자), 키(P-256·P-384 — RSA는 이 빌드가 만들지 못함), 프로필. 리더가 다음 틱에 주문한다.
 */
export function CertificateDialog({
  accounts,
  dns,
  onClose,
}: {
  accounts: readonly AcmeAccount[];
  dns: readonly AcmeDns[];
  onClose(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const [name, setName] = useState("");
  const [names, setNames] = useState("");
  const [account, setAccount] = useState(accounts[0]?.name ?? "");
  const [scope, setScope] = useState<"per_node" | "shared">("shared");
  const [challenge, setChallenge] = useState<"http01" | "dns01">(
    dns.length > 0 ? "dns01" : "http01",
  );
  const [provider, setProvider] = useState(dns[0]?.name ?? "");
  const [key, setKey] = useState<"ecdsa-p256" | "ecdsa-p384">("ecdsa-p256");
  const [profile, setProfile] = useState("");
  const [tried, setTried] = useState(false);
  const list = splitNames(names);
  const profiles = accounts.find((a) => a.name === account)?.status.profiles ?? [];
  const missing =
    challenge === "http01" ? http01Missing(nodes.data ?? [], scope === "per_node", []) : [];
  const errors = {
    name: NAME.test(name) ? undefined : "certs.errors.name",
    names: list.length > 0 ? undefined : "certs.errors.names",
    wildcard:
      challenge === "http01" && list.some((n) => n.startsWith("*."))
        ? "certs.errors.wildcard"
        : undefined,
    account: account ? undefined : "certs.errors.account",
    dns: challenge === "dns01" && !provider ? "certs.errors.dns" : undefined,
  };
  return (
    <FormDialog
      title={t("certs.cert.title")}
      description={t("certs.cert.hint")}
      submitLabel={t("certs.cert.submit")}
      onClose={onClose}
      onSubmit={async () => {
        setTried(true);
        if (Object.values(errors).some(Boolean)) {
          return false;
        }
        await call(
          api.POST("/certificates", {
            body: {
              name,
              account,
              names: list,
              scope: scope === "shared" ? { kind: "shared" } : { kind: "per_node" },
              challenge:
                challenge === "http01" ? { kind: "http01" } : { kind: "dns01", dns: provider },
              key,
              precheck: true,
              ...(profile && { profile }),
            },
          }),
        );
        await refreshCerts(queryClient);
      }}
    >
      <Field id="cert-name" label={t("common.name")} error={tried ? errors.name : undefined}>
        {(a11y) => (
          <Input id="cert-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      <Field
        id="cert-names"
        label={t("certs.cert.names")}
        hint={t("certs.cert.namesHint")}
        error={tried ? (errors.names ?? errors.wildcard) : undefined}
      >
        {(a11y) => (
          <Input
            id="cert-names"
            placeholder="api.example.com, {node}.example.com"
            value={names}
            onChange={(e) => setNames(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="cert-account"
        label={t("certs.cert.account")}
        error={tried ? errors.account : undefined}
      >
        <NativeSelect
          id="cert-account"
          value={account}
          onChange={(e) => setAccount(e.target.value)}
        >
          {accounts.map((a) => (
            <NativeSelectOption key={a.id} value={a.name}>
              {a.name}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      </Field>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field id="cert-scope" label={t("certs.cert.scope")}>
          <NativeSelect
            id="cert-scope"
            value={scope}
            onChange={(e) => setScope(e.target.value as "per_node" | "shared")}
          >
            <NativeSelectOption value="shared">{t("certs.scope.shared")}</NativeSelectOption>
            <NativeSelectOption value="per_node">{t("certs.scope.per_node")}</NativeSelectOption>
          </NativeSelect>
        </Field>
        <Field id="cert-challenge" label={t("certs.cert.challenge")}>
          <NativeSelect
            id="cert-challenge"
            value={challenge}
            onChange={(e) => setChallenge(e.target.value as "http01" | "dns01")}
          >
            <NativeSelectOption value="http01">HTTP-01</NativeSelectOption>
            <NativeSelectOption value="dns01">DNS-01</NativeSelectOption>
          </NativeSelect>
        </Field>
      </div>
      {challenge === "dns01" && (
        <Field id="cert-dns" label={t("certs.cert.dns")} error={tried ? errors.dns : undefined}>
          <NativeSelect
            id="cert-dns"
            value={provider}
            onChange={(e) => setProvider(e.target.value)}
          >
            {dns.map((d) => (
              <NativeSelectOption key={d.id} value={d.name}>
                {d.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
      )}
      {missing.length > 0 && (
        <p role="alert" className="rounded-md bg-warning-soft px-3 py-2 text-sm text-warning-fg">
          {t("certs.cert.http01Missing", { nodes: missing.join(", ") })}
        </p>
      )}
      <div className="grid gap-3 sm:grid-cols-2">
        <Field id="cert-key" label={t("certs.cert.key")} hint={t("certs.cert.keyHint")}>
          {(a11y) => (
            <NativeSelect
              id="cert-key"
              value={key}
              onChange={(e) => setKey(e.target.value as "ecdsa-p256" | "ecdsa-p384")}
              {...a11y}
            >
              <NativeSelectOption value="ecdsa-p256">ECDSA P-256</NativeSelectOption>
              <NativeSelectOption value="ecdsa-p384">ECDSA P-384</NativeSelectOption>
            </NativeSelect>
          )}
        </Field>
        <Field id="cert-profile" label={t("certs.cert.profile")}>
          <NativeSelect
            id="cert-profile"
            value={profile}
            onChange={(e) => setProfile(e.target.value)}
          >
            <NativeSelectOption value="">{t("certs.cert.defaultProfile")}</NativeSelectOption>
            {profiles.map((p) => (
              <NativeSelectOption key={p} value={p}>
                {p}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
      </div>
    </FormDialog>
  );
}
