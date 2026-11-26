// "링크 경보 켜기"(계획 75 §3.4): 업링크 포트 끊김·본딩 저하 규칙 둘을 고른 알림 대상으로 한 번에 만든다(서버
// `POST /alert-rules/link-defaults` — 이미 있는 이름은 서버가 건너뛴다). SDN 묶음과 따로라 SDN을 쓰지 않는 클러스터에서도 보인다.
// 경보 규칙을 바꿀 수 있는(`alert.manage`) 사용자에게만, 링크 조건 규칙이 하나도 없을 때만 홈에 보인다.
import { useQueryClient } from "@tanstack/react-query";
import { BellPlus } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useAlertRules, useNotificationTargets } from "@/alerts/alerts";
import { isLink, LINK_DEFAULTS } from "@/alerts/rules";
import { api, call } from "@/api/client";
import { usePermissions } from "@/auth/usePermissions";
import { FormDialog } from "@/components/FormDialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";

export function LinkAlertsCard() {
  const { t } = useTranslation();
  const manage = usePermissions().can("write", "alert-rule");
  // 규칙을 바꿀 수 없는 사용자는 묻지도 않는다(홈마다 403을 받지 않게).
  const rules = useAlertRules(manage);
  const [open, setOpen] = useState(false);
  if (!manage || !rules.data || rules.data.some((r) => isLink(r.spec.condition.kind))) {
    return null;
  }
  return (
    <section
      aria-labelledby="link-alerts"
      className="flex min-w-0 flex-1 flex-wrap items-center justify-between gap-2 rounded-lg border px-4 py-3"
    >
      <div className="flex flex-col gap-0.5">
        <h2 id="link-alerts" className="text-sm font-medium">
          {t("physical.alerts.title")}
        </h2>
        <p className="text-xs text-muted-foreground">{t("physical.alerts.none")}</p>
      </div>
      <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
        <BellPlus />
        {t("physical.alerts.enable")}
      </Button>
      {open && (
        <LinkDefaultsDialog taken={rules.data.map((r) => r.name)} onClose={() => setOpen(false)} />
      )}
    </section>
  );
}

function LinkDefaultsDialog({ taken, onClose }: { taken: string[]; onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const targets = useNotificationTargets();
  const [chosen, setChosen] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const names = (targets.data ?? []).map((x) => x.name);
  const todo = LINK_DEFAULTS.filter((r) => !taken.includes(r.name));

  async function submit() {
    if (chosen.length === 0) {
      setError(t("alerts.rules.errors.targets"));
      return false;
    }
    setError(null);
    // 오류(섞인 판의 409 등)는 대화상자에 보인다.
    const out = await call(api.POST("/alert-rules/link-defaults", { body: { targets: chosen } }));
    await queryClient.invalidateQueries({ queryKey: ["alert-rules"] });
    toast.success(
      t("physical.alerts.created", { count: out.created.length, existing: out.existing.length }),
    );
  }

  return (
    <FormDialog
      title={t("physical.alerts.enable")}
      description={t("physical.alerts.dialogDescription")}
      submitLabel={t("sdn.alerts.create", { count: todo.length })}
      submitDisabled={todo.length === 0}
      onClose={onClose}
      onSubmit={submit}
    >
      <ul className="flex flex-col gap-1 text-sm">
        {LINK_DEFAULTS.map((r) => (
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
              id={`link-alert-target-${name}`}
              checked={chosen.includes(name)}
              onCheckedChange={(on) =>
                setChosen(on === true ? [...chosen, name] : chosen.filter((n) => n !== name))
              }
            />
            <Label htmlFor={`link-alert-target-${name}`}>{name}</Label>
          </div>
        ))}
        {error && <p className="text-xs text-destructive">{error}</p>}
      </fieldset>
    </FormDialog>
  );
}
