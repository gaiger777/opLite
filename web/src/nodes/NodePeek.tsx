import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import type { Node } from "@/api/types";
import { PeekFacts } from "@/components/data-table/PeekFacts";
import { PinButton } from "@/components/PinButton";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { formatCapacity } from "@/lib/format";

/** 노드 미리 보기(설계 §5.3): 상태·주소·버전·CPU·메모리·VM 수와 열기 */
export function NodePeek({ node, vms }: { node: Node; vms?: number }) {
  const { t } = useTranslation();
  return (
    <>
      <SheetHeader>
        <div className="flex items-center gap-2 pr-8">
          <SheetTitle className="truncate">{node.name}</SheetTitle>
          <PinButton resource={{ kind: "node", id: node.id, name: node.name }} shortcut={false} />
        </div>
        <SheetDescription asChild>
          <div className="flex flex-wrap gap-1">
            <StatusChip tone={node.status.online ? "success" : "danger"}>
              {t(node.status.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
            </StatusChip>
            {node.spec.maintenance && (
              <StatusChip tone="warning">{t("dashboard.maintenance")}</StatusChip>
            )}
          </div>
        </SheetDescription>
      </SheetHeader>
      <PeekFacts
        facts={[
          [t("nodes.address"), <span className="font-mono text-xs">{node.spec.address}</span>],
          [t("nodes.version"), node.status.version ?? "—"],
          [t("nodes.cpus"), node.status.cpus],
          [t("nodes.memory"), formatCapacity(node.status.memory_mib)],
          [t("nodes.vms"), vms ?? "—"],
        ]}
      />
      <div className="flex flex-wrap gap-2 p-4">
        <Button size="sm" asChild>
          <Link to={`/nodes/${encodeURIComponent(node.name)}`}>{t("table.open")}</Link>
        </Button>
      </div>
    </>
  );
}
