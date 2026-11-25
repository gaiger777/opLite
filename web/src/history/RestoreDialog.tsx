import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { ApiError } from "@/api/client";
import { FormDialog } from "@/components/FormDialog";
import { Button } from "@/components/ui/button";
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
  const plan = useQuery({
    queryKey: ["history-restore-plan", entry.kind, entry.id, entry.revision, asName],
    queryFn: () => planRestoreOne(entry, asName ? { name: asName } : {}),
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
      const again = await planRestoreOne(entry, {
        reason: reason.trim(),
        ...(asName ? { name: asName } : {}),
      });
      if (!sameSteps(again, view)) {
        setShown(again);
        throw new Error(t("history.restore.changed"));
      }
      target = again;
    }
    const task = await applyPlan(target);
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
