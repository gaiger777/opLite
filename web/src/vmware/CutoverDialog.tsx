import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/ConfirmDialog";
import { formatBytes } from "@/lib/format";
import { localized, type Migration, type MigrationItem, refreshVmware } from "@/vmware/api";
import { cutoverItem } from "@/vmware/api5";
import { formatDuration, lastDelta } from "@/vmware/inventory";

/**
 * 전환 확인(계획 72 §10.1·§13): VM마다 예상 내릴 시간(마지막 델타 ÷ 잰 속도)과 원본 처리(게스트 종료·전원 끄기, NIC 끊기)를
 * 보이고 고른 항목마다 전환을 요청한다. 하나라도 실패하면 그 까닭을 보이고 나머지는 그대로 간다.
 */
export function CutoverDialog({
  migration,
  items,
  onClose,
}: {
  migration: Migration;
  items: readonly MigrationItem[];
  onClose(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const cutover = migration.spec.cutover;
  const shutdown = cutover?.shutdown ?? "guest";
  const protect = cutover?.protect_source ?? true;
  const startAfter = migration.spec.rules?.start_after ?? true;
  const total = items.reduce((sum, i) => sum + (i.status.estimate_s ?? 0), 0);
  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={t("vmware.cutover.title", { count: items.length })}
      description={t("vmware.cutover.description")}
      confirmLabel={t("vmware.cutover.confirm")}
      onConfirm={async () => {
        const failed: string[] = [];
        for (const item of items) {
          try {
            await cutoverItem(item.id);
          } catch (err) {
            failed.push(`${item.name}: ${(localized(t, err) as Error).message}`);
          }
        }
        await refreshVmware(queryClient);
        if (failed.length === items.length) {
          throw new Error(failed.join("\n"));
        }
        if (failed.length > 0) {
          toast.error(failed.join("\n"));
        }
        toast.success(t("vmware.cutover.started", { count: items.length - failed.length }));
      }}
    >
      <ul className="flex flex-col gap-1 text-sm">
        {items.map((item) => {
          const delta = lastDelta(item);
          return (
            <li key={item.id} className="flex flex-wrap justify-between gap-x-4">
              <span className="font-medium">{item.spec.target_name}</span>
              <span className="text-muted-foreground">
                {item.status.estimate_s !== undefined
                  ? t("vmware.cutover.estimate", { time: formatDuration(item.status.estimate_s) })
                  : t("vmware.cutover.noEstimate")}
                {delta !== undefined &&
                  ` · ${t("vmware.cutover.delta", { size: formatBytes(delta) })}`}
              </span>
            </li>
          );
        })}
      </ul>
      {items.length > 1 && total > 0 && (
        <p className="text-sm">{t("vmware.cutover.total", { time: formatDuration(total) })}</p>
      )}
      <ul className="list-disc pl-5 text-sm text-muted-foreground">
        <li>
          {t(`vmware.cutover.shutdown.${shutdown}`, {
            timeout: cutover?.shutdown_timeout_s ?? 600,
          })}
        </li>
        {protect && <li>{t("vmware.cutover.protect")}</li>}
        <li>{t(startAfter ? "vmware.cutover.startAfter" : "vmware.cutover.stayOff")}</li>
        <li>{t("vmware.cutover.rollback")}</li>
      </ul>
    </ConfirmDialog>
  );
}
