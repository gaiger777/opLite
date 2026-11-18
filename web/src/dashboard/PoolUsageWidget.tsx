import { useTranslation } from "react-i18next";

import { useClusterHistory } from "@/charts/metrics";
import { useTimeRange } from "@/charts/useTimeRange";
import { WidgetCard } from "@/dashboard/WidgetCard";
import { errorMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";

/** 이만큼(%) 넘게 쓴 풀은 경고색이다. */
export const POOL_WARN_PERCENT = 85;

/** 풀 사용률 막대(Raft의 지금 값) */
export function PoolUsageWidget() {
  const { t } = useTranslation();
  const [range] = useTimeRange();
  const history = useClusterHistory(range);
  const pools = history.data?.pools ?? [];
  let body: React.ReactNode;
  if (!history.data) {
    body = (
      <p className="text-sm text-muted-foreground">
        {history.isError ? errorMessage(t, history.error) : t("common.loading")}
      </p>
    );
  } else if (pools.length === 0) {
    body = <p className="text-sm text-muted-foreground">{t("metrics.noPools")}</p>;
  } else {
    body = (
      <ul className="flex flex-col gap-3">
        {pools.map((pool) => {
          const percent =
            pool.total_bytes > 0 ? Math.round((pool.used_bytes / pool.total_bytes) * 100) : 0;
          const warn = percent > POOL_WARN_PERCENT;
          return (
            <li key={pool.pool} className="flex flex-col gap-1">
              <div className="flex items-baseline justify-between gap-2 text-sm">
                <span className="font-medium">{pool.name}</span>
                <span className="text-xs text-muted-foreground tabular-nums">
                  {formatBytes(pool.used_bytes)} / {formatBytes(pool.total_bytes)} · {percent} %
                </span>
              </div>
              <div
                role="meter"
                aria-label={pool.name}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={percent}
                aria-valuetext={warn ? t("metrics.poolWarn", { percent }) : `${percent} %`}
                className="h-2 w-full overflow-hidden rounded-full bg-muted"
              >
                <div
                  className={`h-full rounded-full ${warn ? "bg-warning" : "bg-chart-1"}`}
                  style={{ width: `${Math.min(100, percent)}%` }}
                />
              </div>
            </li>
          );
        })}
      </ul>
    );
  }
  return <WidgetCard title={t("metrics.poolUsage")}>{body}</WidgetCard>;
}
