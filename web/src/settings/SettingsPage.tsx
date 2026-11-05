import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { SETTINGS_KEY, useSettings } from "@/api/live";
import type { Channel, Settings } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { errorMessage } from "@/i18n/errors";

const CHANNELS: Channel[] = ["stable", "testing"];

/** 클러스터 설정. 누구나 읽고 admin만 바꾼다. */
export function SettingsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const settings = useSettings();
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

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const updated = await call(
        api.PATCH("/settings", {
          params: { header: { "If-Match": revision } },
          body: { updates: { channel } },
        }),
      );
      // 응답을 기다리는 사이 watch가 더 새 설정을 넣었으면 그것을 남긴다.
      queryClient.setQueryData<Settings>(SETTINGS_KEY, (old) =>
        old && old.revision > updated.revision ? old : updated,
      );
      setChosen(null);
      toast.success(t("settings.saved"));
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex max-w-xl flex-col gap-6">
      <h1 className="text-2xl font-semibold">{t("nav.settings")}</h1>
      <form noValidate onSubmit={(e) => void save(e)} className="flex flex-col gap-4">
        <h2 className="font-medium">{t("settings.updates")}</h2>
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        <Field id="settings-channel" label={t("settings.channel")} hint={t("settings.channelHint")}>
          {(a11y) => (
            <NativeSelect
              id="settings-channel"
              value={channel}
              disabled={!write || busy}
              onChange={(e) => setChosen(e.target.value as Channel)}
              {...a11y}
            >
              {CHANNELS.map((c) => (
                <NativeSelectOption key={c} value={c}>
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
    </div>
  );
}
