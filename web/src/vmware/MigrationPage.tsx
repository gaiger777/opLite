import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Target } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes, formatTime } from "@/lib/format";
import {
  localized,
  type Migration,
  type MigrationItem,
  MIGRATIONS_KEY,
  type PreflightVm,
  refreshVmware,
  useItems,
  useSources,
  vmwareErrorText,
} from "@/vmware/api";
import { CutoverDialog } from "@/vmware/CutoverDialog";
import {
  canCancel,
  canCutover,
  canRetry,
  copyProgress,
  formatDuration,
  lastDelta,
} from "@/vmware/inventory";
import { ItemStateChip, Progress } from "@/vmware/ItemState";
import { CountsText, MigrationPhaseChip } from "@/vmware/MigrationsPage";
import { PreflightTable } from "@/vmware/PreflightTable";
import { RevertDialog } from "@/vmware/RevertDialog";
import { VmDrawer } from "@/vmware/VmDrawer";

/** 항목의 대상 폴더(덮어쓰기 > 폴더 규칙). 전환·되돌리기 권한을 그 폴더에서 본다(설계 §13). */
function targetOf(migration: Migration, item: MigrationItem): Target {
  const rule = migration.spec.rules?.folders;
  const folder =
    item.spec.overrides?.folder ??
    (rule ? (rule.kind === "flat" ? rule.into : rule.under) : undefined);
  return { type: "object", kind: "vm", folder: folder ?? null };
}

type Dialog =
  | { kind: "cutover"; items: MigrationItem[] }
  | { kind: "revert"; item: MigrationItem }
  | { kind: "finalize"; item: MigrationItem }
  | { kind: "cancel"; items: MigrationItem[] }
  | { kind: "cancel-plan" }
  | { kind: "delete" };

/**
 * 이전 계획 상세(계획 72 §13): VM 표(상태 칩·회차·복사 막대·마지막 델타·예상 내릴 시간·다음 동기화), 고른 VM 일괄 전환·취소,
 * 한 줄 서랍(디스크별 진행·사건·되돌리기·마무리). 계획 동작은 루트 `vm.create`, 전환·되돌리기·마무리는 대상 폴더의
 * `vm.create` + `vm.power`다. 항목은 watch 사건이 오면 다시 읽는다. 늦게 읽는 조각이다.
 */
export function MigrationPage() {
  const { t, i18n } = useTranslation();
  const { name = "" } = useParams();
  const perms = usePermissions();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const migrationQuery = useQuery({
    queryKey: [...MIGRATIONS_KEY, name],
    queryFn: () => call(api.GET("/vmware-migrations/{key}", { params: { path: { key: name } } })),
    retry: false,
  });
  const migration = migrationQuery.data;
  const items = useItems(migration?.id);
  const sources = useSources(perms.reads("vmware-source"));
  const vms = useList("vm");
  const [selected, setSelected] = useState<string[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [busy, setBusy] = useState(false);
  const [preflight, setPreflight] = useState<PreflightVm[] | null>(null);

  if (!migration) {
    return <ListPending error={migrationQuery.error} />;
  }
  const write = perms.has("vm.create");
  const rows = [...(items.data ?? [])].sort((a, b) =>
    a.spec.target_name.localeCompare(b.spec.target_name),
  );
  const chosen = rows.filter((i) => selected.includes(i.id));
  const opened = rows.find((i) => i.id === open);
  const phase = migration.status.phase;
  const source = sources.data?.find((s) => s.id === migration.spec.source);
  const switchable = (item: MigrationItem) => {
    const target = targetOf(migration, item);
    return perms.has("vm.create", target) && perms.has("vm.power", target);
  };
  const time = (s: number) => formatTime(new Date(s * 1000), i18n.language);

  async function act(action: "start" | "pause" | "resume") {
    setBusy(true);
    try {
      const params = { params: { path: { key: migration!.id } } };
      await call(
        action === "start"
          ? api.POST("/vmware-migrations/{key}/actions/start", params)
          : action === "pause"
            ? api.POST("/vmware-migrations/{key}/actions/pause", params)
            : api.POST("/vmware-migrations/{key}/actions/resume", params),
      );
      toast.success(t(`vmware.migration.done.${action}`, { name: migration!.name }));
    } catch (err) {
      toast.error(vmwareErrorText(t, err));
    } finally {
      setBusy(false);
      await refreshVmware(queryClient);
    }
  }

  async function check() {
    setBusy(true);
    try {
      const answer = await call(
        api.POST("/vmware-migrations/{key}/preflight", {
          params: { path: { key: migration!.id } },
          body: {},
        }),
      );
      setPreflight(answer.vms);
    } catch (err) {
      toast.error(vmwareErrorText(t, err));
    } finally {
      setBusy(false);
    }
  }

  async function retry(item: MigrationItem) {
    try {
      await call(api.POST("/vmware-vms/{key}/retry", { params: { path: { key: item.id } } }));
      toast.success(t("vmware.action.retried", { name: item.spec.target_name }));
    } catch (err) {
      toast.error(vmwareErrorText(t, err));
    }
    await refreshVmware(queryClient);
  }

  const actionsFor = (item: MigrationItem) =>
    write
      ? {
          ...(switchable(item)
            ? {
                cutover: () => setDialog({ kind: "cutover", items: [item] }),
                revert: () => setDialog({ kind: "revert", item }),
                finalize: () => setDialog({ kind: "finalize", item }),
              }
            : {}),
          ...(canRetry(phase, item) ? { retry: () => void retry(item) } : {}),
          cancel: () => setDialog({ kind: "cancel", items: [item] }),
        }
      : {};

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="vmware-migration-title"
        title={migration.name}
        description={
          <span className="flex flex-wrap items-center gap-2">
            <MigrationPhaseChip phase={phase} />
            <span>
              {t("vmware.migration.summary", {
                mode: t(`vmware.mode.${migration.spec.mode}`),
                source: source?.name ?? "—",
              })}
            </span>
            <CountsText migration={migration} />
          </span>
        }
        actions={
          write && (
            <>
              {phase === "draft" && (
                <>
                  <Button variant="outline" disabled={busy} onClick={() => void check()}>
                    {t("vmware.migration.check")}
                  </Button>
                  <Button disabled={busy} onClick={() => void act("start")}>
                    {t("vmware.migration.start")}
                  </Button>
                </>
              )}
              {phase === "running" && (
                <Button variant="outline" disabled={busy} onClick={() => void act("pause")}>
                  {t("vmware.migration.pause")}
                </Button>
              )}
              {phase === "paused" && (
                <Button disabled={busy} onClick={() => void act("resume")}>
                  {t("vmware.migration.resume")}
                </Button>
              )}
              {(phase === "running" || phase === "paused") && (
                <Button variant="outline" onClick={() => setDialog({ kind: "cancel-plan" })}>
                  {t("vmware.migration.cancel")}
                </Button>
              )}
              {(phase === "draft" || phase === "finished" || phase === "cancelled") && (
                <Button variant="outline" onClick={() => setDialog({ kind: "delete" })}>
                  {t("common.delete")}
                </Button>
              )}
            </>
          )
        }
      />
      {migration.status.error && (
        <p role="alert" className="text-sm text-destructive">
          {migration.status.error}
        </p>
      )}
      {preflight && <PreflightTable vms={preflight} />}
      {write && (
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-sm text-muted-foreground">
            {t("vmware.migration.selected", { count: chosen.length })}
          </span>
          <Button
            size="sm"
            disabled={chosen.length === 0 || !chosen.every((i) => canCutover(i) && switchable(i))}
            onClick={() => setDialog({ kind: "cutover", items: chosen })}
          >
            {t("vmware.migration.cutoverSelected")}
          </Button>
          <Button
            size="sm"
            variant="outline"
            disabled={chosen.length === 0 || !chosen.every(canCancel)}
            onClick={() => setDialog({ kind: "cancel", items: chosen })}
          >
            {t("vmware.migration.cancelSelected")}
          </Button>
        </div>
      )}
      <div className="overflow-x-auto">
        <Table aria-labelledby="vmware-migration-title">
          <TableHeader>
            <TableRow>
              {write && (
                <TableHead className="w-8">
                  <Checkbox
                    aria-label={t("vmware.migration.selectAll")}
                    checked={rows.length > 0 && selected.length === rows.length}
                    onCheckedChange={(c) => setSelected(c === true ? rows.map((r) => r.id) : [])}
                  />
                </TableHead>
              )}
              <TableHead>{t("vmware.migration.vm")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
              <TableHead>{t("vmware.migration.round")}</TableHead>
              <TableHead>{t("vmware.migration.copied")}</TableHead>
              <TableHead>{t("vmware.migration.lastDelta")}</TableHead>
              <TableHead>{t("vmware.migration.estimate")}</TableHead>
              <TableHead>{t("vmware.migration.nextSync")}</TableHead>
              <TableHead>{t("vmware.migration.target")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {items.data === undefined || rows.length === 0 ? (
              <TableRow>
                <TableCell colSpan={9} className="text-center text-muted-foreground">
                  {items.data === undefined
                    ? items.error
                      ? vmwareErrorText(t, items.error)
                      : t("common.loading")
                    : t("vmware.migration.noVms")}
                </TableCell>
              </TableRow>
            ) : (
              rows.map((item) => {
                const progress = copyProgress(item);
                const delta = lastDelta(item);
                const target = item.status.target_vm
                  ? vms.data?.find((v) => v.id === item.status.target_vm)
                  : undefined;
                return (
                  <TableRow key={item.id}>
                    {write && (
                      <TableCell>
                        <Checkbox
                          aria-label={t("vmware.migration.select", {
                            name: item.spec.target_name,
                          })}
                          checked={selected.includes(item.id)}
                          onCheckedChange={(c) =>
                            setSelected((s) =>
                              c === true ? [...s, item.id] : s.filter((id) => id !== item.id),
                            )
                          }
                        />
                      </TableCell>
                    )}
                    <TableCell className="font-medium">
                      <button
                        type="button"
                        className="underline-offset-4 hover:underline"
                        aria-label={t("vmware.migration.open", { name: item.spec.target_name })}
                        onClick={() => setOpen(item.id)}
                      >
                        {item.spec.target_name}
                      </button>
                      {item.status.source &&
                        item.status.source.vm.name !== item.spec.target_name && (
                          <div className="text-xs text-muted-foreground">
                            {item.status.source.vm.name}
                          </div>
                        )}
                    </TableCell>
                    <TableCell>
                      <ItemStateChip item={item} />
                      {item.status.error && (
                        <div
                          className="max-w-xs truncate text-xs text-destructive"
                          title={item.status.error}
                        >
                          {item.status.error}
                        </div>
                      )}
                    </TableCell>
                    <TableCell>{item.status.rounds ?? 0}</TableCell>
                    <TableCell>
                      {progress !== undefined ? (
                        <Progress
                          label={t("vmware.migration.copiedOf", { name: item.spec.target_name })}
                          value={progress}
                        />
                      ) : (
                        "—"
                      )}
                    </TableCell>
                    <TableCell>{delta !== undefined ? formatBytes(delta) : "—"}</TableCell>
                    <TableCell>
                      {item.status.downtime_s !== undefined
                        ? t("vmware.migration.downtime", {
                            time: formatDuration(item.status.downtime_s),
                          })
                        : item.status.estimate_s !== undefined
                          ? formatDuration(item.status.estimate_s)
                          : "—"}
                    </TableCell>
                    <TableCell>
                      {item.status.next_sync_at !== undefined
                        ? time(item.status.next_sync_at)
                        : "—"}
                    </TableCell>
                    <TableCell>
                      {target ? (
                        <Link
                          to={`/vms/${encodeURIComponent(target.name)}`}
                          className="underline-offset-4 hover:underline"
                        >
                          {target.name}
                        </Link>
                      ) : (
                        "—"
                      )}
                    </TableCell>
                  </TableRow>
                );
              })
            )}
          </TableBody>
        </Table>
      </div>
      {opened && (
        <VmDrawer item={opened} actions={actionsFor(opened)} onClose={() => setOpen(null)} />
      )}
      {dialog?.kind === "cutover" && (
        <CutoverDialog migration={migration} items={dialog.items} onClose={() => setDialog(null)} />
      )}
      {dialog?.kind === "revert" && (
        <RevertDialog item={dialog.item} onClose={() => setDialog(null)} />
      )}
      <ConfirmDialog
        open={dialog?.kind === "finalize"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("vmware.finalize.title", {
          name: dialog?.kind === "finalize" ? dialog.item.spec.target_name : "",
        })}
        description={t("vmware.finalize.description")}
        confirmLabel={t("vmware.action.finalize")}
        onConfirm={async () => {
          if (dialog?.kind !== "finalize") {
            return;
          }
          try {
            await call(
              api.POST("/vmware-vms/{key}/finalize", {
                params: { path: { key: dialog.item.id } },
              }),
            );
          } catch (err) {
            throw localized(t, err);
          }
          await refreshVmware(queryClient);
          toast.success(t("vmware.finalize.started", { name: dialog.item.spec.target_name }));
        }}
      />
      <ConfirmDialog
        open={dialog?.kind === "cancel"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("vmware.cancelItems.title", {
          count: dialog?.kind === "cancel" ? dialog.items.length : 0,
        })}
        description={t("vmware.cancelItems.description")}
        confirmLabel={t("vmware.action.cancel")}
        onConfirm={async () => {
          if (dialog?.kind !== "cancel") {
            return;
          }
          // 하나가 실패해도 나머지는 취소한다(전환 확인과 같다). 모두 실패했을 때만 대화상자에 남는다.
          const failed: string[] = [];
          const done: string[] = [];
          for (const item of dialog.items) {
            try {
              await call(
                api.POST("/vmware-vms/{key}/cancel", { params: { path: { key: item.id } } }),
              );
              done.push(item.id);
            } catch (err) {
              failed.push(`${item.spec.target_name}: ${(localized(t, err) as Error).message}`);
            }
          }
          setSelected((s) => s.filter((id) => !done.includes(id)));
          await refreshVmware(queryClient);
          if (failed.length === dialog.items.length) {
            throw new Error(failed.join("\n"));
          }
          if (failed.length > 0) {
            toast.error(failed.join("\n"));
          }
        }}
      />
      <ConfirmDialog
        open={dialog?.kind === "cancel-plan"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("vmware.migration.cancelTitle", { name: migration.name })}
        description={t("vmware.migration.cancelDescription")}
        confirmLabel={t("vmware.migration.cancel")}
        onConfirm={async () => {
          try {
            await call(
              api.POST("/vmware-migrations/{key}/actions/cancel", {
                params: { path: { key: migration.id } },
              }),
            );
          } catch (err) {
            throw localized(t, err);
          }
          await refreshVmware(queryClient);
        }}
      />
      <ConfirmDialog
        open={dialog?.kind === "delete"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("vmware.migration.deleteTitle", { name: migration.name })}
        description={t("vmware.migration.deleteDescription")}
        confirmLabel={t("common.delete")}
        typeToConfirm={migration.name}
        onConfirm={async () => {
          try {
            await call(
              api.DELETE("/vmware-migrations/{key}", { params: { path: { key: migration.id } } }),
            );
          } catch (err) {
            throw localized(t, err);
          }
          await refreshVmware(queryClient);
          toast.success(t("vmware.migration.deleted", { name: migration.name }));
          void navigate("/vmware-migrations");
        }}
      />
    </div>
  );
}
