import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes, formatTime } from "@/lib/format";
import type { MigrationItem } from "@/vmware/api";
import { canCancel, canCutover, formatDuration, rollbackFailed } from "@/vmware/inventory";
import { ItemStateChip, Progress } from "@/vmware/ItemState";

/** 항목에 할 수 있는 동작(권한과 상태를 본 뒤) */
export interface ItemActions {
  cutover?(): void;
  revert?(): void;
  finalize?(): void;
  retry?(): void;
  cancel?(): void;
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[max-content_1fr] gap-x-4 text-sm">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

/**
 * 이전 항목 한 줄의 서랍(계획 72 §13): 원본(호스트·전원·Tools·IP), 디스크별 진행, 최근 사건, 경고·오류와 동작(전환·되돌리기·
 * 마무리·다시 하기·취소).
 */
export function VmDrawer({
  item,
  actions,
  onClose,
}: {
  item: MigrationItem;
  actions: ItemActions;
  onClose(): void;
}) {
  const { t, i18n } = useTranslation();
  const vms = useList("vm");
  const status = item.status;
  // 빈 목록은 서버가 보내지 않는다.
  const disks = status.disks ?? [];
  const warnings = status.warnings ?? [];
  const history = status.history ?? [];
  const source = status.source;
  const target = status.target_vm ? vms.data?.find((v) => v.id === status.target_vm) : undefined;
  const time = (s: number) => formatTime(new Date(s * 1000), i18n.language);
  const state = status.state;
  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-2xl">
        <SheetTitle className="px-4 pt-4 text-lg">{item.spec.target_name}</SheetTitle>
        <SheetDescription className="px-4">
          {t("vmware.drawer.description", { source: source?.vm.name ?? item.spec.source_ref })}
        </SheetDescription>
        <div className="flex flex-col gap-6 px-4 pb-4">
          <div className="flex flex-wrap gap-2">
            {actions.cutover && canCutover(item) && (
              <Button size="sm" onClick={actions.cutover}>
                {t("vmware.action.cutover")}
              </Button>
            )}
            {actions.revert && state === "done" && (
              <Button size="sm" variant="outline" onClick={actions.revert}>
                {t("vmware.action.revert")}
              </Button>
            )}
            {actions.finalize && state === "done" && (
              <Button size="sm" variant="outline" onClick={actions.finalize}>
                {t("vmware.action.finalize")}
              </Button>
            )}
            {actions.retry && (
              <Button size="sm" variant="outline" onClick={actions.retry}>
                {t(rollbackFailed(item) ? "vmware.action.retryRollback" : "vmware.action.retry")}
              </Button>
            )}
            {actions.cancel && canCancel(item) && (
              <Button size="sm" variant="outline" onClick={actions.cancel}>
                {t("vmware.action.cancel")}
              </Button>
            )}
          </div>
          <dl className="flex flex-col gap-2">
            <Row label={t("common.state")}>
              <ItemStateChip item={item} />
            </Row>
            <Row label={t("vmware.drawer.rounds")}>{status.rounds ?? 0}</Row>
            {status.estimate_s !== undefined && (
              <Row label={t("vmware.drawer.estimate")}>{formatDuration(status.estimate_s)}</Row>
            )}
            {status.cutover_requested_at !== undefined && (
              <Row label={t("vmware.drawer.cutoverRequested")}>
                {time(status.cutover_requested_at)}
              </Row>
            )}
            {status.source_stopped_at !== undefined && (
              <Row label={t("vmware.drawer.sourceStopped")}>{time(status.source_stopped_at)}</Row>
            )}
            {status.downtime_s !== undefined && (
              <Row label={t("vmware.drawer.downtime")}>{formatDuration(status.downtime_s)}</Row>
            )}
            {status.last_sync_at !== undefined && (
              <Row label={t("vmware.drawer.lastSync")}>{time(status.last_sync_at)}</Row>
            )}
            {status.next_sync_at !== undefined && (
              <Row label={t("vmware.drawer.nextSync")}>{time(status.next_sync_at)}</Row>
            )}
            {source?.host && <Row label={t("vmware.drawer.host")}>{source.host}</Row>}
            {source?.power && (
              <Row label={t("vmware.drawer.sourcePower")}>{t(`vmware.power.${source.power}`)}</Row>
            )}
            {source?.tools && <Row label={t("vmware.picker.tools")}>{source.tools}</Row>}
            {source?.guest_ips && Object.keys(source.guest_ips).length > 0 && (
              <Row label={t("vmware.drawer.ips")}>
                {Object.entries(source.guest_ips)
                  .map(([mac, ips]) => `${mac}: ${ips.join(", ")}`)
                  .join("; ")}
              </Row>
            )}
            {status.target_vm && (
              <Row label={t("vmware.drawer.target")}>
                {target ? (
                  <Link
                    to={`/vms/${encodeURIComponent(target.name)}`}
                    className="underline-offset-4 hover:underline"
                  >
                    {target.name}
                  </Link>
                ) : (
                  item.spec.target_name
                )}
              </Row>
            )}
            {(status.source_changes?.nics_disconnected?.length ?? 0) > 0 && (
              <Row label={t("vmware.drawer.sourceNics")}>{t("vmware.drawer.nicsDisconnected")}</Row>
            )}
          </dl>
          {status.error && (
            <div role="alert" className="flex flex-col gap-1 text-sm text-destructive">
              {rollbackFailed(item) && (
                <p className="font-medium">{t("vmware.drawer.rollbackFailed")}</p>
              )}
              <p className="break-words whitespace-pre-wrap">{status.error}</p>
            </div>
          )}
          {warnings.length > 0 && (
            <div className="flex flex-col gap-1">
              <h3 className="text-sm font-medium">{t("vmware.drawer.warnings")}</h3>
              <ul className="list-disc pl-5 text-sm">
                {warnings.map((w) => (
                  <li key={w} className="break-words">
                    {w}
                  </li>
                ))}
              </ul>
            </div>
          )}
          {disks.length > 0 && (
            <div className="flex flex-col gap-1">
              <h3 className="text-sm font-medium">{t("vmware.drawer.disks")}</h3>
              <div className="overflow-x-auto">
                <Table aria-label={t("vmware.drawer.disks")}>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t("vmware.drawer.file")}</TableHead>
                      <TableHead>{t("vmware.drawer.capacity")}</TableHead>
                      <TableHead>{t("vmware.drawer.copied")}</TableHead>
                      <TableHead>{t("vmware.drawer.lastDelta")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {disks.map((d) => (
                      <TableRow key={d.key}>
                        <TableCell className="font-mono text-xs break-all whitespace-normal">
                          {d.file}
                        </TableCell>
                        <TableCell>{formatBytes(d.capacity_bytes)}</TableCell>
                        <TableCell>
                          <Progress
                            label={t("vmware.drawer.copiedOf", { file: d.file })}
                            value={
                              state === "copying"
                                ? Math.min(1, (d.copied_bytes ?? 0) / Math.max(1, d.capacity_bytes))
                                : (d.rounds ?? 0) > 0
                                  ? 1
                                  : 0
                            }
                          />
                        </TableCell>
                        <TableCell>
                          {d.last_delta_bytes !== undefined ? formatBytes(d.last_delta_bytes) : "—"}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            </div>
          )}
          {history.length > 0 && (
            <div className="flex flex-col gap-1">
              <h3 className="text-sm font-medium">{t("vmware.drawer.history")}</h3>
              <ol className="flex flex-col gap-1 text-sm">
                {[...history].reverse().map((h, i) => (
                  <li key={i} className="flex flex-wrap gap-x-3">
                    <span className="text-xs text-muted-foreground tabular-nums">{time(h.at)}</span>
                    <span className="break-words">{h.event}</span>
                  </li>
                ))}
              </ol>
            </div>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}
