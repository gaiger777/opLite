import { useTranslation } from "react-i18next";

import type { Vm } from "@/api/types";
import { StatusChip, type StatusTone } from "@/components/StatusChip";

type Phase = Vm["status"]["phase"];

export const PHASE_TONE: Record<Phase, StatusTone> = {
  stopped: "neutral",
  starting: "info",
  running: "success",
  stopping: "info",
  failed: "danger",
  migrating: "info",
};

/** 바뀌는 중인 단계 */
const BUSY: ReadonlySet<Phase> = new Set(["starting", "stopping", "migrating"]);

export function VmPhaseBadge({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const phase = vm.status.phase;
  return (
    <StatusChip tone={PHASE_TONE[phase]} busy={BUSY.has(phase)} data-testid="vm-phase">
      {t(`vms.phase.${phase}`)}
    </StatusChip>
  );
}
