import { Layers } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { usePermissions } from "@/auth/usePermissions";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { StatusChip } from "@/components/StatusChip";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { type ApplySet, useApplySets } from "@/declarative/api";
import { formatTime } from "@/lib/format";

/** 묶음의 드리프트 칩: 마지막 오류 > 다른 객체 수 > 같음(아직 견주지 않았으면 없음) */
export function DriftChip({ set }: { set: ApplySet }) {
  const { t } = useTranslation();
  const drift = set.status.drift?.items ?? [];
  if (set.status.last_error) {
    return <StatusChip tone="danger">{t("declarative.sets.failed")}</StatusChip>;
  }
  if (drift.length > 0) {
    return (
      <StatusChip tone="warning">
        {t("declarative.sets.drifted", { count: drift.length })}
      </StatusChip>
    );
  }
  if (!set.status.drift) {
    return <StatusChip tone="neutral">{t("declarative.sets.unchecked")}</StatusChip>;
  }
  return <StatusChip tone="success">{t("declarative.sets.inSync")}</StatusChip>;
}

/**
 * 기록 › 코드로 관리(YAML)(계획 73 §12.5·§13): 적용 묶음 목록 — 이름·객체 수·마지막 적용(누가)·드리프트 칩. 루트
 * `system.read`로 읽는다. 늦게 읽는 조각이다.
 */
export function ApplySetsPage() {
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const readable = perms.reads("apply-set");
  const sets = useApplySets(readable);
  const items = [...(sets.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="apply-sets-title"
        title={t("nav.applySets")}
        description={t("declarative.sets.pageHint")}
      />
      {!readable ? (
        <p className="text-sm text-muted-foreground">{t("nav.noAccess")}</p>
      ) : sets.data === undefined ? (
        <ListPending error={sets.error} />
      ) : items.length === 0 ? (
        <EmptyState
          icon={Layers}
          title={t("declarative.sets.empty")}
          description={t("declarative.sets.emptyHint")}
        />
      ) : (
        <div className="overflow-x-auto">
          <Table aria-labelledby="apply-sets-title">
            <TableHeader>
              <TableRow>
                <TableHead>{t("common.name")}</TableHead>
                <TableHead>{t("declarative.sets.objects")}</TableHead>
                <TableHead>{t("declarative.sets.lastApplied")}</TableHead>
                <TableHead>{t("declarative.sets.drift")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((set) => (
                <TableRow key={set.id}>
                  <TableCell className="font-medium">
                    <Link
                      to={`/apply-sets/${encodeURIComponent(set.name)}`}
                      className="underline-offset-4 hover:underline"
                    >
                      {set.name}
                    </Link>
                    {set.spec.description && (
                      <div className="text-xs whitespace-normal text-muted-foreground">
                        {set.spec.description}
                      </div>
                    )}
                  </TableCell>
                  <TableCell className="tabular-nums">{set.status.objects.length}</TableCell>
                  <TableCell>
                    {set.status.applied_at
                      ? t("declarative.sets.appliedBy", {
                          time: formatTime(new Date(set.status.applied_at), i18n.language),
                          user: set.status.applied_by ?? "—",
                        })
                      : "—"}
                  </TableCell>
                  <TableCell>
                    <DriftChip set={set} />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </div>
  );
}
