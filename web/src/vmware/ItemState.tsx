import { useTranslation } from "react-i18next";

import { StatusChip } from "@/components/StatusChip";
import type { MigrationItem } from "@/vmware/api";
import { stateBusy, stateTone, syncStopped } from "@/vmware/inventory";

/** 항목 상태 칩. 전환 중이면 지금 걸음, 되돌림이 실패했으면 그 걸음도, 동기화가 멈췄으면 그것도 보인다. */
export function ItemStateChip({ item }: { item: MigrationItem }) {
  const { t } = useTranslation();
  const { state, cutover_step: step } = item.status;
  const text =
    state === "cutting-over" && step
      ? t("vmware.stateStep", { step: t(`vmware.step.${step}`) })
      : state === "failed" && step
        ? t("vmware.rollbackFailedAt", { step: t(`vmware.step.${step}`) })
        : syncStopped(item)
          ? t("vmware.syncStopped")
          : t(`vmware.state.${state}`);
  return (
    <StatusChip tone={syncStopped(item) ? "warning" : stateTone(state)} busy={stateBusy(state)}>
      {text}
    </StatusChip>
  );
}

/** 진행 막대(0~1) */
export function Progress({ label, value }: { label: string; value: number }) {
  const pct = Math.round(Math.max(0, Math.min(1, value)) * 100);
  return (
    <div className="flex min-w-24 items-center gap-2">
      <div
        role="progressbar"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
        className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted"
      >
        <div className="h-full bg-brand" style={{ width: `${pct}%` }} />
      </div>
      <span className="text-xs text-muted-foreground tabular-nums">{pct}%</span>
    </div>
  );
}
