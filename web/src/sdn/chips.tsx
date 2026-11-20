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

/** 노드 목록·상세의 SDN 칸: chassis 상태와 중앙이면 NB 역할 */
export function NodeSdnCell({ sdn }: { sdn: NodeSdn | undefined }) {
  const { t } = useTranslation();
  if (!sdn) {
    return "—";
  }
  return (
    <span className="flex flex-wrap items-center gap-1">
      <ChassisChip sdn={sdn} />
      {sdn.central && (
        <span className="text-xs text-muted-foreground">
          {t("sdn.centralRole", { role: t(`sdn.role.${sdn.central.nb}`) })}
        </span>
      )}
    </span>
  );
}
