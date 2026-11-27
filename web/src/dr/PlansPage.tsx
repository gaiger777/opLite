import { Plus } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router";

import { useList } from "@/api/live";
import { usePermissions } from "@/auth/usePermissions";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { oldNodes, type RecoveryPlan, usePlans } from "@/dr/api";
import { formatDuration } from "@/dr/model";
import { RpoBar, RunStateChip } from "@/dr/parts";
import { useCreateParam } from "@/lib/createParam";

/** 기능 `dr-plans`를 모르는 노드가 있으면 그 이름(쓰기는 409 `cluster_upgrading`이므로 먼저 알린다) */
export function UpgradeNotice({ old }: { old: string[] }) {
  const { t } = useTranslation();
  if (old.length === 0) {
    return null;
  }
  return (
    <p role="status" className="rounded-md border border-warning bg-warning-soft px-3 py-2 text-sm">
      {t("dr.upgrading", { nodes: old.join(", ") })}
    </p>
  );
}

function vmCount(plan: RecoveryPlan) {
  return plan.spec.groups.reduce((n, g) => n + g.vms.length, 0);
}

/**
 * 데이터 보호 › DR(계획 76 §4): 복구 계획 목록 — 원천(원격), 보호 VM 수, RPO 지킴 막대, 마지막 시험의 RTO·결과, 도는 실행.
 * 만들기는 루트 `cluster.admin`(`recovery-plan` 쓰기)이고 새 계획은 편집 화면(`/dr-plans/-new`)이다. 늦게 읽는 조각이다.
 */
export function PlansPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const navigate = useNavigate();
  const plans = usePlans(perms.reads("recovery-plan"));
  const nodes = useList("node");
  const old = oldNodes(nodes.data);
  const write = perms.can("write", "recovery-plan") && nodes.data !== undefined && old.length === 0;
  useCreateParam(() => void navigate("/dr-plans/-new"), write);
  const items = plans.data ?? [];

  const newButton = write && (
    <Button asChild>
      <Link to="/dr-plans/-new">
        <Plus />
        {t("dr.plans.new")}
      </Link>
    </Button>
  );

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="dr-plans-title"
        title={t("nav.dr")}
        help="dr"
        intro={t("dr.help.intro")}
        actions={items.length > 0 && newButton}
      />
      <UpgradeNotice old={old} />
      {plans.data === undefined ? (
        <ListPending error={plans.error} />
      ) : items.length === 0 ? (
        <EmptyState
          title={t("dr.plans.empty")}
          description={t("dr.plans.emptyHint")}
          diagram="replicationDr"
          help="dr.page"
          action={newButton}
        />
      ) : (
        <div className="overflow-x-auto">
          <Table aria-labelledby="dr-plans-title">
            <TableHeader>
              <TableRow>
                <TableHead>{t("common.name")}</TableHead>
                <TableHead>{t("dr.plan.source")}</TableHead>
                <TableHead>{t("dr.plan.vms")}</TableHead>
                <TableHead>{t("dr.plan.rpo")}</TableHead>
                <TableHead>{t("dr.plan.lastTest")}</TableHead>
                <TableHead>{t("dr.plan.running")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((p) => {
                const last = p.status.last_test;
                return (
                  <TableRow key={p.id}>
                    <TableCell className="font-medium">
                      <Link
                        to={`/dr-plans/${encodeURIComponent(p.name)}`}
                        className="underline-offset-4 hover:underline"
                      >
                        {p.name}
                      </Link>
                    </TableCell>
                    <TableCell>{p.spec.source}</TableCell>
                    <TableCell className="tabular-nums">{vmCount(p)}</TableCell>
                    <TableCell>
                      <RpoBar protection={p.status.protection} />
                    </TableCell>
                    <TableCell>
                      {last ? (
                        <span className="flex flex-wrap items-center gap-2">
                          <RunStateChip state={last.result} />
                          <span className="text-xs tabular-nums">
                            {last.rto_secs !== undefined
                              ? t("dr.rto", { time: formatDuration(last.rto_secs) })
                              : "—"}
                          </span>
                        </span>
                      ) : (
                        <span className="text-xs text-muted-foreground">
                          {t("dr.plan.neverTested")}
                        </span>
                      )}
                    </TableCell>
                    <TableCell>
                      {p.status.run ? (
                        <Link
                          to={`/dr-runs/${p.status.run}`}
                          className="text-sm underline-offset-4 hover:underline"
                        >
                          <RunStateChip state="running" />
                        </Link>
                      ) : (
                        "—"
                      )}
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
