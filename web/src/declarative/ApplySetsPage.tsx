import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { usePermissions } from "@/auth/usePermissions";
import { CopyButton } from "@/components/CopyButton";
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
import { type ApplySet, appliedAtMs, kindCounts, useApplySets } from "@/declarative/api";
import { Diagram } from "@/help/Diagram";
import { kindName } from "@/history/describe";
import { formatTime } from "@/lib/format";

/** 묶음의 비교 칩: 마지막 오류 > 다른 리소스 수 > 같음(아직 견주지 않았으면 그것) */
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

/** "가상 머신 2 · 볼륨 2 · 네트워크 1" — 묶음이 무엇을 관리하나 */
export function useManagedSummary(): (set: ApplySet) => string {
  const { t } = useTranslation();
  return (set) =>
    kindCounts(set)
      .map(([kind, count]) => t("declarative.sets.count", { kind: kindName(kind, t), count }))
      .join(" · ");
}

/** 마지막 적용: 누가·언제와 결과(성공·실패) */
export function LastApplied({ set }: { set: ApplySet }) {
  const { t, i18n } = useTranslation();
  const at = appliedAtMs(set);
  if (at === undefined) {
    return <>—</>;
  }
  return (
    <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">
      <span>
        {t("declarative.sets.appliedBy", {
          time: formatTime(new Date(at), i18n.language),
          user: set.status.applied_by ?? "—",
        })}
      </span>
      {set.status.last_error ? (
        <StatusChip tone="danger">{t("declarative.sets.applyFailed")}</StatusChip>
      ) : (
        <StatusChip tone="success">{t("declarative.sets.applyOk")}</StatusChip>
      )}
    </span>
  );
}

/**
 * "코드로 관리(YAML)"가 무엇인지(진단 §3.5): 그림(파일 → 차이 보기 → 적용 → 드리프트 감시)과 무엇·언제·어떻게 시작 세
 * 문장, 적용 명령(복사). 목록이 있어도 첫 화면에 둔다 — 처음 보는 운영자가 "이게 뭔지" 알게.
 */
function AboutManagedAsCode() {
  const { t } = useTranslation();
  const command = "orbivisorctl apply --set <name> -f <folder>";
  return (
    <section
      aria-labelledby="about-yaml"
      className="grid gap-4 rounded-lg border bg-card p-4 md:grid-cols-[minmax(0,22rem)_1fr]"
    >
      <Diagram id="declarativeApply" className="flex w-full justify-center" />
      <div className="flex min-w-0 flex-col gap-2 text-sm">
        <h2 id="about-yaml" className="font-medium">
          {t("declarative.sets.aboutTitle")}
        </h2>
        {(["aboutWhat", "aboutWhen", "aboutStart"] as const).map((k) => (
          <p key={k}>
            <span className="font-medium">{t(`declarative.sets.${k}Label`)}</span>{" "}
            <span className="text-muted-foreground">{t(`declarative.sets.${k}`)}</span>
          </p>
        ))}
        <div className="flex min-w-0 items-center gap-1">
          <code
            aria-label={t("declarative.sets.command")}
            className="min-w-0 rounded-md bg-muted px-2 py-1 font-mono text-xs break-all"
          >
            {command}
          </code>
          <CopyButton text={command} label={t("declarative.set.copyCommand")} />
        </div>
      </div>
    </section>
  );
}

/**
 * 기록 › 코드로 관리(YAML)(계획 73 §12.5·§13, 계획 77-7): 무엇인지 그림 설명, 묶음 목록 — 이름·무엇을 관리하나(종류별 수)·
 * 마지막 적용(누가·언제·결과)·파일과 비교(같음·n개 다름 → 상세의 비교로). 루트 `system.read`로 읽는다. 늦게 읽는 조각이다.
 */
export function ApplySetsPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const readable = perms.reads("apply-set");
  const sets = useApplySets(readable);
  const managed = useManagedSummary();
  const items = [...(sets.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));
  return (
    <div className="flex flex-col gap-4">
      <PageHeader id="apply-sets-title" title={t("nav.applySets")} help="applySets" />
      <AboutManagedAsCode />
      {!readable ? (
        <p className="text-sm text-muted-foreground">{t("nav.noAccess")}</p>
      ) : sets.data === undefined ? (
        <ListPending error={sets.error} />
      ) : items.length === 0 ? (
        <EmptyState
          title={t("declarative.sets.empty")}
          description={t("declarative.sets.emptyHint")}
          help="applySets.page"
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
              {items.map((set) => {
                const href = `/apply-sets/${encodeURIComponent(set.name)}`;
                const drifted = (set.status.drift?.items ?? []).length > 0;
                return (
                  <TableRow key={set.id}>
                    <TableCell className="font-medium">
                      <Link to={href} className="underline-offset-4 hover:underline">
                        {set.name}
                      </Link>
                      {set.spec.description && (
                        <div className="text-xs font-normal whitespace-normal text-muted-foreground">
                          {set.spec.description}
                        </div>
                      )}
                    </TableCell>
                    <TableCell className="whitespace-normal">{managed(set) || "—"}</TableCell>
                    <TableCell className="whitespace-normal">
                      <LastApplied set={set} />
                    </TableCell>
                    <TableCell className="whitespace-normal">
                      <span className="inline-flex flex-wrap items-center gap-2">
                        <DriftChip set={set} />
                        {drifted && (
                          <Link
                            to={`${href}#compare`}
                            className="text-xs text-brand underline-offset-4 hover:underline"
                          >
                            {t("declarative.sets.whatDiffers")}
                          </Link>
                        )}
                      </span>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      )}
    </div>
  );
}
