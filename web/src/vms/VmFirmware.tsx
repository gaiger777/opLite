import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { trackTask } from "@/tasks/tasks";
import { isStopped } from "@/vms/actions";
import { pendingFieldLabel } from "@/vms/hotplug";

/** 다음 시작 때 적용할 변경 가운데 `fields`의 것("TPM 2.0: off → on") */
export function PendingNote({ vm, fields }: { vm: Vm; fields: readonly string[] }) {
  const { t } = useTranslation();
  const pending = (vm.status.hardware?.pending ?? []).filter((c) => fields.includes(c.field));
  if (pending.length === 0) {
    return null;
  }
  return (
    <ul className="flex flex-col gap-0.5 text-xs text-amber-700 dark:text-amber-400">
      {pending.map((c) => (
        <li key={c.field}>
          {t("vms.security.pending", {
            field: pendingFieldLabel(c.field, t),
            running: c.running,
            wanted: c.wanted,
          })}
        </li>
      ))}
    </ul>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-sm">{children}</dd>
    </div>
  );
}

/** 펌웨어 초기화가 막힌 이유: 멈춘 VM에서만(서버의 409 `vm_running`) */
function resetReason(vm: Vm): string | null {
  return isStopped(vm) ? null : "vms.security.notStopped";
}

/**
 * 보안 카드: TPM 2.0(상태가 있는 노드와 사본), Secure Boot, UEFI 변수가 있는 노드, 다음 시작 때 적용할 변경과 펌웨어 초기화.
 * UEFI 변수가 있는 VM(`firmware_node`)의 Secure Boot는 변수 초기화와 함께만 바꾼다(PUT은 409 `firmware_reset_required`).
 */
export function VmSecurity({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const hint = useId();
  const nodes = useList("node");
  // 열린 초기화 창. `secureBoot`가 있으면 Secure Boot를 그 값으로 바꾸며 연다.
  const [dialog, setDialog] = useState<{ secureBoot?: boolean } | null>(null);
  const nodeName = (id: string) => nodes.data?.find((n) => n.id === id)?.name ?? id;
  const { spec, status } = vm;
  const uefi = (spec.firmware ?? "uefi") === "uefi";
  const tpm = spec.tpm ?? false;
  const secureBoot = spec.secure_boot ?? false;
  const hasTpmState = status.tpm_sha256 != null;
  const copies = (status.firmware_copies ?? []).map(nodeName);
  const firmwareNode = status.firmware_node ? nodeName(status.firmware_node) : null;
  const write = can(me.role, "write", "vm");
  // 지울 것이 있는 VM만: UEFI 변수나 TPM 상태
  const resettable = uefi || tpm || hasTpmState;
  const reason = resetReason(vm);

  let tpmDetail: string | null = null;
  if (tpm && hasTpmState && firmwareNode) {
    tpmDetail =
      copies.length > 0
        ? t("vms.security.tpmStateCopies", { node: firmwareNode, copies: copies.join(", ") })
        : t("vms.security.tpmState", { node: firmwareNode });
  } else if (tpm) {
    tpmDetail = t("vms.security.tpmNoState");
  } else if (hasTpmState && firmwareNode) {
    tpmDetail = t("vms.security.tpmKept", { node: firmwareNode });
  }

  return (
    <Card className="mt-6">
      <CardHeader>
        <CardTitle>
          <h2 id="vm-security">{t("vms.security.title")}</h2>
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <dl aria-labelledby="vm-security" className="grid grid-cols-1 gap-4 sm:grid-cols-3">
          <Field label={t("vms.security.tpm")}>
            {t(tpm ? "vms.on" : "vms.haOff")}
            {tpmDetail && <span className="block text-xs text-muted-foreground">{tpmDetail}</span>}
          </Field>
          <Field label={t("vms.security.secureBoot")}>
            {t(secureBoot ? "vms.on" : "vms.haOff")}
          </Field>
          {uefi && (
            <Field label={t("vms.security.uefiVars")}>
              {firmwareNode
                ? t("vms.security.varsOn", { node: firmwareNode })
                : t("vms.security.noVars")}
            </Field>
          )}
        </dl>
        <PendingNote vm={vm} fields={["tpm", "secure_boot"]} />
        {write && resettable && (
          <>
            <div className="flex flex-wrap gap-2">
              <ReasonButton
                variant="outline"
                size="sm"
                reason={reason}
                reasons={hint}
                onClick={() => setDialog({})}
              >
                {t("vms.security.reset")}
              </ReasonButton>
              {/* UEFI 변수가 없으면 Secure Boot는 VM 고치기에서 바꾼다. */}
              {uefi && status.firmware_node && (
                <ReasonButton
                  variant="outline"
                  size="sm"
                  reason={reason}
                  reasons={hint}
                  onClick={() => setDialog({ secureBoot: !secureBoot })}
                >
                  {t(secureBoot ? "vms.security.secureBootOff" : "vms.security.secureBootOn")}
                </ReasonButton>
              )}
            </div>
            <ReasonList base={hint} reasons={[reason]} />
          </>
        )}
      </CardContent>
      {dialog && (
        <FirmwareResetDialog
          vm={vm}
          secureBoot={dialog.secureBoot}
          onClose={() => setDialog(null)}
        />
      )}
    </Card>
  );
}

/**
 * 펌웨어 초기화 확인(`POST /vms/{key}/firmware/reset`): UEFI 변수와 TPM 상태 가운데 지울 것, Secure Boot 바꾸기(변수
 * 초기화가 있어야 함). VM 이름을 입력해야 하고, 만든 Task를 알림으로 지켜본다. `secureBoot`를 주면 Secure Boot를 그 값으로
 * 바꾸도록 골라 둔 채 연다.
 */
export function FirmwareResetDialog({
  vm,
  secureBoot,
  onClose,
}: {
  vm: Vm;
  secureBoot?: boolean;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const ids = useId();
  const current = vm.spec.secure_boot ?? false;
  const uefi = (vm.spec.firmware ?? "uefi") === "uefi";
  const withTpm = (vm.spec.tpm ?? false) || vm.status.tpm_sha256 != null;
  const [vars, setVars] = useState(secureBoot !== undefined);
  const [tpm, setTpm] = useState(false);
  const [changeSb, setChangeSb] = useState(secureBoot !== undefined && secureBoot !== current);
  const reason = resetReason(vm);
  // Secure Boot를 바꾸려면 변수도 지운다(서버의 400).
  const uefiVars = vars || changeSb;

  async function confirm() {
    const task = await call(
      api.POST("/vms/{key}/firmware/reset", {
        params: { path: { key: vm.id } },
        body: {
          ...(uefiVars && { uefi_vars: true }),
          ...(tpm && { tpm: true }),
          ...(changeSb && { secure_boot: !current }),
        },
      }),
    );
    trackTask(task, t("tasks.op.vm-firmware-reset", { target: vm.name }));
  }

  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={t("vms.security.resetTitle", { name: vm.name })}
      description={t("vms.security.resetDescription")}
      confirmLabel={t("vms.security.reset")}
      typeToConfirm={vm.name}
      confirmDisabled={reason !== null || !(uefiVars || tpm)}
      onConfirm={confirm}
    >
      {uefi && (
        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <Checkbox
              id={`${ids}-vars`}
              checked={uefiVars}
              disabled={changeSb}
              onCheckedChange={(v) => setVars(v === true)}
            />
            <Label htmlFor={`${ids}-vars`}>{t("vms.security.resetVars")}</Label>
          </div>
          <p className="text-xs text-muted-foreground">{t("vms.security.resetVarsHint")}</p>
        </div>
      )}
      {withTpm && (
        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <Checkbox id={`${ids}-tpm`} checked={tpm} onCheckedChange={(v) => setTpm(v === true)} />
            <Label htmlFor={`${ids}-tpm`}>{t("vms.security.resetTpm")}</Label>
          </div>
          {tpm && <p className="text-xs text-destructive">{t("vms.security.resetTpmWarning")}</p>}
        </div>
      )}
      {uefi && (
        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <Checkbox
              id={`${ids}-sb`}
              checked={changeSb}
              onCheckedChange={(v) => setChangeSb(v === true)}
            />
            <Label htmlFor={`${ids}-sb`}>
              {t(current ? "vms.security.resetSecureBootOff" : "vms.security.resetSecureBootOn")}
            </Label>
          </div>
        </div>
      )}
      {reason && <p className="text-sm text-muted-foreground">{t(reason)}</p>}
    </ConfirmDialog>
  );
}
