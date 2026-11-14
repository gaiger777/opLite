import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, Trash2, X } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { HaCheck, HaCheckAction, HaCheckRole } from "@/api/types";
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
  allowedActions,
  type CheckForm,
  checkErrors,
  DEFAULT_GRACE_SECS,
  defaultAction,
  emptyCheckForm,
  emptyTarget,
  fromSpec,
  interfaceNames,
  ROLES,
  sameSpec,
  sharedPools,
  type TargetForm,
  type TargetKind,
  targetKinds,
  targetText,
  toSpec,
} from "@/ha-checks/checks";

/** 검사 목록의 캐시 키. HA 검사는 watch 종류가 아니어서(옛 API 노드의 404) 바꾼 뒤 다시 읽는다. */
const CHECKS_KEY = ["ha-checks"] as const;

/** 인터페이스 입력이 고르는 후보 목록(`datalist`)의 id */
const IFACES = "ha-check-ifaces";

/** HA 상태 검사: 목록과 만들기·바꾸기·지우기. 누구나 읽고 admin만 바꾼다. */
export function HaChecksPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const checks = useQuery({
    queryKey: CHECKS_KEY,
    queryFn: () => call(api.GET("/ha-checks")),
    select: (list) => list.items,
  });
  const nodes = useList("node");
  const pools = useList("storage-pool");
  // 만들기는 `null`, 바꾸기는 그 검사
  const [editing, setEditing] = useState<HaCheck | null | undefined>(undefined);
  const [deleting, setDeleting] = useState<HaCheck | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const write = can(me.role, "write", "ha-check");
  const list = [...(checks.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));
  const reload = () => queryClient.invalidateQueries({ queryKey: CHECKS_KEY });

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="ha-checks-title" className="text-2xl font-semibold">
          {t("nav.haChecks")}
        </h1>
        {write && (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            {/* 노드·풀 목록을 읽어야 대상을 고른다. */}
            {!nodes.isSuccess && <ListPending error={nodes.error} />}
            {nodes.isSuccess && !pools.isSuccess && <ListPending error={pools.error} />}
            <Button
              disabled={!nodes.isSuccess || !pools.isSuccess || !checks.isSuccess}
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
      <p className="text-sm text-muted-foreground">{t("haChecks.pageHint")}</p>
      <Table aria-labelledby="ha-checks-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("haChecks.role")}</TableHead>
            <TableHead>{t("haChecks.target")}</TableHead>
            <TableHead>{t("haChecks.onFail")}</TableHead>
            <TableHead>{t("haChecks.grace")}</TableHead>
            <TableHead>{t("haChecks.overrides")}</TableHead>
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
            <TableMessage colSpan={write ? 8 : 7} query={checks} empty={t("haChecks.empty")} />
          )}
          {list.map((check) => {
            const s = check.spec;
            const overrides = Object.entries(s.nodes ?? {}).sort(([a], [b]) => a.localeCompare(b));
            return (
              <TableRow key={check.id}>
                <TableCell className="font-medium">{check.name}</TableCell>
                <TableCell>{t(`haChecks.roles.${s.role}`)}</TableCell>
                <TableCell className="font-mono text-xs">{targetText(s.target)}</TableCell>
                <TableCell>
                  {s.on_fail ? (
                    t(`haChecks.actions.${s.on_fail}`)
                  ) : (
                    <>
                      {t(`haChecks.actions.${defaultAction(s.role)}`)}
                      <span className="ml-1 text-xs text-muted-foreground">
                        {t("haChecks.roleDefault")}
                      </span>
                    </>
                  )}
                </TableCell>
                <TableCell>
                  {t("haChecks.seconds", { n: s.grace_secs ?? DEFAULT_GRACE_SECS })}
                </TableCell>
                <TableCell className="font-mono text-xs">
                  {overrides.map(([node, target]) => `${node}=${targetText(target)}`).join(", ") ||
                    "—"}
                </TableCell>
                <TableCell>
                  {/* 서버는 켜짐(기본)을 쓰지 않는다. */}
                  <Badge variant={(s.enabled ?? true) ? "default" : "outline"}>
                    {t((s.enabled ?? true) ? "haChecks.enabled" : "haChecks.disabled")}
                  </Badge>
                </TableCell>
                {write && (
                  <TableCell className="text-right whitespace-nowrap">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("haChecks.editNamed", { name: check.name })}
                      disabled={!nodes.isSuccess || !pools.isSuccess}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setEditing(check);
                      }}
                    >
                      <Pencil />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("haChecks.deleteNamed", { name: check.name })}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setDeleting(check);
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
      <p className="text-sm text-muted-foreground">{t("haChecks.autoNote")}</p>
      {editing !== undefined && (
        <CheckDialog
          check={editing}
          taken={(checks.data ?? []).map((c) => c.name)}
          returnFocus={opener}
          onClose={() => setEditing(undefined)}
          onSaved={reload}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("haChecks.deleteTitle", { name: deleting?.name })}
        description={t("haChecks.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const check = deleting!;
          await call(api.DELETE("/ha-checks/{key}", { params: { path: { key: check.id } } }));
          await reload();
          toast.success(t("haChecks.deleted", { name: check.name }));
        }}
      />
    </div>
  );
}

/** 대상 종류에 맞는 입력. `id`는 입력 id의 앞부분, `label`은 줄의 이름(노드별 덮기면 노드 이름)이다. */
function TargetFields({
  id,
  kind,
  value,
  invalid,
  pools,
  label,
  onChange,
}: {
  id: string;
  kind: TargetKind;
  value: TargetForm;
  invalid: boolean;
  pools: string[];
  label?: string;
  onChange(value: TargetForm): void;
}) {
  const { t } = useTranslation();
  const set = (part: Partial<TargetForm>) => onChange({ ...value, ...part });
  // 덮기 줄의 입력은 노드 이름을 넣어 이름 짓는다.
  const name = (key: string) =>
    label ? t("haChecks.ofNode", { field: t(key), node: label }) : t(key);
  const aria = invalid ? { "aria-invalid": true as const } : {};
  if (kind === "link") {
    return (
      <div className="flex flex-col gap-1.5">
        {!label && <Label htmlFor={`${id}-iface`}>{t("haChecks.iface")}</Label>}
        <Input
          id={`${id}-iface`}
          list={IFACES}
          placeholder="bond0"
          aria-label={label ? name("haChecks.iface") : undefined}
          value={value.iface}
          onChange={(e) => set({ iface: e.target.value })}
          {...aria}
        />
      </div>
    );
  }
  if (kind === "pool") {
    const names = value.pool && !pools.includes(value.pool) ? [...pools, value.pool] : pools;
    return (
      <div className="flex flex-col gap-1.5">
        {!label && <Label htmlFor={`${id}-pool`}>{t("haChecks.pool")}</Label>}
        <NativeSelect
          id={`${id}-pool`}
          className="w-full"
          aria-label={label ? name("haChecks.pool") : undefined}
          value={value.pool}
          onChange={(e) => set({ pool: e.target.value })}
          {...aria}
        >
          <NativeSelectOption value="">{t("haChecks.choosePool")}</NativeSelectOption>
          {names.map((p) => (
            <NativeSelectOption key={p} value={p}>
              {p}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      </div>
    );
  }
  return (
    <div className="flex flex-wrap items-end gap-2">
      <div className="flex min-w-48 flex-1 flex-col gap-1.5">
        {!label && <Label htmlFor={`${id}-addresses`}>{t("haChecks.addresses")}</Label>}
        <Input
          id={`${id}-addresses`}
          placeholder="10.0.0.1, 10.0.0.2"
          aria-label={label ? name("haChecks.addresses") : undefined}
          value={value.addresses}
          onChange={(e) => set({ addresses: e.target.value })}
          {...aria}
        />
      </div>
      <div className="flex w-28 flex-col gap-1.5">
        {!label && <Label htmlFor={`${id}-via`}>{t("haChecks.via")}</Label>}
        <Input
          id={`${id}-via`}
          list={IFACES}
          aria-label={label ? name("haChecks.via") : undefined}
          value={value.via}
          onChange={(e) => set({ via: e.target.value })}
          {...aria}
        />
      </div>
      <div className="flex w-20 flex-col gap-1.5">
        {!label && <Label htmlFor={`${id}-need`}>{t("haChecks.need")}</Label>}
        <Input
          id={`${id}-need`}
          inputMode="numeric"
          placeholder="1"
          aria-label={label ? name("haChecks.need") : undefined}
          value={value.need}
          onChange={(e) => set({ need: e.target.value })}
          {...aria}
        />
      </div>
    </div>
  );
}

/** 검사 만들기(`check`가 null)와 바꾸기. 열 때만 그린다. */
function CheckDialog({
  check,
  taken,
  returnFocus,
  onClose,
  onSaved,
}: {
  check: HaCheck | null;
  /** 있는 검사 이름 */
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onSaved(): Promise<void>;
}) {
  const { t } = useTranslation();
  const nodes = useList("node").data ?? [];
  const pools = sharedPools(useList("storage-pool").data ?? []);
  // 위트니스는 검사하지 않는다. 지운 노드가 덮기에 남아 있으면 그 이름도 보인다(서버가 저장할 때 거절한다).
  const nodeNames = nodes.filter((n) => !n.spec.witness).map((n) => n.name);
  const ifaces = interfaceNames(nodes);
  const [form, setForm] = useState<CheckForm>(() => (check ? fromSpec(check) : emptyCheckForm()));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<CheckForm>) => setForm((f) => ({ ...f, ...part }));
  const unchanged = check !== null && sameSpec(check.spec, toSpec(form));
  const used = new Set(form.overrides.map((o) => o.node));
  const free = nodeNames.filter((n) => !used.has(n));

  function setRole(role: HaCheckRole) {
    // 역할이 받지 않는 대상 종류·대응은 버린다.
    const kind = targetKinds(role).includes(form.kind) ? form.kind : "link";
    const onFail =
      form.onFail !== "" && allowedActions(role).includes(form.onFail) ? form.onFail : "";
    change({ role, kind, onFail });
  }

  function setKind(kind: TargetKind) {
    // 덮기의 종류는 본 대상과 같아야 하므로 함께 바꾼다.
    change({
      kind,
      target: emptyTarget(),
      overrides: form.overrides.map((o) => ({ ...o, target: emptyTarget() })),
    });
  }

  async function submit() {
    const found = checkErrors(form, check ? null : taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    if (check) {
      await call(
        api.PUT("/ha-checks/{key}", {
          params: { path: { key: check.id }, header: { "If-Match": check.revision } },
          body: { labels: check.labels, spec: toSpec(form) },
        }),
      );
      await onSaved();
      toast.success(t("haChecks.saved", { name: check.name }));
    } else {
      await call(api.POST("/ha-checks", { body: { name: form.name, spec: toSpec(form) } }));
      await onSaved();
      toast.success(t("haChecks.created", { name: form.name }));
    }
  }

  return (
    <FormDialog
      title={check ? t("haChecks.editTitle", { name: check.name }) : t("haChecks.createTitle")}
      description={t("haChecks.dialogDescription")}
      submitLabel={check ? t("common.save") : t("common.create")}
      submitDisabled={unchanged}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <datalist id={IFACES}>
        {ifaces.map((name) => (
          <option key={name} value={name} />
        ))}
      </datalist>
      {!check && (
        <Field id="check-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="check-name"
              value={form.name}
              onChange={(e) => change({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      <Field id="check-role" label={t("haChecks.role")} hint={t(`haChecks.roleHint.${form.role}`)}>
        {(a11y) => (
          <NativeSelect
            id="check-role"
            className="w-full"
            value={form.role}
            onChange={(e) => setRole(e.target.value as HaCheckRole)}
            {...a11y}
          >
            {ROLES.map((role) => (
              <NativeSelectOption key={role} value={role}>
                {t(`haChecks.roles.${role}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <Field
        id="check-kind"
        label={t("haChecks.targetKind")}
        hint={t(`haChecks.kindHint.${form.kind}`)}
      >
        {(a11y) => (
          <NativeSelect
            id="check-kind"
            className="w-full"
            value={form.kind}
            onChange={(e) => setKind(e.target.value as TargetKind)}
            {...a11y}
          >
            {targetKinds(form.role).map((kind) => (
              <NativeSelectOption key={kind} value={kind}>
                {t(`haChecks.kinds.${kind}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <div className="flex flex-col gap-1.5">
        <TargetFields
          id="check-target"
          kind={form.kind}
          value={form.target}
          invalid={!!errors.target}
          pools={pools}
          onChange={(target) => change({ target })}
        />
        {errors.target && <p className="text-xs text-destructive">{t(errors.target)}</p>}
      </div>
      <Field id="check-on-fail" label={t("haChecks.onFail")} error={errors.onFail}>
        {(a11y) => (
          <NativeSelect
            id="check-on-fail"
            className="w-full"
            value={form.onFail}
            onChange={(e) => change({ onFail: e.target.value as HaCheckAction | "" })}
            {...a11y}
          >
            <NativeSelectOption value="">
              {t("haChecks.defaultAction", {
                action: t(`haChecks.actions.${defaultAction(form.role)}`),
              })}
            </NativeSelectOption>
            {allowedActions(form.role).map((action) => (
              <NativeSelectOption key={action} value={action}>
                {t(`haChecks.actions.${action}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <Field
        id="check-grace"
        label={t("haChecks.graceLabel")}
        error={errors.grace}
        hint={t("haChecks.graceHint")}
      >
        {(a11y) => (
          <Input
            id="check-grace"
            inputMode="numeric"
            placeholder={String(DEFAULT_GRACE_SECS)}
            value={form.grace}
            onChange={(e) => change({ grace: e.target.value })}
            {...a11y}
          />
        )}
      </Field>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1.5 text-sm font-medium">{t("haChecks.overrides")}</legend>
        {form.overrides.map((o, i) => (
          <div key={i} className="flex flex-col gap-1.5">
            <div className="flex items-start gap-2">
              <NativeSelect
                className="w-32"
                aria-label={t("haChecks.overrideNode", { n: i + 1 })}
                value={o.node}
                onChange={(e) =>
                  change({
                    overrides: form.overrides.map((r, j) =>
                      j === i ? { ...r, node: e.target.value } : r,
                    ),
                  })
                }
              >
                <NativeSelectOption value="">{t("haChecks.chooseNode")}</NativeSelectOption>
                {[o.node, ...free]
                  .filter((n) => n !== "")
                  .map((n) => (
                    <NativeSelectOption key={n} value={n}>
                      {n}
                    </NativeSelectOption>
                  ))}
              </NativeSelect>
              <div className="flex-1">
                <TargetFields
                  id={`check-override-${i}`}
                  kind={form.kind}
                  value={o.target}
                  invalid={!!errors[`override.${i}`]}
                  pools={pools}
                  label={o.node || String(i + 1)}
                  onChange={(target) =>
                    change({
                      overrides: form.overrides.map((r, j) => (j === i ? { ...r, target } : r)),
                    })
                  }
                />
              </div>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label={t("haChecks.removeOverride", { node: o.node || String(i + 1) })}
                onClick={() => change({ overrides: form.overrides.filter((_, j) => j !== i) })}
              >
                <X />
              </Button>
            </div>
            {errors[`override.${i}`] && (
              <p className="text-xs text-destructive">{t(errors[`override.${i}`]!)}</p>
            )}
          </div>
        ))}
        <div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={free.length === 0}
            onClick={() =>
              change({
                overrides: [...form.overrides, { node: free[0] ?? "", target: emptyTarget() }],
              })
            }
          >
            <Plus />
            {t("haChecks.addOverride")}
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">{t("haChecks.overridesHint")}</p>
      </fieldset>
      <div className="flex items-center gap-2">
        <Checkbox
          id="check-enabled"
          checked={form.enabled}
          onCheckedChange={(on) => change({ enabled: on === true })}
        />
        <Label htmlFor="check-enabled">{t("haChecks.enabledLabel")}</Label>
      </div>
    </FormDialog>
  );
}
