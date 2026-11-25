import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { ApiError } from "@/api/client";
import { FormDialog } from "@/components/FormDialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { applyPlan, blocked, noop, type PlanView, refreshApplySets } from "@/declarative/api";
import { PlanTable } from "@/declarative/PlanTable";
import { type HistoryEntry, planRestoreOne, refreshHistory } from "@/history/api";
import { errorMessage } from "@/i18n/errors";
import { trackTask } from "@/tasks/tasks";

/** 같은 계획인지(사유를 넣어 다시 세운 계획이 보인 것과 같아야 곧 적용한다) */
function sameSteps(a: PlanView, b: PlanView) {
  const key = (p: PlanView) => JSON.stringify(p.steps.map((s) => [s.n, s.action, s.diff ?? []]));
  return key(a) === key(b);
}

/**
 * 막힘이 모두 다른 관리자(`managed_elsewhere`) 때문인지. 되돌리기는 적용 묶음이 없어 서버는 Terraform 객체만 이렇게 막는다
 * (`force_conflicts`로 넘겨받을 수 있다).
 */
export function onlyManagedElsewhere(plan: PlanView): boolean {
  const errors = plan.steps
    .filter((s) => s.action === "blocked")
    .flatMap((s) => (s.problems ?? []).filter((p) => p.level === "error"));
  return errors.length > 0 && errors.every((p) => p.code === "managed_elsewhere");
}

/** 적용할 때 계획을 다시 세워야 하는 오류(그사이 바뀜 412, 계획이 없음 404 — 만료·다른 노드) */
function replanNeeded(err: unknown): boolean {
  return err instanceof ApiError && (err.code === "plan_stale" || err.code === "plan_not_found");
}

/**
 * 리소스 하나를 이력 줄의 spec으로 되돌린다(계획 73 §12.2·§12.3): 열면 서버가 계획만 세우고(`dry_run`) 그 diff·경고(비밀은
 * 되돌리지 않음 …)를 보인다. 확인하면 그 계획을 그대로 적용한다(`POST /apply {plan}` — 그사이 바뀌었으면 412). 지운 리소스는
 * 같은 id로 살리고, 이름이 그새 쓰였으면(409 `name_taken`) 새 이름을 받아 다시 계획한다.
 */
export function RestoreDialog({ entry, onClose }: { entry: HistoryEntry; onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [reason, setReason] = useState("");
  // 이름이 겹쳐 새 이름으로 살릴 때: 입력 중인 값과 계획에 쓴 값
  const [name, setName] = useState("");
  const [asName, setAsName] = useState<string | undefined>(undefined);
  const [shown, setShown] = useState<PlanView | null>(null);
  // Terraform이 관리하는 객체를 넘겨받아 되돌린다(`force_conflicts`)
  const [force, setForce] = useState(false);
  const options = {
    ...(asName ? { name: asName } : {}),
    ...(force && { force_conflicts: true }),
  };
  const plan = useQuery({
    queryKey: ["history-restore-plan", entry.kind, entry.id, entry.revision, asName, force],
    queryFn: () => planRestoreOne(entry, options),
    retry: false,
    gcTime: 0,
  });
  const view = shown ?? plan.data;
  const nameTaken = plan.error instanceof ApiError && plan.error.code === "name_taken";
  const deleted = entry.op === "delete";

  async function submit() {
    if (!view) {
      return false;
    }
    let target = view;
    // 사유는 계획에 담긴다 — 사유를 적었으면 사유와 함께 다시 세워 같은 계획일 때만 적용한다.
    if (reason.trim()) {
      const again = await planRestoreOne(entry, { ...options, reason: reason.trim() });
      if (!sameSteps(again, view)) {
        setShown(again);
        throw new Error(t("history.restore.changed"));
      }
      target = again;
    }
    let task;
    try {
      task = await applyPlan(target);
    } catch (err) {
      // 같은 계획 id로 다시 보내면 늘 같은 오류다 — 새 계획을 세워 보이고 다시 확인하게 한다.
      if (!replanNeeded(err)) {
        throw err;
      }
      setShown(null);
      await plan.refetch();
      throw new Error(t("history.restore.stale"), { cause: err });
    }
    trackTask(task, t("tasks.op.apply", { target: entry.name }));
    await Promise.all([refreshHistory(queryClient), refreshApplySets(queryClient)]);
  }

  return (
    <FormDialog
      title={t(deleted ? "history.restore.recreateTitle" : "history.restore.title", {
        name: entry.name,
        revision: entry.revision,
      })}
      description={t(deleted ? "history.restore.recreateHint" : "history.restore.hint")}
      submitLabel={t(deleted ? "history.restore.recreate" : "history.restore.submit")}
      submitDisabled={!view || blocked(view) || noop(view)}
      className="sm:max-w-3xl"
      onClose={onClose}
      onSubmit={submit}
    >
      {nameTaken && (
        <div className="flex flex-col gap-2">
          <p role="alert" className="text-sm text-warning-fg">
            {t("history.restore.nameTaken", { name: entry.name })}
          </p>
          <div className="flex flex-wrap items-end gap-2">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="restore-name">{t("history.restore.newName")}</Label>
              <Input id="restore-name" value={name} onChange={(e) => setName(e.target.value)} />
            </div>
            <Button
              type="button"
              variant="outline"
              disabled={!name.trim()}
              onClick={() => {
                setShown(null);
                setAsName(name.trim());
              }}
            >
              {t("history.restore.preview")}
            </Button>
          </div>
        </div>
      )}
      {plan.isError && !nameTaken && (
        <p role="alert" className="text-sm text-destructive">
          {errorMessage(t, plan.error)}
        </p>
      )}
      {!view && plan.isPending && (
        <p role="status" className="text-sm text-muted-foreground">
          {t("history.restore.planning")}
        </p>
      )}
      {(force || (view && onlyManagedElsewhere(view))) && (
        <div className="flex flex-col gap-1 rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm text-warning-fg">
          <div className="flex items-center gap-2">
            <Checkbox
              id="restore-force"
              checked={force}
              onCheckedChange={(v) => {
                setShown(null);
                setForce(v === true);
              }}
            />
            <Label htmlFor="restore-force">{t("history.restore.force")}</Label>
          </div>
          <p>{t("history.restore.forceHint")}</p>
        </div>
      )}
      {view && (
        <div className="max-h-[50vh] overflow-y-auto">
          <PlanTable plan={view} />
          {noop(view) && (
            <p role="status" className="mt-2 text-sm text-muted-foreground">
              {t("history.restore.nothing")}
            </p>
          )}
        </div>
      )}
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="restore-reason">{t("history.reasonOptional")}</Label>
        <Input id="restore-reason" value={reason} onChange={(e) => setReason(e.target.value)} />
      </div>
    </FormDialog>
  );
}
