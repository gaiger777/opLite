import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Task } from "@/api/types";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { SDN_KEY, type SdnView } from "@/sdn/sdn";
import { trackTask } from "@/tasks/tasks";

type LockdownCheck = components["schemas"]["LockdownCheck"];

/**
 * OVN 접근 잠금 카드(계획 67 §10.5): 켜짐, 중앙마다 적용, 노드마다 허용 목록 표, 허용 집합. admin은 미리 확인(`?dry_run`
 * — 집합 밖에서 지금 중앙에 붙어 있는 상대)을 보고 켜거나 끈다. 집합 밖 상대가 있으면 "그래도 켬"(`force`)을 골라야 한다.
 */
export function LockdownCard({ sdn, admin }: { sdn: SdnView; admin: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const lockdown = sdn.lockdown;
  const enabled = lockdown?.enabled ?? false;
  const [check, setCheck] = useState<LockdownCheck | null>(null);
  const [force, setForce] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [turningOff, setTurningOff] = useState(false);
  const centrals = sdn.centrals.length;
  const applied = lockdown?.applied.length ?? 0;
  const firewalls = lockdown?.nodes.filter((n) => n.firewall).length ?? 0;
  const nodes = lockdown?.nodes.length ?? 0;

  async function dryRun() {
    setBusy(true);
    setFailure(null);
    try {
      const result = await call(
        api.POST("/sdn/lockdown", {
          params: { query: { dry_run: true } },
          body: { enabled: true },
        }),
      );
      setCheck(result as LockdownCheck);
      setForce(false);
    } catch (err) {
      setFailure(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  async function set(on: boolean) {
    setFailure(null);
    const task = await call(
      api.POST("/sdn/lockdown", { body: { enabled: on, ...(on && force && { force: true }) } }),
    );
    trackTask(task as Task, t(on ? "sdn.lockdown.turningOn" : "sdn.lockdown.turningOff"));
    setCheck(null);
    await queryClient.invalidateQueries({ queryKey: SDN_KEY });
  }

  return (
    <section aria-labelledby="sdn-lockdown" className="flex flex-col gap-3 rounded-lg border p-4">
      <div className="flex flex-wrap items-center gap-3">
        <h2 id="sdn-lockdown" className="font-medium">
          {t("sdn.lockdown.title")}
        </h2>
        <StatusChip tone={enabled ? "success" : "neutral"} busy={lockdown?.pending}>
          {t(
            lockdown?.pending
              ? enabled
                ? "sdn.lockdown.turningOnShort"
                : "sdn.lockdown.turningOffShort"
              : enabled
                ? "sdn.lockdown.on"
                : "sdn.lockdown.off",
          )}
        </StatusChip>
      </div>
      <p className="text-sm text-muted-foreground">{t("sdn.lockdown.hint")}</p>
      {lockdown && (
        <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-1 text-sm">
          <dt className="text-muted-foreground">{t("sdn.lockdown.centrals")}</dt>
          <dd>{t("sdn.lockdown.applied", { applied, total: centrals })}</dd>
          <dt className="text-muted-foreground">{t("sdn.lockdown.firewall")}</dt>
          <dd>{t("sdn.lockdown.firewallOn", { on: firewalls, total: nodes })}</dd>
        </dl>
      )}
      {lockdown?.error && (
        <p role="alert" className="text-sm text-destructive">
          {lockdown.error}
        </p>
      )}
      {lockdown && lockdown.nodes.length > 0 && (
        <Table aria-labelledby="sdn-lockdown">
          <TableHeader>
            <TableRow>
              <TableHead>{t("sdn.lockdown.node")}</TableHead>
              <TableHead>{t("sdn.lockdown.central")}</TableHead>
              <TableHead>{t("sdn.lockdown.firewall")}</TableHead>
              <TableHead>{t("sdn.lockdown.allowed")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {lockdown.nodes.map((n) => (
              <TableRow key={n.node}>
                <TableCell>{n.name}</TableCell>
                <TableCell>
                  {sdn.centrals.some((c) => c.node === n.node)
                    ? t(
                        lockdown.applied.includes(n.node)
                          ? "sdn.lockdown.locked"
                          : "sdn.lockdown.notLocked",
                      )
                    : "—"}
                </TableCell>
                <TableCell>
                  <span className="flex flex-col items-start gap-0.5">
                    <StatusChip tone={n.firewall ? "success" : enabled ? "warning" : "neutral"}>
                      {t(n.firewall ? "sdn.lockdown.fwOn" : "sdn.lockdown.fwOff")}
                    </StatusChip>
                    {n.error && <span className="text-xs text-destructive">{n.error}</span>}
                  </span>
                </TableCell>
                <TableCell>{n.allowed}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {lockdown && (
        <details className="text-sm">
          <summary className="cursor-pointer">{t("sdn.lockdown.sets")}</summary>
          <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-6 gap-y-1">
            <dt className="text-muted-foreground">{t("sdn.lockdown.members")}</dt>
            <dd className="font-mono text-xs">{lockdown.members.join(", ") || "—"}</dd>
            <dt className="text-muted-foreground">{t("sdn.lockdown.centralSet")}</dt>
            <dd className="font-mono text-xs">{lockdown.centrals.join(", ") || "—"}</dd>
          </dl>
        </details>
      )}
      {check && (
        <div role="status" className="flex flex-col gap-2 rounded-md bg-surface-1 p-3 text-sm">
          <p className="font-medium">{t("sdn.lockdown.checkTitle")}</p>
          <p>
            {t("sdn.lockdown.checkSets", {
              members: check.members.length,
              centrals: check.centrals.length,
            })}
          </p>
          {check.outside.length === 0 ? (
            <p className="text-success-fg">{t("sdn.lockdown.noOutside")}</p>
          ) : (
            <>
              <p className="text-warning-fg">
                {t("sdn.lockdown.outside", { count: check.outside.length })}
              </p>
              <ul className="ml-5 list-disc font-mono text-xs">
                {check.outside.map((o) => (
                  <li key={`${o.central}-${o.peer}`}>
                    {t("sdn.lockdown.peer", { peer: o.peer, central: o.central })}
                  </li>
                ))}
              </ul>
              <div className="flex items-center gap-2">
                <Checkbox
                  id="lockdown-force"
                  checked={force}
                  onCheckedChange={(on) => setForce(on === true)}
                />
                <Label htmlFor="lockdown-force">{t("sdn.lockdown.force")}</Label>
              </div>
            </>
          )}
        </div>
      )}
      {failure && (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      {admin && sdn.phase === "ready" && (
        <div className="flex flex-wrap gap-2">
          {!enabled && (
            <Button variant="outline" disabled={busy} onClick={() => void dryRun()}>
              {t("sdn.lockdown.check")}
            </Button>
          )}
          {!enabled && check && (
            <Button
              disabled={busy || (check.outside.length > 0 && !force)}
              onClick={() => {
                setBusy(true);
                set(true)
                  .catch((err: unknown) => setFailure(errorMessage(t, err)))
                  .finally(() => setBusy(false));
              }}
            >
              {t("sdn.lockdown.turnOn")}
            </Button>
          )}
          {enabled && (
            <Button variant="outline" disabled={busy} onClick={() => setTurningOff(true)}>
              {t("sdn.lockdown.turnOff")}
            </Button>
          )}
        </div>
      )}
      <ConfirmDialog
        open={turningOff}
        onOpenChange={setTurningOff}
        title={t("sdn.lockdown.offTitle")}
        description={t("sdn.lockdown.offDescription")}
        confirmLabel={t("sdn.lockdown.turnOff")}
        onConfirm={() => set(false)}
      />
    </section>
  );
}
