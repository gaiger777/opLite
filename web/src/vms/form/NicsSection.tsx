import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { Network, SecurityGroup } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { MAX_NICS, type NicRow, type VmFormValues } from "@/vms/form";

/** 네트워크 카드 행 */
export function NicsSection({
  values,
  errors,
  onChange,
  onRowsChange,
  networks,
  securityGroups,
  locked,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 행을 넣거나 뺀다. 행 번호로 단 오류가 다른 행을 가리키므로 오류도 지운다. */
  onRowsChange: (nics: NicRow[]) => void;
  networks: Network[] | undefined;
  securityGroups: SecurityGroup[] | undefined;
  /** 실행 중: NIC 개수·순서·MAC은 서버가 바꾸지 않는다. */
  locked: boolean;
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
        {locked && <p className="text-sm text-muted-foreground">{t("vmForm.nicsRunning")}</p>}
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
                // `user` 네트워크(QEMU NAT)에는 보안 그룹을 걸 수 없다.
                onChange={(e) =>
                  setNic(
                    i,
                    e.target.value === "user"
                      ? { network: e.target.value, securityGroups: [] }
                      : { network: e.target.value },
                  )
                }
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
                  disabled={locked}
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
              disabled={locked}
              onClick={() => onRowsChange(values.nics.filter((_, j) => j !== i))}
            >
              <X />
            </Button>
            {nic.network !== "user" && (
              <NicGroups
                index={i}
                chosen={nic.securityGroups}
                groups={securityGroups}
                error={errors[`nics.${i}.securityGroups`]}
                onChange={(chosen) => setNic(i, { securityGroups: chosen })}
              />
            )}
          </fieldset>
        ))}
        {!locked && values.nics.length < MAX_NICS && (
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                onRowsChange([
                  ...values.nics,
                  { network: "user", model: "virtio", mac: "", securityGroups: [] },
                ])
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

/** NIC 하나의 보안 그룹 고르기. 고른 차례대로 규칙을 본다. */
function NicGroups({
  index,
  chosen,
  groups,
  error,
  onChange,
}: {
  index: number;
  chosen: string[];
  groups: SecurityGroup[] | undefined;
  error: string | undefined;
  onChange: (chosen: string[]) => void;
}) {
  const { t } = useTranslation();
  // 목록을 아직 읽지 못했어도 이미 건 그룹은 보인다.
  const names = [...new Set([...(groups ?? []).map((g) => g.name), ...chosen])].sort();
  const errorId = `nic-${index}-groups-error`;
  return (
    <div
      role="group"
      aria-label={t("vmForm.nicGroups", { n: index + 1 })}
      {...(error && { "aria-describedby": errorId })}
      className="col-span-full flex flex-col gap-1.5"
    >
      <span className="text-sm font-medium">{t("vmForm.securityGroups")}</span>
      {names.length === 0 ? (
        <p className="text-xs text-muted-foreground">{t("vmForm.noSecurityGroups")}</p>
      ) : (
        <div className="flex flex-wrap gap-x-4 gap-y-2">
          {names.map((name) => {
            const id = `nic-${index}-group-${name}`;
            return (
              <div key={name} className="flex items-center gap-2">
                <Checkbox
                  id={id}
                  checked={chosen.includes(name)}
                  onCheckedChange={(on) =>
                    onChange(on === true ? [...chosen, name] : chosen.filter((g) => g !== name))
                  }
                />
                <Label htmlFor={id}>{name}</Label>
              </div>
            );
          })}
        </div>
      )}
      {error && (
        <p id={errorId} className="text-xs text-destructive">
          {t(error)}
        </p>
      )}
    </div>
  );
}
