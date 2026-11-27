import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Download, Plus, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { EmptyState } from "@/components/EmptyState";
import { PageHeader } from "@/components/PageHeader";
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
import { AddImageDialog } from "@/images/AddImageDialog";
import { ImageDownloadDialog } from "@/images/ImageDownloadDialog";
import { activeDownloads, type Arch, ARCHES, type CatalogEntry } from "@/images/images";
import { describeProgress } from "@/tasks/tasks";

/** 카탈로그 질의 키(아키텍처마다) */
export const CATALOG_KEY = ["image-catalog"] as const;

/**
 * 이미지 라이브러리: 내장 카탈로그와 관리자 항목(아키텍처로 거름), 이미 받은 볼륨, 받는 중 진행률. 항목을 풀에 받고 원하면
 * 템플릿 VM까지 만든다(`POST /image-catalog/{id}/download`). 관리자는 항목을 더하고 지운다(이미 받은 볼륨·템플릿은 남는다).
 */
export function ImagesPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const [arch, setArch] = useState<Arch | null>(null);
  // 카탈로그 목록은 루트 `system.read`다(서버 `Root(SystemRead)`). 없으면 묻지 않고 한 줄로 알린다.
  const readable = perms.has("system.read");
  const catalog = useQuery({
    queryKey: [...CATALOG_KEY, arch],
    queryFn: () => call(api.GET("/image-catalog", { params: { query: arch ? { arch } : {} } })),
    enabled: readable,
  });
  const tasks = useList("task");
  const [downloading, setDownloading] = useState<CatalogEntry | null>(null);
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<CatalogEntry | null>(null);
  const write = perms.anywhere("storage.allocate");
  const admin = perms.can("write", "settings");
  const items = catalog.data?.items ?? [];
  // 받기가 끝나면 받은 볼륨(`volumes`)을 다시 읽는다.
  const finished = (tasks.data ?? []).filter(
    (task) => task.spec.op.kind === "volume-download" && task.status.state === "succeeded",
  ).length;
  useEffect(() => {
    void queryClient.invalidateQueries({ queryKey: CATALOG_KEY });
  }, [finished, queryClient]);
  const columns = write || admin ? 7 : 6;

  if (!readable) {
    return (
      <div className="flex flex-col gap-4">
        <PageHeader title={t("nav.images")} help="images" intro={t("help.images.intro")} />
        <p className="text-sm text-muted-foreground">{t("images.needsRead")}</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="images-title"
        title={t("nav.images")}
        help="images"
        intro={t("help.images.intro")}
        actions={
          admin && (
            <Button variant="outline" onClick={() => setAdding(true)}>
              <Plus />
              {t("images.add")}
            </Button>
          )
        }
      />
      <div role="group" aria-label={t("images.arch")} className="flex flex-wrap gap-2">
        {[null, ...ARCHES].map((a) => (
          <Button
            key={a ?? "all"}
            variant={arch === a ? "secondary" : "outline"}
            size="sm"
            aria-pressed={arch === a}
            onClick={() => setArch(a)}
          >
            {a ?? t("images.allArches")}
          </Button>
        ))}
      </div>
      {catalog.isSuccess && items.length === 0 && arch === null ? (
        <EmptyState
          icon={Download}
          title={t("images.empty")}
          description={t("help.images.empty")}
          help="images.page"
          action={
            admin && (
              <Button onClick={() => setAdding(true)}>
                <Plus />
                {t("images.add")}
              </Button>
            )
          }
        />
      ) : (
        <Table aria-labelledby="images-title">
          <TableHeader>
            <TableRow>
              <TableHead>{t("images.image")}</TableHead>
              <TableHead>{t("images.arch")}</TableHead>
              <TableHead>{t("vms.firmware")}</TableHead>
              <TableHead>{t("images.checksum")}</TableHead>
              <TableHead>{t("images.source")}</TableHead>
              <TableHead>{t("images.volumes")}</TableHead>
              {(write || admin) && (
                <TableHead>
                  <span className="sr-only">{t("vms.actions")}</span>
                </TableHead>
              )}
            </TableRow>
          </TableHeader>
          <TableBody>
            {items.length === 0 && (
              <TableMessage colSpan={columns} query={catalog} empty={t("images.empty")} />
            )}
            {items.map((entry) => {
              const active = activeDownloads(entry, tasks.data ?? []);
              const label = `${entry.title} (${entry.arch})`;
              return (
                <TableRow key={`${entry.id}/${entry.arch}`}>
                  <TableCell>
                    <span className="font-medium">{entry.title}</span>
                    <span className="block font-mono text-xs text-muted-foreground">
                      {entry.id}
                    </span>
                  </TableCell>
                  <TableCell>{entry.arch}</TableCell>
                  <TableCell>{entry.firmware.toUpperCase()}</TableCell>
                  <TableCell>{t(`images.checksums.${entry.checksum.kind}`)}</TableCell>
                  <TableCell>
                    <Badge variant={entry.source === "builtin" ? "outline" : "secondary"}>
                      {t(`images.sources.${entry.source}`)}
                    </Badge>
                  </TableCell>
                  <TableCell>
                    {entry.volumes.length > 0 ? entry.volumes.join(", ") : "—"}
                    {active.map((task) => (
                      <span key={task.id} className="block text-xs text-muted-foreground">
                        {describeProgress(task, t) ?? t("images.queued")}
                      </span>
                    ))}
                  </TableCell>
                  {(write || admin) && (
                    <TableCell className="text-right whitespace-nowrap">
                      {write && (
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t("images.downloadNamed", { name: label })}
                          onClick={() => setDownloading(entry)}
                        >
                          <Download />
                        </Button>
                      )}
                      {admin && entry.source === "custom" && (
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t("common.deleteNamed", { name: label })}
                          onClick={() => setRemoving(entry)}
                        >
                          <Trash2 />
                        </Button>
                      )}
                    </TableCell>
                  )}
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      {downloading && (
        <ImageDownloadDialog entry={downloading} onClose={() => setDownloading(null)} />
      )}
      {adding && <AddImageDialog onClose={() => setAdding(false)} />}
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("images.removeTitle", { name: removing?.title, arch: removing?.arch })}
        description={t("images.removeDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const entry = removing!;
          await call(
            api.DELETE("/image-catalog/{id}", {
              params: { path: { id: entry.id }, query: { arch: entry.arch } },
            }),
          );
          await queryClient.invalidateQueries({ queryKey: CATALOG_KEY });
        }}
      />
    </div>
  );
}
