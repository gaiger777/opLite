import { useQueryClient } from "@tanstack/react-query";
import { BellPlus } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { useAlertRules, useNotificationTargets } from "@/alerts/alerts";
import { isSdn, SDN_DEFAULTS } from "@/alerts/rules";
import { api, call } from "@/api/client";
import { usePermissions } from "@/auth/usePermissions";
import { FormDialog } from "@/components/FormDialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";

/**
 * "SDN 경보 켜기"(계획 67 §11): 권하는 SDN 규칙 여덟을 고른 알림 대상으로 한 번에 만든다(서버의
 * `POST /alert-rules/sdn-defaults` — 이미 있는 이름은 서버가 건너뛴다). 만든 뒤는 보통 규칙이라 경보 화면에서 고친다. 경보 규칙을 바꿀 수 있는(`alert.manage`) 사용자에게만 보인다.
 */
export function SdnAlertsCard() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const manage = perms.can("write", "alert-rule");
  const rules = useAlertRules();
  const [open, setOpen] = useState(false);
  if (!perms.can("read", "alert-rule")) {
    return null;
  }
  const sdnRules = (rules.data ?? []).filter((r) => isSdn(r.spec.condition.kind));
  return (
    <section aria-labelledby="sdn-alerts" className="flex flex-col gap-2 rounded-lg border p-4">
      <h2 id="sdn-alerts" className="font-medium">
        {t("sdn.alerts.title")}
      </h2>
      <p className="text-sm text-muted-foreground">
        {sdnRules.length > 0
          ? t("sdn.alerts.some", { count: sdnRules.length })
          : t("sdn.alerts.none")}{" "}
        <Link to="/alerts?tab=rules" className="underline">
          {t("sdn.alerts.rulesLink")}
        </Link>
      </p>
      {manage && (
        <div>
          <Button variant="outline" onClick={() => setOpen(true)}>
            <BellPlus />
            {t("sdn.alerts.enable")}
          </Button>
        </div>
      )}
      {open && (
        <SdnDefaultsDialog
          taken={(rules.data ?? []).map((r) => r.name)}
          onClose={() => setOpen(false)}
        />
      )}
    </section>
  );
}

function SdnDefaultsDialog({ taken, onClose }: { taken: string[]; onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const targets = useNotificationTargets();
  const [chosen, setChosen] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const names = (targets.data ?? []).map((x) => x.name);
  const todo = SDN_DEFAULTS.filter((r) => !taken.includes(r.name));

  async function submit() {
    if (chosen.length === 0) {
      setError(t("alerts.rules.errors.targets"));
      return false;
    }
    setError(null);
    // 오류는 대화상자에 보인다.
    const out = await call(api.POST("/alert-rules/sdn-defaults", { body: { targets: chosen } }));
    await queryClient.invalidateQueries({ queryKey: ["alert-rules"] });
    toast.success(
      t("sdn.alerts.created", { count: out.created.length, existing: out.existing.length }),
    );
  }

  return (
    <FormDialog
      title={t("sdn.alerts.dialogTitle")}
      description={t("sdn.alerts.dialogDescription")}
      submitLabel={t("sdn.alerts.create", { count: todo.length })}
      submitDisabled={todo.length === 0}
      onClose={onClose}
      onSubmit={submit}
    >
      <ul className="flex flex-col gap-1 text-sm">
        {SDN_DEFAULTS.map((r) => (
          <li
            key={r.name}
            className={taken.includes(r.name) ? "text-muted-foreground line-through" : undefined}
          >
            {t(`alerts.kind.${r.kind}`)} — {t(`alerts.severity.${r.severity}`)}
            {r.forSecs > 0 && ` · ${t("sdn.alerts.for", { secs: r.forSecs })}`}
            {taken.includes(r.name) && ` (${t("sdn.alerts.exists")})`}
          </li>
        ))}
      </ul>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1 text-sm font-medium">{t("alerts.rules.targets")}</legend>
        {names.length === 0 && (
          <p className="text-sm text-muted-foreground">
            {targets.isPending ? t("common.loading") : t("alerts.rules.noTargets")}
          </p>
        )}
        {names.map((name) => (
          <div key={name} className="flex items-center gap-2">
            <Checkbox
              id={`sdn-alert-target-${name}`}
              checked={chosen.includes(name)}
              onCheckedChange={(on) =>
                setChosen(on === true ? [...chosen, name] : chosen.filter((n) => n !== name))
              }
            />
            <Label htmlFor={`sdn-alert-target-${name}`}>{name}</Label>
          </div>
        ))}
        {error && <p className="text-xs text-destructive">{error}</p>}
      </fieldset>
    </FormDialog>
  );
}
