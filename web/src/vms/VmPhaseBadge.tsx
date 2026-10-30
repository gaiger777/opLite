import { useTranslation } from "react-i18next";

import type { Vm } from "@/api/types";
import { Badge } from "@/components/ui/badge";

const VARIANT = {
  stopped: "outline",
  starting: "secondary",
  running: "default",
  stopping: "secondary",
  failed: "destructive",
  migrating: "secondary",
} as const;

export function VmPhaseBadge({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  return (
    <Badge variant={VARIANT[vm.status.phase]} data-testid="vm-phase">
      {t(`vms.phase.${vm.status.phase}`)}
    </Badge>
  );
}
