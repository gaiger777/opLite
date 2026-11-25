import { useTranslation } from "react-i18next";

import { StatusChip, type StatusTone } from "@/components/StatusChip";
import type { PlanView, StepAction, StepView } from "@/declarative/api";
import { ChangeTable } from "@/history/ChangeTable";

const TONE: Record<StepAction, StatusTone> = {
  create: "success",
  update: "info",
  unchanged: "neutral",
  delete: "danger",
  blocked: "warning",
};

function Step({ step }: { step: StepView }) {
  const { t } = useTranslation();
  const problems = step.problems ?? [];
  const diff = step.diff ?? [];
  return (
    <li className="flex flex-col gap-2 rounded-md border p-3">
      <div className="flex flex-wrap items-center gap-2">
        <StatusChip tone={TONE[step.action]}>{t(`declarative.action.${step.action}`)}</StatusChip>
        <span className="font-medium break-all">
          {step.kind} {step.name}
        </span>
        {step.folder && <span className="text-xs text-muted-foreground">{step.folder}</span>}
        {step.restart && (
          <span className="text-xs text-warning-fg">{t("declarative.plan.restart")}</span>
        )}
      </div>
      {problems.length > 0 && (
        <ul className="flex flex-col gap-1 text-xs">
          {problems.map((p) => (
            <li
              key={`${p.code}:${p.path ?? ""}:${p.message}`}
              className={p.level === "error" ? "text-danger-fg" : "text-warning-fg"}
            >
              <span className="font-mono">{p.code}</span> {p.message}
            </li>
          ))}
        </ul>
      )}
      {diff.length > 0 && (
        <ChangeTable
          changes={diff}
          label={t("declarative.plan.changesOf", { kind: step.kind, name: step.name })}
          // 되돌리기 계획은 spec 전체를 바꾼다(`replace`) — 바꾸는 단계에서 지운 칸은 기본값으로 돌아간다
          removed={step.action === "update" ? t("declarative.plan.toDefault") : undefined}
        />
      )}
    </li>
  );
}

/**
 * 서버가 세운 계획(되돌리기·시점 되돌리기 대화상자 — §12): 요약(만들기·바꾸기·지우기·그대로·막힘)과 단계마다 동작 칩·문제
 * (오류·경고)·바뀌는 칸. 그대로인 단계는 접어 둔다.
 */
export function PlanTable({ plan }: { plan: PlanView }) {
  const { t } = useTranslation();
  const s = plan.summary;
  const shown = plan.steps.filter((step) => step.action !== "unchanged");
  return (
    <div className="flex flex-col gap-3">
      <p role="status" className="text-sm">
        {t("declarative.plan.summary", {
          create: s.create,
          update: s.update,
          delete: s.delete,
          unchanged: s.unchanged,
          blocked: s.blocked,
        })}
      </p>
      {shown.length > 0 && (
        <ol aria-label={t("declarative.plan.steps")} className="flex flex-col gap-2">
          {shown.map((step) => (
            <Step key={step.n} step={step} />
          ))}
        </ol>
      )}
    </div>
  );
}
