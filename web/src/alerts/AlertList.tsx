import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { alertPhase, subjectText, type AlertView } from "@/alerts/alerts";
import { severityTone, type Severity } from "@/alerts/severity";
import { StatusChip } from "@/components/StatusChip";
import { formatTime } from "@/lib/format";

/** 심각도 칩 */
export function SeverityBadge({ severity }: { severity: Severity }) {
  const { t } = useTranslation();
  return <StatusChip tone={severityTone(severity)}>{t(`alerts.severity.${severity}`)}</StatusChip>;
}

/** 값 숫자(단위는 규칙에 따라 다르므로 숫자만) */
function num(v: number): string {
  return String(+v.toFixed(2));
}

/**
 * 활성 경보 목록. 보여 주기만 한다(읽기·폴링은 `useAlerts()`를 쓰는 쪽이 한다). `compact`면 한 줄씩 짧게 보인다
 * (홈 위젯·종 서랍).
 */
export function AlertList({
  alerts,
  compact = false,
  label,
  subjectHref,
}: {
  alerts: AlertView[];
  compact?: boolean;
  /** 목록의 접근 이름 */
  label: string;
  /** 대상의 화면(주면 대상이 링크다) */
  subjectHref?: (subject: AlertView["subject"]) => string;
}) {
  const { t, i18n } = useTranslation();
  if (alerts.length === 0) {
    return <p className="text-sm text-muted-foreground">{t("alerts.none")}</p>;
  }
  return (
    <ul aria-label={label} className="flex flex-col divide-y text-sm">
      {alerts.map((alert) => {
        const phase = alertPhase(alert);
        return (
          <li
            key={`${alert.rule}/${alert.subject.kind}/${alert.subject.id}/${alert.subject.port ?? ""}`}
            className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2"
          >
            <SeverityBadge severity={alert.severity} />
            <span className="font-medium">{alert.rule_name}</span>
            {subjectHref ? (
              <Link
                to={subjectHref(alert.subject)}
                className="font-mono text-xs underline-offset-4 hover:underline"
              >
                {subjectText(alert.subject)}
              </Link>
            ) : (
              <span className="font-mono text-xs">{subjectText(alert.subject)}</span>
            )}
            <span className="text-xs text-muted-foreground">{t(`alerts.phase.${phase}`)}</span>
            {!compact && alert.value !== undefined && (
              <span className="text-xs tabular-nums">
                {alert.threshold !== undefined
                  ? t("alerts.valueThreshold", {
                      value: num(alert.value),
                      threshold: num(alert.threshold),
                    })
                  : t("alerts.value", { value: num(alert.value) })}
              </span>
            )}
            <span className="ml-auto text-xs text-muted-foreground">
              {t("alerts.since", { time: formatTime(new Date(alert.since), i18n.language) })}
            </span>
          </li>
        );
      })}
    </ul>
  );
}
