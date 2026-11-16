import type { RefObject } from "react";
import { useTranslation } from "react-i18next";

import type { Node } from "@/api/types";
import { DiskTable, useNodeDisks } from "@/ceph/AddOsdDialog";
import { focusBack } from "@/components/focusBack";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

/** 노드의 디스크와 Ceph OSD로 쓸 수 있는지(`GET /nodes/{key}/disks`). 연 쪽은 열 때만 그린다. */
export function NodeDisksDialog({
  node,
  onClose,
  returnFocus,
}: {
  node: Node;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const disks = useNodeDisks(node.id);
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-3xl" onCloseAutoFocus={(e) => focusBack(e, returnFocus)}>
        <DialogHeader>
          <DialogTitle>{t("nodes.disks.title", { name: node.name })}</DialogTitle>
          <DialogDescription>{t("nodes.disks.description")}</DialogDescription>
        </DialogHeader>
        <div className="max-h-[60vh] overflow-y-auto">
          {disks.data ? (
            <DiskTable disks={disks.data} label={t("ceph.disk.table")} />
          ) : (
            <ListPending error={disks.error} />
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t("common.done")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
