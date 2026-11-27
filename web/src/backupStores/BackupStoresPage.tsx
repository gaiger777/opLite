import { useQueryClient } from "@tanstack/react-query";
import { Database, Lock, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePermissions } from "@/auth/usePermissions";
import { LastGc, LastVerify, StorePhase } from "@/backupStores/chips";
import { StoreSheet } from "@/backupStores/StoreSheet";
import { StoreWizard } from "@/backupStores/StoreWizard";
import {
  type BackupStore,
  backendText,
  dedupRatio,
  formatRatio,
  isS3,
  refreshStores,
  shortFingerprint,
  useBackupStores,
} from "@/backupStores/stores";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { StatusChip } from "@/components/StatusChip";
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
import { formatBytes } from "@/lib/format";
import { useCreateParam, useOpenParam } from "@/lib/createParam";

/**
 * 백업·복구 › 백업 저장소(계획 68 §14 웹): 목록(이름·백엔드·암호화 배지와 지문·스냅샷 수·저장 크기·dedup 배율·마지막 GC·마지막
 * 검증·상태), 만들기 마법사, 서랍(설정·키 카드·GC·검증·노드별 닿음·읽기 전용이면 scan·넘겨받기). 늦게 읽는 조각이다.
 */
export function BackupStoresPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const stores = useBackupStores(perms.reads("backup-store"));
  const pools = useList("storage-pool");
  const [creating, setCreating] = useState(false);
  const [openId, setOpenId] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<BackupStore | null>(null);
  const write = perms.has("storage.config");
  useCreateParam(() => setCreating(true), write, stores.isSuccess && pools.isSuccess);
  useOpenParam(setOpenId);
  const items = [...(stores.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));
  const open = items.find((s) => s.id === openId);

  const createButton = write && (
    <Button disabled={!pools.isSuccess} onClick={() => setCreating(true)}>
      <Plus />
      {t("backupStores.create")}
    </Button>
  );

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="backup-stores-title"
        title={t("nav.backupStores")}
        description={t("backupStores.pageHint")}
        actions={items.length > 0 && createButton}
      />
      {stores.data === undefined ? (
        <ListPending error={stores.error} />
      ) : items.length === 0 ? (
        <EmptyState
          icon={Database}
          title={t("backupStores.empty.title")}
          description={t("backupStores.empty.description")}
          action={createButton}
        />
      ) : (
        <Table aria-labelledby="backup-stores-title">
          <TableHeader>
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("backupStores.backend")}</TableHead>
              <TableHead>{t("backupStores.encryption")}</TableHead>
              <TableHead>{t("backupStores.snapshots")}</TableHead>
              <TableHead>{t("backupStores.stored")}</TableHead>
              <TableHead>{t("backupStores.dedup")}</TableHead>
              <TableHead>{t("backupStores.lastGc")}</TableHead>
              <TableHead>{t("backupStores.lastVerify")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
              {write && (
                <TableHead>
                  <span className="sr-only">{t("vms.actions")}</span>
                </TableHead>
              )}
            </TableRow>
          </TableHeader>
          <TableBody>
            {items.map((store) => {
              const usage = store.status.usage;
              const ratio = dedupRatio(usage);
              const fingerprint = store.spec.encryption?.fingerprint;
              return (
                <TableRow key={store.id}>
                  <TableCell className="font-medium">
                    <button
                      type="button"
                      className="underline-offset-4 hover:underline"
                      aria-label={t("backupStores.openNamed", { name: store.name })}
                      onClick={() => setOpenId(store.id)}
                    >
                      {store.name}
                    </button>
                    {store.spec.read_only && (
                      <Badge variant="outline" className="ml-2">
                        {t("backupStores.readOnlyBadge")}
                      </Badge>
                    )}
                  </TableCell>
                  <TableCell>
                    <span className="flex flex-col">
                      <span className="text-xs text-muted-foreground">
                        {t(`backupStores.backendKind.${isS3(store) ? "s3" : "pool"}`)}
                      </span>
                      <span className="font-mono text-xs break-all">{backendText(store)}</span>
                    </span>
                  </TableCell>
                  <TableCell>
                    {fingerprint ? (
                      <span className="flex flex-col items-start gap-0.5">
                        <StatusChip tone="info" icon={Lock}>
                          {t("backupStores.encrypted")}
                        </StatusChip>
                        <span
                          className="font-mono text-xs text-muted-foreground"
                          title={fingerprint}
                        >
                          {shortFingerprint(fingerprint)}
                        </span>
                      </span>
                    ) : (
                      <span className="text-muted-foreground">{t("backupStores.plain")}</span>
                    )}
                  </TableCell>
                  <TableCell>{usage ? usage.snapshots : "—"}</TableCell>
                  <TableCell>{usage ? formatBytes(usage.stored_bytes) : "—"}</TableCell>
                  <TableCell>{ratio !== undefined ? formatRatio(ratio) : "—"}</TableCell>
                  <TableCell>
                    <LastGc store={store} />
                  </TableCell>
                  <TableCell>
                    <LastVerify store={store} />
                  </TableCell>
                  <TableCell>
                    <StorePhase store={store} />
                  </TableCell>
                  {write && (
                    <TableCell className="text-right whitespace-nowrap">
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("common.deleteNamed", { name: store.name })}
                        onClick={() => setDeleting(store)}
                      >
                        <Trash2 />
                      </Button>
                    </TableCell>
                  )}
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      {creating && (
        <StoreWizard taken={items.map((s) => s.name)} onClose={() => setCreating(false)} />
      )}
      {open && <StoreSheet store={open} onClose={() => setOpenId(null)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("backupStores.deleteTitle", { name: deleting?.name })}
        description={t("backupStores.deleteDescription")}
        confirmLabel={t("common.delete")}
        typeToConfirm={deleting?.name}
        onConfirm={async () => {
          const store = deleting!;
          await call(api.DELETE("/backup-stores/{key}", { params: { path: { key: store.id } } }));
          await refreshStores(queryClient);
          toast.success(t("backupStores.deleted", { name: store.name }));
        }}
      />
    </div>
  );
}
