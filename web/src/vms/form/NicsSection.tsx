import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { Network, NicSpec, SecurityGroup } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { sdnNetworkNamed } from "@/sdn/ips";
import { hasQos, MAX_NICS, type NicRow, type QosRow, type VmFormValues } from "@/vms/form";
import { type DeviceMode, liveRemovable, savedCount } from "@/vms/hotplug";

/**
 * 네트워크 카드 행. 실행 중(`live`)에는 끝에 붙이기, 실행 중 붙인 것을 끝에서부터 떼기, 보안 그룹 바꾸기만 한다.
 * SDN 네트워크(계획 65)의 NIC는 주소 칸(비면 자동)이 있고 보안 그룹을 걸 수 없다(포트 보안만, 계획 66에 옴). 켜진 VM의
 * 있던 NIC 주소는 바꿀 수 없다(서버 409 `vm_running`).
 */
export function NicsSection({
  values,
  errors,
  onChange,
  onRowsChange,
  networks,
  securityGroups,
  mode,
  saved,
  boot,
  template = false,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 행을 넣거나 뺀다. 행 번호로 단 오류가 다른 행을 가리키므로 오류도 지운다. */
  onRowsChange: (nics: NicRow[]) => void;
  networks: Network[] | undefined;
  securityGroups: SecurityGroup[] | undefined;
  /**
   * NIC를 바꾸는 방식(`deviceMode`). `locked`(하드웨어 기록이 없는 실행 중 VM): 개수·순서·MAC은 서버가 바꾸지
   * 않는다. `restart`: 네트워크·모델도 바꾸지 못한다(보안 그룹만).
   */
  mode: DeviceMode;
  /** 저장된(도는 QEMU에 붙은) NIC. `live`에서만 본다. */
  saved: readonly NicSpec[];
  /** 켤 때 단 NIC 수. 그 뒤의 것만 실행 중에 뗄 수 있다. */
  boot: number;
  /** 템플릿을 고친다: 서버가 템플릿의 주소를 늘 지우므로 주소 칸을 보이지 않는다(리뷰 1 #9). */
  template?: boolean;
}) {
  const { t } = useTranslation();
  const locked = mode === "locked" || mode === "restart";
  const live = mode === "live";
  const savedRows = live
    ? savedCount(
        values.nics,
        saved,
        (n, s) =>
          n.network === s.network && n.model === s.model && n.mac.toLowerCase() === (s.mac ?? ""),
      )
    : 0;
  const setNic = (i: number, patch: Partial<NicRow>) =>
    onChange({ nics: values.nics.map((n, j) => (j === i ? { ...n, ...patch } : n)) });
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vms.nics")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {mode === "locked" && (
          <p className="text-sm text-muted-foreground">{t("vmForm.nicsRunning")}</p>
        )}
        {live && <p className="text-sm text-muted-foreground">{t("vmForm.nicsLive")}</p>}
        {values.nics.map((nic, i) => {
          // 도는 장치: 네트워크·모델·MAC은 바꾸지 못한다(보안 그룹은 된다).
          const fixed = mode === "restart" || (live && i < savedRows);
          const sdn = sdnNetworkNamed(networks, nic.network);
          // 켜진 VM의 저장된 NIC(같은 MAC·네트워크)는 주소를 바꿀 수 없다. 주소가 하나도 없던 NIC에는 줄 수 있다(그 포트는
          // 꺼져 있었다 — 서버 `check_running_ips`, 리뷰 2 N-4).
          const savedNic = saved.find(
            (s) => s.network === nic.network && (s.mac ?? "") === nic.mac.toLowerCase(),
          );
          const stored = savedNic !== undefined;
          const bare = stored && !savedNic.ips?.length;
          const ipsFixed = mode !== "free" && stored && !bare;
          const removable = live ? liveRemovable(i, savedRows, boot) : !locked;
          return (
            <fieldset
              key={i}
              aria-label={t("vmForm.nicN", { n: i + 1 })}
              className="grid grid-cols-2 items-end gap-2 rounded-md border p-3 sm:grid-cols-4"
            >
              <Field id={`nic-${i}-network`} label={t("vms.network")}>
                <NativeSelect
                  id={`nic-${i}-network`}
                  className="w-full"
                  disabled={fixed}
                  value={nic.network}
                  // `user` 네트워크(QEMU NAT)에는 보안 그룹을 걸 수 없다(SDN 네트워크는 계획 66부터 OVN ACL로 건다).
                  // 주소는 네트워크마다다.
                  onChange={(e) =>
                    setNic(
                      i,
                      e.target.value === "user"
                        ? { network: e.target.value, securityGroups: [], ips: "", qos: undefined }
                        : { network: e.target.value, ips: "" },
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
                  disabled={fixed}
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
                    disabled={locked || fixed}
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
                disabled={!removable}
                onClick={() => onRowsChange(values.nics.filter((_, j) => j !== i))}
              >
                <X />
              </Button>
              {sdn && (
                <div className="col-span-full flex flex-col gap-1.5">
                  {!template && (
                    <Field
                      id={`nic-${i}-ips`}
                      label={t("vmForm.nicIps")}
                      error={errors[`nics.${i}.ips`]}
                      // 저장된 NIC를 비워 보내면 서버가 지금 주소를 둔다(새 NIC만 자동으로 받는다).
                      hint={t(
                        ipsFixed
                          ? "vmForm.nicIpsRunning"
                          : bare && mode !== "free"
                            ? "vmForm.nicIpsGiveRunning"
                            : stored
                              ? "vmForm.nicIpsKeepHint"
                              : "vmForm.nicIpsHint",
                      )}
                    >
                      {(a11y) => (
                        <Input
                          id={`nic-${i}-ips`}
                          {...a11y}
                          placeholder={stored ? undefined : t("vmForm.nicIpsAuto")}
                          disabled={ipsFixed}
                          value={nic.ips}
                          onChange={(e) => setNic(i, { ips: e.target.value })}
                        />
                      )}
                    </Field>
                  )}
                  <p className="text-xs text-muted-foreground">{t("vmForm.sdnPortSecurity")}</p>
                </div>
              )}
              {nic.network !== "user" && (
                <NicQos
                  index={i}
                  qos={nic.qos}
                  sdn={sdn}
                  errors={errors}
                  onChange={(qos) => setNic(i, { qos })}
                />
              )}
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
          );
        })}
        {!locked && values.nics.length < MAX_NICS && (
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                onRowsChange([
                  ...values.nics,
                  { network: "user", model: "virtio", mac: "", securityGroups: [], ips: "" },
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

/**
 * NIC 하나의 QoS(계획 67 §8): 보내기·받기 속도(Mbit/s)·버스트(kbit)·DSCP. 속도 제한은 폴리싱(넘으면 버림)이고 실행 중에도
 * 바뀐다. DSCP는 SDN NIC만(단순 NIC는 tap의 tc라 속도만 — 칸이 막히고 까닭을 보인다). 값이 있으면 펼쳐 보인다.
 */
function NicQos({
  index,
  qos,
  sdn,
  errors,
  onChange,
}: {
  index: number;
  qos: QosRow | undefined;
  sdn: boolean;
  errors: Record<string, string>;
  onChange: (qos: QosRow) => void;
}) {
  const { t } = useTranslation();
  const value = qos ?? { out: "", in: "", burst: "", dscp: "" };
  const field = (key: keyof QosRow, label: string, disabled = false, hint?: string) => (
    <Field
      id={`nic-${index}-qos-${key}`}
      label={label}
      error={errors[`nics.${index}.qos.${key}`]}
      hint={hint}
    >
      {(a11y) => (
        <Input
          id={`nic-${index}-qos-${key}`}
          inputMode="numeric"
          autoComplete="off"
          disabled={disabled}
          value={value[key]}
          onChange={(e) => onChange({ ...value, [key]: e.target.value })}
          {...a11y}
        />
      )}
    </Field>
  );
  return (
    <details className="col-span-full" open={hasQos(qos)}>
      <summary className="cursor-pointer text-sm font-medium">{t("vmForm.qos.title")}</summary>
      <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-4">
        {field("out", t("vmForm.qos.out"))}
        {field("in", t("vmForm.qos.in"))}
        {field("burst", t("vmForm.qos.burst"), false, t("vmForm.qos.burstHint"))}
        {field(
          "dscp",
          t("vmForm.qos.dscp"),
          !sdn && value.dscp === "",
          sdn ? undefined : t("vmForm.qos.dscpSdnOnly"),
        )}
      </div>
      <p className="mt-1 text-xs text-muted-foreground">{t("vmForm.qos.hint")}</p>
    </details>
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
