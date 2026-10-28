import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { Network } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { MAX_NICS, type NicRow, type VmFormValues } from "@/vms/form";

/** 네트워크 카드 행 */
export function NicsSection({
  values,
  errors,
  onChange,
  onRowsChange,
  networks,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 행을 넣거나 뺀다. 행 번호로 단 오류가 다른 행을 가리키므로 오류도 지운다. */
  onRowsChange: (nics: NicRow[]) => void;
  networks: Network[] | undefined;
}) {
  const { t } = useTranslation();
  const setNic = (i: number, patch: Partial<NicRow>) =>
    onChange({ nics: values.nics.map((n, j) => (j === i ? { ...n, ...patch } : n)) });
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vms.nics")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {values.nics.map((nic, i) => (
          <fieldset
            key={i}
            aria-label={t("vmForm.nicN", { n: i + 1 })}
            className="grid grid-cols-2 items-end gap-2 rounded-md border p-3 sm:grid-cols-4"
          >
            <Field id={`nic-${i}-network`} label={t("vms.network")}>
              <NativeSelect
                id={`nic-${i}-network`}
                className="w-full"
                value={nic.network}
                onChange={(e) => setNic(i, { network: e.target.value })}
              >
                <NativeSelectOption value="user">{t("vmForm.userNetwork")}</NativeSelectOption>
                {networks?.map((n) => (
                  <NativeSelectOption key={n.id} value={n.name}>
                    {n.name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            </Field>
            <Field id={`nic-${i}-model`} label={t("vms.model")}>
              <NativeSelect
                id={`nic-${i}-model`}
                className="w-full"
                value={nic.model}
                onChange={(e) => setNic(i, { model: e.target.value as NicRow["model"] })}
              >
                <NativeSelectOption value="virtio">virtio</NativeSelectOption>
                <NativeSelectOption value="e1000e">e1000e</NativeSelectOption>
              </NativeSelect>
            </Field>
            <Field id={`nic-${i}-mac`} label={t("vms.mac")} error={errors[`nics.${i}.mac`]}>
              {(a11y) => (
                <Input
                  id={`nic-${i}-mac`}
                  {...a11y}
                  placeholder={t("vmForm.macAuto")}
                  value={nic.mac}
                  onChange={(e) => setNic(i, { mac: e.target.value })}
                />
              )}
            </Field>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label={t("vmForm.removeNic", { n: i + 1 })}
              onClick={() => onRowsChange(values.nics.filter((_, j) => j !== i))}
            >
              <X />
            </Button>
          </fieldset>
        ))}
        {values.nics.length < MAX_NICS && (
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                onRowsChange([...values.nics, { network: "user", model: "virtio", mac: "" }])
              }
            >
              <Plus />
              {t("vmForm.addNic")}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
