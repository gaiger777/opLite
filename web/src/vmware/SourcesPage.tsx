import { useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, RefreshCw, Server, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
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
import { useCreateParam } from "@/lib/createParam";
import { formatTime } from "@/lib/format";
import { shortFingerprint } from "@/backupStores/stores";
import {
  localized,
  refreshVmware,
  useSources,
  vmwareErrorText,
  type VmwareSource,
} from "@/vmware/api";
import { SourceDialog } from "@/vmware/SourceDialog";

/** 원본 상태 칩: 마지막 확인이 실패했으면 그 까닭, 무료 ESXi면 읽기만 */
function SourceState({ source }: { source: VmwareSource }) {
  const { t } = useTranslation();
  const status = source.status;
  if (status.error) {
    return (
      <span className="flex flex-col items-start gap-0.5">
        <StatusChip tone="danger">{t("vmware.source.state.error")}</StatusChip>
        <span className="max-w-xs text-xs whitespace-normal break-words text-destructive">
          {status.error}
        </span>
      </span>
    );
  }
  if (status.restricted) {
    return <StatusChip tone="warning">{t("vmware.source.state.restricted")}</StatusChip>;
  }
  return status.checked_at ? (
    <StatusChip tone="success">{t("vmware.source.state.ok")}</StatusChip>
  ) : (
    <StatusChip tone="neutral">{t("vmware.source.state.unknown")}</StatusChip>
  );
}

/**
 * 가상 머신 › 가져오기 › VMware 원본(계획 72 §13): 저장한 vCenter·ESXi 연결(비밀번호 없는 보기), 더하기(지문 확인)·고치기·
 * 다시 확인·지우기. 바꾸기는 루트 `cluster.admin`이다. 늦게 읽는 조각이다.
 */
export function SourcesPage() {
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const sources = useSources(perms.reads("vmware-source"));
  const admin = perms.has("cluster.admin");
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<VmwareSource | null>(null);
  const [deleting, setDeleting] = useState<VmwareSource | null>(null);
  const [checking, setChecking] = useState<string | null>(null);
  useCreateParam(() => setAdding(true), admin, sources.isSuccess);
  const items = [...(sources.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));

  async function check(source: VmwareSource) {
    setChecking(source.id);
    try {
      const checked = await call(
        api.POST("/vmware-sources/{key}/actions/check", { params: { path: { key: source.id } } }),
      );
      toast.success(
        t("vmware.source.checked", {
          name: source.name,
          kind: t(`vmware.source.kind.${checked.status.kind ?? "vcenter"}`),
          version: checked.status.version ?? "",
        }),
      );
    } catch (err) {
      toast.error(vmwareErrorText(t, err));
    } finally {
      setChecking(null);
      await refreshVmware(queryClient);
    }
  }

  const addButton = admin && (
    <Button onClick={() => setAdding(true)}>
      <Plus />
      {t("vmware.source.add")}
    </Button>
  );

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="vmware-sources-title"
        title={t("vmware.sources.heading")}
        help="vmwareSources"
        intro={t("vmware.help.sourcesIntro")}
        actions={items.length > 0 && addButton}
      />
      {sources.data === undefined ? (
        <ListPending error={sources.error} />
      ) : items.length === 0 ? (
        <EmptyState
          icon={Server}
          title={t("vmware.sources.empty")}
          description={t("vmware.help.sourcesEmpty")}
          help="vmwareSources.page"
          action={addButton}
        />
      ) : (
        <div className="overflow-x-auto">
          <Table aria-labelledby="vmware-sources-title">
            <TableHeader>
              <TableRow>
                <TableHead>{t("common.name")}</TableHead>
                <TableHead>{t("vmware.source.address")}</TableHead>
                <TableHead>{t("vmware.source.user")}</TableHead>
                <TableHead>{t("vmware.source.product")}</TableHead>
                <TableHead>{t("vmware.source.transport")}</TableHead>
                <TableHead>{t("vmware.source.fingerprintShort")}</TableHead>
                <TableHead>{t("vmware.source.checkedAt")}</TableHead>
                <TableHead>{t("common.state")}</TableHead>
                {admin && (
                  <TableHead>
                    <span className="sr-only">{t("vms.actions")}</span>
                  </TableHead>
                )}
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((source) => {
                const status = source.status;
                const vddk = typeof source.spec.transport === "object";
                return (
                  <TableRow key={source.id}>
                    <TableCell className="font-medium">{source.name}</TableCell>
                    <TableCell className="font-mono text-xs">
                      {source.spec.port === 443
                        ? source.spec.host
                        : `${source.spec.host}:${source.spec.port}`}
                    </TableCell>
                    <TableCell>{source.spec.user}</TableCell>
                    <TableCell>
                      {status.kind
                        ? `${t(`vmware.source.kind.${status.kind}`)} ${status.version ?? ""}`.trim()
                        : "—"}
                    </TableCell>
                    <TableCell>
                      {t(vddk ? "vmware.source.transportVddk" : "vmware.source.transportFolder")}
                    </TableCell>
                    <TableCell>
                      <span
                        className="font-mono text-xs text-muted-foreground"
                        title={source.spec.fingerprint}
                      >
                        {shortFingerprint(source.spec.fingerprint)}
                      </span>
                    </TableCell>
                    <TableCell>
                      {status.checked_at
                        ? formatTime(new Date(status.checked_at * 1000), i18n.language)
                        : "—"}
                    </TableCell>
                    <TableCell>
                      <SourceState source={source} />
                    </TableCell>
                    {admin && (
                      <TableCell className="text-right whitespace-nowrap">
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          disabled={checking === source.id}
                          aria-label={t("vmware.source.checkNamed", { name: source.name })}
                          onClick={() => void check(source)}
                        >
                          <RefreshCw />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t("vmware.source.editNamed", { name: source.name })}
                          onClick={() => setEditing(source)}
                        >
                          <Pencil />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t("common.deleteNamed", { name: source.name })}
                          onClick={() => setDeleting(source)}
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
        </div>
      )}
      {adding && <SourceDialog taken={items.map((s) => s.name)} onClose={() => setAdding(false)} />}
      {editing && (
        <SourceDialog
          source={editing}
          taken={items.map((s) => s.name)}
          onClose={() => setEditing(null)}
        />
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("vmware.source.deleteTitle", { name: deleting?.name })}
        description={t("vmware.source.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const source = deleting!;
          try {
            await call(
              api.DELETE("/vmware-sources/{key}", { params: { path: { key: source.id } } }),
            );
          } catch (err) {
            throw localized(t, err);
          }
          await refreshVmware(queryClient);
          toast.success(t("vmware.source.deleted", { name: source.name }));
        }}
      />
    </div>
  );
}
