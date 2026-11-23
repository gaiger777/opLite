import { useTranslation } from "react-i18next";

import { formatUtc } from "@/backups/jobs";
import type { BackupStore } from "@/backupStores/stores";
import { StatusChip, type StatusTone } from "@/components/StatusChip";
import { formatBytes } from "@/lib/format";

const PHASE_TONE: Record<BackupStore["status"]["phase"], StatusTone> = {
  initializing: "info",
  ready: "success",
  failed: "danger",
};

/** 저장소 상태 칩과 그 아래 오류 */
export function StorePhase({ store }: { store: BackupStore }) {
  const { t } = useTranslation();
  const { phase, error } = store.status;
  return (
    <span className="flex flex-col items-start gap-0.5">
      <StatusChip tone={PHASE_TONE[phase]} busy={phase === "initializing"}>
        {t(`backupStores.phase.${phase}`)}
      </StatusChip>
      {error && (
        <span className="max-w-xs text-xs whitespace-normal break-words text-destructive">
          {error}
        </span>
      )}
    </span>
  );
}

/** 마지막 검증: 확인됨·깨짐(청크 수)·안 함 */
export function LastVerify({ store }: { store: BackupStore }) {
  const { t } = useTranslation();
  const last = store.status.last_verify;
  if (!last) {
    return <span className="text-muted-foreground">{t("backupStores.never")}</span>;
  }
  const failed = last.bad > 0 || !!last.error;
  return (
    <span className="flex flex-col items-start gap-0.5">
      <StatusChip tone={failed ? "danger" : "success"}>
        {failed ? t("backupStores.verifyFailed", { count: last.bad }) : t("backupStores.verifyOk")}
      </StatusChip>
      <span className="text-xs text-muted-foreground">{formatUtc(last.finished_at)}</span>
    </span>
  );
}

/** 마지막 GC(진행 중이면 단계) */
export function LastGc({ store }: { store: BackupStore }) {
  const { t } = useTranslation();
  const running = store.status.gc;
  if (running) {
    return (
      <StatusChip tone="info" busy>
        {t("backupStores.gcRunning", { phase: t(`backupStores.gcPhase.${running.phase}`) })}
      </StatusChip>
    );
  }
  const last = store.status.last_gc;
  if (!last) {
    return <span className="text-muted-foreground">{t("backupStores.never")}</span>;
  }
  return (
    <span className="flex flex-col items-start gap-0.5">
      <span>{formatUtc(last.finished_at)}</span>
      {last.error ? (
        <span className="max-w-xs text-xs whitespace-normal break-words text-destructive">
          {last.error}
        </span>
      ) : (
        <span className="text-xs text-muted-foreground">
          {t("backupStores.gcRemoved", {
            count: last.removed_chunks,
            size: formatBytes(last.removed_bytes),
          })}
        </span>
      )}
    </span>
  );
}
