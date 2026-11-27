import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { type DrRun, type FailoverMode, localized, type RecoveryPlan, refreshDr } from "@/dr/api";
import { useRemotes } from "@/remotes/remotes";

const MODES: readonly FailoverMode[] = ["planned", "disaster"];

/**
 * 장애 조치 확인(설계 §3.2·§4): 모드(planned — 운영 쪽을 끄고 마지막 백업을 떠 온 뒤, disaster — 지금 있는 가장 새 사본으로
 * 곧장)를 고르고 계획 이름을 그대로 쳐야 진행한다. disaster는 데이터를 잃을 수 있다는 경고를 보인다.
 */
export function FailoverDialog({
  plan,
  onClose,
  onStarted,
}: {
  plan: RecoveryPlan;
  onClose(): void;
  onStarted(run: DrRun): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const id = useId();
  const [mode, setMode] = useState<FailoverMode>("planned");
  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={t("dr.failover.title", { name: plan.name })}
      description={t("dr.failover.description", { source: plan.spec.source })}
      confirmLabel={t("dr.failover.confirm")}
      typeToConfirm={plan.name}
      onConfirm={async () => {
        try {
          const run = await call(
            api.POST("/dr-plans/{key}/actions/failover", {
              params: { path: { key: plan.id } },
              body: { mode, confirm: plan.name },
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

/**
 * 되돌리기 준비(설계 §3.6): 장애 조치로 되살린 VM에 백업 작업 `dr-reprotect-<plan>`을 만들어 운영 쪽(원격)으로 복제한다.
 * 이미 있으면 이 값으로 바꾼다.
 */
export function ReprotectDialog({ plan, onClose }: { plan: RecoveryPlan; onClose(): void }) {
  const { t } = useTranslation();
  const id = useId();
  const remotes = useRemotes();
  const [remote, setRemote] = useState(plan.spec.source);
  const [pool, setPool] = useState("");
  const [backupPool, setBackupPool] = useState("");
  const [at, setAt] = useState("03:00");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const names = [...new Set([remote, ...(remotes.data ?? []).map((r) => r.name)])].filter(Boolean);
  return (
    <FormDialog
      title={t("dr.reprotect.title", { name: plan.name })}
      description={t("dr.reprotect.description", { job: `dr-reprotect-${plan.name}` })}
      submitLabel={t("dr.reprotect.submit")}
      onClose={onClose}
      onSubmit={async () => {
        const found: Record<string, string> = {};
        if (!remote) {
          found.remote = "dr.invalid.required";
        }
        if (!pool.trim()) {
          found.pool = "dr.invalid.required";
        }
        if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(at)) {
          found.at = "dr.invalid.time";
        }
        setErrors(found);
        if (Object.keys(found).length > 0) {
          return false;
        }
        try {
          await call(
            api.POST("/dr-plans/{key}/actions/reprotect", {
              params: { path: { key: plan.id } },
              body: {
                remote,
                pool: pool.trim(),
                backup_pool: backupPool.trim() || undefined,
                schedule: { at },
              },
            }),
          );
        } catch (err) {
          throw localized(t, err);
        }
        toast.success(t("dr.reprotect.done", { job: `dr-reprotect-${plan.name}` }));
      }}
    >
      <Field id={`${id}-remote`} label={t("dr.reprotect.remote")} error={errors.remote}>
        {(a11y) => (
          <NativeSelect
            id={`${id}-remote`}
            className="w-full"
            value={remote}
            onChange={(e) => setRemote(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">{t("dr.edit.choose")}</NativeSelectOption>
            {names.map((n) => (
              <NativeSelectOption key={n} value={n}>
                {n}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <Field
        id={`${id}-pool`}
        label={t("dr.reprotect.pool")}
        error={errors.pool}
        hint={t("dr.reprotect.poolHint")}
      >
        {(a11y) => (
          <Input
            id={`${id}-pool`}
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id={`${id}-bpool`}
        label={t("dr.reprotect.backupPool")}
        hint={t("dr.reprotect.backupPoolHint")}
      >
        {(a11y) => (
          <Input
            id={`${id}-bpool`}
            value={backupPool}
            onChange={(e) => setBackupPool(e.target.value)}
            {...a11y}
          />
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
      </div>
    </FormDialog>
  );
}
