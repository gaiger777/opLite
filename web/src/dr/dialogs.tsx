import { useQueryClient } from "@tanstack/react-query";
import { cn } from "cn";
import type { TFunction } from "i18next";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Weekday } from "@/api/types";
import { backupPools } from "@/backups/backups";
import { KEEP_UNITS } from "@/backups/jobs";
import { type RemoteTarget, RemoteTargetFields } from "@/backups/RemoteTargetFields";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  detailNames,
  type DrReport,
  type DrRun,
  type FailoverMode,
  type FenceResponse,
  localized,
  type RecoveryPlan,
  refreshDr,
  type TestRequest,
  useReport,
} from "@/dr/api";
import {
  formatDuration,
  localToSecs,
  protectionGaps,
  testNetworkIssues,
  toDraft,
} from "@/dr/model";
import { useRouted } from "@/routing/routing";

const MODES: readonly FailoverMode[] = ["planned", "disaster"];
/** 영향 목록 한 줄에 보일 이름 수 */
const SHOWN = 10;

/** 이름 목록을 한 줄로(많으면 앞 [`SHOWN`]개와 남은 수) */
function names(t: TFunction, list: readonly string[]): string {
  const head = list.slice(0, SHOWN).join(", ");
  return list.length > SHOWN
    ? `${head} ${t("dr.failover.andMore", { count: list.length - SHOWN })}`
    : head;
}

/** 확인 칸 하나(글은 옆에) */
function Check({
  id,
  checked,
  onChange,
  children,
}: {
  id: string;
  checked: boolean;
  onChange(v: boolean): void;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-start gap-2">
      <Checkbox
        id={id}
        className="mt-0.5"
        checked={checked}
        onCheckedChange={(v) => onChange(v === true)}
      />
      <Label htmlFor={id} className="text-sm leading-snug font-normal">
        {children}
      </Label>
    </div>
  );
}

/**
 * 고급: 복구 지점 시각(`point_in_time` — 그 시각 이전의 가장 새 사본). 지역 시각으로 고르고 유닉스 초로 보낸다. 시험과
 * disaster 장애 조치만 받는다.
 */
function PointInTime({
  id,
  value,
  error,
  onChange,
}: {
  id: string;
  value: string;
  error?: string;
  onChange(v: string): void;
}) {
  const { t } = useTranslation();
  return (
    <details className="rounded-md border px-3 py-2 text-sm">
      <summary className="cursor-pointer font-medium">{t("dr.runOptions.advanced")}</summary>
      <div className="flex flex-col gap-1.5 pt-2">
        <Label htmlFor={id}>{t("dr.runOptions.pointInTime")}</Label>
        <Input
          id={id}
          type="datetime-local"
          className="w-auto"
          value={value}
          aria-invalid={error ? true : undefined}
          aria-describedby={`${id}-hint`}
          onChange={(e) => onChange(e.target.value)}
        />
        <p
          id={`${id}-hint`}
          className={cn("text-xs", error ? "text-destructive" : "text-muted-foreground")}
        >
          {error ?? t("dr.runOptions.pointInTimeHint")}
        </p>
      </div>
    </details>
  );
}

/** 복구 지점이 없는 VM과 VM마다 "건너뛰기"(`skip_vms`) — 목록이 있을 때만. 그 시각의 보고를 받는 중이면 그렇다고. */
function MissingPoints({ id, missing }: { id: string; missing: ReturnType<typeof useMissing> }) {
  const { t } = useTranslation();
  if (missing.loading) {
    return (
      <p role="status" className="text-sm text-muted-foreground">
        {t("dr.runOptions.counting")}
      </p>
    );
  }
  if (missing.list.length === 0) {
    return null;
  }
  return (
    <fieldset className="flex flex-col gap-2 rounded-md border border-warning px-3 py-2">
      <legend className="px-1 text-sm">
        {t("dr.runOptions.missing", { count: missing.list.length })}
      </legend>
      <p className="text-xs text-muted-foreground">{t("dr.runOptions.skipHint")}</p>
      <div className="flex max-h-48 flex-col gap-1.5 overflow-y-auto">
        {missing.list.map((vm, i) => (
          <Check
            key={vm}
            id={`${id}-${i}`}
            checked={missing.skip.includes(vm)}
            onChange={(on) => missing.toggle(vm, on)}
          >
            {t("dr.runOptions.skipVm", { vm })}
          </Check>
        ))}
      </div>
    </fieldset>
  );
}

/**
 * 복구 지점이 없는 VM과 건너뛸 VM(리뷰 2 웹 1). 목록은 고른 복구 지점 시각(`point` — 없으면 가장 새 사본)의 보고에서
 * 오고(`GET /report?point_in_time=`), 서버가 400 `dr_missing_recovery_points`로 알려 준 VM이 있으면 그 시각의 것으로 더한다
 * (서버 것이 늘 맞다). 시각이 바뀌면 고른 것과 서버 목록은 버린다 — 다른 시점의 목록은 동의 근거가 아니다. 서버는
 * `skip_vms`에 든 VM만 건너뛰므로 사용자가 보지 못한 VM은 건너뛰지 않는다(400으로 목록에 더해진다).
 */
function useMissing(plan: RecoveryPlan, report: DrReport | undefined, point: number | undefined) {
  const atPoint = useReport(point !== undefined ? plan.name : undefined, point);
  const [fromServer, setFromServer] = useState<{ point?: number; vms: string[] } | null>(null);
  const [picked, setPicked] = useState<{ point?: number; vms: string[] }>({ vms: [] });
  const base = point === undefined ? report : atPoint.data;
  const server = fromServer && fromServer.point === point ? fromServer.vms : [];
  const list = [...new Set([...protectionGaps(base?.protection).missing, ...server])].sort();
  const skip = picked.point === point ? picked.vms.filter((vm) => list.includes(vm)) : [];
  return {
    list,
    skip,
    /** 그 시각의 보고를 받는 중(서버 목록도 아직 없음) */
    loading: point !== undefined && !atPoint.data && !atPoint.error && server.length === 0,
    /** 그 시각 기준 보호 상태(장애 조치 확인의 RPO·검증 실패 줄) */
    protection: base?.protection,
    toggle(vm: string, on: boolean) {
      setPicked({ point, vms: on ? [...skip, vm] : skip.filter((v) => v !== vm) });
    },
    /** 400의 `details.vms`(보낸 `skip_vms`는 빼고 온다 — 고른 것도 목록에 남긴다) */
    fromError(vms: string[]) {
      setFromServer({ point, vms: [...new Set([...vms, ...skip])] });
    },
  };
}

/**
 * 시험 장애 조치(설계 §3.2, 리뷰 1): 운영 쪽을 건드리지 않고 격리망에서 되살린다. 복구 지점이 없는 VM이 있으면 VM마다
 * "건너뛰기"(`skip_vms` — 리뷰 2, 고른 복구 지점 시각 기준의 목록), 시험망이 밖과 닿을 수 있으면(브리지·업링크·라우터 — 서버 `dr_test_network_not_isolated`와 같은 판단,
 * 또는 서버가 알려 준 네트워크) "알고 진행"(`allow_routed_test_networks`), 고급으로 복구 지점 시각(`point_in_time`).
 */
export function TestDialog({
  plan,
  report,
  onClose,
  onStarted,
}: {
  plan: RecoveryPlan;
  report: DrReport | undefined;
  onClose(): void;
  onStarted(run: DrRun): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const id = useId();
  const networks = useList("network");
  const routed = useRouted();
  const [when, setWhen] = useState("");
  const [whenError, setWhenError] = useState<string | undefined>();
  const missing = useMissing(plan, report, localToSecs(when));
  const [serverRouted, setServerRouted] = useState<string[] | null>(null);
  const [allowRouted, setAllowRouted] = useState(false);
  const draft = toDraft(plan.name, plan.spec);
  const issues = testNetworkIssues(
    draft,
    networks.data,
    new Set((routed.data ?? []).map((r) => r.network)),
  );
  const notIsolated =
    serverRouted ??
    [
      ...new Set(
        Object.entries(issues)
          .filter(([, issue]) => issue !== "failover")
          .map(([i]) => draft.testNetworks[Number(i)]!.to.trim()),
      ),
    ].sort();
  return (
    <FormDialog
      title={t("dr.test.title", { name: plan.name })}
      description={t("dr.test.description")}
      submitLabel={t("dr.test.submit")}
      onClose={onClose}
      onSubmit={async () => {
        const point = localToSecs(when);
        if (when.trim() && point === undefined) {
          setWhenError(t("dr.invalid.pointInTime"));
          return false;
        }
        setWhenError(undefined);
        const body: TestRequest = {
          ...(missing.skip.length > 0 && { skip_vms: missing.skip }),
          ...(allowRouted && notIsolated.length > 0 && { allow_routed_test_networks: true }),
          ...(point !== undefined && { point_in_time: point }),
        };
        try {
          const run = await call(
            api.POST("/dr-plans/{key}/actions/test", {
              params: { path: { key: plan.id } },
              body,
            }),
          );
          toast.success(t("dr.test.started", { name: plan.name }));
          onStarted(run);
        } catch (err) {
          if (err instanceof ApiError && err.code === "dr_missing_recovery_points") {
            missing.fromError(detailNames(err, "vms"));
          }
          if (err instanceof ApiError && err.code === "dr_test_network_not_isolated") {
            setServerRouted(detailNames(err, "networks"));
          }
          throw localized(t, err);
        } finally {
          await refreshDr(queryClient);
        }
      }}
    >
      <MissingPoints id={`${id}-missing`} missing={missing} />
      {notIsolated.length > 0 && (
        <div className="flex flex-col gap-2 rounded-md border border-warning px-3 py-2">
          <p className="text-sm">{t("dr.test.notIsolated", { networks: names(t, notIsolated) })}</p>
          <Check id={`${id}-routed`} checked={allowRouted} onChange={setAllowRouted}>
            {t("dr.test.allowRouted")}
          </Check>
        </div>
      )}
      <PointInTime id={`${id}-when`} value={when} error={whenError} onChange={setWhen} />
    </FormDialog>
  );
}

/**
 * 장애 조치 확인(설계 §3.2·§4): 모드(planned — 운영 쪽을 끄고 마지막 백업을 떠 온 뒤, disaster — 지금 있는 가장 새 사본으로
 * 곧장)를 기본값 없이 고르고 계획 이름을 그대로 쳐야 진행한다. 영향 목록은 VM·그룹 수, disaster면 이미 받은 보고의 복구 지점
 * 없는 VM·RPO를 넘은 VM(나이)·검증 실패로 앞 사본을 쓸 VM(고른 복구 지점 시각의 보고로), planned면 운영 쪽에서 끌 VM이다
 * (리뷰 1 웹 10). disaster는 데이터를 잃을 수 있다는 경고와 VM마다 "건너뛰기"(`skip_vms`), 고급으로 복구 지점 시각
 * (`point_in_time`)을 준다.
 * 도는 시험이 있으면 그 시험을 끊고 시작한다고 알린다(`preemptsTest` — 서버가 끊는다).
 */
export function FailoverDialog({
  plan,
  report,
  preemptsTest = false,
  onClose,
  onStarted,
}: {
  plan: RecoveryPlan;
  report: DrReport | undefined;
  preemptsTest?: boolean;
  onClose(): void;
  onStarted(run: DrRun): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const id = useId();
  const [mode, setMode] = useState<FailoverMode | null>(null);
  const [when, setWhen] = useState("");
  const [whenError, setWhenError] = useState<string | undefined>();
  // 복구 지점 시각은 disaster만 받는다(planned는 막 뜬 사본을 쓴다).
  const missing = useMissing(plan, report, mode === "disaster" ? localToSecs(when) : undefined);
  const vms = plan.spec.groups.flatMap((g) => g.vms.map((v) => v.vm));
  const gaps = protectionGaps(missing.protection);
  // planned는 운영 쪽에서 새 백업을 떠 오므로 지금 사본의 빈 곳은 상관없다.
  const fromCopies = mode !== "planned";
  const impact = [
    t("dr.failover.impactVms", { vms: vms.length, groups: plan.spec.groups.length }),
    ...(fromCopies && missing.list.length > 0
      ? [t("dr.failover.noPoint", { vms: names(t, missing.list) })]
      : []),
    ...(fromCopies && gaps.over.length > 0
      ? [
          t("dr.failover.overRpo", {
            vms: names(
              t,
              gaps.over.map((o) => `${o.vm} ${formatDuration(o.age)}`),
            ),
          }),
        ]
      : []),
    ...(fromCopies && gaps.verifyFailed.length > 0
      ? [t("dr.failover.verifyFailed", { vms: names(t, gaps.verifyFailed) })]
      : []),
    ...(mode === "planned"
      ? [t("dr.failover.willStop", { source: plan.spec.source, vms: names(t, vms) })]
      : []),
  ];
  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={t("dr.failover.title", { name: plan.name })}
      description={t("dr.failover.description", { source: plan.spec.source })}
      confirmLabel={t("dr.failover.confirm")}
      typeToConfirm={plan.name}
      impact={impact}
      confirmDisabled={mode === null}
      onConfirm={async () => {
        const point = mode === "disaster" ? localToSecs(when) : undefined;
        if (mode === "disaster" && when.trim() && point === undefined) {
          setWhenError(t("dr.invalid.pointInTime"));
          throw new Error(t("dr.invalid.pointInTime"));
        }
        setWhenError(undefined);
        try {
          const run = await call(
            api.POST("/dr-plans/{key}/actions/failover", {
              params: { path: { key: plan.id } },
              body: {
                mode: mode!,
                confirm: plan.name,
                ...(mode === "disaster" && missing.skip.length > 0 && { skip_vms: missing.skip }),
                ...(point !== undefined && { point_in_time: point }),
              },
            }),
          );
          toast.success(t("dr.failover.started", { name: plan.name }));
          onStarted(run);
        } catch (err) {
          if (err instanceof ApiError && err.code === "dr_missing_recovery_points") {
            missing.fromError(detailNames(err, "vms"));
          }
          throw localized(t, err);
        } finally {
          await refreshDr(queryClient);
        }
      }}
    >
      {preemptsTest && (
        <p
          role="status"
          className="rounded-md border border-warning px-3 py-2 text-sm text-warning-fg"
        >
          {t("dr.failover.preemptsTest")}
        </p>
      )}
      <div role="radiogroup" aria-labelledby={`${id}-mode`} className="flex flex-col gap-2">
        <span id={`${id}-mode`} className="text-sm font-medium">
          {t("dr.failover.mode")}
        </span>
        {MODES.map((m) => (
          <label key={m} className="flex items-start gap-2 text-sm">
            <input
              type="radio"
              name={`${id}-mode`}
              value={m}
              checked={mode === m}
              onChange={() => setMode(m)}
              className="mt-1"
            />
            <span className="flex flex-col">
              <span className="font-medium">{t(`dr.failover.modes.${m}`)}</span>
              <span className="text-xs text-muted-foreground">
                {t(`dr.failover.modeHint.${m}`)}
              </span>
            </span>
          </label>
        ))}
        {mode === null && (
          <span className="text-xs text-muted-foreground">{t("dr.failover.chooseMode")}</span>
        )}
      </div>
      {mode === "disaster" && (
        <>
          <p
            role="alert"
            className="rounded-md border border-danger bg-danger-soft px-3 py-2 text-sm text-danger-fg"
          >
            {t("dr.failover.disasterWarning")}
          </p>
          <MissingPoints id={`${id}-missing`} missing={missing} />
          <PointInTime id={`${id}-when`} value={when} error={whenError} onChange={setWhen} />
        </>
      )}
      <p className="text-xs text-muted-foreground">{t("dr.failover.after")}</p>
    </ConfirmDialog>
  );
}

/**
 * 재해 뒤 돌아온 운영 쪽 막기(리뷰 1, `POST /dr-plans/{key}/actions/fence-source`): 이 계획이 되살린 VM을 운영 쪽에서 강제로 끄고
 * 원하는 상태를 `stopped`로 둔다. 계획 이름을 쳐야 진행한다. 결과(`{results, pending}`)는 `onDone`으로 넘겨 계획 화면이 보인다.
 */
export function FenceDialog({
  plan,
  onClose,
  onDone,
}: {
  plan: RecoveryPlan;
  onClose(): void;
  onDone(result: FenceResponse): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const pending = (plan.status.source_fence_pending ?? []).map((f) => f.vm);
  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={t("dr.fence.title", { source: plan.spec.source })}
      description={t("dr.fence.description", { source: plan.spec.source })}
      confirmLabel={t("dr.fence.confirm")}
      typeToConfirm={plan.name}
      impact={[t("dr.fence.impact", { source: plan.spec.source, vms: names(t, pending) })]}
      onConfirm={async () => {
        try {
          const result = await call(
            api.POST("/dr-plans/{key}/actions/fence-source", {
              params: { path: { key: plan.id } },
            }),
          );
          const failed = result.results.filter((r) => !r.ok).length;
          if (failed > 0) {
            toast.error(t("dr.fence.partial", { count: failed }));
          } else {
            toast.success(t("dr.fence.done", { source: plan.spec.source }));
          }
          onDone(result);
        } catch (err) {
          throw localized(t, err, "dr.fence.errors");
        } finally {
          await refreshDr(queryClient);
        }
      }}
    />
  );
}

type KeepForm = Record<(typeof KEEP_UNITS)[number], string>;

/**
 * 되돌리기 준비(설계 §3.6): 장애 조치로 되살린 VM에 백업 작업 `dr-reprotect-<plan>`을 만들어 운영 쪽(원격)으로 복제한다.
 * 이미 있으면 이 값으로 바꾼다. 그래서 있는 작업의 원격·풀·시각·요일·보존 규칙을 미리 채워 그대로 보낸다(리뷰 1 웹 5 — 보내지
 * 않으면 서버가 "모두 둠"으로 덮는다). 이곳 백업 풀은 백업을 담는 풀에서 고른다.
 */
export function ReprotectDialog({ plan, onClose }: { plan: RecoveryPlan; onClose(): void }) {
  const { t } = useTranslation();
  const id = useId();
  const jobName = `dr-reprotect-${plan.name}`;
  const jobs = useList("backup-job");
  const pools = useList("storage-pool");
  const local = backupPools(pools.data ?? []).map((p) => p.name);
  const [target, setTarget] = useState<RemoteTarget>({ remote: plan.spec.source, pool: "" });
  const [backupPool, setBackupPool] = useState("");
  const [at, setAt] = useState("03:00");
  const [days, setDays] = useState<Weekday[]>([]);
  const [keep, setKeep] = useState<KeepForm>({ last: "7", daily: "", weekly: "", monthly: "" });
  const [filled, setFilled] = useState(false);
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 있는 작업은 목록을 받은 때 한 번 채운다(그 뒤에 고친 것을 덮지 않게).
  if (!filled && jobs.data) {
    setFilled(true);
    const job = jobs.data.find((j) => j.name === jobName);
    if (job) {
      setTarget({
        remote: job.spec.replicate?.remote ?? plan.spec.source,
        pool: job.spec.replicate?.pool ?? "",
      });
      setBackupPool(job.spec.pool ?? "");
      setAt(job.spec.schedule.at);
      setDays(job.spec.schedule.days ?? []);
      const k = job.spec.keep;
      setKeep({
        last: String(k?.last ?? 0),
        daily: String(k?.daily ?? 0),
        weekly: String(k?.weekly ?? 0),
        monthly: String(k?.monthly ?? 0),
      });
    }
  }
  const poolNames = [...new Set([backupPool, ...local])].filter(Boolean);
  return (
    <FormDialog
      title={t("dr.reprotect.title", { name: plan.name })}
      description={t("dr.reprotect.description", { job: jobName })}
      submitLabel={t("dr.reprotect.submit")}
      onClose={onClose}
      onSubmit={async () => {
        const found: Record<string, string> = {};
        if (!target.remote) {
          found.remote = "dr.invalid.required";
        }
        if (!target.pool) {
          found.pool = "dr.invalid.required";
        }
        if (!backupPool) {
          found.backupPool = "dr.invalid.required";
        }
        if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(at)) {
          found.at = "dr.invalid.time";
        }
        if (KEEP_UNITS.some((u) => !/^\d*$/.test(keep[u].trim()))) {
          found.keep = "backupJobs.errors.keep";
        }
        setErrors(found);
        if (Object.keys(found).length > 0) {
          return false;
        }
        const n = (u: (typeof KEEP_UNITS)[number]) => Number(keep[u].trim() || "0");
        try {
          await call(
            api.POST("/dr-plans/{key}/actions/reprotect", {
              params: { path: { key: plan.id } },
              body: {
                remote: target.remote,
                pool: target.pool,
                backup_pool: backupPool,
                schedule: { at, ...(days.length > 0 && { days }) },
                keep: {
                  last: n("last"),
                  daily: n("daily"),
                  weekly: n("weekly"),
                  monthly: n("monthly"),
                },
              },
            }),
          );
        } catch (err) {
          throw localized(t, err);
        }
        toast.success(t("dr.reprotect.done", { job: jobName }));
      }}
    >
      <RemoteTargetFields
        id={`${id}-target`}
        label={t("dr.reprotect.remote")}
        none={t("dr.edit.choose")}
        value={target}
        errors={{ remote: errors.remote, pool: errors.pool }}
        onChange={setTarget}
      />
      <Field
        id={`${id}-bpool`}
        label={t("dr.reprotect.backupPool")}
        help={t("dr.reprotect.backupPoolHint")}
        error={errors.backupPool}
      >
        {(a11y) => (
          <NativeSelect
            id={`${id}-bpool`}
            className="w-full"
            value={backupPool}
            onChange={(e) => setBackupPool(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">{t("dr.edit.choose")}</NativeSelectOption>
            {poolNames.map((n) => (
              <NativeSelectOption key={n} value={n}>
                {n}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor={`${id}-at`}>{t("dr.reprotect.at")}</Label>
        <Input
          id={`${id}-at`}
          className="w-28"
          value={at}
          aria-invalid={errors.at ? true : undefined}
          onChange={(e) => setAt(e.target.value)}
        />
        {errors.at && <p className="text-xs text-destructive">{t(errors.at)}</p>}
        {days.length > 0 && (
          <p className="text-xs text-muted-foreground">
            {t("dr.reprotect.days", {
              days: days.map((d) => t(`backupJobs.day.${d}`)).join(", "),
            })}
          </p>
        )}
      </div>
      <fieldset className="flex flex-col gap-1.5">
        <legend className="mb-1.5 text-sm font-medium">{t("backupJobs.retention")}</legend>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {KEEP_UNITS.map((unit) => (
            <div key={unit} className="flex flex-col gap-1.5">
              <Label htmlFor={`${id}-keep-${unit}`}>{t(`backupJobs.keepLabel.${unit}`)}</Label>
              <Input
                id={`${id}-keep-${unit}`}
                inputMode="numeric"
                placeholder="0"
                value={keep[unit]}
                aria-invalid={errors.keep ? true : undefined}
                onChange={(e) => setKeep({ ...keep, [unit]: e.target.value })}
              />
            </div>
          ))}
        </div>
        {errors.keep ? (
          <p className="text-xs text-destructive">{t(errors.keep)}</p>
        ) : (
          <p className="text-xs text-muted-foreground">{t("dr.reprotect.keepHint")}</p>
        )}
      </fieldset>
    </FormDialog>
  );
}
