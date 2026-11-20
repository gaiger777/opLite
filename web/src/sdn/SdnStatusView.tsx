import { useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { StatusChip } from "@/components/StatusChip";
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
import { formatTime } from "@/lib/format";
import {
  callWarned,
  MAX_CENTRALS,
  nodeBlocker,
  SDN_KEY,
  type SdnView,
  sdnWarnings,
  useVoters,
} from "@/sdn/sdn";
import { ChassisChip, RoleChip } from "@/sdn/chips";
import { trackTask } from "@/tasks/tasks";

/** 설정된 SDN(OVN)의 상태: 경고 띠, 요약·동기화, 중앙 표, chassis 표. `admin`이면 고치는 단추를 보인다. */
export function SdnStatusView({ sdn, admin }: { sdn: SdnView; admin: boolean }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const voters = useVoters();
  const warnings = sdnWarnings(sdn, nodes.data, voters);
  const [error, setError] = useState<string | null>(null);
  const sync = sdn.sync;
  const initName = nodes.data?.find((n) => n.id === sdn.init_node)?.name ?? sdn.init_node;

  async function retryInit() {
    if (!sdn.init_node) {
      return;
    }
    setError(null);
    try {
      const task = await call(api.POST("/sdn", { body: { node: sdn.init_node } }));
      trackTask(task, t("tasks.op.sdn-init", { node: initName }));
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      await queryClient.invalidateQueries({ queryKey: SDN_KEY });
    }
  }

  return (
    <div className="flex flex-col gap-6">
      {warnings.length > 0 && (
        <div role="alert" className="rounded-md border border-warning p-3 text-sm">
          <ul className="flex flex-col gap-1">
            {warnings.map((w) => (
              <li key={w.key}>{t(w.key, w.values)}</li>
            ))}
          </ul>
        </div>
      )}
      <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-1 text-sm">
        <dt className="text-muted-foreground">{t("sdn.phase")}</dt>
        <dd>
          <StatusChip
            tone={sdn.phase === "ready" ? "success" : "info"}
            busy={sdn.phase !== "ready"}
          >
            {t(`sdn.phases.${sdn.phase ?? "bootstrapping"}`)}
          </StatusChip>
        </dd>
        {sdn.release && (
          <>
            <dt className="text-muted-foreground">{t("sdn.release")}</dt>
            <dd>{sdn.release}</dd>
          </>
        )}
        <dt className="text-muted-foreground">{t("sdn.sync.title")}</dt>
        <dd>
          {!sync ? (
            t("sdn.sync.never")
          ) : (
            <span className="flex flex-col">
              <span>
                {t("sdn.sync.at", { at: formatTime(new Date(sync.at * 1000), i18n.language) })}
              </span>
              <span className="text-muted-foreground">
                {sync.hv_cfg >= sync.nb_cfg
                  ? t("sdn.sync.caughtUp")
                  : t("sdn.sync.behind", { hv: sync.hv_cfg, nb: sync.nb_cfg })}
              </span>
              {sync.error && <span className="text-destructive">{sync.error}</span>}
            </span>
          )}
        </dd>
        {sdn.tasks.length > 0 && (
          <>
            <dt className="text-muted-foreground">{t("sdn.tasks")}</dt>
            <dd>{t("sdn.running", { count: sdn.tasks.length })}</dd>
          </>
        )}
      </dl>
      {sdn.last_error && (
        <p role="alert" className="text-sm text-destructive">
          {t("sdn.lastError", { error: sdn.last_error })}
        </p>
      )}
      {admin && sdn.phase !== "ready" && sdn.tasks.length === 0 && sdn.init_node && (
        <div className="flex flex-col gap-2">
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <div>
            <Button variant="outline" onClick={() => void retryInit()}>
              {t("sdn.retryOn", { node: initName })}
            </Button>
          </div>
        </div>
      )}
      <CentralsTable sdn={sdn} admin={admin} />
      <section aria-labelledby="sdn-chassis" className="flex flex-col gap-2">
        <h2 id="sdn-chassis" className="font-medium">
          {t("sdn.chassis.title")}
        </h2>
        <ChassisTable sdn={sdn} admin={admin} />
      </section>
    </div>
  );
}

function CentralsTable({ sdn, admin }: { sdn: SdnView; admin: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<SdnView["centrals"][number] | null>(null);
  const ready = sdn.phase === "ready";
  const busy = sdn.tasks.length > 0;
  const write = admin && ready;

  return (
    <section aria-labelledby="sdn-centrals" className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-2">
        <h2 id="sdn-centrals" className="font-medium">
          {t("sdn.centrals.title")}
        </h2>
        {write && (
          <Button
            variant="outline"
            size="sm"
            disabled={busy || sdn.centrals.length >= MAX_CENTRALS}
            onClick={() => setAdding(true)}
          >
            <Plus />
            {t("sdn.centrals.add")}
          </Button>
        )}
      </div>
      <p className="text-xs text-muted-foreground">{t("sdn.centrals.hint")}</p>
      <Table aria-labelledby="sdn-centrals">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.node")}</TableHead>
            <TableHead>{t("sdn.centrals.address")}</TableHead>
            <TableHead>{t("sdn.centrals.nb")}</TableHead>
            <TableHead>{t("sdn.centrals.sb")}</TableHead>
            <TableHead>{t("sdn.centrals.northd")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {sdn.centrals.length === 0 && (
            <TableRow>
              <TableCell colSpan={write ? 6 : 5} className="text-muted-foreground">
                {t("sdn.centrals.none")}
              </TableCell>
            </TableRow>
          )}
          {sdn.centrals.map((c) => (
            <TableRow key={c.node}>
              <TableCell className="font-medium">{c.name}</TableCell>
              <TableCell className="font-mono text-xs">{c.address}</TableCell>
              <TableCell>
                <RoleChip role={c.health?.nb} />
              </TableCell>
              <TableCell>
                <RoleChip role={c.health?.sb} />
              </TableCell>
              <TableCell>
                {c.health
                  ? t(c.health.northd_active ? "sdn.centrals.active" : "sdn.centrals.standby")
                  : "—"}
              </TableCell>
              {write && (
                <TableCell className="text-right">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("sdn.centrals.removeNamed", { name: c.name })}
                    // 마지막 중앙은 뺄 수 없다(서버 409 sdn_last_central).
                    disabled={busy || sdn.centrals.length <= 1}
                    onClick={() => setRemoving(c)}
                  >
                    <Trash2 />
                  </Button>
                </TableCell>
              )}
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {adding && <AddCentralDialog sdn={sdn} onClose={() => setAdding(false)} />}
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("sdn.centrals.removeTitle", { name: removing?.name })}
        description={t("sdn.centrals.removeDescription")}
        confirmLabel={t("sdn.centrals.remove")}
        onConfirm={async () => {
          const c = removing!;
          const { data, warning } = await callWarned(
            api.DELETE("/sdn/centrals/{node}", { params: { path: { node: c.node } } }),
          );
          trackTask(data, t("tasks.op.sdn-central-remove", { node: c.name }));
          if (warning) {
            toast.warning(warning);
          }
          await queryClient.invalidateQueries({ queryKey: SDN_KEY });
        }}
      />
    </section>
  );
}

/** 중앙 하나를 더한다(`sdn-central-add`). 이미 중앙인 노드와 막힌 노드는 고를 수 없다. */
function AddCentralDialog({ sdn, onClose }: { sdn: SdnView; onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const choices = [...(nodes.data ?? [])]
    .filter((n) => !n.spec.witness && !sdn.centrals.some((c) => c.node === n.id))
    .sort((a, b) => a.name.localeCompare(b.name));
  const [node, setNode] = useState("");
  const chosen = node || (choices.find((n) => nodeBlocker(n, true) === null)?.id ?? "");
  const count = sdn.centrals.length + 1;

  async function submit() {
    const target = choices.find((n) => n.id === chosen);
    if (!target) {
      return false;
    }
    const { data, warning } = await callWarned(
      api.POST("/sdn/centrals", { body: { node: target.id } }),
    );
    trackTask(data, t("tasks.op.sdn-central-add", { node: target.name }));
    if (warning) {
      toast.warning(warning);
    }
    await queryClient.invalidateQueries({ queryKey: SDN_KEY });
  }

  return (
    <FormDialog
      title={t("sdn.centrals.addTitle")}
      description={t("sdn.centrals.addDescription")}
      submitLabel={t("sdn.centrals.add")}
      submitDisabled={chosen === ""}
      onClose={onClose}
      onSubmit={submit}
    >
      {choices.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("sdn.noNodes")}</p>
      ) : (
        <Field id="sdn-central-node" label={t("common.node")}>
          <NativeSelect
            id="sdn-central-node"
            className="w-full"
            value={chosen}
            onChange={(e) => setNode(e.target.value)}
          >
            {choices.map((n) => {
              const blocked = nodeBlocker(n, true);
              return (
                <NativeSelectOption key={n.id} value={n.id} disabled={blocked !== null}>
                  {blocked ? `${n.name} (${t(blocked)})` : n.name}
                </NativeSelectOption>
              );
            })}
          </NativeSelect>
        </Field>
      )}
      {count % 2 === 0 && (
        <p className="text-sm text-warning-fg">{t("sdn.warn.even", { count })}</p>
      )}
    </FormDialog>
  );
}

/**
 * 위트니스가 아닌 노드마다의 chassis 상태. `admin`이면 노드마다 설치와 "모든 노드에"(`POST /sdn/chassis`, 오프라인으로
 * 건너뛴 노드는 응답 헤더 경고)를 보인다. 새로 합류한 노드에 저절로 깔지 않는다.
 */
export function ChassisTable({ sdn, admin }: { sdn: SdnView; admin: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const write = admin && sdn.phase === "ready";
  const node = (id: string) => nodes.data?.find((n) => n.id === id);
  const installable = (id: string) => {
    const n = node(id);
    return n !== undefined && nodeBlocker(n, false) === null;
  };

  async function install(body: { nodes: string[] } | { all: true }) {
    setBusy(true);
    setError(null);
    try {
      const { data, warning } = await callWarned(api.POST("/sdn/chassis", { body }));
      for (const task of data) {
        trackTask(
          task,
          t("tasks.op.sdn-chassis-install", { node: node(task.spec.node)?.name ?? task.spec.node }),
        );
      }
      if (warning) {
        toast.warning(warning);
      }
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
      await queryClient.invalidateQueries({ queryKey: SDN_KEY });
    }
  }

  return (
    <div className="flex flex-col gap-2">
      {write && (
        <div>
          <Button
            variant="outline"
            size="sm"
            disabled={busy || sdn.chassis.every((c) => c.sdn?.chassis === "ready")}
            onClick={() => void install({ all: true })}
          >
            {t("sdn.chassis.installAll")}
          </Button>
        </div>
      )}
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <Table aria-label={t("sdn.chassis.title")}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.node")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("sdn.chassis.encap")}</TableHead>
            <TableHead>{t("sdn.chassis.versions")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {sdn.chassis.map((c) => {
            const blocked = node(c.node) ? nodeBlocker(node(c.node)!, false) : null;
            return (
              <TableRow key={c.node}>
                <TableCell className="font-medium">{c.name}</TableCell>
                <TableCell>
                  <span className="flex flex-col gap-0.5">
                    <ChassisChip sdn={c.sdn} />
                    {c.sdn?.error && (
                      <span className="text-xs text-destructive">{c.sdn.error}</span>
                    )}
                    {blocked && <span className="text-xs text-muted-foreground">{t(blocked)}</span>}
                  </span>
                </TableCell>
                <TableCell className="font-mono text-xs">{c.sdn?.encap_ip ?? "—"}</TableCell>
                <TableCell className="text-xs">
                  {c.sdn?.ovn_version || c.sdn?.ovs_version
                    ? t("sdn.chassis.versionText", {
                        ovn: c.sdn?.ovn_version ?? "—",
                        ovs: c.sdn?.ovs_version ?? "—",
                      })
                    : "—"}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    {c.sdn?.chassis !== "ready" && (
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={busy || !installable(c.node)}
                        aria-label={t("sdn.chassis.installNamed", { name: c.name })}
                        onClick={() => void install({ nodes: [c.node] })}
                      >
                        {t("sdn.chassis.install")}
                      </Button>
                    )}
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </div>
  );
}
