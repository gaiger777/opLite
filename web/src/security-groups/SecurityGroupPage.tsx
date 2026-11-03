import { ArrowDown, ArrowUp, Plus, X } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { SecurityGroup } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
  emptyRule,
  fromRule,
  hasPorts,
  MAX_DESCRIPTION,
  MAX_RULES,
  moveRule,
  type RuleRow,
  toRule,
  validateRules,
} from "@/security-groups/rules";

/** 보안 그룹 하나: 설명과 규칙 표. 쓰기 권한이 없으면 읽기만 한다. */
export function SecurityGroupPage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const groups = useList("security-group");
  if (!groups.data) {
    return <ListPending error={groups.error} />;
  }
  const group = groups.data.find((g) => g.name === name);
  if (!group) {
    return <h1 className="text-2xl font-semibold">{t("securityGroups.notFound", { name })}</h1>;
  }
  return <RulesEditor key={group.id} group={group} />;
}

function RulesEditor({ group }: { group: SecurityGroup }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const write = can(me.role, "write", "security-group");
  const [description, setDescription] = useState(group.spec.description ?? "");
  const [rows, setRows] = useState(() => (group.spec.rules ?? []).map(fromRule));
  // 읽은 revision. 그 사이 다른 사람이 바꿨으면 저장이 실패한다(If-Match).
  const [revision, setRevision] = useState(group.revision);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<{ message: string; conflict: boolean } | null>(null);
  const [busy, setBusy] = useState(false);

  // 행을 넣거나 빼거나 옮기면 행 번호로 단 오류가 다른 행을 가리키므로 지운다.
  const changeRows = (next: RuleRow[]) => {
    setRows(next);
    setErrors({});
  };
  const setRow = (i: number, patch: Partial<RuleRow>) =>
    setRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...patch } : r)));

  /** 다른 사람이 바꾼 그룹을 다시 읽는다. `group`은 watch로 늘 최신이다. */
  function reload() {
    setDescription(group.spec.description ?? "");
    setRows((group.spec.rules ?? []).map(fromRule));
    setRevision(group.revision);
    setErrors({});
    setFailure(null);
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const found = validateRules(rows);
    if (description.length > MAX_DESCRIPTION) {
      found.description = "securityGroups.errors.description";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    setBusy(true);
    setFailure(null);
    try {
      const saved = await call(
        api.PATCH("/security-groups/{key}", {
          params: { path: { key: group.id }, header: { "If-Match": revision } },
          body: { description, rules: rows.map(toRule) },
        }),
      );
      setRevision(saved.revision);
      toast.success(t("securityGroups.saved", { name: group.name }));
    } catch (err) {
      setFailure({
        message: errorMessage(t, err),
        conflict: err instanceof ApiError && err.code === "revision_mismatch",
      });
    } finally {
      setBusy(false);
    }
  }

  const n = (i: number) => ({ n: i + 1 });
  return (
    <form onSubmit={(e) => void submit(e)} className="flex flex-col gap-4" noValidate>
      <div className="flex flex-col gap-1">
        <Link
          to="/security-groups"
          className="text-sm text-muted-foreground underline-offset-4 hover:underline"
        >
          {t("nav.securityGroups")}
        </Link>
        <h1 className="text-2xl font-semibold">{group.name}</h1>
      </div>
      <p className="text-sm text-muted-foreground">{t("securityGroups.order")}</p>
      <p className="text-sm text-muted-foreground">{t("securityGroups.always")}</p>
      {/* 보내는 동안이나 읽기 권한뿐이면 고치지 못하게 한다. */}
      <fieldset disabled={busy || !write} className="flex min-w-0 flex-col gap-4">
        <div className="max-w-xl">
          <Field
            id="group-description"
            label={t("securityGroups.description")}
            error={errors.description}
          >
            {(a11y) => (
              <Input
                id="group-description"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </div>
        <Table aria-label={t("securityGroups.rules")}>
          <TableHeader>
            <TableRow>
              <TableHead>#</TableHead>
              <TableHead>{t("securityGroups.direction")}</TableHead>
              <TableHead>{t("securityGroups.action")}</TableHead>
              <TableHead>{t("securityGroups.protocol")}</TableHead>
              <TableHead>{t("securityGroups.ports")}</TableHead>
              <TableHead>{t("securityGroups.remote")}</TableHead>
              {write && (
                <TableHead>
                  <span className="sr-only">{t("vms.actions")}</span>
                </TableHead>
              )}
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.length === 0 && (
              <TableRow>
                <TableCell colSpan={write ? 7 : 6} className="text-center text-muted-foreground">
                  {t("securityGroups.noRules")}
                </TableCell>
              </TableRow>
            )}
            {rows.map((row, i) => {
              const portsError = errors[`rules.${i}.ports`];
              const remoteError = errors[`rules.${i}.remote`];
              return (
                <TableRow key={i}>
                  <TableCell>{i + 1}</TableCell>
                  <TableCell>
                    <NativeSelect
                      aria-label={t("securityGroups.directionN", n(i))}
                      value={row.direction}
                      onChange={(e) =>
                        setRow(i, { direction: e.target.value as RuleRow["direction"] })
                      }
                    >
                      <NativeSelectOption value="in">{t("securityGroups.in")}</NativeSelectOption>
                      <NativeSelectOption value="out">{t("securityGroups.out")}</NativeSelectOption>
                    </NativeSelect>
                  </TableCell>
                  <TableCell>
                    <NativeSelect
                      aria-label={t("securityGroups.actionN", n(i))}
                      value={row.action}
                      onChange={(e) => setRow(i, { action: e.target.value as RuleRow["action"] })}
                    >
                      <NativeSelectOption value="accept">
                        {t("securityGroups.accept")}
                      </NativeSelectOption>
                      <NativeSelectOption value="drop">
                        {t("securityGroups.drop")}
                      </NativeSelectOption>
                    </NativeSelect>
                  </TableCell>
                  <TableCell>
                    <NativeSelect
                      aria-label={t("securityGroups.protocolN", n(i))}
                      value={row.protocol}
                      onChange={(e) => {
                        const protocol = e.target.value as RuleRow["protocol"];
                        // 포트는 tcp·udp에서만 쓴다.
                        setRow(i, hasPorts(protocol) ? { protocol } : { protocol, ports: "" });
                      }}
                    >
                      <NativeSelectOption value="any">{t("securityGroups.any")}</NativeSelectOption>
                      <NativeSelectOption value="tcp">TCP</NativeSelectOption>
                      <NativeSelectOption value="udp">UDP</NativeSelectOption>
                      <NativeSelectOption value="icmp">ICMP</NativeSelectOption>
                      <NativeSelectOption value="icmpv6">ICMPv6</NativeSelectOption>
                    </NativeSelect>
                  </TableCell>
                  <TableCell>
                    <Input
                      aria-label={t("securityGroups.portsN", n(i))}
                      className="w-28"
                      placeholder={hasPorts(row.protocol) ? t("securityGroups.anyPort") : "—"}
                      disabled={!hasPorts(row.protocol)}
                      value={row.ports}
                      onChange={(e) => setRow(i, { ports: e.target.value })}
                      {...(portsError && {
                        "aria-invalid": true,
                        "aria-describedby": `rule-${i}-ports-error`,
                      })}
                    />
                    {portsError && (
                      <p id={`rule-${i}-ports-error`} className="text-xs text-destructive">
                        {t(portsError)}
                      </p>
                    )}
                  </TableCell>
                  <TableCell>
                    <Input
                      aria-label={t("securityGroups.remoteN", n(i))}
                      className="w-44"
                      placeholder={t("securityGroups.anywhere")}
                      value={row.remote}
                      onChange={(e) => setRow(i, { remote: e.target.value })}
                      {...(remoteError && {
                        "aria-invalid": true,
                        "aria-describedby": `rule-${i}-remote-error`,
                      })}
                    />
                    {remoteError && (
                      <p id={`rule-${i}-remote-error`} className="text-xs text-destructive">
                        {t(remoteError)}
                      </p>
                    )}
                  </TableCell>
                  {write && (
                    <TableCell className="text-right whitespace-nowrap">
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("securityGroups.moveUp", n(i))}
                        disabled={i === 0}
                        onClick={() => changeRows(moveRule(rows, i, -1))}
                      >
                        <ArrowUp />
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("securityGroups.moveDown", n(i))}
                        disabled={i === rows.length - 1}
                        onClick={() => changeRows(moveRule(rows, i, 1))}
                      >
                        <ArrowDown />
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("securityGroups.removeRule", n(i))}
                        onClick={() => changeRows(rows.filter((_, j) => j !== i))}
                      >
                        <X />
                      </Button>
                    </TableCell>
                  )}
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
        {write && rows.length < MAX_RULES && (
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => changeRows([...rows, emptyRule()])}
            >
              <Plus />
              {t("securityGroups.addRule")}
            </Button>
          </div>
        )}
      </fieldset>
      {failure && (
        <div role="alert" className="flex items-center gap-3 text-sm text-destructive">
          <span>{failure.message}</span>
          {failure.conflict && (
            <Button type="button" variant="outline" size="sm" onClick={reload}>
              {t("vmForm.reload")}
            </Button>
          )}
        </div>
      )}
      {write && (
        <div>
          <Button type="submit" disabled={busy}>
            {t("common.save")}
          </Button>
        </div>
      )}
    </form>
  );
}
