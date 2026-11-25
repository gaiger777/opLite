import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { usePermissions } from "@/auth/usePermissions";
import { FormDialog } from "@/components/FormDialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  applyPlan,
  blocked,
  noop,
  type PlanView,
  refreshApplySets,
  useApplySets,
} from "@/declarative/api";
import { PlanTable } from "@/declarative/PlanTable";
import {
  MANIFEST_KINDS,
  planRestorePoint,
  type PointRestoreRequest,
  refreshHistory,
} from "@/history/api";
import { errorMessage } from "@/i18n/errors";
import { trackTask } from "@/tasks/tasks";

/** `datetime-local` 입력의 지금 값(지역 시각, 분까지) */
function localNow(): string {
  const d = new Date(Date.now() - new Date().getTimezoneOffset() * 60_000);
  return d.toISOString().slice(0, 16);
}

function Check({
  id,
  checked,
  disabled,
  onChange,
  children,
}: {
  id: string;
  checked: boolean;
  disabled?: boolean;
  onChange(v: boolean): void;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center gap-2">
      <Checkbox
        id={id}
        checked={checked}
        disabled={disabled}
        onCheckedChange={(v) => onChange(v === true)}
      />
      <Label htmlFor={id} className="font-normal">
        {children}
      </Label>
    </div>
  );
}

/**
 * 여러 리소스를 한 시점으로 되돌린다(계획 73 §12.4): 시각(또는 revision)과 범위(폴더·적용 묶음·종류 — 하나 이상)를 고르고
 * "계획 보기"로 서버 계획(`dry_run`)을 본 뒤 그 계획을 적용한다. 그 뒤 만든 것 지우기는 따로 고르고, 그 가운데 데이터가 있는
 * VM·볼륨·백업 저장소는 "데이터를 잃어도 됨"을 또 따로 골라야 한다. 무엇이든 바꾸면 계획을 다시 본다.
 */
export function PointInTimeDialog({
  folder: initialFolder = "",
  applySet: initialSet = "",
  onClose,
}: {
  folder?: string;
  applySet?: string;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const perms = usePermissions();
  const sets = useApplySets(perms.reads("apply-set"));
  const [mode, setMode] = useState<"time" | "revision">("time");
  const [time, setTime] = useState(localNow);
  const [revision, setRevision] = useState("");
  const [folder, setFolder] = useState(initialFolder);
  const [applySet, setApplySet] = useState(initialSet);
  const [kind, setKind] = useState("");
  const [recreate, setRecreate] = useState(true);
  const [deleteCreated, setDeleteCreated] = useState(false);
  const [dataLoss, setDataLoss] = useState(false);
  const [reason, setReason] = useState("");
  // 본 계획과 그 계획을 세운 입력(`body()`의 JSON). 입력이 그 뒤 바뀌었으면 보이지도 적용하지도 않는다 — 계획 요청 중에
  // 입력을 바꾸면 늦게 온 응답이 지금 입력과 다른 계획이다.
  const [planned, setPlanned] = useState<{ view: PlanView; key: string } | null>(null);
  const [planning, setPlanning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 입력이 바뀌면 본 계획은 더 맞지 않는다.
  const edit =
    <T,>(set: (v: T) => void) =>
    (v: T) => {
      set(v);
      setPlanned(null);
    };
  const at: PointRestoreRequest["at"] | null =
    mode === "time"
      ? Number.isFinite(new Date(time).getTime())
        ? { time: String(new Date(time).getTime()) }
        : null
      : /^\d+$/.test(revision.trim())
        ? { revision: Number(revision.trim()) }
        : null;
  const scope = {
    ...(folder.trim() && { folder: folder.trim() }),
    ...(applySet.trim() && { apply_set: applySet.trim() }),
    ...(kind && { kinds: [kind] }),
  };
  const scoped = Object.keys(scope).length > 0;

  function body(): Omit<PointRestoreRequest, "dry_run"> {
    return {
      at: at!,
      scope,
      recreate_deleted: recreate,
      delete_created: deleteCreated,
      ...(deleteCreated && dataLoss && { allow_data_loss: true }),
      ...(reason.trim() && { reason: reason.trim() }),
    };
  }

  const current = at && scoped ? JSON.stringify(body()) : null;
  const plan = planned && planned.key === current ? planned.view : null;

  async function preview() {
    setPlanning(true);
    setError(null);
    const asked = body();
    try {
      const view = await planRestorePoint(asked);
      setPlanned({ view, key: JSON.stringify(asked) });
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setPlanning(false);
    }
  }

  async function submit() {
    if (!plan) {
      return false;
    }
    const task = await applyPlan(plan);
    trackTask(task, t("tasks.op.apply", { target: t("history.point.taskTarget") }));
    await Promise.all([refreshHistory(queryClient), refreshApplySets(queryClient)]);
  }

  return (
    <FormDialog
      title={t("history.point.title")}
      description={t("history.point.hint")}
      submitLabel={t("history.point.submit")}
      submitDisabled={!plan || blocked(plan) || noop(plan)}
      className="sm:max-w-3xl"
      onClose={onClose}
      onSubmit={submit}
    >
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1 text-sm font-medium">{t("history.point.at")}</legend>
        <div className="flex flex-wrap items-end gap-2">
          <NativeSelect
            aria-label={t("history.point.atKind")}
            value={mode}
            onChange={(e) => edit(setMode)(e.target.value as "time" | "revision")}
          >
            <NativeSelectOption value="time">{t("history.point.byTime")}</NativeSelectOption>
            <NativeSelectOption value="revision">
              {t("history.point.byRevision")}
            </NativeSelectOption>
          </NativeSelect>
          {mode === "time" ? (
            <Input
              type="datetime-local"
              aria-label={t("history.point.time")}
              className="w-auto"
              value={time}
              onChange={(e) => edit(setTime)(e.target.value)}
            />
          ) : (
            <Input
              inputMode="numeric"
              aria-label={t("history.point.revision")}
              className="w-40"
              value={revision}
              onChange={(e) => edit(setRevision)(e.target.value)}
            />
          )}
        </div>
      </fieldset>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1 text-sm font-medium">{t("history.point.scope")}</legend>
        <div className="grid gap-2 sm:grid-cols-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="pit-folder">{t("history.folder")}</Label>
            <Input
              id="pit-folder"
              placeholder="/prod/web"
              value={folder}
              onChange={(e) => edit(setFolder)(e.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="pit-set">{t("history.applySet")}</Label>
            {sets.data && sets.data.length > 0 ? (
              <NativeSelect
                id="pit-set"
                value={applySet}
                onChange={(e) => edit(setApplySet)(e.target.value)}
              >
                <NativeSelectOption value="">{t("history.any")}</NativeSelectOption>
                {sets.data.map((s) => (
                  <NativeSelectOption key={s.id} value={s.name}>
                    {s.name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            ) : (
              <Input
                id="pit-set"
                value={applySet}
                onChange={(e) => edit(setApplySet)(e.target.value)}
              />
            )}
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="pit-kind">{t("history.kind")}</Label>
            <NativeSelect
              id="pit-kind"
              value={kind}
              onChange={(e) => edit(setKind)(e.target.value)}
            >
              <NativeSelectOption value="">{t("history.any")}</NativeSelectOption>
              {MANIFEST_KINDS.map((k) => (
                <NativeSelectOption key={k} value={k}>
                  {k}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          </div>
        </div>
        {!scoped && (
          <p className="text-xs text-muted-foreground">{t("history.point.scopeNeeded")}</p>
        )}
      </fieldset>
      <div className="flex flex-col gap-2">
        <Check id="pit-recreate" checked={recreate} onChange={edit(setRecreate)}>
          {t("history.point.recreate")}
        </Check>
        <Check id="pit-delete" checked={deleteCreated} onChange={edit(setDeleteCreated)}>
          {t("history.point.deleteCreated")}
        </Check>
        <Check
          id="pit-data"
          checked={deleteCreated && dataLoss}
          disabled={!deleteCreated}
          onChange={edit(setDataLoss)}
        >
          {t("history.point.dataLoss")}
        </Check>
      </div>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="pit-reason">{t("history.reasonOptional")}</Label>
        <Input id="pit-reason" value={reason} onChange={(e) => edit(setReason)(e.target.value)} />
      </div>
      <div>
        <Button
          type="button"
          variant="outline"
          disabled={!at || !scoped || planning}
          onClick={() => void preview()}
        >
          {t("history.point.preview")}
        </Button>
      </div>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      {plan && (
        <div className="max-h-[40vh] overflow-y-auto">
          <PlanTable plan={plan} />
        </div>
      )}
    </FormDialog>
  );
}
