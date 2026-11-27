import { useQueryClient } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Weekday } from "@/api/types";
import { backupPools } from "@/backups/backups";
import { KEEP_UNITS } from "@/backups/jobs";
import { type RemoteTarget, RemoteTargetFields } from "@/backups/RemoteTargetFields";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  type DrReport,
  type DrRun,
  type FailoverMode,
  localized,
  type RecoveryPlan,
  refreshDr,
} from "@/dr/api";
import { formatDuration, protectionGaps } from "@/dr/model";

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

/**
 * 장애 조치 확인(설계 §3.2·§4): 모드(planned — 운영 쪽을 끄고 마지막 백업을 떠 온 뒤, disaster — 지금 있는 가장 새 사본으로
 * 곧장)를 기본값 없이 고르고 계획 이름을 그대로 쳐야 진행한다. 영향 목록은 VM·그룹 수, 이미 받은 보고의 복구 지점 없는 VM과
 * RPO를 넘은 VM(나이), planned면 운영 쪽에서 끌 VM이다(리뷰 1 웹 10). disaster는 데이터를 잃을 수 있다는 경고를 보인다.
 */
export function FailoverDialog({
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
  const [mode, setMode] = useState<FailoverMode | null>(null);
  const vms = plan.spec.groups.flatMap((g) => g.vms.map((v) => v.vm));
  const gaps = protectionGaps(report?.protection);
  const impact = [
    t("dr.failover.impactVms", { vms: vms.length, groups: plan.spec.groups.length }),
    ...(gaps.missing.length > 0 ? [t("dr.failover.noPoint", { vms: names(t, gaps.missing) })] : []),
    ...(gaps.over.length > 0
      ? [
          t("dr.failover.overRpo", {
            vms: names(
              t,
              gaps.over.map((o) => `${o.vm} ${formatDuration(o.age)}`),
            ),
          }),
        ]
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
        try {
          const run = await call(
            api.POST("/dr-plans/{key}/actions/failover", {
              params: { path: { key: plan.id } },
              body: { mode: mode!, confirm: plan.name },
            }),
          );
          toast.success(t("dr.failover.started", { name: plan.name }));
          onStarted(run);
        } catch (err) {
          throw localized(t, err);
        } finally {
          await refreshDr(queryClient);
        }
      }}
    >
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
        <p
          role="alert"
          className="rounded-md border border-danger bg-danger-soft px-3 py-2 text-sm text-danger-fg"
        >
          {t("dr.failover.disasterWarning")}
        </p>
      )}
      <p className="text-xs text-muted-foreground">{t("dr.failover.after")}</p>
    </ConfirmDialog>
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
        hint={t("dr.reprotect.backupPoolHint")}
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
