import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { exportBody, filePools, refreshExports, validHwVersion } from "@/exports/exports";
import { objectTarget } from "@/auth/permissions";
import { codeMessage } from "@/i18n/errors";
import { trackTask } from "@/tasks/tasks";

/**
 * VM 메뉴의 "OVA로 내보내기…"(계획 72 §12·§13): 풀(dir·NFS, `storage.allocate`가 있는 것)·OS(VMware guestId)·하드웨어 버전·
 * MAC을 고르고 내보내기 Task를 만든다. VM 메뉴가 내보내기 보기(`/vm-exports?export=<VM>`)를 열면 이 대화상자가 뜨고, 끝나면 그
 * 보기에서 내려받는다. 늦게 읽는 조각이다.
 */
export function ExportDialog({ vm, onClose }: { vm: Vm; onClose(): void }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const pools = useList("storage-pool");
  const choices = filePools(pools.data ?? []).filter((p) =>
    perms.has("storage.allocate", objectTarget("storage-pool", p)),
  );
  const [pool, setPool] = useState("");
  const [name, setName] = useState("");
  const [guestOs, setGuestOs] = useState("");
  const [hwVersion, setHwVersion] = useState("14");
  const [keepMacs, setKeepMacs] = useState(false);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const chosenPool = pool || choices[0]?.name || "";

  async function submit() {
    const found: Record<string, string> = {};
    if (!chosenPool) {
      found.pool = "vmExports.dialog.errors.pool";
    }
    if (!validHwVersion(hwVersion)) {
      found.hwVersion = "vmExports.dialog.errors.hwVersion";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    let started;
    try {
      started = await call(
        api.POST("/vms/{key}/export", {
          params: { path: { key: vm.id } },
          body: exportBody({ pool: chosenPool, name, guestOs, hwVersion, keepMacs }),
        }),
      );
    } catch (err) {
      throw err instanceof ApiError
        ? new Error(codeMessage(t, err.code, err.message, "vmExports.errors"), { cause: err })
        : err;
    }
    trackTask(started.task, t("tasks.op.vm-export", { target: vm.name }));
    await refreshExports(queryClient);
    toast.success(t("vmExports.dialog.started", { name: started.export.name }));
  }

  return (
    <FormDialog
      title={t("vmExports.dialog.title", { name: vm.name })}
      description={t("vmExports.dialog.description")}
      submitLabel={t("vmExports.dialog.submit")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field
        id="export-pool"
        label={t("vmExports.dialog.pool")}
        error={errors.pool}
        hint={t("vmExports.dialog.poolHint")}
      >
        {(a11y) => (
          <NativeSelect
            id="export-pool"
            className="w-full"
            value={chosenPool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          >
            {choices.length === 0 && (
              <NativeSelectOption value="">{t("vmExports.dialog.noPool")}</NativeSelectOption>
            )}
            {choices.map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name} ({p.spec.backend.kind})
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <Field
        id="export-name"
        label={t("vmExports.dialog.name")}
        hint={t("vmExports.dialog.nameHint")}
      >
        {(a11y) => (
          <Input
            id="export-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="export-guest-os"
        label={t("vmExports.dialog.guestOs")}
        hint={t("vmExports.dialog.guestOsHint")}
      >
        {(a11y) => (
          <Input
            id="export-guest-os"
            value={guestOs}
            placeholder="ubuntu64Guest"
            onChange={(e) => setGuestOs(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="export-hw-version"
        label={t("vmExports.dialog.hwVersion")}
        error={errors.hwVersion}
        hint={t("vmExports.dialog.hwVersionHint")}
      >
        {(a11y) => (
          <Input
            id="export-hw-version"
            inputMode="numeric"
            value={hwVersion}
            onChange={(e) => setHwVersion(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <div className="flex items-start gap-2">
        <Checkbox
          id="export-keep-macs"
          checked={keepMacs}
          onCheckedChange={(c) => setKeepMacs(c === true)}
          aria-describedby="export-keep-macs-hint"
        />
        <div className="flex flex-col gap-0.5">
          <Label htmlFor="export-keep-macs">{t("vmExports.dialog.keepMacs")}</Label>
          <p id="export-keep-macs-hint" className="text-xs text-muted-foreground">
            {t("vmExports.dialog.keepMacsHint")}
          </p>
        </div>
      </div>
    </FormDialog>
  );
}
