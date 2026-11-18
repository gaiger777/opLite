import { useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useSettings } from "@/api/live";
import { CopyButton } from "@/components/CopyButton";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { patchSettings, settingsError } from "@/settings/save";

/** 노드마다 긁을 주소(이 노드의 것을 예로 보인다) */
export function scrapeUrl(): string {
  return `${window.location.origin}/api/v1/metrics`;
}

/** Prometheus `scrape_configs` 예. 노드마다 대상 하나, 토큰은 viewer 이상의 API 토큰이다. */
export function scrapeConfig(): string {
  const host = window.location.host;
  return [
    "scrape_configs:",
    "  - job_name: orbivisor",
    "    scheme: https",
    "    metrics_path: /api/v1/metrics",
    "    authorization:",
    "      credentials_file: /etc/prometheus/orbivisor.token",
    "    tls_config:",
    "      ca_file: /etc/prometheus/orbivisor-ca.pem",
    "    static_configs:",
    `      - targets: ["${host}"]`,
  ].join("\n");
}

/** Prometheus 끝점(`GET /api/v1/metrics`) 켜기·끄기와 스크레이프 예 */
export function PrometheusSection({ write }: { write: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const settings = useSettings();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);

  if (!settings.data) {
    return null;
  }
  const revision = settings.data.revision;
  const enabled = settings.data.spec.prometheus?.enabled ?? false;

  async function toggle(next: boolean) {
    setBusy(true);
    setError(null);
    try {
      await patchSettings(queryClient, revision, { prometheus: { enabled: next } });
      toast.success(t(next ? "settings.prometheus.enabled" : "settings.prometheus.disabled"));
      // 누른 버튼이 바뀌어 사라진다.
      heading.current?.focus();
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section aria-labelledby="settings-prometheus" className="flex flex-col gap-4">
      <h2 id="settings-prometheus" ref={heading} tabIndex={-1} className="font-medium">
        {t("settings.prometheus.title")}
      </h2>
      <p className="text-sm text-muted-foreground">{t("settings.prometheus.intro")}</p>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <dl className="grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">{t("common.state")}</dt>
        <dd>
          <Badge variant={enabled ? "default" : "outline"}>
            {enabled ? t("settings.prometheus.on") : t("settings.prometheus.off")}
          </Badge>
        </dd>
        <dt className="text-muted-foreground">{t("settings.prometheus.url")}</dt>
        <dd className="flex items-center gap-2">
          <code className="font-mono text-xs break-all">{scrapeUrl()}</code>
          <CopyButton text={scrapeUrl()} label={t("settings.prometheus.copyUrl")} />
        </dd>
      </dl>
      <p className="text-sm text-muted-foreground">{t("settings.prometheus.auth")}</p>
      <div className="flex items-start gap-2">
        <pre
          aria-label={t("settings.prometheus.example")}
          className="min-w-0 flex-1 overflow-x-auto rounded-md bg-muted p-2 font-mono text-xs"
        >
          {scrapeConfig()}
        </pre>
        <CopyButton text={scrapeConfig()} label={t("settings.prometheus.copyExample")} />
      </div>
      {write && (
        <div>
          {enabled ? (
            <Button variant="outline" disabled={busy} onClick={() => void toggle(false)}>
              {t("settings.prometheus.disable")}
            </Button>
          ) : (
            <Button disabled={busy} onClick={() => void toggle(true)}>
              {t("settings.prometheus.enable")}
            </Button>
          )}
        </div>
      )}
    </section>
  );
}
