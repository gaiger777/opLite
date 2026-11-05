import { useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useSettings } from "@/api/live";
import type { TelemetryReport } from "@/api/types";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { formatTime } from "@/lib/format";
import { patchSettings, settingsError, storeSettings, withSettingsError } from "@/settings/save";

/**
 * 옵트인 텔레메트리. 켜기 전에는 지금 보낼 보고를 그대로 보여 주고 확인을 받는다. 상태(id, 마지막 전송,
 * 오류)는 리더가 설정 status에 적는다.
 */
export function TelemetrySection({ write }: { write: boolean }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const settings = useSettings();
  // 입력하기 전에는 서버의 값을 보인다.
  const [endpoint, setEndpoint] = useState<string | null>(null);
  const [preview, setPreview] = useState<TelemetryReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 대화상자를 닫으면 포커스를 돌려줄 곳: 켜기 버튼, 켜서 버튼이 사라지면 이 부분의 제목
  const returnFocus = useRef<HTMLElement | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);

  if (!settings.data) {
    return null;
  }
  const revision = settings.data.revision;
  const spec = settings.data.spec.telemetry;
  const status = settings.data.status.telemetry;
  const enabled = spec?.enabled ?? false;
  const saved = spec?.endpoint ?? "";
  const typed = endpoint ?? saved;

  async function run(action: () => Promise<void>, done?: string) {
    setBusy(true);
    setError(null);
    try {
      await action();
      if (done) {
        toast.success(done);
      }
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  const saveEndpoint = (e: React.FormEvent) => {
    e.preventDefault();
    void run(async () => {
      await patchSettings(queryClient, revision, { telemetry: { endpoint: typed.trim() } });
      setEndpoint(null);
    }, t("settings.saved"));
  };

  return (
    <section aria-labelledby="settings-telemetry" className="flex flex-col gap-4">
      <h2 id="settings-telemetry" ref={heading} tabIndex={-1} className="font-medium">
        {t("settings.telemetry.title")}
      </h2>
      <p className="text-sm text-muted-foreground">{t("settings.telemetry.intro")}</p>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">{t("common.state")}</dt>
        <dd>
          <Badge variant={enabled ? "default" : "outline"}>
            {enabled ? t("settings.telemetry.on") : t("settings.telemetry.off")}
          </Badge>
        </dd>
        <dt className="text-muted-foreground">{t("settings.telemetry.endpoint")}</dt>
        <dd className="break-all">{saved || t("settings.telemetry.buildDefault")}</dd>
        <dt className="text-muted-foreground">{t("settings.telemetry.id")}</dt>
        <dd className="font-mono text-xs">{status?.id ?? "—"}</dd>
        <dt className="text-muted-foreground">{t("settings.telemetry.lastSent")}</dt>
        <dd>
          {status?.last_sent_at
            ? formatTime(new Date(status.last_sent_at * 1000), i18n.language)
            : t("settings.telemetry.never")}
        </dd>
        {status?.last_error && (
          <>
            <dt className="text-muted-foreground">{t("settings.telemetry.lastError")}</dt>
            <dd className="text-destructive">{status.last_error}</dd>
          </>
        )}
      </dl>
      {write && (
        <>
          <form noValidate onSubmit={saveEndpoint} className="flex flex-col gap-4">
            <Field
              id="settings-telemetry-endpoint"
              label={t("settings.telemetry.endpoint")}
              hint={t("settings.telemetry.endpointHint")}
            >
              {(a11y) => (
                <Input
                  id="settings-telemetry-endpoint"
                  type="url"
                  placeholder="https://"
                  value={typed}
                  disabled={busy}
                  onChange={(e) => setEndpoint(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
            <div>
              <Button type="submit" variant="outline" disabled={busy || typed.trim() === saved}>
                {t("settings.telemetry.saveEndpoint")}
              </Button>
            </div>
          </form>
          <div className="flex flex-wrap gap-2">
            {enabled ? (
              <Button
                variant="outline"
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    await patchSettings(queryClient, revision, { telemetry: { enabled: false } });
                    // 누른 버튼이 "켜기"로 바뀌어 사라진다.
                    heading.current?.focus();
                  }, t("settings.telemetry.disabled"))
                }
              >
                {t("settings.telemetry.disable")}
              </Button>
            ) : (
              <Button
                disabled={busy}
                onClick={(e) => {
                  returnFocus.current = e.currentTarget;
                  // 켜기 전에 보낼 내용을 보여 준다.
                  void run(async () => setPreview(await call(api.GET("/telemetry/preview"))));
                }}
              >
                {t("settings.telemetry.enable")}
              </Button>
            )}
            <Button
              variant="outline"
              disabled={busy}
              onClick={() =>
                void run(async () => {
                  storeSettings(queryClient, await call(api.POST("/telemetry/reset-id")));
                }, t("settings.telemetry.idReset"))
              }
            >
              {t("settings.telemetry.resetId")}
            </Button>
          </div>
        </>
      )}
      {preview && (
        <FormDialog
          title={t("settings.telemetry.previewTitle")}
          description={t("settings.telemetry.previewDescription")}
          submitLabel={t("settings.telemetry.confirmEnable")}
          returnFocus={returnFocus}
          onClose={() => setPreview(null)}
          onSubmit={() =>
            withSettingsError(t, async () => {
              // 입력했지만 저장하지 않은 끝점도 함께 보낸다(켜기는 그 끝점으로 보내야 한다).
              const changed = typed.trim() !== saved;
              await patchSettings(queryClient, revision, {
                telemetry: changed ? { enabled: true, endpoint: typed.trim() } : { enabled: true },
              });
              if (changed) {
                setEndpoint(null);
              }
              returnFocus.current = heading.current;
              toast.success(t("settings.telemetry.enabled"));
            })
          }
        >
          <pre className="max-h-80 overflow-auto rounded-md bg-muted p-2 font-mono text-xs">
            {JSON.stringify(preview, null, 2)}
          </pre>
        </FormDialog>
      )}
    </section>
  );
}
