import { useTranslation } from "react-i18next";

import type { components } from "@/api/schema";
import { StatusChip, type StatusTone } from "@/components/StatusChip";
import type { NodeSdn } from "@/sdn/sdn";

type Schemas = components["schemas"];

const CHASSIS_TONE: Record<Schemas["ChassisState"], StatusTone> = {
  ready: "success",
  configuring: "info",
  down: "danger",
  missing: "neutral",
  unknown: "neutral",
};

const ROLE_TONE: Record<Schemas["RaftRole"], StatusTone> = {
  leader: "success",
  follower: "success",
  candidate: "warning",
  joining: "info",
  down: "danger",
  unknown: "neutral",
};

/** 노드의 chassis 상태 칩(없으면 "없음") */
export function ChassisChip({ sdn }: { sdn: NodeSdn | undefined }) {
  const { t } = useTranslation();
  const state = sdn?.chassis ?? "missing";
  return <StatusChip tone={CHASSIS_TONE[state]}>{t(`sdn.chassis.${state}`)}</StatusChip>;
}

export function RoleChip({ role }: { role: Schemas["RaftRole"] | undefined }) {
  const { t } = useTranslation();
  if (!role) {
    return <span className="text-muted-foreground">—</span>;
  }
  return <StatusChip tone={ROLE_TONE[role]}>{t(`sdn.role.${role}`)}</StatusChip>;
}

/**
 * 노드 목록·상세의 SDN 칸: chassis 상태와 중앙이면 NB 역할. `stray`면(중앙 목록 밖에서 OVN DB를 알린 노드 — `GET /sdn`의
 * `strays`) 중앙 역할 대신 "남은 중앙 DB"다(리뷰 2 웹 I-3).
 */
export function NodeSdnCell({ sdn, stray = false }: { sdn: NodeSdn | undefined; stray?: boolean }) {
  const { t } = useTranslation();
  if (!sdn) {
    return "—";
  }
  return (
    <span className="flex flex-wrap items-center gap-1">
      <ChassisChip sdn={sdn} />
      {sdn.central &&
        (stray ? (
          <StatusChip tone="warning">{t("sdn.strays.chip")}</StatusChip>
        ) : (
          <span className="text-xs text-muted-foreground">
            {t("sdn.centralRole", { role: t(`sdn.role.${sdn.central.nb}`) })}
          </span>
        ))}
    </span>
  );
}

/**
 * 서브넷이 있는 SDN 네트워크에서 주소가 없는 NIC의 포트(꺼짐 — 리뷰 2 웹 I-1). VM이 꺼져 포트가 내려간 것과 다르게
 * 경고로 보이고 고치는 길을 적는다.
 */
export function PortOffChip({ hint = true }: { hint?: boolean }) {
  const { t } = useTranslation();
  return (
    <span className="flex flex-col items-start gap-0.5">
      <StatusChip tone="warning">{t("sdn.ips.off")}</StatusChip>
      {hint && <span className="text-xs text-muted-foreground">{t("sdn.ips.offHint")}</span>}
    </span>
  );
}
