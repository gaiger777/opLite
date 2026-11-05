import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { focusBack } from "@/components/focusBack";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
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
import { codeMessage, errorMessage } from "@/i18n/errors";
import { formatTime } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

/** 노드의 오류 문구(`node_offline`은 VM 문구가 아니라 노드 문구로) */
function nodeError(t: Parameters<typeof errorMessage>[0], err: unknown): string {
  return err instanceof ApiError
    ? codeMessage(t, err.code, err.message, "nodes.updates.errors")
    : errorMessage(t, err);
}

/**
 * 노드의 업데이트 확인 결과. admin은 다시 확인하고 업그레이드한다. 결과는 watch로 바뀐 Node를 따라간다.
 * 연 쪽은 열 때만 그린다.
 */
export function NodeUpdatesDialog({
  node: opened,
  onClose,
  returnFocus,
}: {
  node: Node;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const nodes = useList("node");
  const node = nodes.data?.find((n) => n.id === opened.id) ?? opened;
  const tasks = useTaskIndex().get(node.id) ?? [];
  const checking = tasks.some((task) => task.spec.op.kind === "node-update-check");
  // 업그레이드가 apt를 쓰는 동안은 다시 확인도 새 업그레이드도 하지 않는다(서버도 409).
  const upgradeRunning = tasks.some((task) => task.spec.op.kind === "node-upgrade");
  const write = can(me.role, "write", "node");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [upgrading, setUpgrading] = useState(false);
  const upgradeButton = useRef<HTMLButtonElement>(null);
  const updates = node.status.updates;

  async function refresh() {
    setBusy(true);
    setError(null);
    try {
      const task = await call(
        api.POST("/nodes/{key}/updates/refresh", { params: { path: { key: node.id } } }),
      );
      trackTask(task, t("tasks.op.node-update-check", { target: node.name }));
    } catch (err) {
      setError(nodeError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
        <DialogContent
          aria-describedby={undefined}
          className="sm:max-w-2xl"
          onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
        >
          <DialogHeader>
            <DialogTitle>{t("nodes.updates.title", { name: node.name })}</DialogTitle>
          </DialogHeader>
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <p className="text-sm text-muted-foreground">
            {updates?.checked_at
              ? t("nodes.updates.checked", {
                  time: formatTime(new Date(updates.checked_at * 1000), i18n.language),
                })
              : t("nodes.updates.notChecked")}
          </p>
          {updates?.error ? (
            <div className="flex flex-col gap-1 text-sm">
              <p className="text-destructive">{t("nodes.updates.failed")}</p>
              <pre className="rounded-md bg-muted p-2 text-xs whitespace-pre-wrap">
                {updates.error}
              </pre>
            </div>
          ) : !updates?.checked_at ? null : updates.packages.length === 0 ? (
            <p className="text-sm">{t("nodes.updates.none")}</p>
          ) : (
            <div className="max-h-96 overflow-y-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("nodes.updates.package")}</TableHead>
                    <TableHead>{t("nodes.updates.current")}</TableHead>
                    <TableHead>{t("nodes.updates.candidate")}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {updates.packages.map((p) => (
                    <TableRow key={p.name}>
                      <TableCell className="font-medium">{p.name}</TableCell>
                      {/* 새 의존성은 설치된 버전이 없다. */}
                      <TableCell className="font-mono text-xs">
                        {p.current ?? t("nodes.updates.newPackage")}
                      </TableCell>
                      <TableCell className="font-mono text-xs">{p.candidate}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
          {updates?.truncated && (
            <p className="text-sm text-muted-foreground">
              {t("nodes.updates.truncated", { count: updates.packages.length })}
            </p>
          )}
          <DialogFooter>
            {write && (
              <>
                <Button
                  variant="outline"
                  disabled={busy || checking || upgradeRunning}
                  onClick={() => void refresh()}
                >
                  {t(checking ? "nodes.updates.checking" : "nodes.updates.refresh")}
                </Button>
                {/* 확인이 apt를 쓰는 동안도 업그레이드하지 않는다. */}
                <Button
                  ref={upgradeButton}
                  disabled={busy || checking || upgradeRunning}
                  onClick={() => setUpgrading(true)}
                >
                  {t(upgradeRunning ? "nodes.updates.upgrading" : "nodes.updates.upgrade")}
                </Button>
              </>
            )}
            <Button variant="outline" disabled={busy} onClick={onClose}>
              {t("common.done")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <ConfirmDialog
        open={upgrading}
        onOpenChange={setUpgrading}
        title={t("nodes.updates.upgradeTitle", { name: node.name })}
        description={t("nodes.updates.upgradeDescription")}
        confirmLabel={t("nodes.updates.upgrade")}
        returnFocus={upgradeButton}
        onConfirm={async () => {
          try {
            const task = await call(
              api.POST("/nodes/{key}/upgrade", { params: { path: { key: node.id } } }),
            );
            trackTask(task, t("tasks.op.node-upgrade", { target: node.name }));
          } catch (err) {
            throw new Error(nodeError(t, err), { cause: err });
          }
        }}
      />
    </>
  );
}
