import { useTranslation } from "react-i18next";

import { MetricCharts } from "@/charts/MetricChart";
import { NODE_CHARTS, useClusterHistory } from "@/charts/metrics";
import { RangePicker } from "@/charts/RangePicker";
import { useTimeRange } from "@/charts/useTimeRange";
import { WidgetCard } from "@/dashboard/WidgetCard";

/**
 * 클러스터 CPU·메모리·디스크·네트워크(범위 고르기). CPU는 코어 수로 가중한 %, 나머지는 노드 합이다(AVG만).
 * 답하지 않은 노드·옛 노드·시계가 어긋난 노드는 합에서 빠졌다고 알린다.
 */
export function ClusterChartsWidget() {
  const { t } = useTranslation();
  const [range, setRange] = useTimeRange();
  const history = useClusterHistory(range);
  const data = history.data;
  const left = [
    ...(data?.unreachable ?? []).map((n) => t("metrics.nodeUnreachable", { node: n })),
    ...(data?.unsupported ?? []).map((n) => t("metrics.nodeUnsupported", { node: n })),
    ...(data?.skewed ?? []).map((n) => t("metrics.nodeSkewed", { node: n })),
  ];
  return (
    <WidgetCard
      title={t("metrics.clusterLoad")}
      action={<RangePicker value={range} onChange={setRange} />}
    >
      <MetricCharts specs={NODE_CHARTS} series={data} range={range} query={history} />
      {left.length > 0 && (
        <p className="text-xs text-muted-foreground">
          {t("metrics.leftOut", { nodes: left.join(", ") })}
        </p>
      )}
    </WidgetCard>
  );
}
