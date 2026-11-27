import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useSettings, useSubscription } from "@/api/live";
import type { Channel } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { CpuSettingsSection } from "@/cpu/CpuSettingsSection";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { HaSection } from "@/settings/HaSection";
import { PrometheusSection } from "@/settings/PrometheusSection";
import { patchSettings, settingsError } from "@/settings/save";
import { ShutdownSection } from "@/settings/ShutdownSection";
import { TelemetrySection } from "@/settings/TelemetrySection";

const CHANNELS: Channel[] = ["stable", "testing", "enterprise"];

/** 설정 › 일반: 업데이트·종료·HA·CPU·텔레메트리·Prometheus. 누구나 읽고 admin만 바꾼다. 구독은 따로 탭이다. */
export function SettingsPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const settings = useSettings();
  const subscription = useSubscription();
  const write = perms.can("write", "settings");
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

  const updates = (
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
        info="settings.channel"
        help={
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
  );
  // 목차 + 카드(계획 77-6 — 섹션 여섯이 2000px 넘게 한 줄로 이어지던 것). 카드마다 제 저장 단추가 있다.
  const sections = [
    { id: "updates", title: t("settings.updates"), body: updates },
    {
      id: "shutdown",
      title: t("settings.shutdown.title"),
      body: <ShutdownSection write={write} />,
    },
    { id: "ha", title: t("settings.ha.title"), body: <HaSection write={write} /> },
    { id: "cpu", title: t("cpu.settings.title"), body: <CpuSettingsSection write={write} /> },
    {
      id: "telemetry",
      title: t("settings.telemetry.title"),
      body: <TelemetrySection write={write} />,
    },
    {
      id: "prometheus",
      title: t("settings.prometheus.title"),
      body: <PrometheusSection write={write} />,
    },
  ];

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title={t("nav.settings")} help="settings" intro={t("help.settings.intro")} />
      <div className="grid grid-cols-1 gap-6 lg:grid-cols-[12rem_minmax(0,1fr)]">
        <nav aria-label={t("settings.toc")} className="self-start lg:sticky lg:top-4">
          <ul className="flex flex-wrap gap-x-4 gap-y-1 text-sm lg:flex-col">
            {sections.map((section) => (
              <li key={section.id}>
                <a
                  href={`#card-${section.id}`}
                  className="text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
                >
                  {section.title}
                </a>
              </li>
            ))}
          </ul>
        </nav>
        <div className="flex max-w-2xl min-w-0 flex-col gap-4">
          {sections.map((section) => (
            <div
              key={section.id}
              id={`card-${section.id}`}
              className="scroll-mt-4 rounded-lg border bg-card p-4"
            >
              {section.body}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
