import { useTranslation } from "react-i18next";

import type { Vm } from "@/api/types";
import { MetricCharts } from "@/charts/MetricChart";
import { useVmHistory, VM_CHARTS } from "@/charts/metrics";
import { RangePicker } from "@/charts/RangePicker";
import { useTimeRange } from "@/charts/useTimeRange";

/** VM 상세의 "지표" 탭: 차트 넷(범위 고르기). 기록은 VM의 주인 노드가 답한다. */
export function VmMetrics({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const [range, setRange] = useTimeRange();
  const history = useVmHistory(vm.id, range);
  return (
    <section aria-label={t("vms.tab.metrics")} className="flex flex-col gap-4 pt-2">
      <div className="flex justify-end">
        <RangePicker value={range} onChange={setRange} />
      </div>
      <MetricCharts specs={VM_CHARTS} series={history.data} range={range} query={history} />
    </section>
  );
}
