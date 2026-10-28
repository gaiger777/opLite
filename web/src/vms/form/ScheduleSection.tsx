import { useTranslation } from "react-i18next";

import type { Volume } from "@/api/types";
import { Field } from "@/components/Field";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import type { ScheduleRow, VmFormValues } from "@/vms/form";

/** 예약 스냅샷 */
export function ScheduleSection({
  values,
  errors,
  onChange,
  volumes,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 볼륨 목록. 아직 읽지 못했으면 `undefined`다. */
  volumes: Volume[] | undefined;
}) {
  const { t } = useTranslation();
  const setSchedule = (patch: Partial<ScheduleRow>) =>
    onChange({ snapshotSchedule: { ...values.snapshotSchedule, ...patch } });
  // 예약을 켰는데 붙은 볼륨 중 qcow2가 아닌 것이 있으면 예약 스냅샷이 실패한다. 볼륨 목록을 읽어야 안다.
  // 새로 만드는 디스크는 늘 qcow2다.
  const rawDisk =
    values.snapshotSchedule.every !== "off" && volumes
      ? values.disks
          .filter((d) => d.kind === "existing")
          .map((d) => volumes.find((v) => v.name === d.volume))
          .find((v) => v !== undefined && v.spec.format !== "qcow2")
      : undefined;
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vmForm.schedule")}</CardTitle>
      </CardHeader>
      <CardContent className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        <Field
          id="vm-schedule-every"
          label={t("vmForm.scheduleEvery")}
          hint={t("vmForm.scheduleNote")}
        >
          {(a11y) => (
            <NativeSelect
              id="vm-schedule-every"
              {...a11y}
              className="w-full"
              value={values.snapshotSchedule.every}
              onChange={(e) => setSchedule({ every: e.target.value as ScheduleRow["every"] })}
            >
              <NativeSelectOption value="off">{t("vmForm.scheduleOff")}</NativeSelectOption>
              <NativeSelectOption value="hourly">{t("snapshots.every.hourly")}</NativeSelectOption>
              <NativeSelectOption value="daily">{t("snapshots.every.daily")}</NativeSelectOption>
              <NativeSelectOption value="weekly">{t("snapshots.every.weekly")}</NativeSelectOption>
            </NativeSelect>
          )}
        </Field>
        {rawDisk && (
          <p className="text-sm text-destructive sm:col-span-2">
            {t("vmForm.scheduleRaw", { volume: rawDisk.name, format: rawDisk.spec.format })}
          </p>
        )}
        {/* 끄면 보관 개수와 메모리는 쓰지 않으므로 묻지 않는다. */}
        {values.snapshotSchedule.every !== "off" && (
          <>
            <Field
              id="vm-schedule-keep"
              label={t("vmForm.scheduleKeep")}
              error={errors["schedule.keep"]}
            >
              {(a11y) => (
                <Input
                  id="vm-schedule-keep"
                  {...a11y}
                  type="number"
                  min={1}
                  max={100}
                  value={values.snapshotSchedule.keep}
                  onChange={(e) => setSchedule({ keep: e.target.value })}
                />
              )}
            </Field>
            <div className="flex flex-col gap-1.5 sm:col-span-2">
              <div className="flex items-center gap-2">
                <Checkbox
                  id="vm-schedule-memory"
                  aria-describedby="vm-schedule-memory-hint"
                  checked={values.snapshotSchedule.memory}
                  onCheckedChange={(v) => setSchedule({ memory: v === true })}
                />
                <Label htmlFor="vm-schedule-memory">{t("snapshots.includeMemory")}</Label>
              </div>
              <p id="vm-schedule-memory-hint" className="text-xs text-muted-foreground">
                {t("vmForm.scheduleMemoryHint")}
              </p>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}
