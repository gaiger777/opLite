import { useQueryClient } from "@tanstack/react-query";
import { useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { CEPH_KEY, defaultSize, sizeWarning } from "@/ceph/ceph";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { cephNameError } from "@/storage/pools";
import { trackTask } from "@/tasks/tasks";
import { validName } from "@/vms/form";

/**
 * Ceph 풀을 만들어 Orbivisor RBD 풀로 등록한다(`ceph-pool-create` Task). 복제 수의 기본은 min(3, OSD가 up인 호스트
 * 수)이고(상태가 늦게 와도 사용자가 고르지 않았으면 따라간다), 1(사본 하나)은 확인해야 보낸다. `hosts`는 상태를
 * 읽지 못했으면 undefined다. 열 때만 그린다.
 */
export function CreateCephPoolDialog({
  hosts,
  returnFocus,
  onClose,
}: {
  hosts: number | undefined;
  returnFocus?: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [pool, setPool] = useState("");
  const [chosen, setChosen] = useState<number>();
  const size = chosen ?? defaultSize(hosts ?? 3);
  const [single, setSingle] = useState(false);
  const [errors, setErrors] = useState<{ name?: string; pool?: string }>({});
  const warning = sizeWarning(size);

  async function submit() {
    const found = {
      name: validName(name) ? undefined : "forms.name",
      pool: pool === "" ? undefined : cephNameError(pool),
    };
    setErrors(found);
    if (found.name || found.pool) {
      return false;
    }
    const task = await call(
      api.POST("/ceph/pools", { body: { name, size, ...(pool !== "" && { pool }) } }),
    );
    trackTask(task, t("tasks.op.ceph-pool-create", { name, pool: pool || name, size }));
    await queryClient.invalidateQueries({ queryKey: CEPH_KEY });
  }

  return (
    <FormDialog
      title={t("ceph.pool.createTitle")}
      description={t("ceph.pool.createDescription")}
      submitLabel={t("common.create")}
      submitDisabled={size === 1 && !single}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="ceph-pool-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="ceph-pool-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="ceph-pool-pool"
        label={t("ceph.pool.cephPool")}
        error={errors.pool}
        hint={t("ceph.pool.cephPoolHint")}
      >
        {(a11y) => (
          <Input
            id="ceph-pool-pool"
            placeholder={name}
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="ceph-pool-size"
        label={t("ceph.pool.size")}
        hint={hosts === undefined ? undefined : t("ceph.pool.sizeHint", { hosts })}
      >
        {(a11y) => (
          <NativeSelect
            id="ceph-pool-size"
            value={size}
            onChange={(e) => {
              setChosen(Number(e.target.value));
              setSingle(false);
            }}
            {...a11y}
          >
            {[1, 2, 3].map((n) => (
              <NativeSelectOption key={n} value={n}>
                {n}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {warning && <p className="text-sm text-warning-fg">{t(warning)}</p>}
      {size === 1 && (
        <div className="flex items-center gap-2">
          <Checkbox
            id="ceph-pool-single"
            checked={single}
            onCheckedChange={(v) => setSingle(v === true)}
          />
          <Label htmlFor="ceph-pool-single">{t("ceph.pool.singleConfirm")}</Label>
        </div>
      )}
    </FormDialog>
  );
}
