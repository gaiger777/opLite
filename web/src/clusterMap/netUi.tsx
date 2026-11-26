// 네트워크 층(계획 75 §4.2)의 늦게 받는 조각: 바닥 높이의 합친 경로(레일 띠·선·본딩 테두리·포트 면·빗금·×·장애 표시)와
// 글(레일 이름·"+n"), 포트·레일 카드(지도 화면 서랍의 몸통도). 네트워크 레이어를 켤 때만 받는다(지도 조각을 가볍게). 지도
// 조각의 모듈(`layout`·`paths`·`colors`·`MapScene`)을 실행 때 읽지 않는다 — `netPaths.ts` 머리 설명.
import { memo } from "react";
import { useTranslation } from "react-i18next";

import { iso, type Point, round, shade } from "@/components/iso/iso";
import { StatusChip } from "@/components/StatusChip";
import type { MapScene, PortItem } from "@/clusterMap/layout";
import type { Lod } from "@/clusterMap/lod";
import type { MapNode } from "@/clusterMap/model";
import {
  NET_COLOR as PORT_COLOR,
  NET_RAIL as RAIL_COLOR,
  type NetPiece,
  netPaths,
} from "@/clusterMap/netPaths";
import type { MapRail, RailTone } from "@/clusterMap/network";

/** 오른쪽 면 그늘(`paths.ts`의 `SHADE_OPACITY`와 같은 값) */
const SHADE_OPACITY = 0.28;

/** 글 하나(`MapScene`의 `Text`와 같은 꼴 — 바탕색 테두리) */
function Text({
  at,
  text,
  size,
  muted = false,
}: {
  at: Point;
  text: string;
  size: number;
  muted?: boolean;
}) {
  return (
    <text
      x={round(at.x)}
      y={round(at.y)}
      textAnchor="start"
      fontSize={size}
      style={{ fill: muted ? "var(--muted-foreground)" : "var(--foreground)" }}
      stroke="var(--background)"
      strokeWidth={3}
      paintOrder="stroke"
      strokeLinejoin="round"
    >
      {text}
    </text>
  );
}

/** 네트워크 층 경로 하나(계획 75 — 레일 띠·선·본딩 테두리·포트 면·빗금·×·장애 표시) */
const NetShape = memo(function NetShape({ piece, defs }: { piece: NetPiece; defs: string }) {
  const color = piece.tone ? PORT_COLOR[piece.tone] : RAIL_COLOR;
  switch (piece.kind) {
    case "rail":
      return <path d={piece.d} style={{ fill: shade(RAIL_COLOR, "top") }} aria-hidden />;
    case "wire":
      return (
        <path
          d={piece.d}
          fill="none"
          style={{ stroke: color }}
          strokeWidth={piece.tone === "down" ? 2 : 1.4}
          strokeDasharray={
            piece.tone === "degraded" ? "4 3" : piece.tone === "unknown" ? "1 3" : undefined
          }
          strokeLinejoin="round"
          aria-hidden
        />
      );
    case "wireCross":
    case "cross":
      return (
        <path
          d={piece.d}
          fill="none"
          style={{ stroke: piece.kind === "cross" ? "var(--background)" : color }}
          strokeWidth={1.6}
          strokeLinecap="round"
          aria-hidden
        />
      );
    case "bond":
      return (
        <path
          d={piece.d}
          fill="none"
          style={{ stroke: color }}
          strokeWidth={1.4}
          strokeDasharray={piece.tone === "degraded" ? "3 2" : undefined}
          aria-hidden
        />
      );
    case "side":
      return <path d={piece.d} style={{ fill: shade(color, "left") }} aria-hidden />;
    case "shade":
      return <path d={piece.d} fill="black" fillOpacity={SHADE_OPACITY} aria-hidden />;
    case "top":
      return <path d={piece.d} style={{ fill: shade(color, "top") }} aria-hidden />;
    case "hatch":
      return <path d={piece.d} fill={`url(#${defs}-bad)`} aria-hidden />;
    case "fault":
      return (
        <path
          d={piece.d}
          style={{ fill: "var(--danger)", stroke: "var(--background)" }}
          strokeWidth={1}
          aria-hidden
        />
      );
  }
});

/**
 * 네트워크 층 그림(원소 수는 `netElements`와 같다): 경로 조각마다 하나, 레일마다 이름 글, 판마다 "+n 레일", 포트 줄을 그린
 * 노드마다 "+n". 장면·단계가 바뀔 때만 다시 그린다.
 */
export const NetLayer = memo(function NetLayer({
  scene,
  lod,
  defs,
  railName: name,
}: {
  scene: MapScene;
  lod: Lod;
  defs: string;
  railName?(rail: { carrier: string; kind: string; networks: number }): string;
}) {
  const { t } = useTranslation();
  return (
    <>
      {netPaths(scene, lod).map((p) => (
        <NetShape key={p.key} piece={p} defs={defs} />
      ))}
      {[...scene.rails.values()].map((r) => {
        const a = iso(r.box.x + 4, r.box.y, r.box.z);
        return (
          <Text
            key={r.key}
            at={{ x: a.x + 2, y: a.y - 5 }}
            text={name ? name(r) : r.carrier}
            size={10}
          />
        );
      })}
      {scene.slabs
        .filter((s) => s.railMore > 0)
        .map((s) => {
          const last = s.rails[s.rails.length - 1]!.box;
          const a = iso(last.x + 4, last.y + last.d + 16, last.z);
          return (
            <Text
              key={`more:${s.key}`}
              at={{ x: a.x + 2, y: a.y }}
              text={t("clusterMap.net.moreRails", { count: s.railMore })}
              size={10}
              muted
            />
          );
        })}
      {scene.nodes
        .filter((n) => lod !== "summary" && (n.net?.ports.length ?? 0) > 0 && n.net!.more > 0)
        .map((n) => {
          const last = n.net!.ports[n.net!.ports.length - 1]!.box;
          const a = iso(last.x + last.w + 4, last.y + last.d, last.z);
          return (
            <Text
              key={`pmore:${n.key}`}
              at={{ x: a.x + 2, y: a.y + 4 }}
              text={`+${n.net!.more}`}
              size={9}
              muted
            />
          );
        })}
    </>
  );
});

const LINK_CHIP = {
  up: "success",
  degraded: "warning",
  down: "danger",
  absent: "neutral",
  unknown: "neutral",
} as const;

/**
 * 포트(계획 75): 이름·상태·속도·MTU·LLDP 이웃(없으면 lldpd 안내)·본딩, 장애면 영향(브리지·네트워크·VM 수). 서랍(쪽 보기)도
 * 같은 몸통을 쓴다.
 */
export function PortCard({
  port,
  node,
  speed,
  networkName,
}: {
  port: PortItem;
  node: MapNode;
  /** 속도 글(`labels.ts`의 `speedText` — 이 조각은 지도 조각의 모듈을 읽지 않는다) */
  speed: string | null;
  networkName(id: string): string;
}) {
  const { t } = useTranslation();
  const raw = node.net?.raw.ports.find((p) => p.name === port.name);
  const bond = port.bond ? node.net?.raw.ports.find((p) => p.name === port.bond) : undefined;
  const impact =
    node.net?.raw.impact?.find((i) => i.port === port.name) ??
    (port.bond ? node.net?.raw.impact?.find((i) => i.port === port.bond) : undefined);
  const lldp = raw?.lldp;
  return (
    <div className="flex flex-col gap-1.5">
      <p className="text-sm font-semibold">
        {t("clusterMap.net.portTitle", { name: port.name, node: node.name })}
      </p>
      <div className="flex flex-wrap gap-1">
        <StatusChip tone={LINK_CHIP[port.tone]}>
          {t(`clusterMap.net.health.${port.tone}`)}
        </StatusChip>
        {node.net?.legacy && <StatusChip tone="neutral">{t("clusterMap.net.legacy")}</StatusChip>}
      </div>
      {(speed || raw?.mtu) && (
        <p>
          {[
            speed && `${t("clusterMap.net.speed")} ${speed}`,
            raw?.mtu && `${t("clusterMap.net.mtu")} ${raw.mtu}`,
          ]
            .filter(Boolean)
            .join(" · ")}
        </p>
      )}
      {bond && (
        <p>
          {t("clusterMap.net.bond", { name: bond.name, mode: bond.bond?.mode ?? "—" })} ·{" "}
          {t(`clusterMap.net.health.${bond.health}`)}
        </p>
      )}
      <p className={lldp ? undefined : "text-muted-foreground"}>
        {lldp
          ? `${t("clusterMap.net.switch")}: ${lldp.chassis_name ?? lldp.chassis_id ?? "?"} · ${lldp.port_id ?? "?"}`
          : t("clusterMap.net.noLldp")}
      </p>
      {impact && (
        <div className="font-medium text-danger-fg">
          <p>
            {t("clusterMap.net.impact")}:{" "}
            {t(`clusterMap.label.impact.${impact.severity}`, {
              networks: impact.networks.length,
              vms: impact.vms,
            })}
          </p>
          {impact.bridges.length > 0 && (
            <p>{t("clusterMap.net.bridges", { names: impact.bridges.join(", ") })}</p>
          )}
          {impact.networks.length > 0 && <p>{impact.networks.map(networkName).join(", ")}</p>}
        </div>
      )}
    </div>
  );
}

/** 레일(계획 75): 이름·네트워크들·이 판의 노드 상태 수 */
export function RailCard({
  title,
  data,
  nodeTone,
}: {
  /** 머리글(레일 이름 — `labels.ts`의 `railName`) */
  title: string;
  data: MapRail | undefined;
  /** 이 판에서 이 레일을 쓰는 노드와 상태 */
  nodeTone: readonly [string, RailTone][];
}) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-1.5">
      <p className="text-sm font-semibold">{title}</p>
      <p>
        {t("clusterMap.net.networksOn")}:{" "}
        {(data?.networks ?? [])
          .map((n) => (n.vlan !== null ? `${n.name} (VLAN ${n.vlan})` : n.name))
          .join(", ") || "—"}
      </p>
      <div className="flex flex-wrap gap-1">
        {nodeTone.map(([name, tone]) => (
          <StatusChip key={name} tone={LINK_CHIP[tone]}>
            {name} · {t(`clusterMap.net.health.${tone}`)}
          </StatusChip>
        ))}
      </div>
    </div>
  );
}
