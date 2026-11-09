import { Archive } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { backupPools, newestFirst } from "@/backups/backups";
import { BackupTable } from "@/backups/BackupTable";
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
  const me = usePrincipal();
  const backups = useList("backup");
  const pools = useList("storage-pool");
  const [creating, setCreating] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const mine = newestFirst((backups.data ?? []).filter((b) => b.spec.vm === vm.id));
  // 서버는 시작·정지 중인 VM을 백업하지 않는다(vm_busy).
  const busy = isBusy(vm);

  return (
    <section className="flex flex-col gap-4 pt-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="vm-backups" className="font-medium">
          {t("vms.tab.backups")}
        </h2>
        {can(me.role, "write", "vm") && (
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
      {can(me.role, "write", "vm") && busy && (
        <p className="text-sm text-muted-foreground">{t("backups.busy")}</p>
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
  const pools = backupPools(useList("storage-pool").data ?? []);
  const [chosen, setChosen] = useState<string | null>(null);
  // 고르지 않았으면 첫 백업 풀이다.
  const pool = chosen ?? pools[0]?.name ?? "";
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
        body: { pool, ...(name !== "" && { name }), ...(incremental && { incremental }) },
      }),
    );
    trackTask(task, t("backups.op.create", { vm: vm.name }));
  }

  return (
    <FormDialog
      title={t("backups.createTitle", { name: vm.name })}
      description={t(isStopped(vm) ? "backups.createDescription" : "backups.runningDescription")}
      submitLabel={t("backups.create")}
      submitDisabled={pools.length === 0}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {pools.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("backups.noPools")}</p>
      ) : (
        <Field id="backup-pool" label={t("backups.pool")}>
          <NativeSelect
            id="backup-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setChosen(e.target.value)}
          >
            {pools.map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
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
          {t("backups.incrementalHint")}
        </p>
      </div>
    </FormDialog>
  );
}
