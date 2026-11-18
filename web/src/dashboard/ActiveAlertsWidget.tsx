import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { AlertList } from "@/alerts/AlertList";
import { useAlerts } from "@/alerts/alerts";
import { WidgetCard } from "@/dashboard/WidgetCard";
import { errorMessage } from "@/i18n/errors";

/** 홈에 보이는 활성 경보 수 */
const SHOWN = 5;

/** 활성 경보(심각도 순, 30초마다 — `useAlerts()`)와 경보 화면으로 가는 링크 */
export function ActiveAlertsWidget() {
  const { t } = useTranslation();
  const alerts = useAlerts();
  const list = alerts.data?.alerts ?? [];
  return (
    <WidgetCard
      title={t("alerts.active")}
      action={
        <Link to="/alerts" className="text-sm underline-offset-4 hover:underline">
          {t("alerts.viewAll")}
        </Link>
      }
    >
      {alerts.data ? (
        <AlertList alerts={list.slice(0, SHOWN)} compact label={t("alerts.active")} />
      ) : (
        <p className="text-sm text-muted-foreground">
          {alerts.isError ? errorMessage(t, alerts.error) : t("common.loading")}
        </p>
      )}
      {list.length > SHOWN && (
        <p className="text-xs text-muted-foreground">
          {t("alerts.more", { count: list.length - SHOWN })}
        </p>
      )}
    </WidgetCard>
  );
}
