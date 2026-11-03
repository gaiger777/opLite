import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Backup, Task } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { storedBytes } from "@/backups/backups";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes, formatTime } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";
import { validName } from "@/vms/form";

const PHASE_VARIANT = {
  pending: "secondary",
  ready: "default",
  failed: "destructive",
} as const;

/**
 * 백업 표와 복원·지우기. VM 상세의 백업 탭과 백업 화면이 쓴다. `showVm`이면 VM 열을 보인다(VM을
 * 지웠으면 만든 때의 이름만).
 */
export function BackupTable({
  backups,
  query,
  labelledBy,
  showVm = false,
}: {
  backups: Backup[];
  query: { isPending: boolean; isError: boolean; error: unknown };
  labelledBy: string;
  showVm?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const vms = useList("vm");
  const [restoring, setRestoring] = useState<Backup | null>(null);
  // 닫히는 동안에도 대상을 남겨 두어 대화상자의 글이 바뀌지 않게 한다.
  const [deleting, setDeleting] = useState<Backup | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  // 트리거 없이 연 대화상자는 닫힐 때 포커스를 돌려줄 곳을 모르므로 연 버튼을 기억한다.
  const opener = useRef<HTMLElement | null>(null);
  // 복원은 새 VM과 볼륨을 만든다(서버는 백업·VM·볼륨 쓰기를 모두 본다).
  const restore =
    can(me.role, "write", "backup") &&
    can(me.role, "write", "vm") &&
    can(me.role, "write", "volume");
  const remove = can(me.role, "write", "backup");
  const actions = restore || remove;
  const columns = 5 + (showVm ? 1 : 0) + (actions ? 1 : 0);
  const vmById = new Map((vms.data ?? []).map((v) => [v.id, v]));

  return (
    <>
      <Table aria-labelledby={labelledBy}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            {showVm && <TableHead>{t("backups.vm")}</TableHead>}
            <TableHead>{t("backups.pool")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("backups.created")}</TableHead>
            <TableHead>{t("backups.size")}</TableHead>
            {actions && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {backups.length === 0 && (
            <TableMessage colSpan={columns} query={query} empty={t("backups.empty")} />
          )}
          {backups.map((backup) => {
            const vm = vmById.get(backup.spec.vm);
            const size = storedBytes(backup);
            const { phase, created_at: created, error } = backup.status;
            return (
              <TableRow key={backup.id}>
                <TableCell className="font-medium">{backup.name}</TableCell>
                {showVm && (
                  <TableCell>
                    {vm ? (
                      <Link
                        to={`/vms/${encodeURIComponent(vm.name)}`}
                        className="underline-offset-4 hover:underline"
                      >
                        {vm.name}
                      </Link>
                    ) : (
                      backup.spec.vm_name
                    )}
                  </TableCell>
                )}
                <TableCell>{backup.spec.pool}</TableCell>
                <TableCell>
                  <Badge variant={PHASE_VARIANT[phase]}>{t(`backups.phase.${phase}`)}</Badge>
                  {error && (
                    <div className="max-w-xs text-xs whitespace-normal break-words text-destructive">
                      {error}
                    </div>
                  )}
                </TableCell>
                <TableCell>
                  {created !== null ? formatTime(new Date(created * 1000), i18n.language) : "—"}
                </TableCell>
                <TableCell>{size !== undefined ? formatBytes(size) : "—"}</TableCell>
                {actions && (
                  <TableCell className="text-right whitespace-nowrap">
                    {restore && (
                      <Button
                        variant="outline"
                        size="sm"
                        aria-label={t("backups.restoreNamed", { name: backup.name })}
                        disabled={phase !== "ready"}
                        onClick={(e) => {
                          opener.current = e.currentTarget;
                          setRestoring(backup);
                        }}
                      >
                        {t("backups.restore")}
                      </Button>
                    )}
                    {/* 실패했거나 Task 없이 남은 백업도 지운다. 진행 중인 Task가 있으면 서버가 거절한다. */}
                    {remove && (
                      <Button
                        variant="ghost"
                        size="sm"
                        className="ml-2"
                        aria-label={t("backups.deleteNamed", { name: backup.name })}
                        onClick={(e) => {
                          opener.current = e.currentTarget;
                          setDeleting(backup);
                          setDeleteOpen(true);
                        }}
                      >
                        {t("common.delete")}
                      </Button>
                    )}
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {restoring && (
        <RestoreDialog backup={restoring} returnFocus={opener} onClose={() => setRestoring(null)} />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("backups.deleteTitle", { name: deleting?.name })}
        description={t("backups.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const backup = deleting!;
          // `force` 없이 지우면 응답은 늘 Task다(200 `{ message }`는 force만).
          const task = (await call(
            api.DELETE("/backups/{key}", { params: { path: { key: backup.id } } }),
          )) as Task;
          trackTask(task, t("backups.op.delete", { name: backup.name }));
        }}
      />
    </>
  );
}

/** 백업에서 새 VM(꺼짐)을 만드는 대화상자. 열 때만 그린다. */
function RestoreDialog({
  backup,
  returnFocus,
  onClose,
}: {
  backup: Backup;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const pools = useList("storage-pool");
  const [name, setName] = useState("");
  const [pool, setPool] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    } else if ((vms.data ?? []).some((v) => v.name === name)) {
      found.name = "backups.errors.vmExists";
    }
    if (pool === "") {
      found.pool = "backups.errors.pool";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/backups/{key}/restore", {
        params: { path: { key: backup.id } },
        body: { name, pool },
      }),
    );
    trackTask(task, t("backups.op.restore", { backup: backup.name, name }));
  }

  return (
    <FormDialog
      title={t("backups.restoreTitle", { name: backup.name })}
      description={t("backups.restoreDescription")}
      submitLabel={t("backups.restore")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="restore-name" label={t("backups.newName")} error={errors.name}>
        {(a11y) => (
          <Input
            id="restore-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      {!pools.isSuccess && <ListPending error={pools.error} />}
      <Field id="restore-pool" label={t("backups.diskPool")} error={errors.pool}>
        {(a11y) => (
          <NativeSelect
            id="restore-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">{t("backups.choosePool")}</NativeSelectOption>
            {(pools.data ?? []).map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
    </FormDialog>
  );
}
