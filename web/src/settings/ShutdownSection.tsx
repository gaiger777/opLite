import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useSettings } from "@/api/live";
import type { ShutdownPolicy } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { patchSettings, settingsError } from "@/settings/save";

const POLICIES: ShutdownPolicy[] = ["conditional", "freeze", "failover", "migrate"];

/** 노드 정상 종료 정책: 호스트가 재부팅하거나 꺼질 때 그 노드의 HA VM을 어떻게 할지. */
export function ShutdownSection({ write }: { write: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const settings = useSettings();
  // 고르기 전에는 서버의 값을 보인다(watch로 바뀌어도 따라간다).
  const [chosen, setChosen] = useState<ShutdownPolicy | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!settings.data) {
    return null;
  }
  // 기본(conditional)은 저장하지 않으므로 없으면 기본이다.
  const current = settings.data.spec.shutdown_policy ?? "conditional";
  const policy = chosen ?? current;
  const revision = settings.data.revision;

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await patchSettings(queryClient, revision, { shutdown_policy: policy });
      setChosen(null);
      toast.success(t("settings.saved"));
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form noValidate onSubmit={(e) => void save(e)} className="flex flex-col gap-4">
      <h2 className="font-medium">{t("settings.shutdown.title")}</h2>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <Field
        id="settings-shutdown-policy"
        label={t("settings.shutdown.policy")}
        hint={t(`settings.shutdown.hint.${policy}`)}
      >
        {(a11y) => (
          <NativeSelect
            id="settings-shutdown-policy"
            value={policy}
            disabled={!write || busy}
            onChange={(e) => setChosen(e.target.value as ShutdownPolicy)}
            {...a11y}
          >
            {POLICIES.map((p) => (
              <NativeSelectOption key={p} value={p}>
                {t(`settings.shutdown.policies.${p}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <p className="text-xs text-muted-foreground">{t("settings.shutdown.note")}</p>
      {write && (
        <div>
          <Button type="submit" variant="outline" disabled={busy || policy === current}>
            {t("settings.shutdown.save")}
          </Button>
        </div>
      )}
    </form>
  );
}
