import { ArrowDown, ArrowUp, Plus, X } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "react-router";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { SecurityGroup } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { EmptyState } from "@/components/EmptyState";
import { Field } from "@/components/Field";
import { InfoButton } from "@/components/InfoButton";
import { ListPending } from "@/components/ListPending";
import { ResourceHeader } from "@/components/ResourceHeader";
import { TerraformChip, useConfigMenu } from "@/declarative/ConfigMenu";
import { useVisit } from "@/components/PinButton";
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
import { undoToast } from "@/lib/undo";
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
import { groupNics } from "@/routing/routing";
import { groupUsers } from "@/security-groups/SecurityGroupsPage";

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
    return <h1 className="text-xl font-semibold">{t("securityGroups.notFound", { name })}</h1>;
  }
  return <RulesEditor key={group.id} group={group} />;
}

function RulesEditor({ group }: { group: SecurityGroup }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const write = perms.can("write", "security-group");
  const vms = useList("vm");
  const networks = useList("network");
  // 단순 NIC(nftables)와 SDN NIC(OVN ACL, 계획 66)
  const nics = vms.data && networks.data ? groupNics(group, vms.data, networks.data) : undefined;
  useVisit({ kind: "security-group", id: group.id, name: group.name });
  // 설정 기록·YAML 내보내기(계획 73)
  const config = useConfigMenu({ kind: "security-group", id: group.id, name: group.name });
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
      // 10초 동안 저장 전 설명·규칙으로 되돌린다(`If-Match`, 그 사이 바뀌었으면 실패를 알린다).
      const before = {
        description: group.spec.description ?? "",
        rules: group.spec.rules ?? [],
        revision: saved.revision,
      };
      undoToast(t, t("securityGroups.saved", { name: group.name }), async () => {
        try {
          const back = await call(
            api.PATCH("/security-groups/{key}", {
              params: { path: { key: group.id }, header: { "If-Match": before.revision } },
              body: { description: before.description, rules: before.rules },
            }),
          );
          setDescription(before.description);
          setRows(before.rules.map(fromRule));
          setRevision(back.revision);
          setErrors({});
          toast.success(t("common.undone"));
        } catch (err) {
          toast.error(errorMessage(t, err));
        }
      });
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
      <ResourceHeader
        name={group.name}
        status={<TerraformChip labels={group.labels} />}
        menu={config.items}
        summary={[
          { label: t("securityGroups.rules"), value: (group.spec.rules ?? []).length },
          { label: t("securityGroups.vms"), value: vms.data ? groupUsers(group, vms.data) : "—" },
          {
            label: t("securityGroups.nics"),
            value: nics ? t("securityGroups.nicCounts", nics) : "—",
          },
        ]}
      >
        {config.host}
      </ResourceHeader>
      <div className="flex items-start gap-1">
        <p className="text-sm text-muted-foreground">{t("securityGroups.order")}</p>
        <InfoButton
          topic="securityGroups.rules"
          label={t("securityGroups.rules")}
          line={t("securityGroups.order")}
        />
      </div>
      <p className="text-sm text-muted-foreground">{t("securityGroups.always")}</p>
      {/* 보내는 동안이나 읽기 권한뿐이면 고치지 못하게 한다. */}
      <fieldset disabled={busy || !write} className="flex min-w-0 flex-col gap-4">
        <div className="max-w-xl">
          <Field
            id="group-description"
            label={t("securityGroups.description")}
            error={errors.description}
            help={t("help.securityGroups.description")}
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
        {rows.length > 0 && (
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
                        <NativeSelectOption value="out">
                          {t("securityGroups.out")}
                        </NativeSelectOption>
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
                        <NativeSelectOption value="any">
                          {t("securityGroups.any")}
                        </NativeSelectOption>
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
        )}
        {write && rows.length > 0 && rows.length < MAX_RULES && (
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
      {rows.length === 0 && (
        <EmptyState
          title={t("securityGroups.noRules")}
          description={t("help.securityGroups.rulesEmpty")}
          diagram="securityGroup"
          help="securityGroups.rules"
          action={
            write && (
              <Button type="button" disabled={busy} onClick={() => changeRows([emptyRule()])}>
                <Plus />
                {t("securityGroups.addRule")}
              </Button>
            )
          }
        />
      )}
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
