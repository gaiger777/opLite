import { useQueryClient } from "@tanstack/react-query";
import { Download, PackageOpen, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useSearchParams } from "react-router";
import { toast } from "sonner";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import { objectTarget, ROOT } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { ExportDialog } from "@/exports/ExportDialog";
import { downloadExport, refreshExports, useExports, type VmExport } from "@/exports/exports";
import { codeMessage, errorMessage } from "@/i18n/errors";
import { formatBytes, formatTime, uuidTime } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

const TONE = {
  pending: "neutral",
  writing: "warning",
  ready: "success",
  failed: "danger",
} as const;

/**
 * 가상 머신 › 가져오기 › 내보내기(계획 72 §12·§13): OVA/OVF 내보내기 목록, 내려받기(10분 링크 — 브라우저가 받는다)·지우기.
 * 내보내기는 VM 상세 ⋯ 메뉴에서 시작한다. 권한은 내보낸 VM의 `vm.export`(VM을 지웠으면 루트)다. 늦게 읽는 조각이다.
 */
export function ExportsPage() {
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const exports = useExports();
  const vms = useList("vm");
  const [deleting, setDeleting] = useState<VmExport | null>(null);
  // VM 메뉴의 "OVA로 내보내기…"는 `?export=<VM 이름>`으로 이 보기를 열고 대화상자를 띄운다.
  const [params, setParams] = useSearchParams();
  const exporting = params.get("export");
  const exportVm = exporting ? vms.data?.find((v) => v.name === exporting) : undefined;
  const closeExport = () =>
    setParams(
      (p) => {
        p.delete("export");
        return p;
      },
      { replace: true },
    );
  const items = [...(exports.data ?? [])].sort((a, b) => b.id.localeCompare(a.id));
  const vmOf = (exp: VmExport) => vms.data?.find((v) => v.id === exp.spec.vm);
  const allowed = (exp: VmExport) => {
    const vm = vmOf(exp);
    return perms.has("vm.export", vm ? objectTarget("vm", vm) : ROOT);
  };
  const text = (err: unknown) =>
    err instanceof ApiError
      ? codeMessage(t, err.code, err.message, "vmExports.errors")
      : errorMessage(t, err);

  async function download(exp: VmExport) {
    try {
      await downloadExport(exp);
      toast.success(t("vmExports.downloadStarted", { name: exp.name }));
    } catch (err) {
      toast.error(text(err));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="vm-exports-title"
        title={t("nav.vmExports")}
        description={t("vmExports.pageHint")}
      />
      {exports.data === undefined ? (
        <ListPending error={exports.error} />
      ) : items.length === 0 ? (
        <EmptyState
          icon={PackageOpen}
          title={t("vmExports.empty")}
          description={t("vmExports.emptyHint")}
        />
      ) : (
        <div className="overflow-x-auto">
          <Table aria-labelledby="vm-exports-title">
            <TableHeader>
              <TableRow>
                <TableHead>{t("common.name")}</TableHead>
                <TableHead>{t("vmExports.vm")}</TableHead>
                <TableHead>{t("vmExports.pool")}</TableHead>
                <TableHead>{t("vmExports.size")}</TableHead>
                <TableHead>{t("common.state")}</TableHead>
                <TableHead>{t("vmExports.created")}</TableHead>
                <TableHead>
                  <span className="sr-only">{t("vms.actions")}</span>
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((exp) => {
                const vm = vmOf(exp);
                const phase = exp.status.phase;
                const can = allowed(exp);
                return (
                  <TableRow key={exp.id}>
                    <TableCell className="font-medium">{exp.name}</TableCell>
                    <TableCell>
                      {vm ? (
                        <Link
                          to={`/vms/${encodeURIComponent(vm.name)}`}
                          className="underline-offset-4 hover:underline"
                        >
                          {vm.name}
                        </Link>
                      ) : (
                        <span className="text-muted-foreground">{t("vmExports.vmGone")}</span>
                      )}
                    </TableCell>
                    <TableCell>{exp.spec.pool}</TableCell>
                    <TableCell>
                      {phase === "ready" ? formatBytes(exp.status.ova_bytes) : "—"}
                    </TableCell>
                    <TableCell>
                      <StatusChip tone={TONE[phase]} busy={phase === "writing"}>
                        {t(`vmExports.phase.${phase}`)}
                      </StatusChip>
                      {exp.status.error && (
                        <div className="max-w-xs text-xs whitespace-normal break-words text-destructive">
                          {exp.status.error}
                        </div>
                      )}
                      {exp.status.warnings.length > 0 && (
                        <details className="text-xs">
                          <summary className="cursor-pointer text-muted-foreground">
                            {t("vmExports.warnings", { count: exp.status.warnings.length })}
                          </summary>
                          <ul className="list-disc pl-5">
                            {exp.status.warnings.map((w) => (
                              <li key={w} className="break-words whitespace-normal">
                                {w}
                              </li>
                            ))}
                          </ul>
                        </details>
                      )}
                    </TableCell>
                    <TableCell>{formatTime(uuidTime(exp.id), i18n.language)}</TableCell>
                    <TableCell className="text-right whitespace-nowrap">
                      {can && phase === "ready" && (
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t("vmExports.downloadNamed", { name: exp.name })}
                          onClick={() => void download(exp)}
                        >
                          <Download />
                        </Button>
                      )}
                      {can && (
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t("common.deleteNamed", { name: exp.name })}
                          onClick={() => setDeleting(exp)}
                        >
                          <Trash2 />
                        </Button>
                      )}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      )}
      {exportVm && <ExportDialog vm={exportVm} onClose={closeExport} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("vmExports.deleteTitle", { name: deleting?.name })}
        description={t("vmExports.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const exp = deleting!;
          let task;
          try {
            task = await call(
              api.DELETE("/vm-exports/{key}", { params: { path: { key: exp.id } } }),
            );
          } catch (err) {
            throw new Error(text(err), { cause: err });
          }
          trackTask(task, t("tasks.op.vm-export-delete", { target: exp.name }));
          await refreshExports(queryClient);
        }}
      />
    </div>
  );
}
