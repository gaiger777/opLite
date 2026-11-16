import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { Node, StoragePool } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { poolsUsableOn } from "@/storage/pools";
import type { ReplicationRow, VmFormValues } from "@/vms/form";
import {
  DEFAULT_REPLICATION_MINUTES,
  MAX_REPLICATION_MINUTES,
  MAX_REPLICATION_TARGETS,
} from "@/vms/replication";

/** 로컬 디스크 복제 대상: 노드, 주기(분), 대상 풀. 최대 3개 */
export function ReplicationSection({
  values,
  errors,
  onChange,
  onRowsChange,
  nodes,
  pools,
  ownNode,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 행을 넣거나 뺀다. 행 번호로 단 오류가 다른 행을 가리키므로 오류도 지운다. */
  onRowsChange: (replication: ReplicationRow[]) => void;
  nodes: Node[] | undefined;
  pools: StoragePool[] | undefined;
  /** VM이 도는(고른) 노드. 대상이 될 수 없다. 모르면(자동 배치) 빈 문자열 */
  ownNode: string;
}) {
  const { t } = useTranslation();
  // 위트니스는 복제본을 두지 않고, VM 자신의 노드는 대상이 아니다.
  const candidates = (nodes ?? []).filter((n) => !n.spec.witness && n.id !== ownNode);
  const setRow = (i: number, patch: Partial<ReplicationRow>) =>
    onChange({
      replication: values.replication.map((r, j) => (j === i ? { ...r, ...patch } : r)),
    });
  const add = () => {
    const free = candidates.find((n) => !values.replication.some((r) => r.node === n.id));
    onRowsChange([
      ...values.replication,
      { node: free?.id ?? "", minutes: String(DEFAULT_REPLICATION_MINUTES), pool: "" },
    ]);
  };
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vmForm.replication")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-sm text-muted-foreground">{t("vmForm.replicationNote")}</p>
        {errors.replication && (
          <p role="alert" className="text-sm text-destructive">
            {t(errors.replication)}
          </p>
        )}
        {values.replication.map((row, i) => {
          // 목록에 없는 노드(지운 노드, VM 자신의 노드)도 고른 값으로 보인다(검증·서버가 알린다).
          const choices = candidates.some((n) => n.id === row.node)
            ? candidates
            : [
                ...(row.node === ""
                  ? []
                  : [nodes?.find((n) => n.id === row.node) ?? { id: row.node, name: row.node }]),
                ...candidates,
              ];
          const usable = poolsUsableOn(pools ?? [], nodes ?? [], row.node);
          const poolNames = usable.map((p) => p.name);
          if (row.pool !== "" && !poolNames.includes(row.pool)) {
            poolNames.unshift(row.pool);
          }
          return (
            <fieldset
              key={i}
              aria-label={t("vmForm.replicationN", { n: i + 1 })}
              className="grid grid-cols-1 items-end gap-2 rounded-md border p-3 sm:grid-cols-[1fr_8rem_1fr_auto]"
            >
              <Field
                id={`replication-${i}-node`}
                label={t("common.node")}
                error={errors[`replication.${i}.node`]}
              >
                {(a11y) => (
                  <NativeSelect
                    id={`replication-${i}-node`}
                    {...a11y}
                    className="w-full"
                    value={row.node}
                    // 노드를 바꾸면 그 노드에 없을 수 있는 풀을 비운다(같은 이름의 풀).
                    onChange={(e) => setRow(i, { node: e.target.value, pool: "" })}
                  >
                    {row.node === "" && (
                      <NativeSelectOption value="">
                        {t("vmForm.replicationChoose")}
                      </NativeSelectOption>
                    )}
                    {choices.map((n) => (
                      <NativeSelectOption key={n.id} value={n.id}>
                        {n.name}
                      </NativeSelectOption>
                    ))}
                  </NativeSelect>
                )}
              </Field>
              <Field
                id={`replication-${i}-minutes`}
                label={t("vmForm.replicationMinutes")}
                error={errors[`replication.${i}.minutes`]}
              >
                {(a11y) => (
                  <Input
                    id={`replication-${i}-minutes`}
                    {...a11y}
                    type="number"
                    min={1}
                    max={MAX_REPLICATION_MINUTES}
                    value={row.minutes}
                    onChange={(e) => setRow(i, { minutes: e.target.value })}
                  />
                )}
              </Field>
              <Field id={`replication-${i}-pool`} label={t("vmForm.replicationPool")}>
                <NativeSelect
                  id={`replication-${i}-pool`}
                  className="w-full"
                  value={row.pool}
                  onChange={(e) => setRow(i, { pool: e.target.value })}
                >
                  <NativeSelectOption value="">{t("migrate.samePool")}</NativeSelectOption>
                  {poolNames.map((name) => (
                    <NativeSelectOption key={name} value={name}>
                      {name}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </Field>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={t("vmForm.removeReplication", { n: i + 1 })}
                onClick={() => onRowsChange(values.replication.filter((_, j) => j !== i))}
              >
                <X />
              </Button>
            </fieldset>
          );
        })}
        <div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={values.replication.length >= MAX_REPLICATION_TARGETS}
            onClick={add}
          >
            <Plus />
            {t("vmForm.addReplication")}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
