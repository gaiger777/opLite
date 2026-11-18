import { useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, Send, Trash2, X } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useNotificationTargets, type NotificationTargetView } from "@/alerts/alerts";
import {
  checkTargetForm,
  destinationText,
  emptyTargetForm,
  SMTP_TLS,
  targetFormOf,
  targetRequestOf,
  usesToken,
  WEBHOOK_FORMATS,
  type TargetForm,
} from "@/alerts/targets";
import { api, call } from "@/api/client";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
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

/**
 * 알림 대상(SMTP·webhook): 목록, 만들기·고치기·지우기, 시험 보내기. 비밀이 있어 admin만 다루고 watch로 받지 않는다.
 */
export function NotificationTargetsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const admin = can(me.role, "read", "notification-target");
  const queryClient = useQueryClient();
  const targets = useNotificationTargets(admin);
  const [dialog, setDialog] = useState<{ target: NotificationTargetView | null } | null>(null);
  const [deleting, setDeleting] = useState<NotificationTargetView | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [testing, setTesting] = useState<string | null>(null);
  const opener = useRef<HTMLElement | null>(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["notification-targets"] });

  if (!admin) {
    return (
      <div className="flex flex-col gap-4">
        <h1 className="text-2xl font-semibold">{t("nav.notificationTargets")}</h1>
        <p className="text-sm text-muted-foreground">{t("alerts.targets.adminOnly")}</p>
      </div>
    );
  }

  async function test(target: NotificationTargetView) {
    setTesting(target.id);
    try {
      const result = await call(
        api.POST("/notification-targets/{key}/test", { params: { path: { key: target.id } } }),
      );
      if (result.ok) {
        toast.success(t("alerts.targets.testSent", { name: target.name }));
      } else {
        toast.error(
          t("alerts.targets.testFailed", { name: target.name, error: result.error ?? "" }),
        );
      }
    } catch (err) {
      toast.error(
        t("alerts.targets.testFailed", { name: target.name, error: errorMessage(t, err) }),
      );
    } finally {
      setTesting(null);
    }
  }

  const list = targets.data ?? [];
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="targets-title" className="text-2xl font-semibold">
          {t("nav.notificationTargets")}
        </h1>
        <Button
          disabled={!targets.isSuccess}
          onClick={(e) => {
            opener.current = e.currentTarget;
            setDialog({ target: null });
          }}
        >
          <Plus />
          {t("common.create")}
        </Button>
      </div>
      <p className="text-sm text-muted-foreground">{t("alerts.targets.pageHint")}</p>
      <Table aria-labelledby="targets-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("alerts.targets.kind")}</TableHead>
            <TableHead>{t("alerts.targets.destination")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={5} query={targets} empty={t("alerts.targets.empty")} />
          )}
          {list.map((target) => (
            <TableRow key={target.id}>
              <TableCell className="font-medium">{target.name}</TableCell>
              <TableCell>
                {target.target.kind === "smtp"
                  ? t("alerts.targets.kinds.smtp")
                  : t(`alerts.targets.formats.${target.target.format}`)}
              </TableCell>
              <TableCell className="font-mono text-xs break-all">
                {destinationText(target)}
              </TableCell>
              <TableCell>
                <Badge variant={target.enabled ? "default" : "outline"}>
                  {t(target.enabled ? "alerts.targets.on" : "alerts.targets.off")}
                </Badge>
              </TableCell>
              <TableCell className="text-right whitespace-nowrap">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={testing === target.id}
                  aria-label={t("alerts.targets.testNamed", { name: target.name })}
                  onClick={() => void test(target)}
                >
                  <Send />
                  {t("alerts.targets.test")}
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  className="ml-2"
                  aria-label={t("alerts.targets.editNamed", { name: target.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDialog({ target });
                  }}
                >
                  <Pencil />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("common.deleteNamed", { name: target.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDeleting(target);
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
      {dialog && (
        <TargetDialog
          target={dialog.target}
          taken={list.map((x) => x.name)}
          returnFocus={opener}
          onClose={() => setDialog(null)}
          onDone={changed}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("alerts.targets.deleteTitle", { name: deleting?.name })}
        description={t("alerts.targets.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const target = deleting!;
          await call(
            api.DELETE("/notification-targets/{key}", {
              params: { path: { key: target.id }, header: { "If-Match": target.revision } },
            }),
          );
          toast.success(t("alerts.targets.deleted", { name: target.name }));
          await changed();
        }}
      />
    </div>
  );
}

/** 대상 만들기(`target`이 null)와 고치기. 열 때만 그린다. */
function TargetDialog({
  target,
  taken,
  returnFocus,
  onClose,
  onDone,
}: {
  target: NotificationTargetView | null;
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onDone(): Promise<void>;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState<TargetForm>(() =>
    target ? targetFormOf(target) : emptyTargetForm(),
  );
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<TargetForm>) => setForm((f) => ({ ...f, ...part }));
  const saved = target?.target;
  const hasPassword = saved?.kind === "smtp" && saved.has_password;
  const hasToken = saved?.kind === "webhook" && saved.has_token;

  async function submit() {
    const found = checkTargetForm(form, target, taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const body = targetRequestOf(form, target);
    if (target) {
      await call(
        api.PUT("/notification-targets/{key}", {
          params: { path: { key: target.id }, header: { "If-Match": target.revision } },
          body,
        }),
      );
      toast.success(t("alerts.targets.saved", { name: target.name }));
    } else {
      await call(api.POST("/notification-targets", { body }));
      toast.success(t("alerts.targets.created", { name: form.name }));
    }
    await onDone();
  }

  const text = (
    id: string,
    key: "host" | "port" | "user" | "from" | "to" | "url",
    label: string,
    hint?: string,
  ) => (
    <Field id={id} label={label} error={errors[key]} hint={hint}>
      {(a11y) => (
        <Input
          id={id}
          autoComplete="off"
          spellCheck={false}
          value={form[key]}
          onChange={(e) => change({ [key]: e.target.value })}
          {...a11y}
        />
      )}
    </Field>
  );

  const setHeader = (i: number, part: { name?: string; value?: string }) =>
    change({ headers: form.headers.map((h, j) => (j === i ? { ...h, ...part } : h)) });

  return (
    <FormDialog
      title={
        target
          ? t("alerts.targets.editTitle", { name: target.name })
          : t("alerts.targets.createTitle")
      }
      description={t("alerts.targets.formDescription")}
      submitLabel={target ? t("common.save") : t("common.create")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <div className="-mx-1 flex max-h-[60svh] flex-col gap-4 overflow-y-auto px-1">
        {/* 종류는 만들 때만 고른다. */}
        {!target && (
          <Field id="target-kind" label={t("alerts.targets.kind")}>
            <NativeSelect
              id="target-kind"
              className="w-full"
              value={form.kind}
              onChange={(e) => {
                change({ kind: e.target.value as TargetForm["kind"] });
                setErrors({});
              }}
            >
              <NativeSelectOption value="webhook">
                {t("alerts.targets.kinds.webhook")}
              </NativeSelectOption>
              <NativeSelectOption value="smtp">{t("alerts.targets.kinds.smtp")}</NativeSelectOption>
            </NativeSelect>
          </Field>
        )}
        <Field id="target-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="target-name"
              autoComplete="off"
              readOnly={target !== null}
              value={form.name}
              onChange={(e) => change({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        {form.kind === "smtp" ? (
          <>
            {text("target-host", "host", t("alerts.targets.host"))}
            <div className="grid grid-cols-2 gap-2">
              {text("target-port", "port", t("alerts.targets.port"))}
              <Field id="target-tls" label={t("alerts.targets.tls")}>
                <NativeSelect
                  id="target-tls"
                  className="w-full"
                  value={form.tls}
                  onChange={(e) => change({ tls: e.target.value as TargetForm["tls"] })}
                >
                  {SMTP_TLS.map((tls) => (
                    <NativeSelectOption key={tls} value={tls}>
                      {t(`alerts.targets.tlsModes.${tls}`)}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </Field>
            </div>
            {text("target-user", "user", t("alerts.targets.user"), t("alerts.targets.userHint"))}
            <Field
              id="target-password"
              label={t("alerts.targets.password")}
              error={errors.password}
              hint={hasPassword ? t("alerts.targets.secretSaved") : undefined}
            >
              {(a11y) => (
                <Input
                  id="target-password"
                  type="password"
                  autoComplete="new-password"
                  disabled={form.clearPassword}
                  value={form.password}
                  onChange={(e) => change({ password: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            {hasPassword && (
              <div className="flex items-center gap-2">
                <Checkbox
                  id="target-clear-password"
                  checked={form.clearPassword}
                  onCheckedChange={(on) => change({ clearPassword: on === true, password: "" })}
                />
                <Label htmlFor="target-clear-password">{t("alerts.targets.clearPassword")}</Label>
              </div>
            )}
            {text("target-from", "from", t("alerts.targets.from"))}
            {text("target-to", "to", t("alerts.targets.to"), t("alerts.targets.toHint"))}
          </>
        ) : (
          <>
            {text(
              "target-url",
              "url",
              t("alerts.targets.url"),
              target ? t("alerts.targets.urlSaved") : t("alerts.targets.urlHint"),
            )}
            <Field id="target-format" label={t("alerts.targets.format")}>
              <NativeSelect
                id="target-format"
                className="w-full"
                value={form.format}
                onChange={(e) => change({ format: e.target.value as TargetForm["format"] })}
              >
                {WEBHOOK_FORMATS.map((f) => (
                  <NativeSelectOption key={f} value={f}>
                    {t(`alerts.targets.formats.${f}`)}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            </Field>
            {usesToken(form.format) && (
              <>
                <Field
                  id="target-token"
                  label={t("alerts.targets.token")}
                  error={errors.token}
                  hint={hasToken ? t("alerts.targets.secretSaved") : undefined}
                >
                  {(a11y) => (
                    <Input
                      id="target-token"
                      type="password"
                      autoComplete="new-password"
                      disabled={form.clearToken}
                      value={form.token}
                      onChange={(e) => change({ token: e.target.value })}
                      {...a11y}
                    />
                  )}
                </Field>
                {hasToken && (
                  <div className="flex items-center gap-2">
                    <Checkbox
                      id="target-clear-token"
                      checked={form.clearToken}
                      onCheckedChange={(on) => change({ clearToken: on === true, token: "" })}
                    />
                    <Label htmlFor="target-clear-token">{t("alerts.targets.clearToken")}</Label>
                  </div>
                )}
              </>
            )}
            {errors.token && !usesToken(form.format) && (
              <p className="text-xs text-destructive">{t(errors.token)}</p>
            )}
            <fieldset className="flex flex-col gap-2">
              <legend className="mb-1 text-sm font-medium">{t("alerts.targets.headers")}</legend>
              {form.headers.map((header, i) => (
                <div key={i} className="grid grid-cols-[1fr_1fr_auto] items-center gap-2">
                  <Input
                    aria-label={t("alerts.targets.headerName", { n: i + 1 })}
                    autoComplete="off"
                    spellCheck={false}
                    readOnly={header.saved}
                    value={header.name}
                    onChange={(e) => setHeader(i, { name: e.target.value })}
                  />
                  <Input
                    aria-label={t("alerts.targets.headerValue", { n: i + 1 })}
                    type="password"
                    autoComplete="new-password"
                    placeholder={header.saved ? t("alerts.targets.headerKept") : ""}
                    value={header.value}
                    onChange={(e) => setHeader(i, { value: e.target.value })}
                  />
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("alerts.targets.removeHeader", { n: i + 1 })}
                    onClick={() => change({ headers: form.headers.filter((_, j) => j !== i) })}
                  >
                    <X />
                  </Button>
                </div>
              ))}
              {errors.headers && <p className="text-xs text-destructive">{t(errors.headers)}</p>}
              <div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    change({ headers: [...form.headers, { name: "", value: "", saved: false }] })
                  }
                >
                  <Plus />
                  {t("alerts.targets.addHeader")}
                </Button>
              </div>
            </fieldset>
            <Field
              id="target-ca"
              label={t("alerts.targets.caPem")}
              hint={t("alerts.targets.caPemHint")}
            >
              {(a11y) => (
                <textarea
                  id="target-ca"
                  rows={3}
                  spellCheck={false}
                  className="rounded-md border bg-transparent px-3 py-2 font-mono text-xs"
                  value={form.caPem}
                  onChange={(e) => change({ caPem: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
          </>
        )}
        <div className="flex items-center gap-2">
          <Checkbox
            id="target-enabled"
            checked={form.enabled}
            onCheckedChange={(on) => change({ enabled: on === true })}
          />
          <Label htmlFor="target-enabled">{t("alerts.targets.enabled")}</Label>
        </div>
      </div>
    </FormDialog>
  );
}
