import { useQueryClient } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { Pencil, Plus, Trash2 } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { SeverityBadge } from "@/alerts/AlertList";
import {
  useAlertRules,
  useNotificationTargets,
  type AlertCondition,
  type AlertRule,
} from "@/alerts/alerts";
import {
  aboutVms,
  checkRuleForm,
  CONDITION_KINDS,
  emptyRuleForm,
  isBackupStore,
  isEvent,
  isSdn,
  LINK_ROLES,
  NODE_METRICS,
  ruleFormOf,
  ruleSpecOf,
  takesNodes,
  VM_METRICS,
  type RuleForm,
} from "@/alerts/rules";
import { SEVERITIES } from "@/alerts/severity";
import { api, call } from "@/api/client";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { StatusChip } from "@/components/StatusChip";
import { TableMessage } from "@/components/TableMessage";
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

/** 조건을 한 줄로: "노드 CPU > 90", "풀 사용률 > 85 %", "Task 실패(vm-start)" */
export function conditionText(t: TFunction, c: AlertCondition): string {
  switch (c.kind) {
    case "node_metric":
    case "vm_metric":
      return t(`alerts.condition.${c.kind}`, {
        metric: t(`alerts.metric.${c.metric}`),
        op: c.op === "gt" ? ">" : "<",
        value: c.value,
      });
    case "pool_usage":
      return t("alerts.condition.pool_usage", { percent: c.percent });
    case "task_failed":
      return c.ops?.length
        ? t("alerts.condition.task_failed_ops", { ops: c.ops.join(", ") })
        : t("alerts.condition.task_failed");
    case "node_link_down": {
      const text = t(
        c.role === "any"
          ? "alerts.condition.node_link_down_any"
          : "alerts.condition.node_link_down",
      );
      return c.ports ? `${text} (${c.ports})` : text;
    }
    default:
      return t(`alerts.condition.${c.kind}`);
  }
}

/** 경보 규칙 표. 누구나 읽고 admin만 바꾼다. 규칙은 watch로 받지 않으므로 바꾼 뒤 다시 읽는다. */
export function AlertRulesTab() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const admin = perms.can("write", "alert-rule");
  const queryClient = useQueryClient();
  const rules = useAlertRules();
  const [dialog, setDialog] = useState<{ rule: AlertRule | null } | null>(null);
  const [deleting, setDeleting] = useState<AlertRule | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["alert-rules"] });
  const list = rules.data ?? [];
  const cols = admin ? 6 : 5;

  return (
    <div className="flex flex-col gap-2">
      {admin && (
        <div className="flex flex-wrap items-center justify-end gap-2">
          <Link to="/settings/notifications" className="text-sm underline-offset-4 hover:underline">
            {t("nav.notificationTargets")}
          </Link>
          <Button
            disabled={!rules.isSuccess}
            onClick={(e) => {
              opener.current = e.currentTarget;
              setDialog({ rule: null });
            }}
          >
            <Plus />
            {t("alerts.rules.create")}
          </Button>
        </div>
      )}
      <Table aria-label={t("alerts.tab.rules")}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("alerts.rules.condition")}</TableHead>
            <TableHead>{t("alerts.rules.severity")}</TableHead>
            <TableHead>{t("alerts.rules.targets")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            {admin && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={cols} query={rules} empty={t("alerts.rules.empty")} />
          )}
          {list.map((rule) => (
            <TableRow key={rule.id}>
              <TableCell className="font-medium">{rule.name}</TableCell>
              <TableCell>
                {conditionText(t, rule.spec.condition)}
                {rule.spec.for_secs > 0 && (
                  <span className="block text-xs text-muted-foreground">
                    {t("alerts.rules.forText", { secs: rule.spec.for_secs })}
                  </span>
                )}
              </TableCell>
              <TableCell>
                <SeverityBadge severity={rule.spec.severity} />
              </TableCell>
              <TableCell>{rule.spec.targets.join(", ")}</TableCell>
              <TableCell>
                <StatusChip tone={rule.spec.enabled ? "success" : "neutral"}>
                  {t(rule.spec.enabled ? "alerts.rules.on" : "alerts.rules.off")}
                </StatusChip>
              </TableCell>
              {admin && (
                <TableCell className="text-right whitespace-nowrap">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("alerts.rules.editNamed", { name: rule.name })}
                    onClick={(e) => {
                      opener.current = e.currentTarget;
                      setDialog({ rule });
                    }}
                  >
                    <Pencil />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("common.deleteNamed", { name: rule.name })}
                    onClick={(e) => {
                      opener.current = e.currentTarget;
                      setDeleting(rule);
                      setDeleteOpen(true);
                    }}
                  >
                    <Trash2 />
                  </Button>
                </TableCell>
              )}
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {dialog && (
        <RuleDialog
          rule={dialog.rule}
          taken={list.map((r) => r.name)}
          returnFocus={opener}
          onClose={() => setDialog(null)}
          onDone={changed}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("alerts.rules.deleteTitle", { name: deleting?.name })}
        description={t("alerts.rules.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const rule = deleting!;
          await call(
            api.DELETE("/alert-rules/{key}", {
              params: { path: { key: rule.id }, header: { "If-Match": rule.revision } },
            }),
          );
          toast.success(t("alerts.rules.deleted", { name: rule.name }));
          await changed();
        }}
      />
    </div>
  );
}

/** 규칙 만들기(`rule`이 null)와 고치기. 열 때만 그린다. */
function RuleDialog({
  rule,
  taken,
  returnFocus,
  onClose,
  onDone,
}: {
  rule: AlertRule | null;
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onDone(): Promise<void>;
}) {
  const { t } = useTranslation();
  const targets = useNotificationTargets();
  const [form, setForm] = useState<RuleForm>(() => (rule ? ruleFormOf(rule) : emptyRuleForm()));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<RuleForm>) => setForm((f) => ({ ...f, ...part }));
  const metric = form.kind === "node_metric" || form.kind === "vm_metric";
  const event = isEvent(form.kind);
  // 저장된 규칙이 가리키지만 지금 없는 대상도 고를 수 있게 남긴다.
  const names = [
    ...new Set([...(targets.data ?? []).map((target) => target.name), ...form.targets]),
  ].sort();

  async function submit() {
    const found = checkRuleForm(form, rule ? null : taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const spec = ruleSpecOf(form);
    if (rule) {
      await call(
        api.PUT("/alert-rules/{key}", {
          params: { path: { key: rule.id }, header: { "If-Match": rule.revision } },
          body: { labels: rule.labels, spec },
        }),
      );
      toast.success(t("alerts.rules.saved", { name: rule.name }));
    } else {
      await call(api.POST("/alert-rules", { body: { name: form.name, spec } }));
      toast.success(t("alerts.rules.created", { name: form.name }));
    }
    await onDone();
  }

  const input = (
    id: string,
    key: "value" | "percent" | "ops" | "ports" | "forSecs" | "repeatSecs" | "nodes" | "vms",
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

  return (
    <FormDialog
      title={
        rule ? t("alerts.rules.editTitle", { name: rule.name }) : t("alerts.rules.createTitle")
      }
      submitLabel={rule ? t("common.save") : t("common.create")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <div className="-mx-1 flex max-h-[60svh] flex-col gap-4 overflow-y-auto px-1">
        <Field id="rule-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="rule-name"
              autoComplete="off"
              readOnly={rule !== null}
              value={form.name}
              onChange={(e) => change({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        <Field id="rule-kind" label={t("alerts.rules.condition")}>
          <NativeSelect
            id="rule-kind"
            className="w-full"
            value={form.kind}
            onChange={(e) => {
              change({ kind: e.target.value as RuleForm["kind"] });
              setErrors({});
            }}
          >
            {CONDITION_KINDS.filter((kind) => !isSdn(kind) && !isBackupStore(kind)).map((kind) => (
              <NativeSelectOption key={kind} value={kind}>
                {t(`alerts.kind.${kind}`)}
              </NativeSelectOption>
            ))}
            {/* SDN 묶음(계획 67 §11) */}
            <optgroup label={t("alerts.sdnGroup")}>
              {CONDITION_KINDS.filter(isSdn).map((kind) => (
                <NativeSelectOption key={kind} value={kind}>
                  {t(`alerts.kind.${kind}`)}
                </NativeSelectOption>
              ))}
            </optgroup>
            {/* 백업 저장소 묶음(계획 68) */}
            <optgroup label={t("alerts.backupGroup")}>
              {CONDITION_KINDS.filter(isBackupStore).map((kind) => (
                <NativeSelectOption key={kind} value={kind}>
                  {t(`alerts.kind.${kind}`)}
                </NativeSelectOption>
              ))}
            </optgroup>
          </NativeSelect>
        </Field>
        {metric && (
          <div className="grid grid-cols-[1fr_auto_1fr] items-end gap-2">
            <Field id="rule-metric" label={t("alerts.rules.metric")}>
              <NativeSelect
                id="rule-metric"
                className="w-full"
                value={form.kind === "node_metric" ? form.nodeMetric : form.vmMetric}
                onChange={(e) =>
                  change(
                    form.kind === "node_metric"
                      ? { nodeMetric: e.target.value as RuleForm["nodeMetric"] }
                      : { vmMetric: e.target.value as RuleForm["vmMetric"] },
                  )
                }
              >
                {(form.kind === "node_metric" ? NODE_METRICS : VM_METRICS).map((m) => (
                  <NativeSelectOption key={m} value={m}>
                    {t(`alerts.metric.${m}`)}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            </Field>
            <Field id="rule-op" label={t("alerts.rules.op")}>
              <NativeSelect
                id="rule-op"
                value={form.op}
                onChange={(e) => change({ op: e.target.value as RuleForm["op"] })}
              >
                <NativeSelectOption value="gt">{t("alerts.rules.gt")}</NativeSelectOption>
                <NativeSelectOption value="lt">{t("alerts.rules.lt")}</NativeSelectOption>
              </NativeSelect>
            </Field>
            {input("rule-value", "value", t("alerts.rules.value"))}
          </div>
        )}
        {metric && (
          <p className="-mt-2 text-xs text-muted-foreground">{t("alerts.rules.unitHint")}</p>
        )}
        {form.kind === "pool_usage" && input("rule-percent", "percent", t("alerts.rules.percent"))}
        {form.kind === "task_failed" &&
          input("rule-ops", "ops", t("alerts.rules.ops"), t("alerts.rules.opsHint"))}
        {form.kind === "node_link_down" && (
          <>
            <Field id="rule-role" label={t("alerts.rules.role")}>
              <NativeSelect
                id="rule-role"
                className="w-full"
                value={form.role}
                onChange={(e) => change({ role: e.target.value as RuleForm["role"] })}
              >
                {LINK_ROLES.map((r) => (
                  <NativeSelectOption key={r} value={r}>
                    {t(`alerts.rules.roles.${r}`)}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            </Field>
            {input("rule-ports", "ports", t("alerts.rules.ports"), t("alerts.rules.portsHint"))}
          </>
        )}
        {!event &&
          input("rule-for", "forSecs", t("alerts.rules.forSecs"), t("alerts.rules.forSecsHint"))}
        <Field id="rule-severity" label={t("alerts.rules.severity")}>
          <NativeSelect
            id="rule-severity"
            className="w-full"
            value={form.severity}
            onChange={(e) => change({ severity: e.target.value as RuleForm["severity"] })}
          >
            {SEVERITIES.map((s) => (
              <NativeSelectOption key={s} value={s}>
                {t(`alerts.severity.${s}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
        <fieldset
          className="flex flex-col gap-2"
          aria-invalid={errors.targets ? true : undefined}
          aria-describedby={errors.targets ? "rule-targets-error" : undefined}
        >
          <legend className="mb-1 text-sm font-medium">{t("alerts.rules.targets")}</legend>
          {names.length === 0 && (
            <p className="text-sm text-muted-foreground">
              {targets.isPending ? t("common.loading") : t("alerts.rules.noTargets")}
            </p>
          )}
          {names.map((name) => (
            <div key={name} className="flex items-center gap-2">
              <Checkbox
                id={`rule-target-${name}`}
                checked={form.targets.includes(name)}
                onCheckedChange={(on) =>
                  change({
                    targets:
                      on === true
                        ? [...form.targets, name]
                        : form.targets.filter((n) => n !== name),
                  })
                }
              />
              <Label htmlFor={`rule-target-${name}`}>{name}</Label>
            </div>
          ))}
          {errors.targets && (
            <p id="rule-targets-error" className="text-xs text-destructive">
              {t(errors.targets)}
            </p>
          )}
        </fieldset>
        {!event &&
          input(
            "rule-repeat",
            "repeatSecs",
            t("alerts.rules.repeatSecs"),
            t("alerts.rules.repeatSecsHint"),
          )}
        {!event && (
          <div className="flex items-center gap-2">
            <Checkbox
              id="rule-send-resolved"
              checked={form.sendResolved}
              onCheckedChange={(on) => change({ sendResolved: on === true })}
            />
            <Label htmlFor="rule-send-resolved">{t("alerts.rules.sendResolved")}</Label>
          </div>
        )}
        {takesNodes(form.kind) &&
          input("rule-nodes", "nodes", t("alerts.rules.nodes"), t("alerts.rules.namesHint"))}
        {aboutVms(form.kind) &&
          input("rule-vms", "vms", t("alerts.rules.vms"), t("alerts.rules.namesHint"))}
        <div className="flex items-center gap-2">
          <Checkbox
            id="rule-enabled"
            checked={form.enabled}
            onCheckedChange={(on) => change({ enabled: on === true })}
          />
          <Label htmlFor="rule-enabled">{t("alerts.rules.enabled")}</Label>
        </div>
      </div>
    </FormDialog>
  );
}
