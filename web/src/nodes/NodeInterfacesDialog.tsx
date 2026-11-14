import { Fragment, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import type { Node, NodeInterface } from "@/api/types";
import { focusBack } from "@/components/focusBack";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatTime } from "@/lib/format";

/** 링크 속도: 10000 → "10 Gb/s", 1000 → "1 Gb/s", 100 → "100 Mb/s" */
export function speedText(mbps: number | undefined): string {
  if (mbps === undefined) {
    return "—";
  }
  return mbps >= 1000 && mbps % 1000 === 0 ? `${mbps / 1000} Gb/s` : `${mbps} Mb/s`;
}

function carrierText(t: ReturnType<typeof useTranslation>["t"], iface: NodeInterface): string {
  return iface.carrier === undefined
    ? "—"
    : t(iface.carrier ? "nodes.interfaces.carrierYes" : "nodes.interfaces.carrierNo");
}

/**
 * 노드가 보고한 네트워크 인터페이스(30초마다 목록, 5초마다 링크·본딩 상태)와 HA 검사 결과. 본딩 포트는 아래 줄에
 * 보이고 `up`이 아닌 포트는 빨간 글자와 상태 문구다. 결과는 watch로 바뀐 Node를 따라간다. 연 쪽은 열 때만 그린다.
 */
export function NodeInterfacesDialog({
  node: opened,
  onClose,
  returnFocus,
}: {
  node: Node;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t, i18n } = useTranslation();
  const nodes = useList("node");
  const node = nodes.data?.find((n) => n.id === opened.id) ?? opened;
  const interfaces = node.status.interfaces ?? [];
  const checks = node.status.checks ?? [];

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-4xl" onCloseAutoFocus={(e) => focusBack(e, returnFocus)}>
        <DialogHeader>
          <DialogTitle>{t("nodes.interfaces.title", { name: node.name })}</DialogTitle>
          <DialogDescription>{t("nodes.interfaces.description")}</DialogDescription>
        </DialogHeader>
        <div className="max-h-[60vh] overflow-y-auto">
          {interfaces.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t("nodes.interfaces.none")}</p>
          ) : (
            <Table aria-label={t("nodes.interfaces.table")}>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("common.name")}</TableHead>
                  <TableHead>{t("nodes.interfaces.kind")}</TableHead>
                  <TableHead>{t("common.state")}</TableHead>
                  <TableHead>{t("nodes.interfaces.carrier")}</TableHead>
                  <TableHead>{t("nodes.interfaces.speed")}</TableHead>
                  <TableHead>{t("nodes.interfaces.master")}</TableHead>
                  <TableHead>{t("nodes.interfaces.addresses")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {interfaces.map((iface) => (
                  <Fragment key={iface.name}>
                    <TableRow>
                      <TableCell className="font-mono text-xs font-medium">{iface.name}</TableCell>
                      <TableCell>
                        {t(`nodes.interfaces.kinds.${iface.kind}`)}
                        {iface.bond && (
                          <span className="block text-xs text-muted-foreground">
                            {iface.bond.mode}
                          </span>
                        )}
                      </TableCell>
                      <TableCell>
                        <span className="font-mono text-xs">{iface.state}</span>
                        {iface.bond && iface.bond.state !== "up" && (
                          <Badge
                            variant={iface.bond.state === "unknown" ? "secondary" : "destructive"}
                            className="ml-2"
                          >
                            {t(`nodes.interfaces.bondState.${iface.bond.state}`)}
                          </Badge>
                        )}
                      </TableCell>
                      <TableCell>{carrierText(t, iface)}</TableCell>
                      <TableCell className="whitespace-nowrap">
                        {speedText(iface.speed_mbps)}
                      </TableCell>
                      <TableCell className="font-mono text-xs">{iface.master ?? "—"}</TableCell>
                      <TableCell className="font-mono text-xs">
                        {(iface.addresses ?? []).join(", ") || "—"}
                      </TableCell>
                    </TableRow>
                    {iface.bond?.ports.map((port) => {
                      // 이 화면이 모르는 상태(뒤 버전)는 이상으로 보이지 않는다.
                      const bad = port.state !== "up" && port.state !== "unknown";
                      return (
                        <TableRow key={`${iface.name}/${port.name}`}>
                          <TableCell className="pl-6 font-mono text-xs">
                            <span className="sr-only">
                              {t("nodes.interfaces.portOf", { bond: iface.name })}
                            </span>
                            {port.name}
                          </TableCell>
                          <TableCell className="text-muted-foreground">
                            {t("nodes.interfaces.port")}
                            {port.aggregator !== undefined && (
                              <span className="block text-xs">
                                {t("nodes.interfaces.aggregator", { id: port.aggregator })}
                              </span>
                            )}
                          </TableCell>
                          <TableCell
                            colSpan={2}
                            className={bad ? "font-medium text-destructive" : undefined}
                          >
                            {t(`nodes.interfaces.portState.${port.state}`)}
                          </TableCell>
                          <TableCell className="whitespace-nowrap">
                            {speedText(port.speed_mbps)}
                          </TableCell>
                          <TableCell colSpan={2} />
                        </TableRow>
                      );
                    })}
                  </Fragment>
                ))}
              </TableBody>
            </Table>
          )}
          <h3 className="mt-4 mb-2 text-sm font-medium">{t("nodes.interfaces.checks")}</h3>
          {checks.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t("nodes.interfaces.noChecks")}</p>
          ) : (
            <Table aria-label={t("nodes.interfaces.checks")}>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("common.name")}</TableHead>
                  <TableHead>{t("common.state")}</TableHead>
                  <TableHead>{t("nodes.interfaces.since")}</TableHead>
                  <TableHead>{t("tasks.error")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {checks.map((check) => (
                  <TableRow key={check.name}>
                    <TableCell className="font-medium">{check.name}</TableCell>
                    <TableCell>
                      <Badge variant={CHECK_BADGE[check.state]}>
                        {t(`nodes.checkState.${check.state}`)}
                      </Badge>
                    </TableCell>
                    <TableCell className="whitespace-nowrap">
                      {check.since ? formatTime(new Date(check.since * 1000), i18n.language) : "—"}
                    </TableCell>
                    <TableCell className="text-xs">{check.error ?? ""}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
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

/** 검사 상태의 배지 모양 */
export const CHECK_BADGE = {
  ok: "outline",
  degraded: "secondary",
  failing: "destructive",
  unknown: "outline",
} as const;
