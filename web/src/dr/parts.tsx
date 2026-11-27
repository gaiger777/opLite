import { cn } from "cn";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { StatusChip } from "@/components/StatusChip";
import type { DrPhase, DrRunState, VmProtection } from "@/dr/api";
import { type DotState, PHASE_TONE, phaseDots, rpoCounts, RUN_TONE } from "@/dr/model";

/** 실행 상태 칩 */
export function RunStateChip({ state }: { state: DrRunState }) {
  const { t } = useTranslation();
  return (
    <StatusChip tone={RUN_TONE[state]} busy={state === "running"}>
      {t(`dr.runState.${state}`)}
    </StatusChip>
  );
}

/** VM 단계 칩 */
export function PhaseChip({ phase }: { phase: DrPhase }) {
  const { t } = useTranslation();
  const busy = PHASE_TONE[phase] === "info";
  return (
    <StatusChip tone={PHASE_TONE[phase]} busy={busy}>
      {t(`dr.phase.${phase}`)}
    </StatusChip>
  );
}

const DOT: Record<DotState, string> = {
  done: "bg-success border-success",
  current: "bg-info border-info animate-pulse motion-reduce:animate-none",
  failed: "bg-danger border-danger",
  todo: "bg-transparent border-muted-foreground/50",
};

/**
 * VM 하나의 단계 점(복원 → 시작 → 기다림 → 준비). 점은 꾸밈이고 뜻은 옆의 단계 칩이 글로 전한다(색만으로 전하지 않는다).
 * 점마다 단계 이름을 `title`로 단다.
 */
export function PhaseDots({ phase, dots }: { phase: DrPhase; dots: readonly DrPhase[] }) {
  const { t } = useTranslation();
  const states = phaseDots(phase, dots);
  return (
    <span aria-hidden className="inline-flex items-center gap-1">
      {dots.map((d, i) => (
        <span
          key={d}
          title={t(`dr.phase.${d}`)}
          data-state={states[i]}
          className={cn("size-2.5 rounded-full border", DOT[states[i]!])}
        />
      ))}
    </span>
  );
}

/** RPO 지킴 막대: 지키는 VM(초록)과 넘은 VM(빨강)의 비율, 옆에 `ok/전체` 글 */
export function RpoBar({ protection }: { protection: readonly VmProtection[] | undefined }) {
  const { t } = useTranslation();
  const { ok, bad, total } = rpoCounts(protection);
  if (total === 0) {
    return <span className="text-xs text-muted-foreground">—</span>;
  }
  const label = t("dr.rpoMeter", { ok, total });
  return (
    <span className="flex min-w-28 items-center gap-2">
      <span
        role="meter"
        aria-label={t("dr.rpoMeterLabel")}
        aria-valuemin={0}
        aria-valuemax={total}
        aria-valuenow={ok}
        aria-valuetext={label}
        className="flex h-2 w-20 overflow-hidden rounded-full bg-muted"
      >
        <span className="h-full bg-success" style={{ width: `${(ok / total) * 100}%` }} />
        <span className="h-full bg-danger" style={{ width: `${(bad / total) * 100}%` }} />
      </span>
      <span className={cn("text-xs tabular-nums", bad > 0 && "font-medium text-danger-fg")}>
        {label}
      </span>
    </span>
  );
}

/** 1초마다 바뀌는 지금 시각(유닉스 초). `active`가 거짓이면 멈춘다. */
export function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    if (!active) {
      return;
    }
    const timer = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    return () => clearInterval(timer);
  }, [active]);
  return now;
}
