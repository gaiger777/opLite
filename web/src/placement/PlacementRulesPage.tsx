import { Pencil, Plus, Trash2 } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { PlacementRule, RuleKind } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
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
import {
  checkRuleForm,
  DEFAULT_WEIGHT,
  emptyRuleForm,
  isHard,
  isVmRule,
  RULE_KINDS,
  type RuleForm,
  ruleFormOf,
  ruleSpecOf,
  sameRuleSpec,
} from "@/placement/rules";

/** 배치 규칙: 목록과 만들기·바꾸기·지우기. 누구나 읽고 admin만 바꾼다. */
export function PlacementRulesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const rules = useList("placement-rule");
  const nodes = useList("node");
  // 만들기는 `null`, 바꾸기는 그 규칙
  const [editing, setEditing] = useState<PlacementRule | null | undefined>(undefined);
  const [deleting, setDeleting] = useState<PlacementRule | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const write = can(me.role, "write", "placement-rule");
  const list = [...(rules.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="placement-rules-title" className="text-2xl font-semibold">
          {t("nav.placementRules")}
        </h1>
        {write && (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            {/* 노드 목록을 읽어야 규칙의 노드를 고른다. */}
            {!nodes.isSuccess && <ListPending error={nodes.error} />}
            <Button
              disabled={!nodes.isSuccess || !rules.isSuccess}
              onClick={(e) => {
                opener.current = e.currentTarget;
                setEditing(null);
              }}
            >
              <Plus />
              {t("common.create")}
            </Button>
          </div>
        )}
      </div>
      <p className="text-sm text-muted-foreground">{t("placementRules.pageHint")}</p>
      <Table aria-labelledby="placement-rules-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("placementRules.kind")}</TableHead>
            <TableHead>{t("placementRules.vms")}</TableHead>
            <TableHead>{t("placementRules.nodes")}</TableHead>
            <TableHead>{t("placementRules.weight")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={write ? 7 : 6} query={rules} empty={t("placementRules.empty")} />
          )}
          {list.map((rule) => {
            const s = rule.spec;
            return (
              <TableRow key={rule.id}>
                <TableCell className="font-medium">{rule.name}</TableCell>
                <TableCell>{t(`placementRules.kinds.${s.kind}`)}</TableCell>
                <TableCell>
                  {s.vm_tag != null ? (
                    <span className="font-mono text-xs">{s.vm_tag}</span>
                  ) : (
                    (s.vms ?? []).join(", ")
                  )}
                </TableCell>
                <TableCell>
                  {s.node_tag != null ? (
                    <span className="font-mono text-xs">{s.node_tag}</span>
                  ) : (
                    (s.nodes ?? []).join(", ") || "—"
                  )}
                </TableCell>
                <TableCell>{isHard(s.kind) ? "—" : (s.weight ?? DEFAULT_WEIGHT)}</TableCell>
                <TableCell>
                  <div className="flex flex-wrap gap-1">
                    <Badge variant={s.enabled ? "secondary" : "outline"}>
                      {t(
                        s.enabled
                          ? "placementRules.state.enabled"
                          : "placementRules.state.disabled",
                      )}
                    </Badge>
                    {s.ha_may_violate && (
                      <Badge variant="outline">{t("placementRules.haMayViolateShort")}</Badge>
                    )}
                  </div>
                </TableCell>
                {write && (
                  <TableCell className="text-right whitespace-nowrap">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("placementRules.editNamed", { name: rule.name })}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setEditing(rule);
                      }}
                    >
                      <Pencil />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("placementRules.deleteNamed", { name: rule.name })}
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
            );
          })}
        </TableBody>
      </Table>
      {editing !== undefined && (
        <RuleDialog
          rule={editing}
          taken={(rules.data ?? []).map((r) => r.name)}
          returnFocus={opener}
          onClose={() => setEditing(undefined)}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("placementRules.deleteTitle", { name: deleting?.name })}
        description={t("placementRules.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const rule = deleting!;
          await call(api.DELETE("/placement-rules/{key}", { params: { path: { key: rule.id } } }));
          toast.success(t("placementRules.deleted", { name: rule.name }));
        }}
      />
    </div>
  );
}

/** 규칙 만들기(`rule`이 null)와 바꾸기. 열 때만 그린다. */
function RuleDialog({
  rule,
  taken,
  returnFocus,
  onClose,
}: {
  rule: PlacementRule | null;
  /** 있는 규칙 이름 */
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  // 위트니스는 VM을 돌리지 않는다. 지운 노드가 규칙에 남아 있으면 그 이름도 보인다(서버가 저장할 때 거절한다).
  const names = (useList("node").data ?? []).filter((n) => !n.spec.witness).map((n) => n.name);
  for (const n of rule?.spec.nodes ?? []) {
    if (!names.includes(n)) {
      names.push(n);
    }
  }
  names.sort();
  const [form, setForm] = useState<RuleForm>(() => (rule ? ruleFormOf(rule) : emptyRuleForm()));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<RuleForm>) => setForm((f) => ({ ...f, ...part }));
  const unchanged = rule !== null && sameRuleSpec(rule.spec, ruleSpecOf(form));
  const hard = isHard(form.kind);

  async function submit() {
    const found = checkRuleForm(form, rule ? null : taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    if (rule) {
      await call(
        api.PUT("/placement-rules/{key}", {
          params: { path: { key: rule.id }, header: { "If-Match": rule.revision } },
          body: { labels: rule.labels, spec: ruleSpecOf(form) },
        }),
      );
      toast.success(t("placementRules.saved", { name: rule.name }));
    } else {
      await call(
        api.POST("/placement-rules", { body: { name: form.name, spec: ruleSpecOf(form) } }),
      );
      toast.success(t("placementRules.created", { name: form.name }));
    }
  }

  return (
    <FormDialog
      title={
        rule ? t("placementRules.editTitle", { name: rule.name }) : t("placementRules.createTitle")
      }
      description={t("placementRules.dialogDescription")}
      submitLabel={rule ? t("common.save") : t("common.create")}
      submitDisabled={unchanged}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {!rule && (
        <Field id="rule-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="rule-name"
              value={form.name}
              onChange={(e) => change({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      <Field
        id="rule-kind"
        label={t("placementRules.kind")}
        hint={t(`placementRules.kindHint.${form.kind}`)}
      >
        {(a11y) => (
          <NativeSelect
            id="rule-kind"
            className="w-full"
            value={form.kind}
            onChange={(e) => change({ kind: e.target.value as RuleKind })}
            {...a11y}
          >
            {RULE_KINDS.map((kind) => (
              <NativeSelectOption key={kind} value={kind}>
                {t(`placementRules.kinds.${kind}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <Field id="rule-vm-by" label={t("placementRules.vmBy")}>
        <NativeSelect
          id="rule-vm-by"
          className="w-full"
          value={form.vmBy}
          onChange={(e) => change({ vmBy: e.target.value as RuleForm["vmBy"] })}
        >
          <NativeSelectOption value="names">{t("placementRules.byNames")}</NativeSelectOption>
          <NativeSelectOption value="tag">{t("placementRules.byLabel")}</NativeSelectOption>
        </NativeSelect>
      </Field>
      {form.vmBy === "names" ? (
        <Field
          id="rule-vms"
          label={t("placementRules.vmNames")}
          error={errors.vms}
          hint={t("placementRules.vmNamesHint")}
        >
          {(a11y) => (
            <Input
              id="rule-vms"
              placeholder="web1, web2"
              value={form.vms}
              onChange={(e) => change({ vms: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      ) : (
        <Field
          id="rule-vm-tag"
          label={t("placementRules.vmTag")}
          error={errors.vms}
          hint={t("placementRules.vmTagHint")}
        >
          {(a11y) => (
            <Input
              id="rule-vm-tag"
              placeholder="app=web"
              value={form.vmTag}
              onChange={(e) => change({ vmTag: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      {!isVmRule(form.kind) && (
        <>
          <Field id="rule-node-by" label={t("placementRules.nodeBy")}>
            <NativeSelect
              id="rule-node-by"
              className="w-full"
              value={form.nodeBy}
              onChange={(e) => change({ nodeBy: e.target.value as RuleForm["nodeBy"] })}
            >
              <NativeSelectOption value="names">{t("placementRules.byNames")}</NativeSelectOption>
              <NativeSelectOption value="tag">{t("placementRules.byLabel")}</NativeSelectOption>
            </NativeSelect>
          </Field>
          {form.nodeBy === "names" ? (
            <fieldset className="flex flex-col gap-1.5">
              <legend className="mb-1.5 text-sm font-medium">{t("placementRules.nodes")}</legend>
              <div className="flex flex-wrap gap-3">
                {names.map((node) => (
                  <div key={node} className="flex items-center gap-1.5">
                    <Checkbox
                      id={`rule-node-${node}`}
                      checked={form.nodes.includes(node)}
                      onCheckedChange={(on) =>
                        change({
                          nodes:
                            on === true
                              ? [...form.nodes, node]
                              : form.nodes.filter((n) => n !== node),
                        })
                      }
                    />
                    <Label htmlFor={`rule-node-${node}`}>{node}</Label>
                  </div>
                ))}
              </div>
              {errors.nodes && <p className="text-xs text-destructive">{t(errors.nodes)}</p>}
            </fieldset>
          ) : (
            <Field
              id="rule-node-tag"
              label={t("placementRules.nodeTag")}
              error={errors.nodes}
              hint={t("placementRules.nodeTagHint")}
            >
              {(a11y) => (
                <Input
                  id="rule-node-tag"
                  placeholder="zone=rack-a"
                  value={form.nodeTag}
                  onChange={(e) => change({ nodeTag: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
          )}
        </>
      )}
      {!hard && (
        <Field
          id="rule-weight"
          label={t("placementRules.weight")}
          error={errors.weight}
          hint={t("placementRules.weightHint")}
        >
          {(a11y) => (
            <Input
              id="rule-weight"
              inputMode="numeric"
              placeholder={String(DEFAULT_WEIGHT)}
              value={form.weight}
              onChange={(e) => change({ weight: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      {hard && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center gap-2">
            <Checkbox
              id="rule-ha-may-violate"
              checked={form.haMayViolate}
              onCheckedChange={(on) => change({ haMayViolate: on === true })}
            />
            <Label htmlFor="rule-ha-may-violate">{t("placementRules.haMayViolate")}</Label>
          </div>
          <p className="text-xs text-muted-foreground">{t("placementRules.haMayViolateHint")}</p>
        </div>
      )}
      <div className="flex items-center gap-2">
        <Checkbox
          id="rule-enabled"
          checked={form.enabled}
          onCheckedChange={(on) => change({ enabled: on === true })}
        />
        <Label htmlFor="rule-enabled">{t("placementRules.enabled")}</Label>
      </div>
    </FormDialog>
  );
}
