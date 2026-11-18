import { useQueryClient } from "@tanstack/react-query";
import { Fragment, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { CephStatusView as Status, CephView } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { AddMonDialog } from "@/ceph/AddMonDialog";
import { AddOsdDialog } from "@/ceph/AddOsdDialog";
import {
  CEPH_KEY,
  healthTone,
  osdHosts,
  percentOf,
  pgSummary,
  useCephStatus,
  type HealthTone,
} from "@/ceph/ceph";
import { CreateCephPoolDialog } from "@/ceph/CreateCephPoolDialog";
import { InstallClientsDialog } from "@/ceph/InstallClientsDialog";
import { ListPending } from "@/components/ListPending";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatBytes, formatTime } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

const TONE_BADGE = {
  ok: "default",
  warn: "secondary",
  err: "destructive",
  unknown: "outline",
} as const satisfies Record<HealthTone, string>;

type Dialog = "mon" | "mgr" | "osd" | "pool" | "clients";

/** 관리 Ceph의 상태: 설정, health, mon·mgr, 호스트별 OSD, PG, 사용량, 풀, admin 동작 */
export function CephStatusView({ ceph }: { ceph: CephView }) {
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const status = useCephStatus(ceph.mons.length > 0);
  const [open, setOpen] = useState<Dialog | null>(null);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const admin = perms.can("write", "ceph");
  const ready = ceph.phase === "ready";
  const nodeName = (id: string) => nodes.data?.find((n) => n.id === id)?.name ?? id;
  const report = status.data;
  const tasks = useList("task");
  // mon이 서기 전에 부트스트랩 노드가 꺼졌거나 빠졌으면 다른 노드로 옮겨 다시 할 수 있다(그 노드의 Task는 버려진다).
  const initNode = nodes.data?.find((n) => n.id === ceph.init_node);
  const initOffline =
    !ready &&
    ceph.mons.length === 0 &&
    nodes.data !== undefined &&
    (initNode === undefined || !initNode.status.online);
  const busy = ceph.tasks.some(
    (id) => !(initOffline && tasks.data?.find((x) => x.id === id)?.spec.node === ceph.init_node),
  );
  const movable = (nodes.data ?? []).filter((n) => !n.spec.witness && n.status.online);
  const [moveTo, setMoveTo] = useState("");
  const retryNode = initOffline
    ? (movable.find((n) => n.id === moveTo) ?? movable[0])?.id
    : ceph.init_node;

  async function retry() {
    const node = retryNode;
    if (!node) {
      return;
    }
    try {
      const task = await call(api.POST("/ceph", { body: { node } }));
      trackTask(task, t("tasks.op.ceph-init", { node: nodeName(node) }));
    } catch (err) {
      toast.error(errorMessage(t, err));
    } finally {
      await queryClient.invalidateQueries({ queryKey: CEPH_KEY });
    }
  }

  const action = (dialog: Dialog, label: string, disabled = !ready) => (
    <Button
      variant="outline"
      disabled={disabled}
      onClick={(e) => {
        trigger.current = e.currentTarget;
        setOpen(dialog);
      }}
    >
      {label}
    </Button>
  );

  return (
    <div className="flex flex-col gap-6">
      <dl className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
        <Item label={t("ceph.phase")}>{t(`ceph.phases.${ceph.phase}`)}</Item>
        <Item label={t("ceph.release")}>{ceph.release ?? "—"}</Item>
        <Item label={t("ceph.fsid")}>
          <span className="font-mono text-xs">{ceph.fsid}</span>
        </Item>
        <Item label={t("ceph.publicNetwork")}>{ceph.public_network}</Item>
        <Item label={t("ceph.clusterNetwork")}>{ceph.cluster_network ?? "—"}</Item>
        <Item label={t("ceph.managedConf")}>
          <span className="font-mono text-xs">{ceph.managed_conf}</span>
        </Item>
      </dl>
      {!ready && (
        <div role="alert" className="flex flex-col gap-2 rounded-md border p-3 text-sm">
          <p>
            {busy
              ? t("ceph.settingUp", { node: nodeName(ceph.init_node ?? "") })
              : t("ceph.notReady")}
          </p>
          {ceph.last_error && <p className="text-destructive">{ceph.last_error}</p>}
          {admin && !busy && ceph.init_node && (
            <div className="flex flex-wrap items-center gap-2">
              {initOffline && (
                <>
                  <label htmlFor="ceph-retry-node">
                    {t("ceph.retryElsewhere", { node: nodeName(ceph.init_node) })}
                  </label>
                  <NativeSelect
                    id="ceph-retry-node"
                    value={retryNode ?? ""}
                    onChange={(e) => setMoveTo(e.target.value)}
                  >
                    {movable.map((n) => (
                      <NativeSelectOption key={n.id} value={n.id}>
                        {n.name}
                      </NativeSelectOption>
                    ))}
                  </NativeSelect>
                </>
              )}
              <Button variant="outline" disabled={!retryNode} onClick={() => void retry()}>
                {t("ceph.retry")}
              </Button>
            </div>
          )}
        </div>
      )}
      {admin && (
        <div className="flex flex-wrap gap-2">
          {action("mon", t("ceph.mon.add"))}
          {action("mgr", t("ceph.mgr.add"))}
          {action("osd", t("ceph.osd.add"))}
          {action("pool", t("ceph.pool.create"))}
          {action("clients", t("ceph.clients.install"), false)}
        </div>
      )}
      {report?.stale && (
        <p role="alert" className="rounded-md border border-warning p-3 text-sm">
          {t("ceph.stale", {
            time: formatTime(new Date(report.read_at * 1000), i18n.language),
            error: report.error ?? "",
          })}
        </p>
      )}
      {!report ? (
        ceph.mons.length > 0 && <ListPending error={status.error} />
      ) : (
        <Report report={report} />
      )}
      {open === "mon" || open === "mgr" ? (
        <AddMonDialog ceph={ceph} role={open} returnFocus={trigger} onClose={() => setOpen(null)} />
      ) : open === "osd" ? (
        <AddOsdDialog returnFocus={trigger} onClose={() => setOpen(null)} />
      ) : open === "pool" ? (
        <CreateCephPoolDialog
          hosts={report && !report.stale ? report.osd_hosts_up : undefined}
          returnFocus={trigger}
          onClose={() => setOpen(null)}
        />
      ) : open === "clients" ? (
        <InstallClientsDialog ceph={ceph} returnFocus={trigger} onClose={() => setOpen(null)} />
      ) : null}
    </div>
  );
}

function Item({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-sm">{children}</dd>
    </div>
  );
}

/** 사용률 막대 */
function Meter({ label, percent }: { label: string; percent: number }) {
  return (
    <div
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={percent}
      className="h-2 w-32 overflow-hidden rounded-full bg-muted"
    >
      <div
        className={percent > 85 ? "h-full bg-destructive" : "h-full bg-primary"}
        style={{ width: `${Math.min(100, percent)}%` }}
      />
    </div>
  );
}

function Report({ report }: { report: Status }) {
  const { t } = useTranslation();
  const tone = healthTone(report.health.status);
  const hosts = osdHosts(report);
  const pgs = pgSummary(report.pgs);
  const usage = report.usage;
  const up = report.osds.filter((o) => o.up).length;
  const inside = report.osds.filter((o) => o.in).length;
  return (
    <>
      <section className="flex flex-col gap-2">
        <h2 className="flex items-center gap-2 font-medium">
          {t("ceph.health")}
          <Badge variant={TONE_BADGE[tone]}>{report.health.status || t("ceph.unknown")}</Badge>
        </h2>
        {report.health.checks.length > 0 && (
          <ul aria-label={t("ceph.checks")} className="flex flex-col gap-1 text-sm">
            {report.health.checks.map((c) => (
              <li key={c.code} className={c.muted ? "text-muted-foreground" : undefined}>
                <Badge variant={c.severity === "HEALTH_ERR" ? "destructive" : "secondary"}>
                  {c.severity === "HEALTH_ERR" ? "ERR" : "WRN"}
                </Badge>{" "}
                <span className="font-mono text-xs">{c.code}</span> {c.summary}
                {c.muted && ` (${t("ceph.muted")})`}
              </li>
            ))}
          </ul>
        )}
      </section>
      <section className="grid grid-cols-1 gap-6 lg:grid-cols-2">
        <div className="flex flex-col gap-2">
          <h2 id="ceph-mons" className="font-medium">
            {t("ceph.monitors", { n: report.mons.length, quorum: report.quorum.length })}
          </h2>
          <Table aria-labelledby="ceph-mons">
            <TableHeader>
              <TableRow>
                <TableHead>{t("common.name")}</TableHead>
                <TableHead>{t("ceph.address")}</TableHead>
                <TableHead>{t("ceph.quorum")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {report.mons.map((m) => (
                <TableRow key={m.name}>
                  <TableCell className="font-medium">{m.name}</TableCell>
                  <TableCell className="font-mono text-xs">{m.addr}</TableCell>
                  <TableCell>
                    <Badge variant={m.in_quorum ? "outline" : "destructive"}>
                      {t(m.in_quorum ? "ceph.inQuorum" : "ceph.outOfQuorum")}
                    </Badge>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
        <div className="flex flex-col gap-2">
          <h2 className="font-medium">{t("ceph.managers")}</h2>
          <p className="text-sm">
            {report.mgr.active
              ? t("ceph.mgrActive", { name: report.mgr.active })
              : t("ceph.mgrNone")}
            {report.mgr.standbys.length > 0 &&
              ` · ${t("ceph.mgrStandbys", { names: report.mgr.standbys.join(", ") })}`}
          </p>
          <h2 className="font-medium">{t("ceph.usage")}</h2>
          <div className="flex items-center gap-2 text-sm">
            <Meter
              label={t("ceph.usage")}
              percent={percentOf(usage.used_bytes, usage.total_bytes)}
            />
            {t("ceph.usageText", {
              used: formatBytes(usage.used_bytes),
              available: formatBytes(usage.available_bytes),
              total: formatBytes(usage.total_bytes),
            })}
          </div>
          <h2 className="font-medium">{t("ceph.pgs", { n: report.pgs.total })}</h2>
          {pgs.length === 0 ? (
            <p className="text-sm text-muted-foreground">—</p>
          ) : (
            <ul aria-label={t("ceph.pgStates")} className="flex flex-col gap-1 text-sm">
              {pgs.map((p) => (
                <li key={p.state} className="flex items-center gap-2">
                  <Meter label={p.state} percent={Math.round(p.percent * 10) / 10} />
                  <span className={p.clean ? undefined : "text-warning-fg"}>
                    {`${p.count} ${p.state}`}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      </section>
      <section className="flex flex-col gap-2">
        <h2 id="ceph-osds" className="font-medium">
          {t("ceph.osds", { up, in: inside, total: report.osds.length, hosts: hosts.length })}
        </h2>
        <Table aria-labelledby="ceph-osds">
          <TableHeader>
            <TableRow>
              <TableHead>{t("ceph.osdId")}</TableHead>
              <TableHead>{t("ceph.disk.device")}</TableHead>
              <TableHead>{t("ceph.class")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
              <TableHead>{t("ceph.used")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {hosts.length === 0 && (
              <TableRow>
                <TableCell colSpan={5} className="text-center text-muted-foreground">
                  {t("ceph.noOsds")}
                </TableCell>
              </TableRow>
            )}
            {hosts.map((h) => (
              <Fragment key={h.host}>
                <TableRow>
                  <TableCell colSpan={5} className="bg-muted/50 font-medium">
                    {t("ceph.host", { host: h.host, up: h.up, total: h.osds.length })}
                  </TableCell>
                </TableRow>
                {h.osds.map((o) => (
                  <TableRow key={o.id}>
                    <TableCell className="font-mono text-xs">{`osd.${o.id}`}</TableCell>
                    <TableCell className="font-mono text-xs">{o.device ?? "—"}</TableCell>
                    <TableCell>{o.class ?? "—"}</TableCell>
                    <TableCell>
                      <div className="flex gap-1">
                        <Badge variant={o.up ? "outline" : "destructive"}>
                          {t(o.up ? "ceph.up" : "ceph.down")}
                        </Badge>
                        <Badge variant={o.in ? "outline" : "secondary"}>
                          {t(o.in ? "ceph.in" : "ceph.out")}
                        </Badge>
                      </div>
                    </TableCell>
                    <TableCell>
                      {o.total_bytes > 0
                        ? `${formatBytes(o.used_bytes)} / ${formatBytes(o.total_bytes)}`
                        : "—"}
                    </TableCell>
                  </TableRow>
                ))}
              </Fragment>
            ))}
          </TableBody>
        </Table>
      </section>
      <section className="flex flex-col gap-2">
        <h2 id="ceph-pools" className="font-medium">
          {t("ceph.pools")}
        </h2>
        <Table aria-labelledby="ceph-pools">
          <TableHeader>
            <TableRow>
              <TableHead>{t("ceph.pool.cephPool")}</TableHead>
              <TableHead>{t("ceph.pool.storagePool")}</TableHead>
              <TableHead>{t("ceph.pool.size")}</TableHead>
              <TableHead>{t("ceph.pool.stored")}</TableHead>
              <TableHead>{t("ceph.pool.available")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {report.pools.map((p) => (
              <TableRow key={p.id}>
                <TableCell className="font-mono text-xs">{p.name}</TableCell>
                <TableCell>
                  {p.orbivisor_pool ? (
                    <Link to="/storage-pools" className="underline-offset-4 hover:underline">
                      {p.orbivisor_pool}
                    </Link>
                  ) : (
                    "—"
                  )}
                </TableCell>
                <TableCell>{p.size ?? "—"}</TableCell>
                <TableCell>
                  {`${formatBytes(p.stored_bytes)} (${(p.percent_used * 100).toFixed(1)}%)`}
                </TableCell>
                <TableCell>{formatBytes(p.max_available_bytes)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </section>
    </>
  );
}
