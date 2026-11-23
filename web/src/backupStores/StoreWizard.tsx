import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Weekday } from "@/api/types";
import { backupPools } from "@/backups/backups";
import { WEEKDAYS } from "@/backups/jobs";
import { RecoveryKeyView } from "@/backupStores/RecoveryKey";
import {
  checkStoreStep,
  createBodyOf,
  emptyStoreForm,
  type EncryptionChoice,
  refreshStores,
  type StoreForm,
  type VerifyMode,
  WIZARD_STEPS,
  type WizardStep,
} from "@/backupStores/stores";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import { errorMessage } from "@/i18n/errors";
import { trackTask } from "@/tasks/tasks";

export const TEXTAREA =
  "w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 font-mono text-xs break-all outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30";

/** 요일 칸들(비면 매일) */
export function DaysField({
  id,
  legend,
  days,
  onChange,
}: {
  id: string;
  legend: string;
  days: Weekday[];
  onChange(days: Weekday[]): void;
}) {
  const { t } = useTranslation();
  return (
    <fieldset className="flex flex-col gap-1.5">
      <legend className="mb-1.5 text-sm font-medium">{legend}</legend>
      <div className="flex flex-wrap gap-3">
        {WEEKDAYS.map((day) => (
          <div key={day} className="flex items-center gap-1.5">
            <Checkbox
              id={`${id}-${day}`}
              checked={days.includes(day)}
              onCheckedChange={(on) =>
                onChange(on === true ? [...days, day] : days.filter((d) => d !== day))
              }
            />
            <Label htmlFor={`${id}-${day}`}>{t(`backupJobs.day.${day}`)}</Label>
          </div>
        ))}
      </div>
      <p className="text-xs text-muted-foreground">{t("backupJobs.daysHint")}</p>
    </fieldset>
  );
}

/** 검증 정책 칸(마법사 ③과 설정 대화상자가 함께 쓴다) */
export function VerifyFields({
  id,
  form,
  errors,
  onChange,
}: {
  id: string;
  form: Pick<
    StoreForm,
    "verify" | "verifyAt" | "verifyDays" | "reverifyDays" | "verifyMode" | "verifyNew"
  >;
  errors: Record<string, string>;
  onChange(part: Partial<StoreForm>): void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <div className="flex items-center gap-2">
        <Checkbox
          id={`${id}-on`}
          checked={form.verify}
          onCheckedChange={(on) => onChange({ verify: on === true })}
        />
        <Label htmlFor={`${id}-on`}>{t("backupStores.verifyScheduled")}</Label>
      </div>
      {form.verify && (
        <>
          <Field
            id={`${id}-at`}
            label={t("backupStores.verifyAt")}
            error={errors.verifyAt}
            hint={t("backupJobs.atHint")}
          >
            {(a11y) => (
              <Input
                id={`${id}-at`}
                placeholder="06:00"
                value={form.verifyAt}
                onChange={(e) => onChange({ verifyAt: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <DaysField
            id={`${id}-day`}
            legend={t("backupStores.verifyDays")}
            days={form.verifyDays}
            onChange={(verifyDays) => onChange({ verifyDays })}
          />
          <Field
            id={`${id}-reverify`}
            label={t("backupStores.reverifyDays")}
            error={errors.reverifyDays}
            hint={t("backupStores.reverifyHint")}
          >
            {(a11y) => (
              <Input
                id={`${id}-reverify`}
                inputMode="numeric"
                value={form.reverifyDays}
                onChange={(e) => onChange({ reverifyDays: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field id={`${id}-mode`} label={t("backupStores.verifyMode")}>
            <NativeSelect
              id={`${id}-mode`}
              className="w-full"
              value={form.verifyMode}
              onChange={(e) => onChange({ verifyMode: e.target.value as VerifyMode })}
            >
              {(["full", "metadata"] as const).map((mode) => (
                <NativeSelectOption key={mode} value={mode}>
                  {t(`backupStores.mode.${mode}`)}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          </Field>
          <div className="flex items-center gap-2">
            <Checkbox
              id={`${id}-new`}
              checked={form.verifyNew}
              onCheckedChange={(on) => onChange({ verifyNew: on === true })}
            />
            <Label htmlFor={`${id}-new`}>{t("backupStores.verifyNew")}</Label>
          </div>
        </>
      )}
    </>
  );
}

/**
 * 저장소 만들기 마법사(설계 §14 웹): ① 백엔드(백업 풀 / S3 끝점·버킷·자격 증명·CA, 읽기 전용 붙이기) ② 암호화(끔·새 키·
 * 가져오기 — 복구 키 또는 escrow 암호 문구) ③ 일정(GC·검증) → 만들기 ④ 새 키면 복구 키 화면 — 인쇄·복사, "저장했습니다"를
 * 확인하기 전에는 닫히지 않는다. 연결은 만든 뒤 초기화 Task와 서랍의 "연결 확인"이 본다(만들기 전에 확인할 API는 없다 —
 * 서버가 닿으면 만들기가 미리 확인해 400·409로 돌려준다).
 */
export function StoreWizard({ taken, onClose }: { taken: string[]; onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const pools = backupPools(useList("storage-pool").data ?? []);
  const [form, setForm] = useState<StoreForm>(() => emptyStoreForm(pools[0]?.name ?? ""));
  const [step, setStep] = useState<WizardStep>("backend");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [created, setCreated] = useState<{
    name: string;
    fingerprint?: string;
    key: string;
  } | null>(null);
  const [saved, setSaved] = useState(false);
  const [nag, setNag] = useState(false);
  const change = (part: Partial<StoreForm>) => setForm((f) => ({ ...f, ...part }));
  // 고르지 않았으면 첫 백업 풀(목록이 늦게 와도)
  const chosen = { ...form, pool: form.pool || pools[0]?.name || "" };
  const steps = WIZARD_STEPS.filter(
    (s) => s !== "key" || created || chosen.encryption === "generate",
  );
  const at = steps.indexOf(step);

  function next() {
    const found = checkStoreStep(chosen, step, taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    setStep(steps[at + 1]!);
  }

  async function create() {
    const found = checkStoreStep(chosen, "schedule", taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    setBusy(true);
    setError(null);
    let result;
    try {
      result = await call(api.POST("/backup-stores", { body: createBodyOf(chosen) }));
    } catch (err) {
      setError(errorMessage(t, err));
      setBusy(false);
      return;
    }
    // 저장소는 이미 있다 — 한 번만 오는 복구 키를 먼저 보이고(설계 §9.4), Task 따라가기·목록 다시 읽기는 그 뒤에 한다.
    setBusy(false);
    if (result.recovery_key) {
      setCreated({
        name: chosen.name,
        fingerprint: result.store.spec.encryption?.fingerprint,
        key: result.recovery_key,
      });
      setStep("key");
    } else {
      toast.success(t("backupStores.created", { name: chosen.name }));
      onClose();
    }
    trackTask(result.task, t("tasks.op.backup-store-init", { target: chosen.name }));
    void refreshStores(queryClient);
  }

  function close() {
    // 복구 키는 이때 한 번만 보인다(설계 §9.4) — 저장했다고 확인하기 전에는 닫지 않는다.
    if (created && !saved) {
      setNag(true);
      return;
    }
    if (!busy) {
      onClose();
    }
  }

  return (
    <Sheet open onOpenChange={(open) => !open && close()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-2xl">
        <SheetTitle className="px-4 pt-4 text-lg">{t("backupStores.createTitle")}</SheetTitle>
        <SheetDescription className="px-4 text-sm text-muted-foreground">
          {t("backupStores.createDescription")}
        </SheetDescription>
        <ol
          className="flex flex-wrap gap-x-4 gap-y-1 px-4 pt-2 text-sm"
          aria-label={t("backupStores.steps")}
        >
          {steps.map((s, i) => (
            <li
              key={s}
              aria-current={step === s ? "step" : undefined}
              className={step === s ? "font-medium text-foreground" : "text-muted-foreground"}
            >
              {i + 1}. {t(`backupStores.step.${s}`)}
            </li>
          ))}
        </ol>
        <form
          noValidate
          className="flex flex-col gap-4 p-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (step === "schedule") {
              void create();
            } else if (step !== "key") {
              next();
            }
          }}
        >
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
            {step === "backend" && (
              <BackendStep
                form={chosen}
                pools={pools.map((p) => p.name)}
                errors={errors}
                onChange={change}
              />
            )}
            {step === "encryption" && (
              <EncryptionStep form={chosen} errors={errors} onChange={change} />
            )}
            {step === "schedule" && (
              <ScheduleStep form={chosen} errors={errors} onChange={change} />
            )}
          </fieldset>
          {step === "key" && created && (
            <>
              <RecoveryKeyView
                store={created.name}
                fingerprint={created.fingerprint}
                recoveryKey={created.key}
                saved={saved}
                onSavedChange={(on) => {
                  setSaved(on);
                  setNag(false);
                }}
              />
              {nag && (
                <p role="alert" className="text-sm text-destructive">
                  {t("backupStores.key.confirmFirst")}
                </p>
              )}
            </>
          )}
          <div className="flex flex-wrap justify-end gap-2">
            {step === "key" ? (
              <Button
                type="button"
                disabled={!saved}
                onClick={() => {
                  toast.success(t("backupStores.created", { name: created!.name }));
                  onClose();
                }}
              >
                {t("backupStores.done")}
              </Button>
            ) : (
              <>
                <Button type="button" variant="outline" disabled={busy} onClick={close}>
                  {t("common.cancel")}
                </Button>
                {at > 0 && (
                  <Button
                    type="button"
                    variant="outline"
                    disabled={busy}
                    onClick={() => setStep(steps[at - 1]!)}
                  >
                    {t("backupStores.back")}
                  </Button>
                )}
                <Button type="submit" disabled={busy}>
                  {step === "schedule" ? t("common.create") : t("backupStores.next")}
                </Button>
              </>
            )}
          </div>
        </form>
      </SheetContent>
    </Sheet>
  );
}

interface StepProps {
  form: StoreForm;
  errors: Record<string, string>;
  onChange(part: Partial<StoreForm>): void;
}

/** ① 이름과 백엔드 */
function BackendStep({ form, pools, errors, onChange }: StepProps & { pools: string[] }) {
  const { t } = useTranslation();
  const text = (
    id: string,
    key: keyof StoreForm,
    label: string,
    extra: { hint?: string; placeholder?: string; type?: string } = {},
  ) => (
    <Field id={id} label={label} error={errors[key]} hint={extra.hint}>
      {(a11y) => (
        <Input
          id={id}
          type={extra.type}
          // 비밀번호 칸의 `off`는 Chromium이 무시하고 로그인 암호를 채운다 — 새 비밀로 두고, 나머지 칸은 암호 관리자가 보지 않게.
          autoComplete={extra.type === "password" ? "new-password" : "off"}
          {...(extra.type !== "password" && { "data-1p-ignore": "", "data-lpignore": "true" })}
          placeholder={extra.placeholder}
          value={form[key] as string}
          onChange={(e) => onChange({ [key]: e.target.value })}
          {...a11y}
        />
      )}
    </Field>
  );
  return (
    <>
      {text("store-name", "name", t("common.name"))}
      <Field id="store-backend" label={t("backupStores.backend")}>
        <NativeSelect
          id="store-backend"
          className="w-full"
          value={form.backend}
          onChange={(e) => onChange({ backend: e.target.value as StoreForm["backend"] })}
        >
          <NativeSelectOption value="pool">{t("backupStores.backendKind.pool")}</NativeSelectOption>
          <NativeSelectOption value="s3">{t("backupStores.backendKind.s3")}</NativeSelectOption>
        </NativeSelect>
      </Field>
      {form.backend === "pool" ? (
        pools.length === 0 ? (
          // 고를 풀이 없으면 "다음"이 막힌 까닭을 여기서 보인다.
          <p
            role={errors.pool ? "alert" : undefined}
            className={`text-sm ${errors.pool ? "text-destructive" : "text-muted-foreground"}`}
          >
            {t("backups.noPools")}
          </p>
        ) : (
          <Field
            id="store-pool"
            label={t("backups.pool")}
            error={errors.pool}
            hint={t("backupStores.poolHint")}
          >
            {(a11y) => (
              <NativeSelect
                id="store-pool"
                className="w-full"
                value={form.pool}
                onChange={(e) => onChange({ pool: e.target.value })}
                {...a11y}
              >
                {pools.map((p) => (
                  <NativeSelectOption key={p} value={p}>
                    {p}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
        )
      ) : (
        <>
          {text("store-endpoint", "endpoint", t("backupStores.endpoint"), {
            placeholder: "https://s3.example.com",
            hint: t("backupStores.endpointHint"),
          })}
          {text("store-region", "region", t("backupStores.region"))}
          {text("store-bucket", "bucket", t("backupStores.bucket"))}
          {text("store-prefix", "prefix", t("backupStores.prefix"), {
            hint: t("backupStores.prefixHint"),
          })}
          <div className="flex items-center gap-2">
            <Checkbox
              id="store-path-style"
              checked={form.pathStyle}
              onCheckedChange={(on) => onChange({ pathStyle: on === true })}
            />
            <Label htmlFor="store-path-style">{t("backupStores.pathStyle")}</Label>
          </div>
          {text("store-access-key", "accessKeyId", t("backupStores.accessKeyId"))}
          {text("store-secret-key", "secretKey", t("backupStores.secretKey"), {
            type: "password",
            hint: t("backupStores.secretHint"),
          })}
          <Field
            id="store-ca"
            label={t("backupStores.caPem")}
            error={errors.caPem}
            hint={t("backupStores.caHint")}
          >
            {(a11y) => (
              <textarea
                id="store-ca"
                rows={3}
                spellCheck={false}
                autoComplete="off"
                value={form.caPem}
                onChange={(e) => onChange({ caPem: e.target.value })}
                className={TEXTAREA}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      <div className="flex flex-col gap-1.5">
        <div className="flex items-center gap-2">
          <Checkbox
            id="store-read-only"
            checked={form.readOnly}
            aria-describedby="store-read-only-hint"
            onCheckedChange={(on) =>
              onChange({
                readOnly: on === true,
                // 읽기 전용은 있는 저장소를 연다 — 새 키를 만들지 않는다.
                ...(on === true && form.encryption === "generate" && { encryption: "none" }),
              })
            }
          />
          <Label htmlFor="store-read-only">{t("backupStores.readOnly")}</Label>
        </div>
        <p id="store-read-only-hint" className="text-xs text-muted-foreground">
          {t("backupStores.readOnlyHint")}
        </p>
      </div>
      {form.readOnly &&
        form.backend === "pool" &&
        text("store-id", "storeId", t("backupStores.storeId"), {
          hint: t("backupStores.storeIdHint"),
        })}
    </>
  );
}

/** ② 암호화 */
function EncryptionStep({ form, errors, onChange }: StepProps) {
  const { t } = useTranslation();
  const choices: EncryptionChoice[] = form.readOnly
    ? ["none", "recovery", "escrow"]
    : ["none", "generate", "recovery", "escrow"];
  const wraps =
    !form.readOnly && (form.encryption === "generate" || form.encryption === "recovery");
  return (
    <>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1.5 text-sm font-medium">{t("backupStores.encryption")}</legend>
        {choices.map((choice) => (
          <label key={choice} className="flex items-start gap-2 text-sm">
            <input
              type="radio"
              name="store-encryption"
              value={choice}
              checked={form.encryption === choice}
              onChange={() => onChange({ encryption: choice })}
              className="mt-1"
            />
            <span className="flex flex-col">
              <span className="font-medium">{t(`backupStores.encrypt.${choice}`)}</span>
              <span className="text-xs text-muted-foreground">
                {t(`backupStores.encryptHint.${choice}`)}
              </span>
            </span>
          </label>
        ))}
      </fieldset>
      {form.encryption === "recovery" && (
        <Field
          id="store-recovery-key"
          label={t("backupStores.key.recoveryKey")}
          error={errors.recoveryKey}
          hint={t("backupStores.recoveryKeyHint")}
        >
          {(a11y) => (
            <Input
              id="store-recovery-key"
              autoComplete="off"
              spellCheck={false}
              placeholder="OPK1-…"
              className="font-mono"
              value={form.recoveryKey}
              onChange={(e) => onChange({ recoveryKey: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      {form.encryption === "escrow" && (
        <Field
          id="store-unlock"
          label={t("backupStores.unlockPassphrase")}
          error={errors.unlockPassphrase}
          hint={t("backupStores.unlockHint")}
        >
          {(a11y) => (
            <Input
              id="store-unlock"
              type="password"
              autoComplete="new-password"
              value={form.unlockPassphrase}
              onChange={(e) => onChange({ unlockPassphrase: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      {wraps && (
        <Field
          id="store-escrow"
          label={t("backupStores.escrowPassphrase")}
          error={errors.escrowPassphrase}
          hint={t("backupStores.escrowHint")}
        >
          {(a11y) => (
            <Input
              id="store-escrow"
              type="password"
              autoComplete="new-password"
              value={form.escrowPassphrase}
              onChange={(e) => onChange({ escrowPassphrase: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
    </>
  );
}

/** ③ GC·검증 일정(UTC) */
function ScheduleStep({ form, errors, onChange }: StepProps) {
  const { t } = useTranslation();
  return (
    <>
      <p className="text-sm text-muted-foreground">{t("backupStores.scheduleHint")}</p>
      <Field
        id="store-gc-at"
        label={t("backupStores.gcAt")}
        error={errors.gcAt}
        hint={t("backupJobs.atHint")}
      >
        {(a11y) => (
          <Input
            id="store-gc-at"
            placeholder="05:00"
            value={form.gcAt}
            onChange={(e) => onChange({ gcAt: e.target.value })}
            {...a11y}
          />
        )}
      </Field>
      <DaysField
        id="store-gc-day"
        legend={t("backupStores.gcDays")}
        days={form.gcDays}
        onChange={(gcDays) => onChange({ gcDays })}
      />
      <VerifyFields id="store-verify" form={form} errors={errors} onChange={onChange} />
    </>
  );
}
