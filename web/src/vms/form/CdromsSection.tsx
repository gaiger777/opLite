import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { Volume } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { type CdromRow, MAX_CDROMS, type VmFormValues } from "@/vms/form";
import type { DeviceMode } from "@/vms/hotplug";

/**
 * CD-ROM 드라이브 행: ISO를 넣거나 비운다. 실행 중에는 드라이브를 더하거나 빼지 못하고(서버의 409) 매체만 바꾼다 —
 * 하드웨어 기록이 없는 실행 중 VM(`locked`)은 그것도 못 한다.
 */
export function CdromsSection({
  values,
  errors,
  onChange,
  onRowsChange,
  isos,
  mode,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  onRowsChange: (cdroms: CdromRow[]) => void;
  /** 넣을 수 있는 ISO 볼륨 */
  isos: Volume[];
  mode: DeviceMode;
}) {
  const { t } = useTranslation();
  const running = mode !== "free";
  const setCdrom = (i: number, patch: Partial<CdromRow>) =>
    onChange({ cdroms: values.cdroms.map((c, j) => (j === i ? { ...c, ...patch } : c)) });
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vmForm.cdroms")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {running && mode !== "locked" && (
          <p className="text-sm text-muted-foreground">{t("vmForm.cdromsLive")}</p>
        )}
        {values.cdroms.length === 0 && (
          <p className="text-sm text-muted-foreground">{t("vmForm.noCdroms")}</p>
        )}
        {values.cdroms.map((cdrom, i) => (
          <fieldset
            key={i}
            disabled={mode === "locked"}
            aria-label={t("vmForm.cdromN", { n: i })}
            className="flex items-end gap-2 rounded-md border p-3"
          >
            <div className="flex-1">
              <Field
                id={`cdrom-${i}-iso`}
                label={t("vmForm.cdromMedia", { n: i })}
                info="vmForm.cdromMedia"
              >
                <NativeSelect
                  id={`cdrom-${i}-iso`}
                  className="w-full"
                  value={cdrom.iso}
                  onChange={(e) => setCdrom(i, { iso: e.target.value })}
                >
                  <NativeSelectOption value="">{t("vmForm.cdromEmpty")}</NativeSelectOption>
                  {/* 고를 수 없게 된 매체(지운 ISO 따위)도 지금 값으로는 보인다. */}
                  {cdrom.iso !== "" && !isos.some((v) => v.name === cdrom.iso) && (
                    <NativeSelectOption value={cdrom.iso}>{cdrom.iso}</NativeSelectOption>
                  )}
                  {isos.map((v) => (
                    <NativeSelectOption key={v.id} value={v.name}>
                      {v.name}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </Field>
            </div>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label={t("vmForm.removeCdrom", { n: i })}
              disabled={running}
              onClick={() => onRowsChange(values.cdroms.filter((_, j) => j !== i))}
            >
              <X />
            </Button>
          </fieldset>
        ))}
        {errors.cdroms && <p className="text-xs text-destructive">{t(errors.cdroms)}</p>}
        {!running && values.cdroms.length < MAX_CDROMS && (
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => onRowsChange([...values.cdroms, { iso: "", bootOrder: "" }])}
            >
              <Plus />
              {t("vmForm.addCdrom")}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
