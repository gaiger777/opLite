import { useEffect, useMemo, useRef } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";

import { useChartColors, withAlpha, type ChartColors } from "@/charts/chartColors";
import { formatValue, type Unit } from "@/charts/format";
import { formatTime } from "@/lib/format";

/** 차트의 선 하나. `max`·`min`이 있으면 그 사이를 옅은 띠로 그린다. `null`은 끊어 그린다. */
export interface ChartLine {
  label: string;
  /** `--chart-N`의 N - 1 */
  color: number;
  values: (number | null)[];
  max?: (number | null)[];
  min?: (number | null)[];
}

export interface TimeChartProps {
  /** 점마다의 시각(유닉스 ms) */
  times: number[];
  lines: ChartLine[];
  unit: Unit;
  height: number;
}

function options(
  lines: ChartLine[],
  unit: Unit,
  width: number,
  height: number,
  colors: ChartColors,
): uPlot.Options {
  const series: uPlot.Series[] = [{}];
  const bands: uPlot.Band[] = [];
  const mains: number[] = [];
  for (const line of lines) {
    const color = colors.series[line.color % colors.series.length]!;
    mains.push(series.length);
    series.push({ label: line.label, stroke: color, width: 1.5, points: { show: false } });
    if (line.max && line.min) {
      const edge = { stroke: withAlpha(color, 0.25), width: 1, points: { show: false } };
      series.push({ label: `${line.label} max`, ...edge }, { label: `${line.label} min`, ...edge });
      bands.push({ series: [series.length - 2, series.length - 1], fill: withAlpha(color, 0.12) });
    }
  }
  const axis = {
    stroke: colors.text,
    grid: { stroke: colors.grid, width: 1 },
    ticks: { stroke: colors.grid, width: 1 },
  };
  return {
    width,
    height,
    series,
    bands,
    legend: { show: false },
    plugins: [tooltipPlugin(mains, unit)],
    scales: { y: { range: (_u, _min, max) => [0, max > 0 ? max * 1.1 : 1] } },
    axes: [
      axis,
      { ...axis, size: 72, values: (_u, splits) => splits.map((v) => formatValue(v, unit)) },
    ],
  };
}

/**
 * 가리키는 시각의 값 풍선(범례를 끄면 uPlot은 십자선만 그린다). 차트 위 겹(`u.over`)에 붙고 포인터를 가로채지 않는다.
 * `mains`는 AVG 선의 시리즈 번호(MAX·MIN 띠 가장자리는 빼고)다. 값은 `setData`로 바뀌어도 늘 `u.data`에서 읽는다.
 */
export function tooltipPlugin(mains: number[], unit: Unit): uPlot.Plugin {
  let tip: HTMLDivElement | null = null;
  const row = (text: string, value?: string, color?: string) => {
    const el = document.createElement("div");
    el.className = "flex items-center gap-1.5";
    if (color) {
      const dot = document.createElement("span");
      dot.className = "inline-block size-2 shrink-0 rounded-full";
      dot.style.background = color;
      el.append(dot);
    }
    const name = document.createElement("span");
    name.textContent = text;
    el.append(name);
    if (value !== undefined) {
      const v = document.createElement("span");
      v.className = "ml-auto pl-3 font-medium tabular-nums";
      v.textContent = value;
      el.append(v);
    }
    return el;
  };
  return {
    hooks: {
      init: (u) => {
        tip = document.createElement("div");
        tip.setAttribute("aria-hidden", "true");
        tip.dataset.slot = "chart-tooltip";
        tip.className =
          "pointer-events-none absolute top-1 left-0 z-10 rounded-md border bg-popover px-2 py-1 text-xs whitespace-nowrap text-popover-foreground shadow-md";
        tip.style.display = "none";
        u.over.append(tip);
      },
      setCursor: (u) => {
        if (!tip) {
          return;
        }
        const { idx, left } = u.cursor;
        const time = idx === null || idx === undefined ? undefined : u.data[0][idx];
        if (time === undefined || left === undefined || left < 0) {
          tip.style.display = "none";
          return;
        }
        tip.replaceChildren(
          row(formatTime(new Date(time * 1000), document.documentElement.lang || "en")),
          ...mains.map((i) => {
            const s = u.series[i];
            const color = typeof s?.stroke === "string" ? s.stroke : undefined;
            return row(String(s?.label ?? ""), formatValue(u.data[i]?.[idx!], unit), color);
          }),
        );
        tip.style.display = "block";
        // 커서 오른쪽, 넘치면 왼쪽
        const width = tip.offsetWidth;
        const room = u.over.clientWidth;
        const x = left + 12 + width > room ? left - 12 - width : left + 12;
        tip.style.transform = `translateX(${Math.max(0, Math.round(x))}px)`;
      },
    },
  };
}

/** uPlot의 데이터: 시각(초)과 선마다 AVG(·MAX·MIN) 열 */
function toData(times: number[], lines: ChartLine[]): uPlot.AlignedData {
  const columns: (number | null)[][] = [];
  for (const line of lines) {
    columns.push(line.values);
    if (line.max && line.min) {
      columns.push(line.max, line.min);
    }
  }
  return [times.map((t) => t / 1000), ...columns] as uPlot.AlignedData;
}

/**
 * uPlot 시간 차트. 폭은 컨테이너를 따르고(`ResizeObserver`), 색은 테마 토큰에서 읽는다. 무거우므로
 * `lazyTimeChart`로 차트를 그릴 때만 내려받는다. 시험에서는 가짜로 바꾼다(`test/setup.ts`).
 */
export default function TimeChart({ times, lines, unit, height }: TimeChartProps) {
  const box = useRef<HTMLDivElement>(null);
  const plot = useRef<uPlot | null>(null);
  const colors = useChartColors();
  const data = useMemo(() => toData(times, lines), [times, lines]);
  // 선의 구성이 바뀔 때만 차트를 새로 만든다. 값만 바뀌면 `setData`다.
  const shape = lines.map((l) => `${l.label}:${l.color}:${l.max && l.min ? 1 : 0}`).join("|");
  const latest = useRef({ data, lines });

  // 새 값은 그리던 차트에 넣는다. 아래에서 차트를 새로 만들 때도 이 값을 쓴다(이 효과가 먼저 돈다).
  useEffect(() => {
    latest.current = { data, lines };
    plot.current?.setData(data);
  }, [data, lines]);

  useEffect(() => {
    const el = box.current!;
    const { data: first, lines: current } = latest.current;
    const u = new uPlot(options(current, unit, el.clientWidth || 300, height, colors), first, el);
    plot.current = u;
    const observer = new ResizeObserver(([entry]) => {
      const width = Math.floor(entry?.contentRect.width ?? 0);
      if (width > 0 && width !== u.width) {
        u.setSize({ width, height });
      }
    });
    observer.observe(el);
    return () => {
      observer.disconnect();
      u.destroy();
      plot.current = null;
    };
  }, [shape, unit, height, colors]);

  return <div ref={box} className="w-full" />;
}
