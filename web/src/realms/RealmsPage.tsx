import { useQuery, useQueryClient } from "@tanstack/react-query";
import { FlaskConical, Pencil, Plus, Trash2, X } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { Role } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { CopyButton } from "@/components/CopyButton";
import { Field } from "@/components/Field";
import { focusBack } from "@/components/focusBack";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import {
  checkForm,
  emptyForm,
  formOf,
  type RealmForm,
  type RealmTestResult,
  type RealmView,
  redirectUri,
  requestOf,
} from "@/realms/realms";

const ROLES: readonly Role[] = ["admin", "operator", "viewer"];

type Dialogs = { kind: "create" } | { kind: "edit" | "test"; realm: RealmView };

/**
 * 인증 영역: 목록과 LDAP·OIDC 영역 만들기·고치기·지우기·시험. admin만 다룬다. 서비스 계정 비밀번호가 있어
 * watch로 받지 않으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다.
 */
export function RealmsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const admin = can(me.role, "read", "realm");
  const queryClient = useQueryClient();
  const realms = useQuery({
    queryKey: ["realms"],
    queryFn: () => call(api.GET("/realms")),
    select: (list) => [...list.items].sort((a, b) => a.name.localeCompare(b.name)),
    enabled: admin,
  });
  const [dialog, setDialog] = useState<Dialogs | null>(null);
  const [deleting, setDeleting] = useState<RealmView | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const close = () => setDialog(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["realms"] });

  // 메뉴에 없지만 주소로 직접 들어올 수 있다. 관리자가 아니면 읽지 않고 안내한다.
  if (!admin) {
    return (
      <div className="flex flex-col gap-4">
        <h1 className="text-2xl font-semibold">{t("nav.realms")}</h1>
        <p className="text-sm text-muted-foreground">{t("realms.adminOnly")}</p>
      </div>
    );
  }

  const list = realms.data ?? [];
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="realms-title" className="text-2xl font-semibold">
          {t("nav.realms")}
        </h1>
        <Button
          disabled={!realms.isSuccess}
          onClick={(e) => {
            opener.current = e.currentTarget;
            setDialog({ kind: "create" });
          }}
        >
          <Plus />
          {t("common.create")}
        </Button>
      </div>
      <p className="text-sm text-muted-foreground">{t("realms.pageHint")}</p>
      <Table aria-labelledby="realms-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("realms.display")}</TableHead>
            <TableHead>{t("realms.kind")}</TableHead>
            <TableHead>{t("realms.url")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={5} query={realms} empty={t("realms.empty")} />
          )}
          {list.map((realm) => (
            <TableRow key={realm.id}>
              <TableCell className="font-medium">{realm.name}</TableCell>
              <TableCell>{realm.display ?? "—"}</TableCell>
              <TableCell>{t(`realms.kinds.${realm.backend.kind}`)}</TableCell>
              <TableCell className="font-mono text-xs">
                {realm.backend.kind === "ldap" ? realm.backend.url : realm.backend.issuer}
              </TableCell>
              <TableCell className="text-right whitespace-nowrap">
                <Button
                  variant="outline"
                  size="sm"
                  aria-label={t("realms.testNamed", { name: realm.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDialog({ kind: "test", realm });
                  }}
                >
                  <FlaskConical />
                  {t("realms.test")}
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  className="ml-2"
                  aria-label={t("realms.editNamed", { name: realm.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDialog({ kind: "edit", realm });
                  }}
                >
                  <Pencil />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("common.deleteNamed", { name: realm.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDeleting(realm);
                    setDeleteOpen(true);
                  }}
                >
                  <Trash2 />
                </Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {(dialog?.kind === "create" || dialog?.kind === "edit") && (
        <RealmDialog
          realm={dialog.kind === "edit" ? dialog.realm : null}
          taken={list.map((r) => r.name)}
          returnFocus={opener}
          onClose={close}
          onDone={changed}
        />
      )}
      {dialog?.kind === "test" && (
        <TestDialog realm={dialog.realm} returnFocus={opener} onClose={close} />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("realms.deleteTitle", { name: deleting?.name })}
        description={t("realms.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const realm = deleting!;
          await call(api.DELETE("/realms/{key}", { params: { path: { key: realm.name } } }));
          toast.success(t("realms.deleted", { name: realm.name }));
          await changed();
        }}
      />
    </div>
  );
}

function RoleOptions() {
  const { t } = useTranslation();
  return ROLES.map((role) => (
    <NativeSelectOption key={role} value={role}>
      {t(`roles.${role}`)}
    </NativeSelectOption>
  ));
}

/** 영역 만들기(`realm`이 null — 종류를 고른다)와 고치기. 열 때만 그린다. */
function RealmDialog({
  realm,
  taken,
  returnFocus,
  onClose,
  onDone,
}: {
  realm: RealmView | null;
  /** 있는 영역 이름 */
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onDone(): Promise<void>;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState<RealmForm>(() => (realm ? formOf(realm) : emptyForm()));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<RealmForm>) => setForm((f) => ({ ...f, ...part }));
  // 행을 빼면 행 번호로 단 오류가 다른 행을 가리키므로 지운다.
  const changeGroups = (groups: RealmForm["groups"]) => {
    change({ groups });
    setErrors({});
  };
  const saved = (realm?.backend.kind === "ldap" && realm.backend.has_bind_password) || false;
  const savedSecret = (realm?.backend.kind === "oidc" && realm.backend.has_client_secret) || false;
  const oidc = form.kind === "oidc";

  async function submit() {
    const found = checkForm(form, realm ? null : taken, realm);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const body = requestOf(form, realm !== null);
    if (realm) {
      await call(
        api.PUT("/realms/{key}", {
          params: { path: { key: realm.name }, header: { "If-Match": realm.revision } },
          body,
        }),
      );
      toast.success(t("realms.saved", { name: realm.name }));
    } else {
      await call(api.POST("/realms", { body }));
      toast.success(t("realms.created", { name: form.name }));
    }
    await onDone();
  }

  const text = (id: string, key: keyof RealmForm, label: string, hint?: string) => (
    <Field id={id} label={label} error={errors[key]} hint={hint}>
      {(a11y) => (
        <Input
          id={id}
          autoComplete="off"
          spellCheck={false}
          value={form[key] as string}
          onChange={(e) => change({ [key]: e.target.value })}
          {...a11y}
        />
      )}
    </Field>
  );

  return (
    <FormDialog
      title={
        realm
          ? t("realms.editTitle", { name: realm.name })
          : t(oidc ? "realms.createOidcTitle" : "realms.createTitle")
      }
      description={t("realms.formDescription")}
      submitLabel={realm ? t("common.save") : t("common.create")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {/* 항목이 많으므로 대화상자 안에서 넘긴다. */}
      <div className="-mx-1 flex max-h-[60svh] flex-col gap-4 overflow-y-auto px-1">
        {/* 종류는 만들 때만 고른다. */}
        {!realm && (
          <Field id="realm-kind" label={t("realms.kind")}>
            <NativeSelect
              id="realm-kind"
              className="w-full"
              value={form.kind}
              onChange={(e) => {
                change({ kind: e.target.value as RealmForm["kind"] });
                setErrors({});
              }}
            >
              <NativeSelectOption value="ldap">{t("realms.kinds.ldap")}</NativeSelectOption>
              <NativeSelectOption value="oidc">{t("realms.kinds.oidc")}</NativeSelectOption>
            </NativeSelect>
          </Field>
        )}
        <Field id="realm-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="realm-name"
              autoComplete="off"
              readOnly={realm !== null}
              value={form.name}
              onChange={(e) => change({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        {text("realm-display", "display", t("realms.display"), t("realms.displayHint"))}
        {oidc ? (
          <>
            {text("realm-issuer", "issuer", t("realms.issuer"), t("realms.issuerHint"))}
            {text("realm-client-id", "clientId", t("realms.clientId"))}
            <Field
              id="realm-client-secret"
              label={t("realms.clientSecret")}
              error={errors.clientSecret}
              hint={t(savedSecret ? "realms.clientSecretSaved" : "realms.clientSecretHint")}
            >
              {(a11y) => (
                <Input
                  id="realm-client-secret"
                  type="password"
                  autoComplete="new-password"
                  disabled={form.clearClientSecret}
                  value={form.clientSecret}
                  onChange={(e) => change({ clientSecret: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            {savedSecret && (
              <div className="flex items-center gap-2">
                <Checkbox
                  id="realm-clear-secret"
                  checked={form.clearClientSecret}
                  onCheckedChange={(on) => change({ clearClientSecret: on === true })}
                />
                <Label htmlFor="realm-clear-secret">{t("realms.clearClientSecret")}</Label>
              </div>
            )}
            {text("realm-scopes", "scopes", t("realms.scopes"), t("realms.scopesHint"))}
            {text("realm-username-claim", "usernameClaim", t("realms.usernameClaim"))}
            {text(
              "realm-groups-claim",
              "groupsClaim",
              t("realms.groupsClaim"),
              t("realms.groupsClaimHint"),
            )}
            <Field
              id="realm-redirect-uri"
              label={t("realms.redirectUri")}
              hint={t("realms.redirectUriHint")}
            >
              {(a11y) => (
                <div className="flex items-center gap-2">
                  <Input
                    id="realm-redirect-uri"
                    readOnly
                    className="font-mono text-xs"
                    value={redirectUri()}
                    onFocus={(e) => e.target.select()}
                    {...a11y}
                  />
                  <CopyButton text={redirectUri()} label={t("realms.copyRedirectUri")} />
                </div>
              )}
            </Field>
          </>
        ) : (
          <>
            {text("realm-url", "url", t("realms.url"), t("realms.urlHint"))}
            {text("realm-base-dn", "baseDn", t("realms.baseDn"))}
            {text("realm-bind-dn", "bindDn", t("realms.bindDn"), t("realms.bindDnHint"))}
            <Field
              id="realm-bind-password"
              label={t("realms.bindPassword")}
              error={errors.bindPassword}
              hint={saved ? t("realms.bindPasswordSaved") : undefined}
            >
              {(a11y) => (
                <Input
                  id="realm-bind-password"
                  type="password"
                  autoComplete="new-password"
                  disabled={form.clearBindPassword}
                  value={form.bindPassword}
                  onChange={(e) => change({ bindPassword: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            {saved && (
              <div className="flex items-center gap-2">
                <Checkbox
                  id="realm-clear-password"
                  checked={form.clearBindPassword}
                  onCheckedChange={(on) => change({ clearBindPassword: on === true })}
                />
                <Label htmlFor="realm-clear-password">{t("realms.clearBindPassword")}</Label>
              </div>
            )}
            {text(
              "realm-user-filter",
              "userFilter",
              t("realms.userFilter"),
              t("realms.userFilterHint"),
            )}
            {text("realm-group-attr", "groupAttr", t("realms.groupAttr"))}
          </>
        )}
        <Field
          id="realm-ca"
          label={t("realms.caPem")}
          hint={t(oidc ? "realms.oidcCaPemHint" : "realms.caPemHint")}
        >
          {(a11y) => (
            <textarea
              id="realm-ca"
              rows={3}
              spellCheck={false}
              autoComplete="off"
              value={form.caPem}
              onChange={(e) => change({ caPem: e.target.value })}
              className="w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 font-mono text-xs break-all outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30"
              {...a11y}
            />
          )}
        </Field>
        <Field id="realm-timeout" label={t("realms.timeout")} error={errors.timeout}>
          {(a11y) => (
            <Input
              id="realm-timeout"
              inputMode="numeric"
              value={form.timeout}
              onChange={(e) => change({ timeout: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-1.5 text-sm font-medium">{t("realms.groups")}</legend>
          <p className="text-xs text-muted-foreground">
            {t(oidc ? "realms.oidcGroupsHint" : "realms.groupsHint")}
          </p>
          {form.groups.map((row, i) => {
            const n = i + 1;
            const error = errors[`group-${i}`];
            const errorId = `realm-group-${i}-error`;
            return (
              <div key={i} className="flex flex-col gap-1">
                <div className="flex items-center gap-2">
                  <Input
                    className="min-w-0 flex-1"
                    aria-label={t("realms.groupN", { n })}
                    aria-invalid={error ? true : undefined}
                    aria-describedby={error ? errorId : undefined}
                    spellCheck={false}
                    placeholder={oidc ? "opl-operators" : "cn=ops,ou=groups,dc=example,dc=com"}
                    value={row.group}
                    onChange={(e) =>
                      changeGroups(
                        form.groups.map((g, j) => (j === i ? { ...g, group: e.target.value } : g)),
                      )
                    }
                  />
                  {/* 역할 이름이 다 보이게 내용 너비를 지킨다. */}
                  <NativeSelect
                    className="min-w-28 shrink-0"
                    aria-label={t("realms.groupRoleN", { n })}
                    value={row.role}
                    onChange={(e) =>
                      changeGroups(
                        form.groups.map((g, j) =>
                          j === i ? { ...g, role: e.target.value as Role } : g,
                        ),
                      )
                    }
                  >
                    <RoleOptions />
                  </NativeSelect>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("realms.removeGroupN", { n })}
                    onClick={() => changeGroups(form.groups.filter((_, j) => j !== i))}
                  >
                    <X />
                  </Button>
                </div>
                {error && (
                  <p id={errorId} className="text-xs text-destructive">
                    {t(error)}
                  </p>
                )}
              </div>
            );
          })}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="self-start"
            onClick={() => changeGroups([...form.groups, { group: "", role: "viewer" }])}
          >
            <Plus />
            {t("realms.addGroup")}
          </Button>
        </fieldset>
        <Field id="realm-default-role" label={t("realms.defaultRole")}>
          <NativeSelect
            id="realm-default-role"
            className="w-full"
            value={form.defaultRole}
            onChange={(e) => change({ defaultRole: e.target.value as Role | "" })}
          >
            <NativeSelectOption value="">{t("realms.noDefaultRole")}</NativeSelectOption>
            <RoleOptions />
          </NativeSelect>
        </Field>
      </div>
    </FormDialog>
  );
}

/**
 * 영역 시험: LDAP은 디렉터리 사용자로 단계마다, OIDC는 입력 없이 IdP의 discovery와 키를 확인한다. 사용자·세션을
 * 만들지 않는다. 결과를 보이도록 시험한 뒤에도 열어 둔다.
 */
function TestDialog({
  realm,
  returnFocus,
  onClose,
}: {
  realm: RealmView;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<RealmTestResult | null>(null);
  const oidc = realm.backend.kind === "oidc";

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      setResult(
        await call(
          api.POST("/realms/{key}/actions/test", {
            params: { path: { key: realm.name } },
            ...(!oidc && { body: { username, password } }),
          }),
        ),
      );
    } catch (err) {
      setResult(null);
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent className="sm:max-w-2xl" onCloseAutoFocus={(e) => focusBack(e, returnFocus)}>
        <form onSubmit={(e) => void submit(e)} className="flex min-w-0 flex-col gap-4">
          <DialogHeader>
            <DialogTitle>{t("realms.testTitle", { name: realm.name })}</DialogTitle>
            <DialogDescription>
              {t(oidc ? "realms.oidcTestDescription" : "realms.testDescription")}
            </DialogDescription>
          </DialogHeader>
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          {!oidc && (
            <fieldset disabled={busy} className="grid gap-4 sm:grid-cols-2">
              <Field id="realm-test-username" label={t("login.username")}>
                <Input
                  id="realm-test-username"
                  autoComplete="off"
                  required
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                />
              </Field>
              <Field id="realm-test-password" label={t("login.password")}>
                <Input
                  id="realm-test-password"
                  type="password"
                  autoComplete="off"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </Field>
            </fieldset>
          )}
          {result && <TestResult result={result} oidc={oidc} />}
          <DialogFooter>
            <Button type="button" variant="outline" disabled={busy} onClick={onClose}>
              {t("common.done")}
            </Button>
            <Button type="submit" disabled={busy || (!oidc && !username)}>
              {t("realms.test")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** 단계 표. LDAP이면 찾은 DN·그룹·정해질 역할도 보인다(OIDC는 사용자가 없다). */
function TestResult({ result, oidc }: { result: RealmTestResult; oidc: boolean }) {
  const { t } = useTranslation();
  return (
    <div className="flex min-w-0 flex-col gap-3">
      <Table aria-label={t("realms.steps")}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("realms.step")}</TableHead>
            <TableHead>{t("realms.result")}</TableHead>
            <TableHead>{t("realms.message")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {result.steps.map((step) => (
            <TableRow key={step.name}>
              <TableCell>
                {t(`realms.stepNames.${step.name}`, { defaultValue: step.name })}
              </TableCell>
              <TableCell className={step.ok ? "" : "text-destructive"}>
                {t(step.ok ? "realms.ok" : "realms.failed")}
              </TableCell>
              <TableCell className="whitespace-normal break-words">{step.message}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {!oidc && (
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">{t("realms.dn")}</dt>
          <dd className={result.dn ? "font-mono text-xs break-all" : undefined}>
            {result.dn ?? "—"}
          </dd>
          <dt className="text-muted-foreground">{t("realms.groupsFound")}</dt>
          <dd>
            {result.groups.length === 0 ? (
              "—"
            ) : (
              <ul className="font-mono text-xs break-all">
                {result.groups.map((g) => (
                  <li key={g}>{g}</li>
                ))}
              </ul>
            )}
          </dd>
          <dt className="text-muted-foreground">{t("users.role")}</dt>
          <dd>{result.role ? t(`roles.${result.role}`) : t("realms.noRole")}</dd>
        </dl>
      )}
    </div>
  );
}
