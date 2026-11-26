// 네트워크 사슬도(계획 75 §4.1 자세히 판 4칸): 평면 SVG — 왼쪽 물리 포트에서 오른쪽 네트워크까지 열마다 칸, 칸 사이 선. 장애 포트에서
// 닿는 칸·선은 빨강(끊김)·주황(저하)이고 색만이 아니라 ×·빗금·점선과 글이 함께 간다. 그림 아래 포트 표가 LLDP·속도·carrier·
// rx/tx·오류(실시간 표본)를 글로 보인다.
import { useId } from "react";
import { useTranslation } from "react-i18next";

import type { components } from "@/api/schema";
import { formatValue } from "@/charts/metrics";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { type Chain, CHAIN_COLUMNS, type ChainItem } from "@/physical/chain";
import { ForgetPort } from "@/physical/ForgetPort";
import { HEALTH_TOKEN, tokenColor } from "@/physical/colors";

type IfaceSample = components["schemas"]["IfaceSample"];

const COL_W = 104;
const BOX_W = 88;
const BOX_H = 28;
const ROW_H = 38;

function short(text: string, max = 12): string {
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

function stroke(item: ChainItem): string {
  if (item.fault) {
    return tokenColor(HEALTH_TOKEN[item.fault]);
  }
  return tokenColor(item.health === "up" ? "--border" : HEALTH_TOKEN[item.health]);
}

export function ChainDiagram({
  chain,
  highlight,
  samples,
  node,
}: {
  chain: Chain;
  /** 강조할 포트 이름(포트를 골랐을 때 — 관리자가 내린 브리지면 브리지 칸) */
  highlight?: string;
  samples?: readonly IfaceSample[];
  /** 노드 이름(사라진 포트 "잊기") */
  node?: string;
}) {
  const { t } = useTranslation();
  const hatch = useId().replace(/:/g, "");
  const columns = CHAIN_COLUMNS.filter((k) => chain.items.some((i) => i.kind === k));
  const place = new Map<string, { x: number; y: number }>();
  let rows = 0;
  columns.forEach((kind, col) => {
    chain.items
      .filter((i) => i.kind === kind)
      .forEach((item, row) => {
        place.set(item.id, { x: col * COL_W, y: row * ROW_H });
        rows = Math.max(rows, row + 1);
      });
  });
  const width = Math.max(1, columns.length * COL_W - (COL_W - BOX_W));
  const height = Math.max(1, rows * ROW_H - (ROW_H - BOX_H));
  const faults = chain.items.filter((i) => i.fault);
  const summary =
    faults.length === 0
      ? t("physical.chain.summaryOk", { count: chain.items.length })
      : t("physical.chain.summaryFault", {
          items: faults.map((i) => `${i.label} (${t(`physical.health.${i.fault}`)})`).join(", "),
        });
  const sample = new Map((samples ?? []).map((s) => [s.name, s]));
  const ports = chain.items.filter((i) => i.port);
  const rate = (v: number | null | undefined) => formatValue(v ?? null, "bytesRate");

  return (
    <div className="flex flex-col gap-2">
      {chain.items.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("physical.chain.empty")}</p>
      ) : (
        <div className="overflow-x-auto">
          <svg
            role="img"
            aria-label={summary}
            width={width}
            height={height}
            viewBox={`0 0 ${width} ${height}`}
            className="text-[10px]"
          >
            <defs>
              <pattern
                id={hatch}
                width="6"
                height="6"
                patternUnits="userSpaceOnUse"
                patternTransform="rotate(45)"
              >
                <rect width="2" height="6" fill={tokenColor(HEALTH_TOKEN.degraded)} opacity="0.5" />
              </pattern>
            </defs>
            {chain.edges.map((e) => {
              const a = place.get(e.from);
              const b = place.get(e.to);
              if (!a || !b) return null;
              const x1 = a.x + BOX_W;
              const y1 = a.y + BOX_H / 2;
              const x2 = b.x;
              const y2 = b.y + BOX_H / 2;
              const mid = (x1 + x2) / 2;
              return (
                <path
                  key={`${e.from}>${e.to}`}
                  data-fault={e.fault}
                  d={`M${x1},${y1} C${mid},${y1} ${mid},${y2} ${x2},${y2}`}
                  fill="none"
                  stroke={e.fault ? tokenColor(HEALTH_TOKEN[e.fault]) : "var(--muted-foreground)"}
                  strokeWidth={e.fault ? 2 : 1}
                  strokeDasharray={e.fault === "degraded" ? "4 3" : undefined}
                />
              );
            })}
            {chain.items.map((item) => {
              const p = place.get(item.id)!;
              const on = highlight !== undefined && (item.port?.name ?? item.label) === highlight;
              const mark = item.fault === "down" || item.health === "down" ? "× " : "";
              const sub = [
                item.note,
                item.vms !== undefined ? t("physical.chain.vms", { count: item.vms }) : undefined,
              ]
                .filter(Boolean)
                .join(" · ");
              return (
                <g
                  key={item.id}
                  data-item={item.id}
                  data-fault={item.fault}
                  data-highlight={on || undefined}
                  transform={`translate(${p.x},${p.y})`}
                >
                  <title>
                    {`${item.label} — ${t(`physical.health.${item.fault ?? item.health}`)}${sub ? ` (${sub})` : ""}`}
                  </title>
                  <rect
                    width={BOX_W}
                    height={BOX_H}
                    rx={4}
                    fill={item.fault === "degraded" ? `url(#${hatch})` : "var(--card)"}
                    stroke={on ? "var(--ring)" : stroke(item)}
                    strokeWidth={on ? 3 : item.fault ? 2 : 1}
                  />
                  <circle
                    cx={8}
                    cy={BOX_H / 2}
                    r={3}
                    fill={tokenColor(HEALTH_TOKEN[item.health])}
                  />
                  <text x={15} y={sub ? 11 : 17} fill="currentColor" className="font-medium">
                    {mark}
                    {short(item.label)}
                  </text>
                  {sub && (
                    <text x={15} y={23} fill="var(--muted-foreground)">
                      {short(sub, 14)}
                    </text>
                  )}
                </g>
              );
            })}
          </svg>
        </div>
      )}
      {chain.unused > 0 && (
        <p className="text-xs text-muted-foreground">
          {t("physical.chain.unused", { count: chain.unused })}
        </p>
      )}
      {ports.length > 0 && (
        <Table aria-label={t("physical.chain.ports")} className="text-xs">
          <TableHeader>
            <TableRow>
              <TableHead>{t("physical.chain.port")}</TableHead>
              <TableHead>{t("physical.chain.state")}</TableHead>
              <TableHead>{t("physical.chain.switch")}</TableHead>
              <TableHead>{t("physical.chain.traffic")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {ports.map((item) => {
              const p = item.port!;
              const s = sample.get(p.name);
              const errors =
                (s?.rx_errors ?? 0) +
                (s?.tx_errors ?? 0) +
                (s?.rx_dropped ?? 0) +
                (s?.tx_dropped ?? 0);
              return (
                <TableRow
                  key={item.id}
                  aria-current={p.name === highlight ? "true" : undefined}
                  className="aria-[current]:bg-muted"
                >
                  <TableCell className="font-medium">{p.name}</TableCell>
                  <TableCell>
                    {item.fault === "down" || p.health === "down" ? "× " : ""}
                    {t(`physical.health.${item.fault ?? p.health}`)}
                    {p.speed_mbps ? ` · ${t("physical.chain.speed", { speed: p.speed_mbps })}` : ""}
                    {p.carrier === false ? ` · ${t("physical.chain.noCarrier")}` : ""}
                    {p.bond ? ` · ${p.bond.mode}` : ""}
                    {/* 본딩의 이상한 구성 포트(사라짐·LACP 어긋남 — 리뷰 2: 왜 저하인지) */}
                    {(p.bond?.members ?? [])
                      .filter((m) => m.state !== "up")
                      .map((m) => ` · ${m.name} ${t(`nodes.interfaces.portState.${m.state}`)}`)
                      .join("")}
                    {p.addresses?.length ? ` · ${p.addresses.join(", ")}` : ""}
                    {p.state === "missing" && node && (
                      <>
                        {" "}
                        <ForgetPort node={node} port={p.name} />
                      </>
                    )}
                  </TableCell>
                  <TableCell>
                    {p.lldp
                      ? [p.lldp.chassis_name, p.lldp.port_id].filter(Boolean).join(" / ")
                      : p.kind === "ethernet"
                        ? "—"
                        : ""}
                  </TableCell>
                  <TableCell className="tabular-nums">
                    {s ? `↓ ${rate(s.rx_bps)} ↑ ${rate(s.tx_bps)}` : "—"}
                    {errors > 0 && (
                      <span className="ml-1 text-danger-fg">
                        {t("physical.chain.errors", { count: errors })}
                      </span>
                    )}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      {/* 이웃이 하나도 없으면 lldpd가 없는 노드다. */}
      {ports.some((i) => i.kind === "ethernet") && !ports.some((i) => i.port?.lldp) && (
        <p className="text-xs text-muted-foreground">{t("physical.chain.lldpHint")}</p>
      )}
    </div>
  );
}
