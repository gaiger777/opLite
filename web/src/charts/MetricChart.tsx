import { Suspense, useId, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { TimeChart } from "@/charts/lazyTimeChart";
import {
  fieldStats,
  formatValue,
  pointTimes,
  type ChartSpec,
  type Range,
  type Series,
} from "@/charts/metrics";
import type { ChartLine } from "@/charts/TimeChart";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";

export const CHART_HEIGHT = 160;

/** 차트 높이만 한 자리(불러오는 동안) */
export function ChartSkeleton({ height = CHART_HEIGHT }: { height?: number }) {
  return (
    <div aria-hidden className="w-full animate-pulse rounded-md bg-muted" style={{ height }} />
  );
}

/** 시각을 범위에 맞게: 1시간·1일은 날짜 없이 */
function timeText(ms: number, range: Range, language: string): string {
  const date = new Date(ms);
  return range === "hour" || range === "day"
    ? date.toLocaleTimeString(language, { hour: "2-digit", minute: "2-digit" })
    : date.toLocaleString(language, { dateStyle: "short", timeStyle: "short" });
}

/**
 * 지표 차트 하나: 제목, uPlot 차트(AVG 선과 MAX·MIN 띠), 범례(AVG·MAX·MIN, 있으면 p50·p95 — 근사면 "≈"), "표로 보기".
 * 차트에는 화면 읽기 프로그램이 읽을 요약(`aria-label`)을 단다.
 */
export function MetricChart({
  spec,
  series,
  range,
  query,
  height = CHART_HEIGHT,
}: {
  spec: ChartSpec;
  series: Series | undefined;
  range: Range;
  query: { isError: boolean; error: unknown };
  height?: number;
}) {
  const { t, i18n } = useTranslation();
  const id = useId();
  const [table, setTable] = useState(false);
  const title = t(`metrics.chart.${spec.id}`);
  const times = useMemo(() => (series ? pointTimes(series, spec.fields) : []), [series, spec]);
  const lines = useMemo<ChartLine[]>(
    () =>
      spec.fields.map((field, i) => {
        const cf = series?.columns[field];
        return {
          label: t(`metrics.field.${field}`),
          color: i,
          values: cf?.avg ?? [],
          ...(cf?.max?.length && cf.min?.length ? { max: cf.max, min: cf.min } : {}),
        };
      }),
    [series, spec, t],
  );

  let body: React.ReactNode;
  if (!series) {
    body = query.isError ? (
      <p className="text-sm text-destructive">{errorMessage(t, query.error)}</p>
    ) : (
      <ChartSkeleton height={height} />
    );
  } else {
    const stats = spec.fields.map((field) => ({ field, stat: fieldStats(series, field) }));
    const pct = spec.fields.map((field) => series.pct?.[field]);
    const hasPct = pct.some((p) => p !== undefined);
    const approx = pct.some((p) => p?.approx);
    const fmt = (v: number | null | undefined) => formatValue(v, spec.unit);
    const summary = t("metrics.summary", {
      title,
      range: t(`metrics.ranges.${range}`),
      values: stats
        .map(({ field, stat }) =>
          t("metrics.summaryField", {
            field: t(`metrics.field.${field}`),
            avg: fmt(stat.avg),
            max: fmt(stat.max),
            min: fmt(stat.min),
          }),
        )
        .join("; "),
    });
    body = (
      <>
        <div role="img" aria-label={summary}>
          <Suspense fallback={<ChartSkeleton height={height} />}>
            <TimeChart times={times} lines={lines} unit={spec.unit} height={height} />
          </Suspense>
        </div>
        <Table aria-label={t("metrics.legendOf", { title })} className="text-xs">
          <TableHeader>
            <TableRow>
              <TableHead>{t("metrics.series")}</TableHead>
              <TableHead>{t("metrics.avg")}</TableHead>
              <TableHead>{t("metrics.max")}</TableHead>
              <TableHead>{t("metrics.min")}</TableHead>
              {hasPct && (
                <>
                  <TableHead>{approx ? `${t("metrics.p50")} ≈` : t("metrics.p50")}</TableHead>
                  <TableHead>{approx ? `${t("metrics.p95")} ≈` : t("metrics.p95")}</TableHead>
                </>
              )}
            </TableRow>
          </TableHeader>
          <TableBody>
            {stats.map(({ field, stat }, i) => (
              <TableRow key={field}>
                <TableCell>
                  <span
                    aria-hidden
                    className="mr-1.5 inline-block size-2 rounded-full"
                    style={{ background: `var(--chart-${(i % 5) + 1})` }}
                  />
                  {t(`metrics.field.${field}`)}
                </TableCell>
                <TableCell className="tabular-nums">{fmt(stat.avg)}</TableCell>
                <TableCell className="tabular-nums">{fmt(stat.max)}</TableCell>
                <TableCell className="tabular-nums">{fmt(stat.min)}</TableCell>
                {hasPct && (
                  <>
                    <TableCell className="tabular-nums">{fmt(pct[i]?.p50)}</TableCell>
                    <TableCell className="tabular-nums">{fmt(pct[i]?.p95)}</TableCell>
                  </>
                )}
              </TableRow>
            ))}
          </TableBody>
        </Table>
        {approx && <p className="text-xs text-muted-foreground">{t("metrics.approxNote")}</p>}
        {series.covered_from !== undefined && (
          <p className="text-xs text-muted-foreground">
            {t("metrics.coveredFrom", {
              time: timeText(series.covered_from, "week", i18n.language),
            })}
          </p>
        )}
        <div>
          <Button variant="ghost" size="sm" aria-expanded={table} onClick={() => setTable(!table)}>
            {t(table ? "metrics.hideTable" : "metrics.showTable")}
          </Button>
        </div>
        {table && (
          <div className="max-h-64 overflow-y-auto">
            <Table aria-label={t("metrics.tableOf", { title })} className="text-xs">
              <TableHeader>
                <TableRow>
                  <TableHead>{t("metrics.time")}</TableHead>
                  {spec.fields.map((field) => (
                    <TableHead key={field}>{t(`metrics.field.${field}`)}</TableHead>
                  ))}
                </TableRow>
              </TableHeader>
              <TableBody>
                {times.map((time, i) => (
                  <TableRow key={time}>
                    <TableCell>{timeText(time, range, i18n.language)}</TableCell>
                    {lines.map((line) => (
                      <TableCell key={line.label} className="tabular-nums">
                        {fmt(line.values[i])}
                      </TableCell>
                    ))}
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </>
    );
  }

  return (
    <section aria-labelledby={id} className="flex min-w-0 flex-col gap-2">
      <h3 id={id} className="text-sm font-medium">
        {title}
      </h3>
      {body}
    </section>
  );
}

/** 차트 넷을 한 묶음으로(두 칸 격자). 바깥 여백은 부르는 쪽이 정한다. */
export function MetricCharts({
  specs,
  series,
  range,
  query,
}: {
  specs: ChartSpec[];
  series: Series | undefined;
  range: Range;
  query: { isError: boolean; error: unknown };
}) {
  return (
    <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
      {specs.map((spec) => (
        <MetricChart key={spec.id} spec={spec} series={series} range={range} query={query} />
      ))}
    </div>
  );
}
