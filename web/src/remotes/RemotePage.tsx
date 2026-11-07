import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, MoreHorizontal } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ListPending } from "@/components/ListPending";
import { ReasonMenuItem } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatMib } from "@/lib/format";
import { REMOTE_ACTIONS, remoteVmAction, remoteVms, type RemoteVmAction } from "@/remotes/remotes";
import { availability } from "@/vms/actions";
import { vcpus } from "@/vms/VmListPage";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";

/** 원격 상태와 VM 목록을 다시 읽는 간격 */
const REFRESH_MS = 10_000;

/**
 * 원격 클러스터 하나의 VM 목록. 이 클러스터의 중계로 10초마다 읽는다. admin은 시작·종료·강제 정지를
 * 요청한다(원격 쪽에서는 토큰 주인의 역할이 한 번 더 정한다).
 */
export function RemotePage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const me = usePrincipal();
  const admin = can(me.role, "write", "remote");
  const queryClient = useQueryClient();
  const remote = useQuery({
    queryKey: ["remotes", name],
    queryFn: () => call(api.GET("/remotes/{key}", { params: { path: { key: name } } })),
    refetchInterval: REFRESH_MS,
  });
  const vms = useQuery({
    queryKey: ["remotes", name, "vms"],
    queryFn: ({ signal }) => remoteVms(name, signal),
    select: (items) => [...items].sort((a, b) => a.name.localeCompare(b.name)),
    refetchInterval: REFRESH_MS,
    enabled: remote.isSuccess,
  });
  const [forceStopping, setForceStopping] = useState<Vm | null>(null);

  async function run(vm: Vm, action: RemoteVmAction) {
    try {
      await remoteVmAction(name, vm, action);
      toast.success(
        t("remotes.actionRequested", {
          action: t(`vms.action.${action}`),
          name: vm.name,
          remote: name,
        }),
      );
      await queryClient.invalidateQueries({ queryKey: ["remotes", name, "vms"] });
    } catch (err) {
      toast.error(t("vms.actionFailed", { name: vm.name, error: errorMessage(t, err) }));
    }
  }

  const back = (
    <Link
      to="/remotes"
      className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:underline"
    >
      <ArrowLeft className="size-4" />
      {t("remotes.backToList")}
    </Link>
  );
  if (!remote.data) {
    return (
      <div className="flex flex-col gap-4">
        {back}
        <ListPending error={remote.error} />
      </div>
    );
  }
  const { status } = remote.data;
  const list = vms.data ?? [];
  return (
    <div className="flex flex-col gap-4">
      {back}
      <div className="flex flex-col gap-1">
        <h1 id="remote-vms-title" className="text-2xl font-semibold">
          {t("remotes.vmsTitle", { name })}
        </h1>
        <p className="font-mono text-xs text-muted-foreground">
          {status.cluster_name ? `${status.cluster_name} · ` : ""}
          {remote.data.url}
        </p>
      </div>
      {/* 다시 읽지 못하면 마지막으로 읽은 목록을 그대로 보이고 이유를 알린다. */}
      {vms.isError && vms.data && (
        <p role="alert" className="text-sm text-destructive">
          {errorMessage(t, vms.error)}
        </p>
      )}
      <Table aria-labelledby="remote-vms-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("vms.vcpus")}</TableHead>
            <TableHead>{t("vms.memory")}</TableHead>
            {admin && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={admin ? 5 : 4} query={vms} empty={t("vms.empty")} />
          )}
          {list.map((vm) => (
            <TableRow key={vm.id}>
              <TableCell className="font-medium">{vm.name}</TableCell>
              <TableCell>
                <VmPhaseBadge vm={vm} />
              </TableCell>
              <TableCell>{vcpus(vm)}</TableCell>
              <TableCell>{formatMib(vm.spec.memory_mib)}</TableCell>
              {admin && (
                <TableCell className="text-right">
                  <RemoteVmMenu
                    vm={vm}
                    onRun={(action) =>
                      action === "force-stop" ? setForceStopping(vm) : void run(vm, action)
                    }
                  />
                </TableCell>
              )}
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <ConfirmDialog
        open={forceStopping !== null}
        onOpenChange={(open) => !open && setForceStopping(null)}
        title={t("vms.forceStopTitle", { name: forceStopping?.name })}
        description={t("vms.forceStopDescription")}
        confirmLabel={t("vms.action.force-stop")}
        onConfirm={() => run(forceStopping!, "force-stop")}
      />
    </div>
  );
}

/** 원격 VM의 동작 메뉴. 상태로 할 수 없는 동작은 이유와 함께 막는다(원격의 Task는 모른다). */
function RemoteVmMenu({ vm, onRun }: { vm: Vm; onRun(action: RemoteVmAction): void }) {
  const { t } = useTranslation();
  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon-sm" aria-label={t("vms.actionsFor", { name: vm.name })}>
          <MoreHorizontal />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        {REMOTE_ACTIONS.map((action) => {
          const able = availability(vm, action, []);
          return (
            <ReasonMenuItem
              key={action}
              reason={able.ok ? null : able.reason}
              onSelect={() => onRun(action)}
            >
              {t(`vms.action.${action}`)}
            </ReasonMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
