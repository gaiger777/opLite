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

/** 노드마다 범위 평균(CPU·메모리·디스크·네트워크). 20개 넘으면 평균 CPU 상위 20개다. */
export function NodeStripWidget() {
  const { t } = useTranslation();
  const [range] = useTimeRange();
  const history = useClusterHistory(range);
  const rows = history.data?.nodes ?? [];
  // 둘 다 모르면 모른다(옛 응답에 칸이 없을 수도 있다).
  const sum = (avg: Record<string, number | null>, a: string, b: string) =>
    avg[a] == null && avg[b] == null ? null : (avg[a] ?? 0) + (avg[b] ?? 0);
  return (
    <WidgetCard title={t("metrics.nodeLoad")}>
      <Table aria-label={t("metrics.nodeLoad")}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.node")}</TableHead>
            <TableHead>{t("metrics.chart.cpu")}</TableHead>
            <TableHead>{t("metrics.chart.memory")}</TableHead>
            <TableHead>{t("metrics.chart.disk")}</TableHead>
            <TableHead>{t("metrics.chart.network")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.length === 0 && (
            <TableMessage colSpan={5} query={history} empty={t("metrics.noNodeHistory")} />
          )}
          {rows.map((row) => (
            <TableRow key={row.node}>
              <TableCell className="font-medium">
                <Link
                  to={`/nodes/${encodeURIComponent(row.name)}`}
                  className="underline-offset-4 hover:underline"
                >
                  {row.name}
                </Link>
              </TableCell>
              <TableCell className="tabular-nums">
                {formatValue(row.avg.cpu, "percent")}
                <span className="ml-1 text-xs text-muted-foreground">
                  {t("metrics.cores", { count: row.cpus })}
                </span>
              </TableCell>
              <TableCell className="tabular-nums">
                {formatValue(row.avg.mem_used, "mib")} / {formatValue(row.avg.mem_total, "mib")}
              </TableCell>
              <TableCell className="tabular-nums">
                {formatValue(sum(row.avg, "disk_read", "disk_write"), "bytesRate")}
              </TableCell>
              <TableCell className="tabular-nums">
                {formatValue(sum(row.avg, "net_rx", "net_tx"), "bytesRate")}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {history.data && history.data.node_count > rows.length && (
        <p className="text-xs text-muted-foreground">
          {t("metrics.topNodes", { shown: rows.length, total: history.data.node_count })}
        </p>
      )}
    </WidgetCard>
  );
}
