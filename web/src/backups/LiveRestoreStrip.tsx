import { History } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import type { Vm } from "@/api/types";
import { useVmCan } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { StatusChip, type StatusTone } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { formatBytes } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

type Phase = components["schemas"]["LiveRestorePhase"];

const PHASE_TONE: Record<Phase, StatusTone> = {
  starting: "info",
  copying: "info",
  pivoting: "info",
  paused: "warning",
  stopped: "warning",
  failed: "danger",
};

/** 옮긴 비율(0~100). 전체 크기를 아직 모르면 undefined */
export function livePercent(done = 0, total = 0): number | undefined {
  return total > 0 ? Math.min(100, Math.floor((done / total) * 100)) : undefined;
}

/**
 * VM 머리 아래의 라이브 복원 띠(계획 68 §14 웹): 단계·진행 막대·멈춘 까닭과 취소. 취소는 VM을 끄고 VM·대상 볼륨·오버레이를
 * 지운다 — 게스트가 쓴 것을 버리므로 이름을 쳐서 확인한다(새 VM의 `vm.delete`). 막힌 동작(정지·지우기·옮기기·백업)은
 * 그 자리에서 까닭과 함께 막힌다.
 */
export function LiveRestoreStrip({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const can = useVmCan(vm);
  const backups = useList("backup");
  const [cancelling, setCancelling] = useState(false);
  const live = vm.status.live_restore;
  if (!live) {
    return null;
  }
  const backup = backups.data?.find((b) => b.id === live.backup);
  const percent = livePercent(live.done_bytes, live.total_bytes);
  const busy = live.phase === "starting" || live.phase === "copying" || live.phase === "pivoting";
  return (
    <section
      aria-label={t("liveRestore.title")}
      className="flex flex-col gap-2 rounded-lg border border-info/40 bg-info-soft/40 p-3"
    >
      <div className="flex flex-wrap items-center gap-2">
        <History className="size-4" aria-hidden />
        <span className="font-medium">{t("liveRestore.title")}</span>
        <StatusChip tone={PHASE_TONE[live.phase]} busy={busy}>
          {live.cancel ? t("liveRestore.cancelling") : t(`liveRestore.phase.${live.phase}`)}
        </StatusChip>
        <span className="text-sm text-muted-foreground">
          {t("liveRestore.from", { backup: backup?.name ?? live.backup })}
        </span>
        {can("vm.delete") && !live.cancel && (
          <Button
            variant="outline"
            size="sm"
            className="ml-auto"
            onClick={() => setCancelling(true)}
          >
            {t("liveRestore.cancel")}
          </Button>
        )}
      </div>
      <div className="flex items-center gap-3">
        <div
          role="progressbar"
          aria-label={t("liveRestore.progress")}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent}
          className="h-2 flex-1 overflow-hidden rounded-full bg-muted"
        >
          <div className="h-full bg-info" style={{ width: `${percent ?? 0}%` }} />
        </div>
        <span className="text-sm whitespace-nowrap tabular-nums">
          {percent === undefined
            ? t("liveRestore.waiting")
            : t("liveRestore.copied", {
                percent,
                done: formatBytes(live.done_bytes ?? 0),
                total: formatBytes(live.total_bytes ?? 0),
              })}
        </span>
      </div>
      {live.error && (
        <p role="status" className="text-sm text-warning-fg">
          {t(live.phase === "failed" ? "liveRestore.failedBecause" : "liveRestore.pausedBecause", {
            error: live.error,
          })}
        </p>
      )}
      <p className="text-xs text-muted-foreground">{t("liveRestore.hint")}</p>
      <ConfirmDialog
        open={cancelling}
        onOpenChange={setCancelling}
        title={t("liveRestore.cancelTitle", { name: vm.name })}
        description={t("liveRestore.cancelDescription")}
        confirmLabel={t("liveRestore.cancelConfirm")}
        typeToConfirm={vm.name}
        onConfirm={async () => {
          const task = await call(
            api.POST("/vms/{key}/live-restore/cancel", {
              params: { path: { key: vm.id } },
              body: { discard: true },
            }),
          );
          trackTask(task, t("liveRestore.op.cancel", { name: vm.name }));
        }}
      />
    </section>
  );
}
