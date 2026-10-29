import { useTranslation } from "react-i18next";

import { Field } from "@/components/Field";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import type { ScheduleRow, VmFormValues } from "@/vms/form";
import type { SnapshotSupport } from "@/vms/snapshots";

/** 예약 스냅샷 */
export function ScheduleSection({
  values,
  errors,
  onChange,
  support,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 디스크의 스냅샷 가능 여부. 볼륨·풀 목록을 아직 읽지 못했으면 `undefined`다. */
  support: SnapshotSupport | undefined;
}) {
  const { t } = useTranslation();
  const setSchedule = (patch: Partial<ScheduleRow>) =>
    onChange({ snapshotSchedule: { ...values.snapshotSchedule, ...patch } });
  // 예약을 켰는데 디스크가 스냅샷을 담지 못하거나 방식이 섞이면 예약 스냅샷이 실패한다.
  const on = values.snapshotSchedule.every !== "off";
  // 블록 스냅샷은 메모리를 담지 못하므로 묻지 않는다(예약 작업이 메모리 없이 찍는다).
  const block = support?.kind === "ok" && support.block;
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
        {on && support?.kind === "format" && (
          <p className="text-sm text-destructive sm:col-span-2">
            {t("vmForm.scheduleRaw", { volume: support.disk.volume, format: support.disk.format })}
          </p>
        )}
        {on && support?.kind === "mixed" && (
          <p className="text-sm text-destructive sm:col-span-2">{t("vmForm.scheduleMixed")}</p>
        )}
        {/* 끄면 보관 개수와 메모리는 쓰지 않으므로 묻지 않는다. */}
        {on && (
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
            {block ? (
              <p className="text-xs text-muted-foreground sm:col-span-2">
                {t("snapshots.blockHint")}
              </p>
            ) : (
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
            )}
          </>
        )}
      </CardContent>
    </Card>
  );
}
