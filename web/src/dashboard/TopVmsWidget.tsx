import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { formatValue, useClusterHistory } from "@/charts/metrics";
import { useTimeRange } from "@/charts/useTimeRange";
import { TableMessage } from "@/components/TableMessage";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { WidgetCard } from "@/dashboard/WidgetCard";

/** 범위 평균 CPU 상위 VM 10개 */
export function TopVmsWidget() {
  const { t } = useTranslation();
  const [range] = useTimeRange();
  const history = useClusterHistory(range);
  const top = history.data?.top ?? [];
  return (
    <WidgetCard title={t("metrics.topVms")}>
      <Table aria-label={t("metrics.topVms")}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("common.node")}</TableHead>
            <TableHead>{t("metrics.chart.cpu")}</TableHead>
            <TableHead>{t("metrics.field.mem_rss")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {top.length === 0 && (
            <TableMessage colSpan={4} query={history} empty={t("metrics.noVms")} />
          )}
          {top.map((vm) => (
            <TableRow key={vm.vm}>
              <TableCell className="font-medium">
                <Link
                  to={`/vms/${encodeURIComponent(vm.name)}?tab=metrics`}
                  className="underline-offset-4 hover:underline"
                >
                  {vm.name}
                </Link>
              </TableCell>
              <TableCell>{vm.node ?? "—"}</TableCell>
              <TableCell className="tabular-nums">{formatValue(vm.cpu, "percent")}</TableCell>
              <TableCell className="tabular-nums">{formatValue(vm.mem_rss, "mib")}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </WidgetCard>
  );
}
