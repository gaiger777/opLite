import { CircleAlert, CircleCheck, LoaderCircle } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Backup, Task } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { chainBytes, chainReplicas, storedBytes } from "@/backups/backups";
import { type RemoteTarget, RemoteTargetFields } from "@/backups/RemoteTargetFields";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { type Column, DataTable } from "@/components/data-table/DataTable";
import { useTableState } from "@/components/data-table/state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { codeMessage } from "@/i18n/errors";
import { formatBytes, formatTime } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";
import { validName } from "@/vms/form";

const PHASE_VARIANT = {
  pending: "secondary",
  ready: "default",
  failed: "destructive",
  receiving: "secondary",
} as const;

type Replica = NonNullable<Backup["status"]["replicas"]>[number];

const REPLICA_ICON = {
  ready: CircleCheck,
  copying: LoaderCircle,
  failed: CircleAlert,
} as const;

/**
 * 원격 사본 하나: 상태 아이콘과 원격 이름. 풀·상태·오류는 툴팁(title)에, 풀과 상태는 화면 읽기용 글에도
 * 둔다(실패한 사본의 오류는 표가 사본 아래에 글로 보인다).
 */
function ReplicaBadge({ replica }: { replica: Replica }) {
  const { t } = useTranslation();
  const Icon = REPLICA_ICON[replica.state];
  const state = t(`backups.replicaState.${replica.state}`);
  const title = t("backups.replicaTitle", { remote: replica.remote, pool: replica.pool, state });
  return (
    <span
      className={`inline-flex items-center gap-1 ${replica.state === "failed" ? "text-destructive" : ""}`}
      title={replica.error ? `${title}. ${replica.error}` : title}
    >
      <Icon className="size-4" aria-hidden />
      <span aria-hidden>{replica.remote}</span>
      <span className="sr-only">{title}</span>
    </span>
  );
}

/**
 * 백업 표와 복원·원격 복제·지우기. VM 상세의 백업 탭과 백업 화면이 쓴다. `showVm`이면 VM 열을 보인다(VM을
 * 지웠으면 만든 때의 이름만). 작업 열은 백업을 만든 백업 작업이다(손으로 만든 백업은 비어 있다). 받은 사본은
 * 이름 옆에 보낸 클러스터를, 원격 사본 열은 원격으로 보낸 사본의 상태를 보이고 증분이면 원격 사본마다 체인의
 * 몇 백업이 원격에 있는지도 보인다(복제 중이 아니어도 보이는 상태, 다 있으면 뺀다). 종류 열은 전체·증분과
 * 증분의 부모(같은 표의 그 행으로 가는 링크), 증분을 청했는데 전체로 한 이유를 보이고, 증분의 크기 열은 자기
 * 파일 크기 아래에 복원에 드는 체인 전체 크기를 보인다(부모는 같은 VM의 백업이라 표에 있다).
 */
export function BackupTable({
  backups,
  query,
  labelledBy,
  showVm = false,
  url = false,
}: {
  backups: Backup[];
  query: { isPending: boolean; isError: boolean; error: unknown };
  labelledBy: string;
  showVm?: boolean;
  /** 화면의 주 목록이면 검색·정렬을 주소에 두고 목록 단축키를 받는다. */
  url?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const vms = useList("vm");
  const jobs = useList("backup-job");
  const [restoring, setRestoring] = useState<Backup | null>(null);
  const [replicating, setReplicating] = useState<Backup | null>(null);
  // 닫히는 동안에도 대상을 남겨 두어 대화상자의 글이 바뀌지 않게 한다.
  const [deleting, setDeleting] = useState<Backup | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  // 트리거 없이 연 대화상자는 닫힐 때 포커스를 돌려줄 곳을 모르므로 연 버튼을 기억한다.
  const opener = useRef<HTMLElement | null>(null);
  // 복원은 원래 VM의 backup.restore와 새 VM을 둘 곳의 vm.create다(계획 62, 폴더마다 다르므로 서버가 막는다).
  const restore = perms.anywhere("backup.restore") && perms.anywhere("vm.create");
  const remove = perms.anywhere("backup.delete");
  const actions = restore || remove;
  const state = useTableState("backups", [], url);
  const vmById = new Map((vms.data ?? []).map((v) => [v.id, v]));
  const originHint = (origin: NonNullable<Backup["spec"]["origin"]>) =>
    t(origin.job ? "backups.originJobHint" : "backups.originHint", {
      cluster: origin.cluster,
      backup: origin.backup,
      job: origin.job,
    });
  const jobName = (id: string | null | undefined) => {
    if (!id) {
      return "—";
    }
    const job = (jobs.data ?? []).find((j) => j.id === id);
    // 작업을 지워도 백업은 작업 id를 그대로 가진다.
    return job ? job.name : jobs.isSuccess ? t("backups.jobDeleted") : "—";
  };

  /** 행 하나를 그릴 값 */
  const rowInfo = (backup: Backup) => {
    const { phase, created_at: created, error, replicas = [] } = backup.status;
    const { parent: parentId, full_reason: fullReason } = backup.status;
    return {
      vm: vmById.get(backup.spec.vm),
      size: storedBytes(backup),
      phase,
      created,
      error,
      replicas,
      origin: backup.spec.origin,
      parentId,
      fullReason,
      parent: parentId ? backups.find((b) => b.id === parentId) : undefined,
      chain: parentId ? chainBytes(backup, backups) : undefined,
      // 체인이 다 원격에 있으면 진행 줄을 뺀다.
      chainCopies: chainReplicas(backup, backups).filter((c) => c.sent < c.total),
    };
  };
  const columns: Column<Backup>[] = [
    {
      id: "name",
      header: t("common.name"),
      value: (backup) => backup.name,
      className: "font-medium",
      cell: (backup) => {
        const { origin } = rowInfo(backup);
        return (
          <>
            {backup.name}
            {/* 다른 클러스터에서 받은 사본 */}
            {origin && (
              <Badge variant="outline" className="ml-2" title={originHint(origin)}>
                {t("backups.origin", { cluster: origin.cluster })}
                <span className="sr-only">{originHint(origin)}</span>
              </Badge>
            )}
          </>
        );
      },
    },
    ...(showVm
      ? ([
          {
            id: "vm",
            header: t("backups.vm"),
            priority: 2,
            cell: (backup) => {
              const { vm } = rowInfo(backup);
              return (
                <>
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
                </>
              );
            },
          },
        ] satisfies Column<Backup>[])
      : []),
    {
      id: "pool",
      header: t("backups.pool"),
      value: (backup) => backup.spec.pool,
      priority: 2,
      cell: (backup) => backup.spec.pool,
    },
    {
      id: "job",
      header: t("backups.job"),
      priority: 3,
      cell: (backup) => jobName(backup.spec.job),
    },
    {
      id: "state",
      header: t("common.state"),
      value: (backup) => backup.status.phase,
      cell: (backup) => {
        const { phase, error } = rowInfo(backup);
        return (
          <>
            <Badge variant={PHASE_VARIANT[phase]}>{t(`backups.phase.${phase}`)}</Badge>
            {error && (
              <div className="max-w-xs text-xs whitespace-normal break-words text-destructive">
                {error}
              </div>
            )}
          </>
        );
      },
    },
    {
      id: "created",
      header: t("backups.created"),
      value: (backup) => backup.status.created_at ?? 0,
      cell: (backup) => {
        const { created } = rowInfo(backup);
        return <>{created !== null ? formatTime(new Date(created * 1000), i18n.language) : "—"}</>;
      },
    },
    {
      id: "size",
      header: t("backups.size"),
      value: (backup) => storedBytes(backup) ?? -1,
      priority: 2,
      cell: (backup) => {
        const { size, chain } = rowInfo(backup);
        return (
          <>
            {size !== undefined ? formatBytes(size) : "—"}
            {chain !== undefined && (
              <div className="text-xs text-muted-foreground">
                {t("backups.chainSize", { size: formatBytes(chain) })}
              </div>
            )}
          </>
        );
      },
    },
    {
      id: "type",
      header: t("backups.type"),
      priority: 3,
      cell: (backup) => {
        const { parentId, fullReason, parent } = rowInfo(backup);
        return (
          <>
            <Badge variant="outline">
              {t(parentId ? "backups.kind.incremental" : "backups.kind.full")}
            </Badge>
            {parentId && (
              <div className="text-xs text-muted-foreground">
                {t("backups.parent")}{" "}
                {parent ? (
                  <a href={`#backup-${parent.id}`} className="underline-offset-4 hover:underline">
                    {parent.name}
                  </a>
                ) : (
                  parentId
                )}
              </div>
            )}
            {fullReason && (
              <div className="max-w-xs text-xs whitespace-normal break-words text-muted-foreground">
                {t("backups.fullReason", { reason: fullReason })}
              </div>
            )}
          </>
        );
      },
    },
    {
      id: "replicas",
      header: t("backups.replicas"),
      priority: 3,
      cell: (backup) => {
        const { replicas, chainCopies } = rowInfo(backup);
        return (
          <>
            {replicas.length === 0 && chainCopies.length === 0 ? (
              "—"
            ) : (
              <>
                {replicas.length > 0 && (
                  <div className="flex flex-wrap gap-2">
                    {replicas.map((r) => (
                      <ReplicaBadge key={`${r.remote}/${r.pool}`} replica={r} />
                    ))}
                  </div>
                )}
                {replicas
                  .filter((r) => r.state === "failed" && r.error)
                  .map((r) => (
                    <div
                      key={`${r.remote}/${r.pool}`}
                      className="max-w-xs text-xs whitespace-normal break-words text-destructive"
                    >
                      {t("backups.replicaError", {
                        remote: r.remote,
                        pool: r.pool,
                        error: r.error,
                      })}
                    </div>
                  ))}
                {/* 증분은 복제가 체인을 전체 백업부터 차례로 보낸다. */}
                {chainCopies.map((c) => (
                  <div
                    key={`chain-${c.remote}/${c.pool}`}
                    className="text-xs text-muted-foreground"
                  >
                    {t("backups.chainSent", {
                      remote: c.remote,
                      pool: c.pool,
                      sent: c.sent,
                      total: c.total,
                    })}
                  </div>
                ))}
              </>
            )}
          </>
        );
      },
    },
    ...(actions
      ? ([
          {
            id: "menu",
            header: t("vms.actions"),
            actions: true,
            cell: (backup) => {
              const { phase } = rowInfo(backup);
              return (
                <>
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
                  {/* 복제 API는 백업 쓰기 권한이다(지우기와 같다). 원격 쪽 권한은 원격 토큰의 역할이다. */}
                  {remove && (
                    <Button
                      variant="outline"
                      size="sm"
                      className="ml-2"
                      aria-label={t("backups.replicateNamed", { name: backup.name })}
                      disabled={phase !== "ready"}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setReplicating(backup);
                      }}
                    >
                      {t("backups.replicate")}
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
                </>
              );
            },
          },
        ] satisfies Column<Backup>[])
      : []),
  ];

  return (
    <>
      <DataTable
        id="backups"
        rows={backups}
        columns={columns}
        rowId={(backup) => backup.id}
        rowName={(backup) => backup.name}
        query={query}
        empty={t("backups.empty")}
        labelledBy={labelledBy}
        state={url ? state : undefined}
        search={url ? (backup) => `${backup.name} ${backup.spec.vm_name}` : undefined}
        keys={url}
        rowProps={(backup) => ({ id: `backup-${backup.id}` })}
      />
      {restoring && (
        <RestoreDialog backup={restoring} returnFocus={opener} onClose={() => setRestoring(null)} />
      )}
      {replicating && (
        <ReplicateDialog
          backup={replicating}
          returnFocus={opener}
          onClose={() => setReplicating(null)}
        />
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

/** 백업을 원격 클러스터의 백업 풀에 사본으로 보내는 대화상자. 열 때만 그린다. */
function ReplicateDialog({
  backup,
  returnFocus,
  onClose,
}: {
  backup: Backup;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [target, setTarget] = useState<RemoteTarget>({ remote: "", pool: "" });
  const [errors, setErrors] = useState<{ remote?: string; pool?: string }>({});

  async function submit() {
    const found: { remote?: string; pool?: string } = {};
    if (target.remote === "") {
      found.remote = "backups.errors.remote";
    } else if (target.pool === "") {
      found.pool = "backups.errors.pool";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    let task;
    try {
      task = await call(
        api.POST("/backups/{key}/actions/replicate", {
          params: { path: { key: backup.id } },
          body: target,
        }),
      );
    } catch (err) {
      // 복제의 backup_busy는 체인의 다른 백업 복제일 수도 있어 이 동작의 문구로 보인다.
      throw err instanceof ApiError
        ? new Error(codeMessage(t, err.code, err.message, "backups.replicateErrors"))
        : err;
    }
    trackTask(task, t("tasks.op.backup-replicate", { target: backup.name, remote: target.remote }));
  }

  return (
    <FormDialog
      title={t("backups.replicateNamed", { name: backup.name })}
      description={t("backups.replicateDescription")}
      submitLabel={t("backups.replicateSubmit")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <RemoteTargetFields
        id="replicate"
        label={t("backups.remote")}
        none={t("backups.chooseRemote")}
        value={target}
        errors={errors}
        onChange={setTarget}
      />
    </FormDialog>
  );
}
