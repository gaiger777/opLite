import { useQueryClient } from "@tanstack/react-query";
import { Rocket, Trash2, Undo2 } from "lucide-react";
import { useId, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { trackTask, waitForTask } from "@/tasks/tasks";
import { DeployDialog } from "@/templates/DeployDialog";
import { cloneOwners, revertReason, templateRows } from "@/templates/templates";

/**
 * 템플릿: 템플릿 VM마다 디스크·풀·연결 복제 수, 배포·VM으로 되돌리기·삭제. 연결 복제가 남은 템플릿은 되돌리거나 지울 수
 * 없다(서버의 409 `template_in_use`) — 복제를 지우거나 평탄화한다. 삭제는 되돌리기 Task를 기다린 뒤 VM을 지운다(디스크
 * 볼륨은 남는다, VM 삭제와 같다).
 */
export function TemplatesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const vms = useList("vm");
  const volumes = useList("volume");
  const [deploying, setDeploying] = useState<Vm | null>(null);
  const [reverting, setReverting] = useState<Vm | null>(null);
  const [deleting, setDeleting] = useState<Vm | null>(null);
  const reasonBase = useId();
  const write = can(me.role, "write", "vm");
  // 배포는 새 VM과 새 볼륨을 만든다.
  const deploy = write && can(me.role, "write", "volume");
  const rows = useMemo(
    () => templateRows(vms.data ?? [], volumes.data ?? []),
    [vms.data, volumes.data],
  );
  const columns = write ? 5 : 4;

  return (
    <div className="flex flex-col gap-4">
      <h1 id="templates-title" className="text-2xl font-semibold">
        {t("nav.templates")}
      </h1>
      <p className="text-sm text-muted-foreground">{t("templates.note")}</p>
      <Table aria-labelledby="templates-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("templates.disks")}</TableHead>
            <TableHead>{t("volumes.pool")}</TableHead>
            <TableHead>{t("templates.clones")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.length === 0 && (
            <TableMessage colSpan={columns} query={vms} empty={t("templates.empty")} />
          )}
          {rows.map(({ vm, disks, pools, clones }) => {
            const blocked = revertReason(vm, volumes.data);
            const reasons = `${reasonBase}-${vm.id}`;
            const owners = cloneOwners(clones, vms.data ?? []);
            return (
              <TableRow key={vm.id}>
                <TableCell className="font-medium">
                  <Link
                    to={`/vms/${encodeURIComponent(vm.name)}`}
                    className="underline-offset-4 hover:underline"
                  >
                    {vm.name}
                  </Link>
                </TableCell>
                <TableCell>{disks.length > 0 ? disks.join(", ") : "—"}</TableCell>
                <TableCell>{pools.length > 0 ? pools.join(", ") : "—"}</TableCell>
                <TableCell>
                  <span title={owners.join(", ")}>{clones.length}</span>
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    {deploy && (
                      <ReasonButton
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("templates.deployNamed", { name: vm.name })}
                        reasons={reasons}
                        onClick={() => setDeploying(vm)}
                      >
                        <Rocket />
                      </ReasonButton>
                    )}
                    <ReasonButton
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("templates.revertNamed", { name: vm.name })}
                      reason={blocked}
                      reasons={reasons}
                      onClick={() => setReverting(vm)}
                    >
                      <Undo2 />
                    </ReasonButton>
                    <ReasonButton
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: vm.name })}
                      reason={blocked}
                      reasons={reasons}
                      onClick={() => setDeleting(vm)}
                    >
                      <Trash2 />
                    </ReasonButton>
                    <ReasonList base={reasons} reasons={[blocked]} />
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {deploying && <DeployDialog template={deploying} onClose={() => setDeploying(null)} />}
      <ConfirmDialog
        open={reverting !== null}
        onOpenChange={(open) => !open && setReverting(null)}
        title={t("templates.revertTitle", { name: reverting?.name })}
        description={t("templates.revertDescription")}
        confirmLabel={t("templates.revert")}
        onConfirm={async () => {
          const vm = reverting!;
          const task = await call(
            api.DELETE("/vms/{key}/template", { params: { path: { key: vm.id } } }),
          );
          trackTask(task, t("tasks.op.vm-untemplate", { target: vm.name }));
        }}
      />
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("templates.deleteTitle", { name: deleting?.name })}
        description={t("templates.deleteDescription")}
        confirmLabel={t("common.delete")}
        typeToConfirm={deleting?.name}
        onConfirm={async () => {
          const vm = deleting!;
          // 되돌리기 Task가 끝나야 VM을 지울 수 있다(템플릿은 지우지 않는다).
          const task = await call(
            api.DELETE("/vms/{key}/template", { params: { path: { key: vm.id } } }),
          );
          await waitForTask(queryClient, task.id);
          await call(api.DELETE("/vms/{key}", { params: { path: { key: vm.id } } }));
          toast.success(t("vms.deleted", { name: vm.name }));
        }}
      />
    </div>
  );
}
