import { useQueryClient } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePermissions } from "@/auth/usePermissions";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { type RemoteView, useRemotes } from "@/remotes/remotes";

/**
 * 보호 › 원격 복제: 원격 클러스터의 상태 카드, 원격으로 사본을 보내는 백업 작업, 원격 VM 보기(카드의 이름)다.
 * 상태는 리더가 모은 것을 10초마다 다시 읽는다. 추가·고치기·지우기는 설정 › 원격 클러스터(`RemotesManagePage`)에
 * 있다.
 */
export function RemotesPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const admin = perms.can("write", "remote");
  const queryClient = useQueryClient();
  const remotes = useRemotes();
  const changed = () => queryClient.invalidateQueries({ queryKey: ["remotes"] });

  async function refresh(remote: RemoteView) {
    try {
      await call(
        api.POST("/remotes/{key}/actions/refresh", { params: { path: { key: remote.name } } }),
      );
      await changed();
    } catch (err) {
      toast.error(t("remotes.refreshFailed", { name: remote.name, error: errorMessage(t, err) }));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t("nav.replication")}
        description={t("remotes.pageHint")}
        actions={
          admin && (
            <Link to="/settings/remotes" className="text-sm underline-offset-4 hover:underline">
              {t("remotes.manage")}
            </Link>
          )
        }
      />
      {!remotes.data ? (
        <ListPending error={remotes.error} />
      ) : remotes.data.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("remotes.empty")}</p>
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {remotes.data.map((remote) => (
            <RemoteCard
              key={remote.id}
              remote={remote}
              admin={admin}
              onRefresh={() => void refresh(remote)}
            />
          ))}
        </div>
      )}
      <ReplicatingJobs />
    </div>
  );
}

/** 원격 대상(`replicate`)이 있는 백업 작업. 고치기는 백업 작업 화면에서 한다. */
function ReplicatingJobs() {
  const { t } = useTranslation();
  const jobs = useList("backup-job");
  const list = (jobs.data ?? [])
    .filter((job) => job.spec.replicate)
    .sort((a, b) => a.name.localeCompare(b.name));
  return (
    <section aria-labelledby="replicating-jobs" className="flex flex-col gap-2">
      <h2 id="replicating-jobs" className="font-medium">
        {t("remotes.replicatingJobs")}
      </h2>
      <Table aria-labelledby="replicating-jobs">
        <TableHeader>
          <TableRow>
            <TableHead>{t("remotes.job")}</TableHead>
            <TableHead>{t("remotes.target")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={2} query={jobs} empty={t("remotes.noReplicatingJobs")} />
          )}
          {list.map((job) => (
            <TableRow key={job.id}>
              <TableCell className="font-medium">
                <Link to="/backup-jobs" className="underline-offset-4 hover:underline">
                  {job.name}
                </Link>
              </TableCell>
              <TableCell>
                {job.spec.replicate!.remote} / {job.spec.replicate!.pool}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </section>
  );
}

/** 원격 하나의 카드: 이름(VM 목록으로 가는 링크), 연결 상태, 모은 요약 */
function RemoteCard({
  remote,
  admin,
  onRefresh,
}: {
  remote: RemoteView;
  admin: boolean;
  onRefresh(): void;
}) {
  const { t } = useTranslation();
  const id = useId();
  const { status } = remote;
  const rows: [string, string][] = [
    [t("remotes.cluster"), status.cluster_name ?? "—"],
    [t("remotes.version"), status.version ?? "—"],
    [t("remotes.nodes"), status.nodes ? t("remotes.nodesValue", status.nodes) : "—"],
    [t("remotes.vms"), status.vms ? t("remotes.vmsValue", status.vms) : "—"],
    [t("remotes.cpus"), status.cpus != null ? String(status.cpus) : "—"],
    [
      t("remotes.memory"),
      status.memory_gib != null ? t("remotes.memoryValue", { gib: status.memory_gib }) : "—",
    ],
  ];
  return (
    <Card role="group" aria-labelledby={id}>
      <CardHeader>
        <CardTitle id={id} className="flex flex-wrap items-center gap-2">
          <Link
            to={`/remotes/${encodeURIComponent(remote.name)}`}
            className="underline-offset-4 hover:underline"
          >
            {remote.name}
          </Link>
          {status.reachable ? (
            <Badge>{t("remotes.reachable")}</Badge>
          ) : (
            <Badge variant="destructive">{t("remotes.unreachable")}</Badge>
          )}
        </CardTitle>
        <p className="truncate font-mono text-xs text-muted-foreground">{remote.url}</p>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {!status.reachable && status.error && (
          <p className="text-sm break-words text-destructive">{status.error}</p>
        )}
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
          {rows.map(([label, value]) => (
            <div key={label} className="contents">
              <dt className="text-muted-foreground">{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
        {admin && (
          <div className="flex justify-end gap-1">
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t("remotes.refreshNamed", { name: remote.name })}
              onClick={onRefresh}
            >
              <RefreshCw />
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
