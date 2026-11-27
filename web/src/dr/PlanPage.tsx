import { useQueryClient } from "@tanstack/react-query";
import {
  Ban,
  Eraser,
  FlaskConical,
  PowerOff,
  ShieldAlert,
  ShieldCheck,
  Trash2,
} from "lucide-react";
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
import { StatusChip } from "@/components/StatusChip";
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
  type FenceResponse,
  localized,
  oldNodes,
  type RecoveryPlan,
  refreshDr,
  usePlan,
  usePlanRuns,
  useReport,
} from "@/dr/api";
import { FailoverDialog, FenceDialog, ReprotectDialog, TestDialog } from "@/dr/dialogs";
import {
  type Draft,
  emptyDraft,
  formatDuration,
  sameSpec,
  toDraft,
  toSpec,
  validate,
} from "@/dr/model";
import { PlanEditor } from "@/dr/PlanEditor";
import { UpgradeNotice } from "@/dr/PlansPage";
import { cancelText, ReadinessCell, RpoBar, RunStateChip } from "@/dr/parts";
import { formatTime } from "@/lib/format";
import { useRemotes } from "@/remotes/remotes";

const TABS = ["overview", "plan", "runs"] as const;
type Tab = (typeof TABS)[number];

type Dialog = "test" | "failover" | "fence" | "cleanup" | "cancel" | "delete" | "reprotect";

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
  const [fenced, setFenced] = useState<FenceResponse | null>(null);
  const tab: Tab = TABS.find((id) => id === params.get("tab")) ?? "overview";
  const running = plan.status.run;
  const runningKind = runs.data?.find((r) => r.id === running)?.spec.kind;
  // 뒷정리가 지울 것은 서버가 센 것(보고의 `leftovers` — 시험 실행이 만든 VM과 거품 네트워크). 남은 것이 없으면 막는다.
  const leftovers = [
    ...(report.data?.leftovers.vms ?? []),
    ...(report.data?.leftovers.networks ?? []),
  ];
  const nothingToClean = report.data !== undefined && leftovers.length === 0;
  // 장애 조치는 도는 시험을 끊고 시작한다(서버). 다른 실행(장애 조치·뒷정리)이 돌면 막는다.
  const preemptsTest = !!running && runningKind === "test";
  const fencePending = (plan.status.source_fence_pending ?? []).map((f) => f.vm);
  const openRun = (run: DrRun) => void navigate(`/dr-runs/${run.id}`);

  const actions = operate && (
    <>
      <Button variant="outline" disabled={!!running} onClick={() => setDialog("test")}>
        <FlaskConical />
        {t("dr.action.test")}
      </Button>
      <Button
        variant="outline"
        disabled={!!running || nothingToClean}
        title={nothingToClean ? t("dr.cleanup.nothing") : undefined}
        onClick={() => setDialog("cleanup")}
      >
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
      <Button
        variant="destructive"
        disabled={!!running && !preemptsTest}
        onClick={() => setDialog("failover")}
      >
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
      {fencePending.length > 0 && (
        <div
          role="alert"
          className="flex flex-wrap items-center gap-3 rounded-md border border-danger bg-danger-soft px-3 py-2 text-sm text-danger-fg"
        >
          <span className="min-w-0 flex-1">
            {t("dr.fence.band", { source: plan.spec.source, vms: fencePending.join(", ") })}
          </span>
          {operate && (
            <Button variant="destructive" size="sm" onClick={() => setDialog("fence")}>
              <PowerOff />
              {t("dr.fence.button")}
            </Button>
          )}
        </div>
      )}
      {fenced && <FenceResult result={fenced} onClose={() => setFenced(null)} />}
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
          {report.data ? (
            <Overview report={report.data} lastTest={plan.status.last_test} />
          ) : (
            <ListPending error={report.error} />
          )}
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

      {dialog === "test" && (
        <TestDialog
          plan={plan}
          report={report.data}
          onClose={() => setDialog(null)}
          onStarted={openRun}
        />
      )}
      {dialog === "failover" && (
        <FailoverDialog
          plan={plan}
          report={report.data}
          preemptsTest={preemptsTest}
          onClose={() => setDialog(null)}
          onStarted={openRun}
        />
      )}
      {dialog === "fence" && (
        <FenceDialog
          plan={plan}
          onClose={() => setDialog(null)}
          onDone={(result) => {
            setFenced(result);
            setDialog(null);
          }}
        />
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
        description={cancelText(t, runningKind)}
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

/** fence-source 결과: VM마다 멈춤·실패와 아직 남은 VM */
function FenceResult({ result, onClose }: { result: FenceResponse; onClose(): void }) {
  const { t } = useTranslation();
  return (
    <section
      aria-label={t("dr.fence.result")}
      className="flex flex-col gap-1 rounded-md border px-3 py-2 text-sm"
    >
      <div className="flex items-center justify-between gap-2">
        <h2 className="font-medium">{t("dr.fence.result")}</h2>
        <Button variant="ghost" size="sm" onClick={onClose}>
          {t("dr.fence.close")}
        </Button>
      </div>
      <ul className="flex flex-col gap-0.5">
        {result.results.map((r) => (
          <li key={r.vm} className="flex flex-wrap gap-x-2">
            <span className="font-medium">{r.vm}</span>
            <span className={r.ok ? "text-success-fg" : "text-danger-fg"}>
              {t(r.ok ? "dr.fence.stopped" : "dr.fence.failed")}
            </span>
            {r.error && <span className="text-muted-foreground">{r.error}</span>}
          </li>
        ))}
      </ul>
      {result.pending.length > 0 && (
        <p className="text-danger-fg">
          {t("dr.fence.stillPending", { vms: result.pending.map((f) => f.vm).join(", ") })}
        </p>
      )}
    </section>
  );
}

/**
 * 개요: RPO 지킴, VM별 복구 지점과 장애 조치 준비(`failover_readiness` — 실제 장애 조치가 쓸 네트워크·보안 그룹·풀·노드),
 * 최근 시험 셋의 RTO와 마지막 시험이 본 준비 문제 수, 지난 장애 조치(`GET /report` — 나이는 보고를 센 때의 것)
 */
function Overview({
  report,
  lastTest,
}: {
  report: DrReport;
  lastTest: RecoveryPlan["status"]["last_test"];
}) {
  const { t } = useTranslation();
  const time = useTime();
  const readiness = new Map(report.failover_readiness.map((r) => [r.vm, r]));
  const notReady = report.failover_readiness.filter((r) => !r.ok).length;
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
            {notReady > 0 && (
              <StatusChip tone="warning">{t("dr.readiness.badge", { count: notReady })}</StatusChip>
            )}
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
                <TableHead>{t("dr.readiness.column")}</TableHead>
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
                  <TableCell>
                    <ReadinessCell readiness={readiness.get(p.vm)} />
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
        {lastTest?.readiness_issues ? (
          <p className="text-sm text-warning-fg">
            {t("dr.readiness.lastTest", { count: lastTest.readiness_issues })}
          </p>
        ) : null}
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

/** 고치는 중인 초안: 편집을 시작한 때의 계획(`base`)과 그 판. 계획 탭을 떠났다 와도 같은 세션이면 남는다(리뷰 1 웹 7). */
interface Edit {
  draft: Draft;
  base: RecoveryPlan["spec"];
  revision: number;
}
const edits = new Map<string, Edit>();

/** 시험이 서로의 초안을 보지 않게 */
export function forgetPlanEdits() {
  edits.clear();
}

/**
 * 계획 탭: 편집과 저장(PUT 통째, `If-Match`). 계획의 판은 status 쓰기(보호 상태·실행)로도 오르므로, 409이면 계획을 다시 읽어
 * spec이 편집을 시작한 때와 같으면 새 판으로 한 번 더 저장한다. spec이 바뀌었으면 그때만 알리고 "내 것으로 덮기"와 "다시 읽기
 * (고친 것 버림)"를 준다(리뷰 1 웹 1).
 */
function EditPlan({ plan, readOnly }: { plan: RecoveryPlan; readOnly: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const remotes = useRemotes();
  // 고치지 않았으면 늘 지금 계획을 보인다. 초안은 spec이 바뀔 때만 다시 만든다(그룹 key가 그리기마다 바뀌지 않게).
  const specKey = JSON.stringify(plan.spec);
  const [fresh, setFresh] = useState(() => ({
    key: specKey,
    draft: toDraft(plan.name, plan.spec),
  }));
  if (fresh.key !== specKey) {
    setFresh({ key: specKey, draft: toDraft(plan.name, plan.spec) });
  }
  const [stored, setStored] = useState(() => edits.get(plan.id));
  const edit: Edit = stored ?? { draft: fresh.draft, base: plan.spec, revision: plan.revision };
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<{ message: string; latest?: RecoveryPlan } | null>(null);
  const [busy, setBusy] = useState(false);

  function keep(next: Edit | undefined) {
    if (next) {
      edits.set(plan.id, next);
    } else {
      edits.delete(plan.id);
    }
    setStored(next);
  }

  async function discard() {
    keep(undefined);
    setErrors({});
    setFailure(null);
    await refreshDr(queryClient);
  }

  const put = (revision: number) =>
    call(
      api.PUT("/dr-plans/{key}", {
        params: { path: { key: plan.id }, header: { "If-Match": revision } },
        body: { labels: plan.labels, spec: toSpec(edit.draft) },
      }),
    );
  const conflict = (err: unknown) => err instanceof ApiError && err.code === "revision_mismatch";

  async function save(over?: RecoveryPlan) {
    const found = validate(edit.draft, false);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    setBusy(true);
    setFailure(null);
    try {
      try {
        await put(over ? over.revision : edit.revision);
      } catch (err) {
        if (over || !conflict(err)) {
          throw err;
        }
        const latest = await call(
          api.GET("/dr-plans/{key}", { params: { path: { key: plan.id } } }),
        );
        if (!sameSpec(latest.spec, edit.base)) {
          setFailure({ message: drErrorText(t, err), latest });
          return;
        }
        // 바뀐 것은 status뿐이다: 새 판으로 다시 저장한다.
        await put(latest.revision);
      }
      toast.success(t("dr.edit.saved", { name: plan.name }));
      await refreshDr(queryClient);
      keep(undefined);
    } catch (err) {
      setFailure({ message: drErrorText(t, err) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-4 pt-2">
      <PlanEditor
        draft={edit.draft}
        onChange={(draft) => keep({ ...edit, draft })}
        isNew={false}
        readOnly={readOnly || busy}
        errors={errors}
        remotes={remotes.data ?? []}
      />
      {failure && (
        <div role="alert" className="flex flex-wrap items-center gap-2 text-sm text-destructive">
          <span>{failure.message}</span>
          {failure.latest && (
            <>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={busy}
                onClick={() => void save(failure.latest)}
              >
                {t("dr.edit.overwrite")}
              </Button>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={busy}
                onClick={() => void discard()}
              >
                {t("dr.edit.reload")}
              </Button>
            </>
          )}
        </div>
      )}
      {Object.keys(errors).length > 0 && (
        <p role="alert" className="text-sm text-destructive">
          {t("dr.edit.fix")}
        </p>
      )}
      {!readOnly && (
        <div className="flex flex-wrap gap-2">
          <Button disabled={busy} onClick={() => void save()}>
            {t("common.save")}
          </Button>
          {stored && (
            <Button variant="outline" disabled={busy} onClick={() => void discard()}>
              {t("dr.edit.discard")}
            </Button>
          )}
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
