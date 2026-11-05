import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useSettings, useSubscription } from "@/api/live";
import type { Channel } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { patchSettings, settingsError } from "@/settings/save";
import { SubscriptionSection } from "@/settings/SubscriptionSection";
import { TelemetrySection } from "@/settings/TelemetrySection";

const CHANNELS: Channel[] = ["stable", "testing", "enterprise"];

/** 클러스터 설정. 누구나 읽고 admin만 바꾼다. */
export function SettingsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const settings = useSettings();
  const subscription = useSubscription();
  const write = can(me.role, "write", "settings");
  // 고르기 전에는 서버의 값을 보인다(watch로 바뀌어도 따라간다).
  const [chosen, setChosen] = useState<Channel | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!settings.data) {
    return <ListPending error={settings.error} />;
  }
  const current = settings.data.spec.updates?.channel ?? "stable";
  const channel = chosen ?? current;
  const revision = settings.data.revision;
  // enterprise 저장소는 구독 키로 인증하므로 구독이 active일 때만 고른다(서버도 확인한다).
  const subscribed = subscription.data?.state === "active";

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await patchSettings(queryClient, revision, { updates: { channel } });
      setChosen(null);
      toast.success(t("settings.saved"));
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex max-w-xl flex-col gap-6">
      <h1 className="text-2xl font-semibold">{t("nav.settings")}</h1>
      <SubscriptionSection write={write} />
      <form noValidate onSubmit={(e) => void save(e)} className="flex flex-col gap-4">
        <h2 className="font-medium">{t("settings.updates")}</h2>
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        <Field
          id="settings-channel"
          label={t("settings.channel")}
          hint={
            subscribed
              ? t("settings.channelHint")
              : `${t("settings.channelHint")} ${t("settings.enterpriseHint")}`
          }
        >
          {(a11y) => (
            <NativeSelect
              id="settings-channel"
              value={channel}
              disabled={!write || busy}
              onChange={(e) => setChosen(e.target.value as Channel)}
              {...a11y}
            >
              {CHANNELS.map((c) => (
                <NativeSelectOption key={c} value={c} disabled={c === "enterprise" && !subscribed}>
                  {t(`settings.channels.${c}`)}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )}
        </Field>
        {write ? (
          <div>
            <Button type="submit" disabled={busy || channel === current}>
              {t("common.save")}
            </Button>
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">{t("settings.adminOnly")}</p>
        )}
      </form>
      <TelemetrySection write={write} />
    </div>
  );
}
