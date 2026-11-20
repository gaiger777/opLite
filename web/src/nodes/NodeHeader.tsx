import { useId } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import type { Node } from "@/api/types";
import { useVisit } from "@/components/PinButton";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { ResourceHeader } from "@/components/ResourceHeader";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { formatCapacity } from "@/lib/format";
import { isFrozen, useNodeActions } from "@/nodes/NodeActions";
import { usePermissions } from "@/auth/usePermissions";
import { NodeSdnCell } from "@/sdn/chips";
import { strayNodes, useSdn } from "@/sdn/sdn";

/**
 * 노드 상세의 머리(`ResourceHeader`): 이름·상태·★, 주 동작(유지보수·비우기), 케밥(업데이트·인터페이스·디스크·
 * 레이블·펜스·얼림 풀기·지우기), 요약 카드(주소·CPU·메모리·VM). 동작은 노드 목록과 같은 `useNodeActions`다.
 */
export function NodeHeader({ node, vms }: { node: Node; vms?: number }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const hint = useId();
  const actions = useNodeActions({ onRemoved: () => void navigate("/nodes") });
  // 중앙 역할을 알린 노드만 `GET /sdn`으로 목록의 중앙인지(아니면 남은 중앙 DB) 본다(리뷰 2 웹 I-3).
  const readsSdn = usePermissions().reads("sdn");
  const sdn = useSdn(false, readsSdn && !!node.status.sdn?.central);
  const stray = strayNodes(sdn.data).has(node.id);
  useVisit({ kind: "node", id: node.id, name: node.name });
  const witness = node.spec.witness;
  const drain = actions.write && !witness ? actions.drainReason(node) : null;
  const menu = actions.write || node.status.updates || !witness;
  return (
    <>
      <ResourceHeader
        name={node.name}
        pin={{ kind: "node", id: node.id, name: node.name }}
        status={
          <>
            <StatusChip tone={node.status.online ? "success" : "danger"}>
              {t(node.status.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
            </StatusChip>
            {node.spec.maintenance && (
              <StatusChip tone="warning">{t("dashboard.maintenance")}</StatusChip>
            )}
            {witness && <StatusChip tone="neutral">{t("dashboard.witness")}</StatusChip>}
            {isFrozen(node) && <StatusChip tone="info">{t("nodes.frozen")}</StatusChip>}
          </>
        }
        actions={
          actions.write && (
            <>
              <Button
                variant="outline"
                size="sm"
                onPointerDown={actions.rememberTrigger}
                onKeyDown={actions.rememberTrigger}
                onClick={() => actions.toggleMaintenance(node)}
              >
                {t(node.spec.maintenance ? "nodes.endMaintenance" : "nodes.startMaintenance")}
              </Button>
              {!witness && (
                <ReasonButton
                  variant="outline"
                  size="sm"
                  reason={drain}
                  reasons={hint}
                  onPointerDown={actions.rememberTrigger}
                  onKeyDown={actions.rememberTrigger}
                  onClick={() => actions.show("drain", node)}
                >
                  {t("nodes.drain.menu")}
                </ReasonButton>
              )}
            </>
          )
        }
        menu={menu ? actions.items(node, { primary: false }) : undefined}
        menuTrigger={{
          onPointerDown: actions.rememberTrigger,
          onKeyDown: actions.rememberTrigger,
        }}
        summary={[
          {
            label: t("nodes.address"),
            value: <span className="font-mono text-sm">{node.spec.address}</span>,
          },
          { label: t("nodes.cpus"), value: node.status.cpus },
          { label: t("nodes.memory"), value: formatCapacity(node.status.memory_mib) },
          { label: t("nodes.vms"), value: vms ?? "—" },
          // SDN(OVN) chassis·중앙 역할(계획 65). SDN을 세우지 않았으면 없다.
          ...(node.status.sdn
            ? [
                {
                  label: t("nodes.sdn"),
                  value: <NodeSdnCell sdn={node.status.sdn} stray={stray} />,
                },
              ]
            : []),
        ]}
      >
        <ReasonList base={hint} reasons={[drain]} className="basis-full" />
      </ResourceHeader>
      {actions.dialogs}
    </>
  );
}
