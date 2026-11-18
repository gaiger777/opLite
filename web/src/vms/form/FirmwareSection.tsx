import { useTranslation } from "react-i18next";

import type { Node } from "@/api/types";
import { Field } from "@/components/Field";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import { firmwareUnavailable, type VmFormValues } from "@/vms/form";

type Reason = { key: string; values?: Record<string, string> } | null;

/** 스위치 하나와 설명·막힌 이유·오류 */
function Toggle({
  id,
  label,
  hint,
  checked,
  disabled,
  reason,
  error,
  onChange,
}: {
  id: string;
  label: string;
  hint: string;
  checked: boolean;
  disabled: boolean;
  reason: Reason;
  error?: string;
  onChange: (checked: boolean) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-2">
        <Switch
          id={id}
          aria-describedby={`${id}-hint`}
          aria-invalid={error ? true : undefined}
          disabled={disabled}
          checked={checked}
          onCheckedChange={onChange}
        />
        <Label htmlFor={id}>{label}</Label>
      </div>
      <p id={`${id}-hint`} className="text-xs text-muted-foreground">
        {hint}
      </p>
      {reason && (
        // 켠 채로 쓸 수 없으면 오류처럼 알린다.
        <p className={checked ? "text-sm text-destructive" : "text-sm text-muted-foreground"}>
          {t(reason.key, reason.values)}
        </p>
      )}
      {error && <p className="text-sm text-destructive">{t(error)}</p>}
    </div>
  );
}

/**
 * 보안 카드: TPM 2.0과 Secure Boot. 둘 다 UEFI가 있어야 하고, 고른 노드(자동이면 어느 노드든)가 그 능력을 알려야 켤 수
 * 있다(이미 켠 것은 끌 수 있다). Secure Boot는 멈춘 VM에서만, UEFI 변수가 이미 있는 VM(`firmwareNode`)이면 VM 화면의 펌웨어
 * 초기화와 함께만 바꾼다(PUT은 409 `firmware_reset_required`).
 */
export function SecuritySection({
  values,
  errors,
  onChange,
  nodes,
  stopped,
  firmwareNode,
  live,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  nodes: Node[] | undefined;
  /** 멈춘 VM이거나 새 VM이다. */
  stopped: boolean;
  /** UEFI 변수가 있는 노드(`status.firmware_node`). 새 VM은 없다. */
  firmwareNode: string | null;
  /** 실행 중이다(TPM은 다음 시작 때 적용). */
  live: boolean;
}) {
  const { t } = useTranslation();
  const bios = values.firmware !== "uefi";
  const capReason = (cap: "tpm" | "secure_boot"): Reason =>
    bios ? { key: "vmForm.needsUefi" } : firmwareUnavailable(nodes, values.node, cap);
  const tpmReason = capReason("tpm");
  // Secure Boot를 바꿀 수 없는 이유가 먼저다(켤 수 없는 이유보다).
  const sbLocked: Reason = !stopped
    ? { key: "vmForm.secureBootRunning" }
    : firmwareNode
      ? { key: "vmForm.secureBootReset" }
      : null;
  const sbReason = sbLocked ?? capReason("secure_boot");
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vmForm.security")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {live && <p className="text-sm text-muted-foreground">{t("vmForm.securityLive")}</p>}
        <Toggle
          id="vm-tpm"
          label={t("vmForm.tpm")}
          hint={t("vmForm.tpmHint")}
          checked={values.tpm}
          disabled={tpmReason !== null && !values.tpm}
          reason={tpmReason}
          error={errors.tpm}
          onChange={(tpm) => onChange({ tpm })}
        />
        <Toggle
          id="vm-secure-boot"
          label={t("vmForm.secureBoot")}
          hint={t("vmForm.secureBootHint")}
          checked={values.secureBoot}
          disabled={sbLocked !== null || (sbReason !== null && !values.secureBoot)}
          reason={sbReason}
          error={errors.secureBoot}
          onChange={(secureBoot) => onChange({ secureBoot })}
        />
      </CardContent>
    </Card>
  );
}

/**
 * 게스트 에이전트 카드: QEMU 게스트 에이전트 채널, 스냅샷·백업 때 파일 시스템 얼리기, 끄는 방법. 채널을 붙이거나 떼는 것은
 * 다음 시작 때, 얼리기·끄는 방법은 곧바로 적용된다.
 */
export function GuestAgentSection({
  values,
  onChange,
  live,
}: {
  values: VmFormValues;
  onChange: (patch: Partial<VmFormValues>) => void;
  live: boolean;
}) {
  const { t } = useTranslation();
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vmForm.guestAgent")}</CardTitle>
      </CardHeader>
      <CardContent className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        <div className="sm:col-span-2">
          <Toggle
            id="vm-guest-agent"
            label={t("vmForm.guestAgentOn")}
            hint={t(live ? "vmForm.guestAgentLive" : "vmForm.guestAgentHint")}
            checked={values.guestAgent}
            disabled={false}
            reason={null}
            onChange={(guestAgent) => onChange({ guestAgent })}
          />
        </div>
        {values.guestAgent && (
          <>
            <div className="sm:col-span-2">
              <Toggle
                id="vm-agent-freeze"
                label={t("vmForm.agentFreeze")}
                hint={t("vmForm.agentFreezeHint")}
                checked={values.agentFreeze}
                disabled={false}
                reason={null}
                onChange={(agentFreeze) => onChange({ agentFreeze })}
              />
            </div>
            <Field id="vm-agent-shutdown" label={t("vmForm.agentShutdown")}>
              <NativeSelect
                id="vm-agent-shutdown"
                className="w-full"
                value={values.agentShutdown}
                onChange={(e) =>
                  onChange({ agentShutdown: e.target.value as VmFormValues["agentShutdown"] })
                }
              >
                <NativeSelectOption value="agent">
                  {t("vmForm.agentShutdownAgent")}
                </NativeSelectOption>
                <NativeSelectOption value="acpi">
                  {t("vmForm.agentShutdownAcpi")}
                </NativeSelectOption>
              </NativeSelect>
            </Field>
          </>
        )}
      </CardContent>
    </Card>
  );
}
