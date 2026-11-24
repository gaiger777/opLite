// 지도의 범례(계획 70 §7): 타일 상태 색, 블록 윗면의 부하 문턱(70·90%), 모양(빗금·줄무늬·테두리)과 켠 레이어의 색.
// 색만으로 알리지 않는다 — 모양마다 글이 붙는다.
import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";

import { HEAT_COLOR, NEUTRAL, POOL_COLOR, TILE_COLOR, UPLINK_COLOR } from "@/clusterMap/colors";
import type { MapLayer } from "@/clusterMap/paths";

const hatch = (base: string, line: string): CSSProperties => ({
  background: `repeating-linear-gradient(45deg, ${line} 0 2px, ${base} 2px 6px)`,
});

export function Legend({ layers }: { layers: readonly MapLayer[] }) {
  const { t } = useTranslation();
  const k = (key: string, values?: Record<string, unknown>) =>
    t(`clusterMap.legend.${key}`, values);
  const rows: [CSSProperties, string][] = [
    [{ background: TILE_COLOR.running }, t("vms.phase.running")],
    [{ background: TILE_COLOR.stopped }, t("vms.phase.stopped")],
    [{ background: TILE_COLOR.busy }, k("busy")],
    [{ background: TILE_COLOR.failed }, `${t("vms.phase.failed")} (×)`],
    [{ background: TILE_COLOR.migrating, borderColor: "var(--info)" }, t("vms.phase.migrating")],
    [{ borderColor: "var(--info)", borderStyle: "dashed" }, k("ghost")],
  ];
  if (layers.includes("load")) {
    rows.push(
      [{ background: HEAT_COLOR.cool }, k("cool")],
      [{ background: HEAT_COLOR.warm }, k("warm")],
      [{ background: HEAT_COLOR.hot }, k("hot")],
      [hatch(NEUTRAL, "var(--muted-foreground)"), k("unknown")],
    );
  }
  rows.push(
    [hatch("var(--warning)", "var(--foreground)"), t("dashboard.maintenance")],
    [
      { background: NEUTRAL, borderColor: "var(--danger)", borderWidth: 2 },
      t("dashboard.nodeOffline"),
    ],
  );
  if (layers.includes("storage")) {
    rows.push(
      [{ background: POOL_COLOR.cool }, k("pool")],
      [hatch("var(--danger-soft)", "var(--danger)"), k("poolBad")],
    );
  }
  if (layers.includes("network")) {
    rows.push(
      [{ background: UPLINK_COLOR.up }, k("uplinkUp")],
      [{ background: UPLINK_COLOR.degraded }, k("uplinkDegraded")],
      [{ background: UPLINK_COLOR.down }, k("uplinkDown")],
    );
  }
  if (layers.includes("ha")) {
    rows.push(
      [{ borderColor: "var(--muted-foreground)", borderStyle: "dashed" }, k("noFencing")],
      [{ borderColor: "var(--brand)", borderWidth: 2 }, k("haVm")],
    );
  }
  if (layers.includes("recommendations")) {
    rows.push([
      { borderColor: "var(--muted-foreground)", borderStyle: "dotted" },
      k("recommendation"),
    ]);
  }
  return (
    <ul aria-label={t("clusterMap.legendLabel")} className="flex flex-wrap gap-x-4 gap-y-1 text-xs">
      {rows.map(([style, label]) => (
        <li key={label} className="flex items-center gap-1.5">
          <span aria-hidden className="size-3 rounded-sm border" style={style} />
          {label}
        </li>
      ))}
    </ul>
  );
}
