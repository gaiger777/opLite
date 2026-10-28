import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { StoragePool, Volume } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { type DiskRow, emptyDisk, type VmFormValues } from "@/vms/form";

/** 디스크 행: 있는 볼륨을 붙이거나(고치기·만들기) 새 빈 볼륨을 만든다(만들기). */
export function DisksSection({
  values,
  errors,
  onChange,
  onRowsChange,
  volumes,
  pools,
  creating,
  locked,
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
  /** 멈추지 않은 VM의 디스크는 바꾸지 못한다. */
  locked: boolean;
}) {
  const { t } = useTranslation();
  const setDisk = (i: number, patch: Partial<DiskRow>) =>
    onChange({ disks: values.disks.map((d, j) => (j === i ? { ...d, ...patch } : d)) });
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vms.disks")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {values.disks.map((disk, i) => (
          <fieldset
            key={i}
            disabled={locked}
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
                    value={disk.volume}
                    onChange={(e) => setDisk(i, { volume: e.target.value })}
                  >
                    <NativeSelectOption value="">—</NativeSelectOption>
                    {volumes
                      .filter(
                        (v) =>
                          v.name === disk.volume || !values.disks.some((d) => d.volume === v.name),
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
                value={disk.bus}
                onChange={(e) => setDisk(i, { bus: e.target.value as DiskRow["bus"] })}
              >
                <NativeSelectOption value="virtio-blk">virtio-blk</NativeSelectOption>
                <NativeSelectOption value="sata">sata</NativeSelectOption>
              </NativeSelect>
            </Field>
            <div className="flex items-end gap-2">
              <Field
                id={`disk-${i}-boot`}
                label={t("vms.bootOrder")}
                error={errors[`disks.${i}.bootOrder`]}
              >
                {(a11y) => (
                  <Input
                    id={`disk-${i}-boot`}
                    {...a11y}
                    type="number"
                    min={1}
                    max={255}
                    className="w-20"
                    value={disk.bootOrder}
                    onChange={(e) => setDisk(i, { bootOrder: e.target.value })}
                  />
                )}
              </Field>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={t("vmForm.removeDisk", { n: i + 1 })}
                onClick={() => onRowsChange(values.disks.filter((_, j) => j !== i))}
              >
                <X />
              </Button>
            </div>
          </fieldset>
        ))}
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
