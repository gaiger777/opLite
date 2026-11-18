import { MoreHorizontal } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { Node } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { NodeDisksDialog } from "@/nodes/NodeDisksDialog";
import { NodeFenceDialog } from "@/nodes/NodeFenceDialog";
import { NodeInterfacesDialog } from "@/nodes/NodeInterfacesDialog";
import { NodeUpdatesDialog } from "@/nodes/NodeUpdatesDialog";

type Dialog = "updates" | "interfaces" | "disks" | "fence";

/**
 * 노드 상세의 머리: 이름·상태와 동작 메뉴. 동작은 노드 목록의 대화상자를 그대로 쓴다(유지보수·비우기·지우기는
 * 노드 목록에 남는다 — 계획 61이 머리를 `ResourceHeader`로 바꾸며 모은다).
 */
export function NodeHeader({ node }: { node: Node }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const write = can(me.role, "write", "node");
  const [open, setOpen] = useState<Dialog | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const close = () => setOpen(null);
  const items: { id: Dialog; label: string }[] = [
    ...(node.status.updates ? [{ id: "updates" as const, label: t("nodes.updates.menu") }] : []),
    ...(!node.spec.witness
      ? [{ id: "interfaces" as const, label: t("nodes.interfaces.menu") }]
      : []),
    ...(write && !node.spec.witness
      ? [
          { id: "disks" as const, label: t("nodes.disks.menu") },
          { id: "fence" as const, label: t("nodes.fence.menu") },
        ]
      : []),
  ];
  return (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-2xl font-semibold">{node.name}</h1>
        <Badge variant={node.status.online ? "default" : "destructive"}>
          {t(node.status.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
        </Badge>
        {node.spec.maintenance && <Badge variant="outline">{t("dashboard.maintenance")}</Badge>}
        {node.spec.witness && <Badge variant="outline">{t("dashboard.witness")}</Badge>}
        <span className="font-mono text-xs text-muted-foreground">{node.spec.address}</span>
      </div>
      {items.length > 0 && (
        <DropdownMenu modal={false}>
          <DropdownMenuTrigger asChild>
            <Button
              ref={trigger}
              variant="outline"
              size="sm"
              aria-label={t("vms.actionsFor", { name: node.name })}
            >
              <MoreHorizontal />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {items.map((item) => (
              <DropdownMenuItem key={item.id} onSelect={() => setOpen(item.id)}>
                {item.label}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      )}
      {open === "updates" && (
        <NodeUpdatesDialog node={node} returnFocus={trigger} onClose={close} />
      )}
      {open === "interfaces" && (
        <NodeInterfacesDialog node={node} returnFocus={trigger} onClose={close} />
      )}
      {open === "disks" && <NodeDisksDialog node={node} returnFocus={trigger} onClose={close} />}
      {open === "fence" && <NodeFenceDialog node={node} returnFocus={trigger} onClose={close} />}
    </div>
  );
}
