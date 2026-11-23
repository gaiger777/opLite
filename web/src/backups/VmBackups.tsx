import { Archive } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePermissions, useVmCan } from "@/auth/usePermissions";
import { backupPools, newestFirst } from "@/backups/backups";
import { BackupTable } from "@/backups/BackupTable";
import { type BackupStore, useBackupStores } from "@/backupStores/stores";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { trackTask } from "@/tasks/tasks";
import { isBusy, isStopped } from "@/vms/actions";
import { validName } from "@/vms/form";

/** VM 상세의 백업 탭: 이 VM의 백업 목록과 지금 백업하기 */
export function VmBackups({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const can = useVmCan(vm);
  const backups = useList("backup");
  const pools = useList("storage-pool");
  const [creating, setCreating] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const mine = newestFirst((backups.data ?? []).filter((b) => b.spec.vm === vm.id));
  // 서버는 시작·정지 중인 VM을 백업하지 않는다(vm_busy). 라이브 복원 중에도 받지 않는다(vm_restoring, 계획 68).
  const restoring = !!vm.status.live_restore;
  const busy = isBusy(vm) || restoring;

  return (
    <section className="flex flex-col gap-4 pt-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="vm-backups" className="font-medium">
          {t("vms.tab.backups")}
        </h2>
        {can("backup.create") && (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            {/* 풀 목록을 읽어야 백업 풀을 고른다. */}
            {!pools.isSuccess && <ListPending error={pools.error} />}
            <Button
              size="sm"
              disabled={busy || !pools.isSuccess || !backups.isSuccess}
              onClick={(e) => {
                opener.current = e.currentTarget;
                setCreating(true);
              }}
            >
              <Archive />
              {t("backups.create")}
            </Button>
          </div>
        )}
      </div>
      {can("backup.create") && busy && (
        <p className="text-sm text-muted-foreground">
          {t(restoring ? "liveRestore.blockedBackup" : "backups.busy")}
        </p>
      )}
      <BackupTable backups={mine} query={backups} labelledBy="vm-backups" />
      {creating && (
        <CreateBackupDialog
          vm={vm}
          taken={(backups.data ?? []).map((b) => b.name)}
          returnFocus={opener}
          onClose={() => setCreating(false)}
        />
      )}
    </section>
  );
}

function CreateBackupDialog({
  vm,
  taken,
  returnFocus,
  onClose,
}: {
  vm: Vm;
  /** 클러스터의 백업 이름(이름은 VM마다가 아니라 클러스터에서 고유하다) */
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const pools = backupPools(useList("storage-pool").data ?? []);
  // 백업을 받을 수 있는 저장소(계획 68 — 준비됐고 읽기 전용이 아님). 없으면 대상 고르기는 지금처럼 백업 풀만이다.
  const stores: BackupStore[] = (useBackupStores(perms.reads("backup-store")).data ?? []).filter(
    (s) => s.status.phase === "ready" && !s.spec.read_only,
  );
  const [chosen, setChosen] = useState<string | null>(null);
  // 고르지 않았으면 첫 백업 풀(없으면 첫 저장소)이다. 값은 `pool:<이름>`·`store:<이름>`이다.
  const target =
    chosen ?? (pools[0] ? `pool:${pools[0].name}` : stores[0] ? `store:${stores[0].name}` : "");
  const colon = target.indexOf(":");
  const kind = target.slice(0, colon);
  const targetName = target.slice(colon + 1);
  const [name, setName] = useState("");
  const [incremental, setIncremental] = useState(false);
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    // 비우면 서버가 `{VM 이름}-{UTC 시각}`으로 정한다.
    if (name !== "" && !validName(name)) {
      found.name = "forms.name";
    } else if (taken.includes(name)) {
      found.name = "backups.errors.exists";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/vms/{key}/backups", {
        params: { path: { key: vm.id } },
        // 증분은 청할 때만 보낸다(옛 서버는 모르는 필드를 거절한다).
        body: {
          ...(kind === "store" ? { store: targetName } : { pool: targetName }),
          ...(name !== "" && { name }),
          ...(incremental && { incremental }),
        },
      }),
    );
    trackTask(task, t("backups.op.create", { vm: vm.name }));
  }

  return (
    <FormDialog
      title={t("backups.createTitle", { name: vm.name })}
      description={t(isStopped(vm) ? "backups.createDescription" : "backups.runningDescription")}
      submitLabel={t("backups.create")}
      submitDisabled={target === ""}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {target === "" ? (
        <p className="text-sm text-muted-foreground">{t("backups.noPools")}</p>
      ) : stores.length === 0 ? (
        <Field id="backup-pool" label={t("backups.pool")}>
          <NativeSelect
            id="backup-pool"
            className="w-full"
            value={target}
            onChange={(e) => setChosen(e.target.value)}
          >
            {pools.map((p) => (
              <NativeSelectOption key={p.id} value={`pool:${p.name}`}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
      ) : (
        // 백업 풀 | 저장소(계획 68 §14 웹)
        <Field id="backup-target" label={t("backups.target")} hint={t("backups.targetHint")}>
          {(a11y) => (
            <NativeSelect
              id="backup-target"
              className="w-full"
              value={target}
              onChange={(e) => setChosen(e.target.value)}
              {...a11y}
            >
              {pools.length > 0 && (
                <optgroup label={t("backups.poolsGroup")}>
                  {pools.map((p) => (
                    <NativeSelectOption key={p.id} value={`pool:${p.name}`}>
                      {p.name}
                    </NativeSelectOption>
                  ))}
                </optgroup>
              )}
              <optgroup label={t("backups.storesGroup")}>
                {stores.map((s) => (
                  <NativeSelectOption key={s.id} value={`store:${s.name}`}>
                    {s.spec.encryption ? t("backups.encryptedStore", { name: s.name }) : s.name}
                  </NativeSelectOption>
                ))}
              </optgroup>
            </NativeSelect>
          )}
        </Field>
      )}
      <Field
        id="backup-name"
        label={t("backups.name")}
        error={errors.name}
        hint={t("backups.nameHint")}
      >
        {(a11y) => (
          <Input
            id="backup-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      {/* 할 수 없으면(꺼진 VM, 첫 백업 등) 서버가 전체로 하고 이유를 백업에 적는다. */}
      <div className="flex flex-col gap-1.5">
        <div className="flex items-center gap-2">
          <Checkbox
            id="backup-incremental"
            checked={incremental}
            aria-describedby="backup-incremental-hint"
            onCheckedChange={(on) => setIncremental(on === true)}
          />
          <Label htmlFor="backup-incremental">{t("backups.incremental")}</Label>
        </div>
        <p id="backup-incremental-hint" className="text-xs text-muted-foreground">
          {t(kind === "store" ? "backups.incrementalStoreHint" : "backups.incrementalHint")}
        </p>
      </div>
    </FormDialog>
  );
}
