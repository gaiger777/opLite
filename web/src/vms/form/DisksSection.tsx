import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { StoragePool, Volume } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { type DiskRow, emptyDisk, type VmFormValues } from "@/vms/form";
import { type DeviceMode, liveRemovable, savedCount } from "@/vms/hotplug";

/**
 * 디스크 행: 있는 볼륨을 붙이거나(고치기·만들기) 새 빈 볼륨을 만든다(만들기). 실행 중(`live`)에는 끝에 붙이기와
 * 실행 중 붙인 것을 끝에서부터 떼기만 한다.
 */
export function DisksSection({
  values,
  errors,
  onChange,
  onRowsChange,
  volumes,
  pools,
  creating,
  mode,
  saved,
  boot,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 행을 넣거나 뺀다. 행 번호로 단 오류가 다른 행을 가리키므로 오류도 지운다. */
  onRowsChange: (disks: DiskRow[]) => void;
  /** 붙일 수 있는 볼륨 */
  volumes: Volume[];
  pools: StoragePool[] | undefined;
  creating: boolean;
  /** 디스크를 바꾸는 방식(`deviceMode`). `restart`·`locked`면 바꾸지 못한다. */
  mode: DeviceMode;
  /** 저장된(도는 QEMU에 붙은) 볼륨 이름. `live`에서만 본다. */
  saved: readonly string[];
  /** 켤 때 단 디스크 수. 그 뒤의 것만 실행 중에 뗄 수 있다. */
  boot: number;
}) {
  const { t } = useTranslation();
  const locked = mode === "locked" || mode === "restart";
  const live = mode === "live";
  // 실행 중이면 저장된 행은 고치지 못하고, 폼에서 더한 행만 고친다.
  const savedRows = live
    ? savedCount(values.disks, saved, (d, volume) => d.kind === "existing" && d.volume === volume)
    : 0;
  const setDisk = (i: number, patch: Partial<DiskRow>) =>
    onChange({ disks: values.disks.map((d, j) => (j === i ? { ...d, ...patch } : d)) });
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vms.disks")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {live && <p className="text-sm text-muted-foreground">{t("vmForm.disksLive")}</p>}
        {values.disks.map((disk, i) => {
          // 실행 중 저장된 행: 고치지 못하고, 실행 중 붙인 마지막 것만 뺄 수 있다.
          const fixed = live && i < savedRows;
          const removable = !live || liveRemovable(i, savedRows, boot);
          return (
            <fieldset
              key={i}
              disabled={locked || (fixed && !removable)}
              aria-label={t("vmForm.diskN", { n: i + 1 })}
              className="grid grid-cols-2 items-end gap-2 rounded-md border p-3 sm:grid-cols-5"
            >
              {disk.kind === "existing" ? (
                <Field
                  id={`disk-${i}-volume`}
                  label={t("vms.volume")}
                  error={errors[`disks.${i}.volume`]}
                >
                  {(a11y) => (
                    <NativeSelect
                      id={`disk-${i}-volume`}
                      {...a11y}
                      className="w-full"
                      disabled={fixed}
                      value={disk.volume}
                      onChange={(e) => setDisk(i, { volume: e.target.value })}
                    >
                      <NativeSelectOption value="">—</NativeSelectOption>
                      {volumes
                        .filter(
                          (v) =>
                            v.name === disk.volume ||
                            !values.disks.some((d) => d.volume === v.name),
                        )
                        .map((v) => (
                          <NativeSelectOption key={v.id} value={v.name}>
                            {v.name}
                          </NativeSelectOption>
                        ))}
                    </NativeSelect>
                  )}
                </Field>
              ) : (
                <>
                  <Field
                    id={`disk-${i}-volume`}
                    label={t("vmForm.newVolume")}
                    error={errors[`disks.${i}.volume`]}
                  >
                    {(a11y) => (
                      <Input
                        id={`disk-${i}-volume`}
                        {...a11y}
                        value={disk.volume}
                        onChange={(e) => setDisk(i, { volume: e.target.value })}
                      />
                    )}
                  </Field>
                  <Field
                    id={`disk-${i}-pool`}
                    label={t("vmForm.pool")}
                    error={errors[`disks.${i}.pool`]}
                  >
                    {(a11y) => (
                      <NativeSelect
                        id={`disk-${i}-pool`}
                        {...a11y}
                        className="w-full"
                        value={disk.pool}
                        onChange={(e) => setDisk(i, { pool: e.target.value })}
                      >
                        <NativeSelectOption value="">—</NativeSelectOption>
                        {pools?.map((p) => (
                          <NativeSelectOption key={p.id} value={p.name}>
                            {p.name}
                          </NativeSelectOption>
                        ))}
                      </NativeSelect>
                    )}
                  </Field>
                  <Field
                    id={`disk-${i}-size`}
                    label={t("vmForm.sizeGib")}
                    error={errors[`disks.${i}.size`]}
                  >
                    {(a11y) => (
                      <Input
                        id={`disk-${i}-size`}
                        {...a11y}
                        type="number"
                        min={1}
                        value={disk.sizeGib}
                        onChange={(e) => setDisk(i, { sizeGib: e.target.value })}
                      />
                    )}
                  </Field>
                </>
              )}
              <Field id={`disk-${i}-bus`} label={t("vms.bus")}>
                <NativeSelect
                  id={`disk-${i}-bus`}
                  className="w-full"
                  disabled={fixed}
                  value={disk.bus}
                  onChange={(e) => setDisk(i, { bus: e.target.value as DiskRow["bus"] })}
                >
                  <NativeSelectOption value="virtio-blk">virtio-blk</NativeSelectOption>
                  {/* 실행 중에는 virtio-blk만 붙인다(서버의 409). */}
                  <NativeSelectOption value="sata" disabled={live && !fixed}>
                    sata
                  </NativeSelectOption>
                </NativeSelect>
              </Field>
              {/* 부팅 순서는 부팅 순서 카드에서 디스크·CD-ROM을 함께 정한다. */}
              <div className="flex items-end gap-2">
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  aria-label={t("vmForm.removeDisk", { n: i + 1 })}
                  disabled={!removable}
                  onClick={() => onRowsChange(values.disks.filter((_, j) => j !== i))}
                >
                  <X />
                </Button>
              </div>
            </fieldset>
          );
        })}
        {!locked && (
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                onRowsChange([
                  ...values.disks,
                  { ...emptyDisk("", values.disks.length), kind: "existing" },
                ])
              }
            >
              <Plus />
              {t("vmForm.addExisting")}
            </Button>
            {creating && (
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() =>
                  onRowsChange([...values.disks, emptyDisk(values.name, values.disks.length)])
                }
              >
                <Plus />
                {t("vmForm.addNew")}
              </Button>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
