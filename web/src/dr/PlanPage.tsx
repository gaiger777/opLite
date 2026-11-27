import { useQueryClient } from "@tanstack/react-query";
import { Ban, Eraser, FlaskConical, ShieldAlert, ShieldCheck, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  type DrReport,
  type DrRun,
  type DrRunSummary,
  drErrorText,
  localized,
  oldNodes,
  type RecoveryPlan,
  refreshDr,
  usePlan,
  usePlanRuns,
  useReport,
} from "@/dr/api";
import { FailoverDialog, ReprotectDialog } from "@/dr/dialogs";
import { type Draft, emptyDraft, formatDuration, toDraft, toSpec, validate } from "@/dr/model";
import { PlanEditor } from "@/dr/PlanEditor";
import { UpgradeNotice } from "@/dr/PlansPage";
import { RpoBar, RunStateChip } from "@/dr/parts";
import { formatTime } from "@/lib/format";
import { useRemotes } from "@/remotes/remotes";

const TABS = ["overview", "plan", "runs"] as const;
type Tab = (typeof TABS)[number];

type Dialog = "failover" | "cleanup" | "cancel" | "delete" | "reprotect";

function useTime() {
  const { i18n } = useTranslation();
  return (secs: number) => formatTime(new Date(secs * 1000), i18n.language);
}

/** 계획 하나(계획 76 §4): 개요(보호 상태·보고), 계획(편집), 실행 기록 탭과 시험·장애 조치·뒷정리·취소 단추. 늦게 읽는 조각이다. */
export function PlanPage() {
  const { name = "" } = useParams();
  const plan = usePlan(name);
  if (!plan.data) {
    return <ListPending error={plan.error} />;
  }
  return <PlanView key={plan.data.id} plan={plan.data} />;
}

function PlanView({ plan }: { plan: RecoveryPlan }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [params, setParams] = useSearchParams();
  const report = useReport(plan.name);
  const runs = usePlanRuns(plan.id);
  const nodes = useList("node");
  const old = oldNodes(nodes.data);
  const ready = nodes.data !== undefined && old.length === 0;
  const write = perms.can("write", "recovery-plan");
  const operate = perms.can("operate", "recovery-plan") && ready;
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [busy, setBusy] = useState(false);
  const tab: Tab = TABS.find((id) => id === params.get("tab")) ?? "overview";
  const running = plan.status.run;
  const latestTest = runs.data?.find((r) => r.spec.kind === "test");
  const leftovers = latestTest
    ? [
        ...(latestTest.status.steps ?? []).flatMap((s) => (s.new_vm ? [s.new_vm] : [])),
        ...(latestTest.status.bubbles ?? [])
          .filter((b) => b.state !== "removed")
          .map((b) => b.name),
      ]
    : [];
  const openRun = (run: DrRun) => void navigate(`/dr-runs/${run.id}`);

  async function test() {
    setBusy(true);
    try {
      const run = await call(
        api.POST("/dr-plans/{key}/actions/test", { params: { path: { key: plan.id } } }),
      );
      toast.success(t("dr.test.started", { name: plan.name }));
      openRun(run);
    } catch (err) {
      toast.error(drErrorText(t, err));
    } finally {
      setBusy(false);
      await refreshDr(queryClient);
    }
  }

  const actions = operate && (
    <>
      <Button variant="outline" disabled={busy || !!running} onClick={() => void test()}>
        <FlaskConical />
        {t("dr.action.test")}
      </Button>
      <Button variant="outline" disabled={!!running} onClick={() => setDialog("cleanup")}>
        <Eraser />
        {t("dr.action.cleanup")}
      </Button>
      {running && (
        <Button variant="outline" onClick={() => setDialog("cancel")}>
          <Ban />
          {t("dr.action.cancel")}
        </Button>
      )}
      {report.data?.last_failover && (
        <Button variant="outline" onClick={() => setDialog("reprotect")}>
          <ShieldCheck />
          {t("dr.action.reprotect")}
        </Button>
      )}
      <Button variant="destructive" disabled={!!running} onClick={() => setDialog("failover")}>
        <ShieldAlert />
        {t("dr.action.failover")}
      </Button>
    </>
  );

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="dr-plan-title"
        title={plan.name}
        description={t("dr.plan.summary", { source: plan.spec.source, pool: plan.spec.pool })}
        actions={actions}
      />
      <UpgradeNotice old={old} />
      {running && (
        <p
          role="status"
          className="flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-sm"
        >
          <RunStateChip state="running" />
          <Link to={`/dr-runs/${running}`} className="underline-offset-4 hover:underline">
            {t("dr.plan.openRun")}
          </Link>
        </p>
      )}
      <Tabs
        value={tab}
        onValueChange={(value) =>
          setParams(
            (p) => {
              p.set("tab", value);
              return p;
            },
            { replace: true },
          )
        }
      >
        <TabsList>
          {TABS.map((id) => (
            <TabsTrigger key={id} value={id}>
              {t(`dr.tab.${id}`)}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="overview">
          {report.data ? <Overview report={report.data} /> : <ListPending error={report.error} />}
        </TabsContent>
        <TabsContent value="plan">
          {tab === "plan" && <EditPlan plan={plan} readOnly={!write || !ready} />}
        </TabsContent>
        <TabsContent value="runs">
          {runs.data ? (
            <RunsTable
              runs={runs.data.map((r) => ({
                id: r.id,
                name: r.name,
                kind: r.spec.kind,
                state: r.status.state,
                started_at: r.status.started_at,
                finished_at: r.status.finished_at,
                rto_secs: r.status.rto_secs,
                prepare_secs: r.status.prepare_secs,
                failed_vms: [],
              }))}
              label={t("dr.tab.runs")}
            />
          ) : (
            <ListPending error={runs.error} />
          )}
        </TabsContent>
      </Tabs>

      {dialog === "failover" && (
        <FailoverDialog plan={plan} onClose={() => setDialog(null)} onStarted={openRun} />
      )}
      {dialog === "reprotect" && <ReprotectDialog plan={plan} onClose={() => setDialog(null)} />}
      <ConfirmDialog
        open={dialog === "cleanup"}
        onOpenChange={(open) => !open && setDialog(null)}
        title={t("dr.cleanup.title", { name: plan.name })}
        description={t("dr.cleanup.description")}
        confirmLabel={t("dr.action.cleanup")}
        impact={leftovers}
        onConfirm={async () => {
          try {
            const run = await call(
              api.POST("/dr-plans/{key}/actions/cleanup", { params: { path: { key: plan.id } } }),
            );
            toast.success(t("dr.cleanup.started", { name: plan.name }));
            openRun(run);
          } catch (err) {
            throw localized(t, err);
          } finally {
            await refreshDr(queryClient);
          }
        }}
      />
      <ConfirmDialog
        open={dialog === "cancel"}
        onOpenChange={(open) => !open && setDialog(null)}
        title={t("dr.cancel.title", { name: plan.name })}
        description={t("dr.cancel.description")}
        confirmLabel={t("dr.action.cancel")}
        onConfirm={async () => {
          try {
            await call(
              api.POST("/dr-plans/{key}/actions/cancel", { params: { path: { key: plan.id } } }),
            );
            toast.success(t("dr.cancel.done", { name: plan.name }));
          } catch (err) {
            throw localized(t, err);
          } finally {
            await refreshDr(queryClient);
          }
        }}
      />
      <ConfirmDialog
        open={dialog === "delete"}
        onOpenChange={(open) => !open && setDialog(null)}
        title={t("common.deleteNamed", { name: plan.name })}
        description={t("dr.delete.description")}
        confirmLabel={t("common.delete")}
        typeToConfirm={plan.name}
        onConfirm={async () => {
          try {
            await call(
              api.DELETE("/dr-plans/{key}", {
                params: { path: { key: plan.id }, header: { "If-Match": plan.revision } },
              }),
            );
          } catch (err) {
            throw localized(t, err);
          }
          toast.success(t("dr.delete.done", { name: plan.name }));
          await refreshDr(queryClient);
          void navigate("/dr-plans");
        }}
      />
      {write && ready && tab === "plan" && (
        <div>
          <Button
            variant="ghost"
            className="text-destructive"
            disabled={!!running}
            onClick={() => setDialog("delete")}
          >
            <Trash2 />
            {t("dr.delete.button")}
          </Button>
        </div>
      )}
    </div>
  );
}

/** 개요: RPO 지킴, VM별 복구 지점, 최근 시험 셋의 RTO, 지난 장애 조치(`GET /report` — 나이는 보고를 센 때의 것) */
function Overview({ report }: { report: DrReport }) {
  const { t } = useTranslation();
  const time = useTime();
  return (
    <div className="flex flex-col gap-6 pt-2">
      {!report.source_registered && (
        <p role="alert" className="text-sm text-destructive">
          {t("dr.report.sourceMissing", { source: report.source })}
        </p>
      )}
      <section aria-labelledby="dr-protection" className="flex flex-col gap-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 id="dr-protection" className="text-base font-semibold">
            {t("dr.report.protection")}
          </h2>
          <span className="flex flex-wrap items-center gap-3 text-sm">
            <RpoBar protection={report.protection} />
            <span className="text-muted-foreground">
              {report.rpo_target_secs > 0
                ? t("dr.report.target", { time: formatDuration(report.rpo_target_secs) })
                : t("dr.report.noTarget")}
            </span>
          </span>
        </div>
        <div className="overflow-x-auto">
          <Table aria-labelledby="dr-protection">
            <TableHeader>
              <TableRow>
                <TableHead>{t("dr.report.vm")}</TableHead>
                <TableHead>{t("dr.report.point")}</TableHead>
                <TableHead>{t("dr.report.pointAt")}</TableHead>
                <TableHead>{t("dr.report.age")}</TableHead>
                <TableHead>{t("dr.report.status")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {report.protection.map((p) => (
                <TableRow key={p.vm}>
                  <TableCell className="font-medium">{p.vm}</TableCell>
                  <TableCell>
                    {p.backup ? (
                      <Link
                        to={`/backups?q=${encodeURIComponent(p.backup)}`}
                        className="underline-offset-4 hover:underline"
                      >
                        {p.backup}
                      </Link>
                    ) : (
                      "—"
                    )}
                  </TableCell>
                  <TableCell>{p.point_at !== undefined ? time(p.point_at) : "—"}</TableCell>
                  <TableCell className="tabular-nums">
                    {p.age_secs !== undefined ? formatDuration(p.age_secs) : "—"}
                  </TableCell>
                  <TableCell>
                    {p.ok ? (
                      <span className="text-success-fg">{t("dr.report.ok")}</span>
                    ) : (
                      <span className="font-medium text-danger-fg">
                        {t(`dr.issue.${p.issue ?? "no-recovery-point"}`)}
                      </span>
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </section>
      <section aria-labelledby="dr-tests" className="flex flex-col gap-2">
        <h2 id="dr-tests" className="text-base font-semibold">
          {t("dr.report.tests")}
        </h2>
        {report.tests.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("dr.plan.neverTested")}</p>
        ) : (
          <RunsTable runs={report.tests} label={t("dr.report.tests")} showFailed />
        )}
      </section>
      <section aria-labelledby="dr-failover" className="flex flex-col gap-2">
        <h2 id="dr-failover" className="text-base font-semibold">
          {t("dr.report.lastFailover")}
        </h2>
        {report.last_failover ? (
          <RunsTable runs={[report.last_failover]} label={t("dr.report.lastFailover")} showFailed />
        ) : (
          <p className="text-sm text-muted-foreground">{t("dr.report.noFailover")}</p>
        )}
      </section>
    </div>
  );
}

/** 실행 요약 표(보고의 시험·장애 조치와 실행 기록 탭) */
function RunsTable({
  runs,
  label,
  showFailed = false,
}: {
  runs: readonly DrRunSummary[];
  label: string;
  showFailed?: boolean;
}) {
  const { t } = useTranslation();
  const time = useTime();
  if (runs.length === 0) {
    return <p className="pt-2 text-sm text-muted-foreground">{t("dr.runs.empty")}</p>;
  }
  return (
    <div className="overflow-x-auto">
      <Table aria-label={label}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("dr.runs.kind")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("dr.runs.started")}</TableHead>
            <TableHead>{t("dr.runs.rto")}</TableHead>
            {showFailed && <TableHead>{t("dr.runs.failedVms")}</TableHead>}
          </TableRow>
        </TableHeader>
        <TableBody>
          {runs.map((r) => (
            <TableRow key={r.id}>
              <TableCell>
                <Link to={`/dr-runs/${r.id}`} className="underline-offset-4 hover:underline">
                  {t(`dr.kind.${r.kind}`)}
                </Link>
              </TableCell>
              <TableCell>
                <RunStateChip state={r.state} />
              </TableCell>
              <TableCell>{time(r.started_at)}</TableCell>
              <TableCell className="tabular-nums">
                {r.rto_secs !== undefined ? formatDuration(r.rto_secs) : "—"}
                {r.prepare_secs !== undefined &&
                  ` (${t("dr.runs.prepare", { time: formatDuration(r.prepare_secs) })})`}
              </TableCell>
              {showFailed && <TableCell>{r.failed_vms.join(", ") || "—"}</TableCell>}
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}

/** 계획 탭: 편집과 저장(PUT 통째, `If-Match`). 다른 사람이 먼저 바꿨으면 알리고 다시 읽기를 준다. */
function EditPlan({ plan, readOnly }: { plan: RecoveryPlan; readOnly: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const remotes = useRemotes();
  const [draft, setDraft] = useState<Draft>(() => toDraft(plan.name, plan.spec));
  const [revision, setRevision] = useState(plan.revision);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<{ message: string; conflict: boolean } | null>(null);
  const [busy, setBusy] = useState(false);

  function reload() {
    setDraft(toDraft(plan.name, plan.spec));
    setRevision(plan.revision);
    setErrors({});
    setFailure(null);
  }

  async function save() {
    const found = validate(draft, false);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    setBusy(true);
    setFailure(null);
    try {
      const saved = await call(
        api.PUT("/dr-plans/{key}", {
          params: { path: { key: plan.id }, header: { "If-Match": revision } },
          body: { labels: plan.labels, spec: toSpec(draft) },
        }),
      );
      setRevision(saved.revision);
      toast.success(t("dr.edit.saved", { name: plan.name }));
      await refreshDr(queryClient);
    } catch (err) {
      setFailure({
        message: drErrorText(t, err),
        conflict: err instanceof ApiError && err.code === "revision_mismatch",
      });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-4 pt-2">
      <PlanEditor
        draft={draft}
        onChange={setDraft}
        isNew={false}
        readOnly={readOnly || busy}
        errors={errors}
        remotes={remotes.data ?? []}
      />
      {failure && (
        <div role="alert" className="flex flex-wrap items-center gap-2 text-sm text-destructive">
          <span>{failure.message}</span>
          {failure.conflict && (
            <Button type="button" variant="outline" size="sm" onClick={reload}>
              {t("dr.edit.reload")}
            </Button>
          )}
        </div>
      )}
      {Object.keys(errors).length > 0 && (
        <p role="alert" className="text-sm text-destructive">
          {t("dr.edit.fix")}
        </p>
      )}
      {!readOnly && (
        <div className="flex gap-2">
          <Button disabled={busy} onClick={() => void save()}>
            {t("common.save")}
          </Button>
        </div>
      )}
    </div>
  );
}

/** 새 계획(`/dr-plans/-new`): 같은 편집으로 만든다(`POST /dr-plans`). */
export function NewPlanPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const perms = usePermissions();
  const remotes = useRemotes();
  const nodes = useList("node");
  const old = oldNodes(nodes.data);
  const [draft, setDraft] = useState<Draft>(emptyDraft);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const allowed = perms.can("write", "recovery-plan");

  async function create() {
    const found = validate(draft, true);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    setBusy(true);
    setFailure(null);
    try {
      const plan = await call(
        api.POST("/dr-plans", { body: { name: draft.name, spec: toSpec(draft) } }),
      );
      toast.success(t("dr.edit.created", { name: plan.name }));
      await refreshDr(queryClient);
      void navigate(`/dr-plans/${encodeURIComponent(plan.name)}`);
    } catch (err) {
      setFailure(drErrorText(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t("dr.plans.newTitle")} description={t("dr.plans.newDescription")} />
      <UpgradeNotice old={old} />
      {!allowed ? (
        <p role="alert" className="text-sm text-destructive">
          {t("nav.noAccess")}
        </p>
      ) : (
        <>
          <PlanEditor
            draft={draft}
            onChange={setDraft}
            isNew
            readOnly={busy}
            errors={errors}
            remotes={remotes.data ?? []}
          />
          {failure && (
            <p role="alert" className="text-sm text-destructive">
              {failure}
            </p>
          )}
          {Object.keys(errors).length > 0 && (
            <p role="alert" className="text-sm text-destructive">
              {t("dr.edit.fix")}
            </p>
          )}
          <div className="flex gap-2">
            <Button disabled={busy || old.length > 0} onClick={() => void create()}>
              {t("common.create")}
            </Button>
            <Button variant="outline" asChild>
              <Link to="/dr-plans">{t("common.cancel")}</Link>
            </Button>
          </div>
        </>
      )}
    </div>
  );
}
