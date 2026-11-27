import { Disc, Download, Trash2, Upload } from "lucide-react";
import { Fragment, useId, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Volume } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { EmptyState } from "@/components/EmptyState";
import { PageHeader } from "@/components/PageHeader";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { DownloadDialog } from "@/isos/DownloadDialog";
import { isoVolumes, usedBy } from "@/isos/isos";
import { useCreateParam } from "@/lib/createParam";
import { formatBytes } from "@/lib/format";
import { isShared } from "@/storage/pools";
import { UploadDialog } from "@/storage/UploadDialog";
import { describeProgress, trackTask } from "@/tasks/tasks";

/**
 * ISO 이미지 라이브러리: 풀별 ISO 볼륨, 크기, 노드(노드 로컬 풀), 넣은 VM. 올리기·URL에서 받기·지우기(넣은 VM이 있으면
 * 서버가 지우지 않는다).
 */
export function IsosPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const volumes = useList("volume");
  const vms = useList("vm");
  const nodes = useList("node");
  const pools = useList("storage-pool");
  const tasksByTarget = useTaskIndex();
  const [uploading, setUploading] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [deleting, setDeleting] = useState<Volume | null>(null);
  const write = perms.anywhere("storage.allocate");
  useCreateParam(() => setUploading(true), write);
  const reasonBase = useId();
  // 풀 이름, 이름 순
  const isos = useMemo(
    () =>
      isoVolumes(volumes.data ?? []).sort(
        (a, b) => a.spec.pool.localeCompare(b.spec.pool) || a.name.localeCompare(b.name),
      ),
    [volumes.data],
  );
  const poolsByName = new Map(pools.data?.map((p) => [p.name, p]));
  const columns = write ? 7 : 6;

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="isos-title"
        title={t("nav.item.isos")}
        help="isos"
        intro={t("help.isos.intro")}
        actions={
          write && (
            <div className="flex gap-2">
              <Button variant="outline" onClick={() => setDownloading(true)}>
                <Download />
                {t("downloads.open")}
              </Button>
              <Button onClick={() => setUploading(true)}>
                <Upload />
                {t("isos.upload")}
              </Button>
            </div>
          )
        }
      />
      {volumes.isSuccess && isos.length === 0 ? (
        <EmptyState
          icon={Disc}
          title={t("isos.empty")}
          description={t("help.isos.empty")}
          help="isos.page"
          action={
            write && (
              <div className="flex flex-wrap justify-center gap-2">
                <Button variant="outline" onClick={() => setDownloading(true)}>
                  <Download />
                  {t("downloads.open")}
                </Button>
                <Button onClick={() => setUploading(true)}>
                  <Upload />
                  {t("isos.upload")}
                </Button>
              </div>
            )
          }
        />
      ) : (
        <Table aria-labelledby="isos-title">
          <TableHeader>
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("volumes.pool")}</TableHead>
              <TableHead>{t("volumes.size")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
              <TableHead>{t("common.node")}</TableHead>
              <TableHead>{t("isos.usedBy")}</TableHead>
              {write && (
                <TableHead>
                  <span className="sr-only">{t("vms.actions")}</span>
                </TableHead>
              )}
            </TableRow>
          </TableHeader>
          <TableBody>
            {isos.length === 0 && (
              <TableMessage colSpan={columns} query={volumes} empty={t("isos.empty")} />
            )}
            {isos.map((iso) => {
              const ready = iso.status.phase === "ready";
              const users = usedBy(vms.data ?? [], iso.name);
              const pool = poolsByName.get(iso.spec.pool);
              // 노드 로컬 풀의 ISO는 그 노드의 VM만 쓴다.
              const node =
                pool && !isShared(pool) && iso.status.node
                  ? (nodes.data?.find((n) => n.id === iso.status.node)?.name ??
                    iso.status.node.slice(-8))
                  : null;
              const download = (tasksByTarget.get(iso.id) ?? []).find(
                (task) => task.spec.op.kind === "volume-download",
              );
              const progress = download?.status.progress;
              const deleteReason = !ready
                ? "volumes.reason.notReady"
                : !vms.isSuccess
                  ? "volumes.reason.vmsUnknown"
                  : users.length > 0
                    ? "isos.reason.inUse"
                    : null;
              const reasons = `${reasonBase}-${iso.id}`;
              return (
                <TableRow key={iso.id}>
                  <TableCell className="font-medium">{iso.name}</TableCell>
                  <TableCell>{iso.spec.pool}</TableCell>
                  <TableCell>
                    {iso.spec.size_bytes > 0 ? formatBytes(iso.spec.size_bytes) : "—"}
                  </TableCell>
                  <TableCell>
                    <div className="flex flex-col gap-1">
                      <Badge variant={ready ? "default" : "secondary"}>
                        {t(`volumes.phase.${iso.status.phase}`)}
                      </Badge>
                      {download && progress && (
                        <>
                          {progress.total_bytes > 0 && (
                            <progress
                              className="h-1.5 w-32"
                              aria-label={t("isos.progressFor", { name: iso.name })}
                              max={progress.total_bytes}
                              value={Math.min(progress.done_bytes, progress.total_bytes)}
                            />
                          )}
                          <span className="text-xs text-muted-foreground">
                            {describeProgress(download, t)}
                          </span>
                        </>
                      )}
                    </div>
                  </TableCell>
                  <TableCell>{node ?? "—"}</TableCell>
                  <TableCell>
                    {users.length === 0
                      ? "—"
                      : users.map((name, i) => (
                          <Fragment key={name}>
                            {i > 0 && ", "}
                            <Link
                              to={`/vms/${encodeURIComponent(name)}`}
                              className="underline-offset-4 hover:underline"
                            >
                              {name}
                            </Link>
                          </Fragment>
                        ))}
                  </TableCell>
                  {write && (
                    <TableCell className="text-right">
                      <ReasonButton
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("common.deleteNamed", { name: iso.name })}
                        reason={deleteReason}
                        reasons={reasons}
                        onClick={() => setDeleting(iso)}
                      >
                        <Trash2 />
                      </ReasonButton>
                      <ReasonList base={reasons} reasons={[deleteReason]} />
                    </TableCell>
                  )}
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      {uploading && <UploadDialog content="iso" onClose={() => setUploading(false)} />}
      {downloading && <DownloadDialog content="iso" onClose={() => setDownloading(false)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("isos.deleteTitle", { name: deleting?.name })}
        description={t("isos.deleteDescription")}
        confirmLabel={t("common.delete")}
        typeToConfirm={deleting?.name}
        onConfirm={async () => {
          const iso = deleting!;
          const task = await call(
            api.DELETE("/volumes/{key}", { params: { path: { key: iso.id } } }),
          );
          trackTask(task, t("tasks.op.volume-delete", { target: iso.name }));
        }}
      />
    </div>
  );
}
