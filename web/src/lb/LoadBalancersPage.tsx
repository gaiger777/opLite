import { useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, Scale, Trash2, X } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { RowConfigMenu } from "@/declarative/ConfigMenu";
import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { objectTarget } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { EmptyState } from "@/components/EmptyState";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { StatusChip, type StatusTone } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { FolderSelect, useNewFolder } from "@/folders/FolderSelect";
import { useFolderPaths } from "@/folders/useFolderPaths";
import { useOpenParam } from "@/lib/createParam";
import {
  checkListeners,
  healthCounts,
  type ListenerRow,
  listenerName,
  listenerRows,
  type LoadBalancer,
  type LbState,
  type MemberChoice,
  memberChoices,
  memberNetworks,
  type MemberRow,
  MAX_LISTENERS,
  newListener,
  refreshLbs,
  useLoadBalancers,
  vipNetworks,
} from "@/lb/lb";
import { PermissionsButton, PermissionsSheet } from "@/permissions/PermissionsSheet";
import { routedOf, serverText, useRouted, useRouters } from "@/routing/routing";
import { ipFamily } from "@/sdn/ips";
import { callWarned, sdnReady, useSdn } from "@/sdn/sdn";
import { validName } from "@/vms/form";

const HEALTH_TONE: Record<string, StatusTone> = {
  online: "success",
  offline: "danger",
  error: "danger",
  unchecked: "neutral",
  missing: "warning",
};

/** VIP·바깥 VIP의 상태 칩 */
function StateChip({ state }: { state: LbState }) {
  const { t } = useTranslation();
  return state.state === "ready" ? (
    <StatusChip tone="success">{t("lb.state.ready")}</StatusChip>
  ) : (
    <span className="flex flex-col items-start gap-0.5">
      <StatusChip tone="warning">{t("lb.state.unrouted")}</StatusChip>
      <span className="text-xs text-muted-foreground">{serverText(t, state.reason)}</span>
    </span>
  );
}

/** 멤버 건강 막대(살아 있음·죽음·검사 없음·닿지 않음의 몫) */
export function HealthBar({ lb }: { lb: LoadBalancer }) {
  const { t } = useTranslation();
  const c = healthCounts(lb.members);
  const total = lb.members.length;
  if (total === 0) {
    return <span className="text-muted-foreground">{t("lb.noMembers")}</span>;
  }
  const parts: [number, string][] = [
    [c.online, "bg-success"],
    [c.offline, "bg-danger"],
    [c.missing, "bg-warning"],
    [c.unchecked, "bg-muted-foreground/40"],
  ];
  return (
    <span className="flex flex-col gap-1">
      <span
        role="img"
        aria-label={t("lb.healthSummary", { ...c, total })}
        className="flex h-1.5 w-28 overflow-hidden rounded-full bg-muted"
      >
        {parts.map(([n, cls], i) =>
          n > 0 ? (
            <span key={i} className={cls} style={{ width: `${(n / total) * 100}%` }} />
          ) : null,
        )}
      </span>
      <span className="text-xs text-muted-foreground">
        {t("lb.healthSummary", { ...c, total })}
      </span>
    </span>
  );
}

/**
 * 인프라 › 네트워크 › 부하 분산(계획 67 §7): 목록(이름·VIP·바깥 VIP·리스너·멤버 건강 막대)과 서랍(리스너 표·멤버
 * 건강·보안 그룹 경고·바깥 VIP), 만들기·고치기 대화상자. SDN을 세우기 전에는 빈 상태와 안내만 보인다.
 */
export function LoadBalancersPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const lbs = useLoadBalancers();
  const folderPath = useFolderPaths();
  const readsSdn = perms.reads("sdn");
  const sdn = useSdn(false, readsSdn);
  const blocked = sdn.isSuccess && !sdnReady(sdn.data);
  const queryClient = useQueryClient();
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState<LoadBalancer | null>(null);
  const [opened, setOpened] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<LoadBalancer | null>(null);
  const [sharing, setSharing] = useState<LoadBalancer | null>(null);
  useOpenParam(setOpened);
  const write = perms.anywhere("network.config");
  const writes = (l: LoadBalancer) => perms.has("network.config", objectTarget("load-balancer", l));
  const delegates = (l: LoadBalancer) =>
    perms.has("permission.modify", objectTarget("load-balancer", l));
  const items = lbs.data ?? [];
  const openedLb = items.find((l) => l.id === opened);

  const createButton = write && (
    <Button disabled={blocked} onClick={() => setCreating(true)}>
      <Plus />
      {t("lb.create")}
    </Button>
  );

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="lbs-title"
        title={t("nav.loadBalancers")}
        description={t("lb.pageHint")}
        actions={items.length > 0 && createButton}
      />
      {blocked && (
        <p role="status" className="text-sm text-warning-fg">
          {t("lb.sdnNotReady")}
        </p>
      )}
      {lbs.data === undefined ? (
        <ListPending error={lbs.error} />
      ) : items.length === 0 ? (
        <EmptyState
          icon={Scale}
          title={t("lb.empty.title")}
          description={t("lb.empty.description")}
          action={createButton}
        />
      ) : (
        <Table aria-labelledby="lbs-title">
          <TableHeader>
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("lb.vip")}</TableHead>
              <TableHead>{t("lb.externalVip")}</TableHead>
              <TableHead>{t("lb.listeners")}</TableHead>
              <TableHead>{t("lb.memberHealth")}</TableHead>
              <TableHead>{t("folders.folder")}</TableHead>
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {items.map((l) => (
              <TableRow key={l.id}>
                <TableCell className="font-medium">
                  <button
                    type="button"
                    className="underline-offset-4 hover:underline"
                    aria-label={t("lb.open", { name: l.name })}
                    onClick={() => setOpened(l.id)}
                  >
                    {l.name}
                  </button>
                </TableCell>
                <TableCell>
                  <span className="flex flex-col">
                    <span className="font-mono">{l.spec.vip ?? "—"}</span>
                    <span className="text-xs text-muted-foreground">{l.spec.network}</span>
                  </span>
                </TableCell>
                <TableCell>
                  {l.spec.external ? (
                    <span className="flex flex-col">
                      <span className="font-mono">{l.spec.external.ip ?? "—"}</span>
                      <span className="text-xs text-muted-foreground">
                        {l.spec.external.network}
                      </span>
                    </span>
                  ) : (
                    "—"
                  )}
                </TableCell>
                <TableCell className="font-mono text-xs">
                  {l.spec.listeners.map(listenerName).join(", ")}
                </TableCell>
                <TableCell>
                  <HealthBar lb={l} />
                </TableCell>
                <TableCell className="text-sm">
                  {l.spec.folder ? folderPath(l.spec.folder) : "/"}
                </TableCell>
                <TableCell className="text-right whitespace-nowrap">
                  {writes(l) && (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("lb.editNamed", { name: l.name })}
                      onClick={() => setEditing(l)}
                    >
                      <Pencil />
                    </Button>
                  )}
                  {delegates(l) && (
                    <PermissionsButton name={l.name} onClick={() => setSharing(l)} />
                  )}
                  {writes(l) && (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: l.name })}
                      onClick={() => setDeleting(l)}
                    >
                      <Trash2 />
                    </Button>
                  )}
                  <RowConfigMenu target={{ kind: "load-balancer", id: l.id, name: l.name }} />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {creating && <LbDialog lb={null} onClose={() => setCreating(false)} />}
      {editing && <LbDialog lb={editing} onClose={() => setEditing(null)} />}
      {openedLb && (
        <LbSheet
          lb={openedLb}
          writes={writes(openedLb)}
          onEdit={() => setEditing(openedLb)}
          onClose={() => setOpened(null)}
        />
      )}
      {sharing && (
        <PermissionsSheet
          target={objectTarget("load-balancer", sharing)}
          label={sharing.name}
          onClose={() => setSharing(null)}
        />
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("lb.deleteTitle", { name: deleting?.name })}
        description={t("lb.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const lb = deleting!;
          await call(
            api.DELETE("/load-balancers/{key}", {
              params: { path: { key: lb.id }, header: { "If-Match": lb.revision } },
            }),
          );
          await refreshLbs(queryClient);
          setOpened((id) => (id === lb.id ? null : id));
          toast.success(t("lb.deleted", { name: lb.name }));
        }}
      />
    </div>
  );
}

/** 서랍: 상태, 리스너마다 건강 검사와 멤버 표(건강·경고) */
function LbSheet({
  lb,
  writes,
  onEdit,
  onClose,
}: {
  lb: LoadBalancer;
  writes: boolean;
  onEdit(): void;
  onClose(): void;
}) {
  const { t } = useTranslation();
  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-3xl" aria-describedby={undefined}>
        <SheetTitle className="px-4 pt-4 text-lg">
          {t("lb.sheetTitle", { name: lb.name })}
        </SheetTitle>
        <div className="flex flex-col gap-6 px-4 pb-4">
          {lb.spec.description && (
            <p className="text-sm text-muted-foreground">{lb.spec.description}</p>
          )}
          {lb.live_error && (
            <p role="status" className="text-sm text-warning-fg">
              {t("lb.liveError", { error: lb.live_error })}
            </p>
          )}
          <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
            <dt className="text-muted-foreground">{t("lb.vip")}</dt>
            <dd className="flex flex-wrap items-center gap-2">
              <span className="font-mono">{lb.spec.vip ?? "—"}</span>
              <span className="text-muted-foreground">{lb.spec.network}</span>
              <StateChip state={lb.vip_state} />
            </dd>
            <dt className="text-muted-foreground">{t("lb.externalVip")}</dt>
            <dd className="flex flex-wrap items-center gap-2">
              {lb.spec.external ? (
                <>
                  <span className="font-mono">{lb.spec.external.ip ?? "—"}</span>
                  <span className="text-muted-foreground">{lb.spec.external.network}</span>
                  {lb.external_state && <StateChip state={lb.external_state} />}
                </>
              ) : (
                <span className="text-muted-foreground">{t("lb.noExternal")}</span>
              )}
            </dd>
            <dt className="text-muted-foreground">{t("lb.affinity")}</dt>
            <dd>
              {lb.spec.affinity_secs
                ? t("lb.affinitySecs", { secs: lb.spec.affinity_secs })
                : t("lb.affinityOff")}
            </dd>
            {(lb.spec.monitors ?? []).length > 0 && (
              <>
                <dt className="text-muted-foreground">{t("lb.monitors")}</dt>
                <dd className="font-mono text-xs">
                  {(lb.spec.monitors ?? []).map((m) => `${m.ip} (${m.network})`).join(", ")}
                </dd>
              </>
            )}
          </dl>
          {writes && (
            <div>
              <Button variant="outline" size="sm" onClick={onEdit}>
                <Pencil />
                {t("lb.edit")}
              </Button>
            </div>
          )}
          {lb.spec.listeners.map((l) => {
            const name = listenerName(l);
            const members = lb.members.filter((m) => m.listener === name);
            return (
              <section key={name} aria-labelledby={`lb-l-${name}`} className="flex flex-col gap-2">
                <h2 id={`lb-l-${name}`} className="font-medium">
                  {t("lb.listenerTitle", { listener: name })}
                </h2>
                <p className="text-xs text-muted-foreground">
                  {l.health
                    ? t("lb.healthCheck", {
                        interval: l.health.interval_secs,
                        timeout: l.health.timeout_secs,
                        rise: l.health.rise,
                        fall: l.health.fall,
                      })
                    : t("lb.noHealthCheck")}
                </p>
                <Table aria-labelledby={`lb-l-${name}`}>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t("lb.member")}</TableHead>
                      <TableHead>{t("lb.address")}</TableHead>
                      <TableHead>{t("lb.port")}</TableHead>
                      <TableHead>{t("lb.health")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {members.length === 0 && (
                      <TableRow>
                        <TableCell colSpan={4} className="text-muted-foreground">
                          {t("lb.noMembersReject")}
                        </TableCell>
                      </TableRow>
                    )}
                    {members.map((m, i) => (
                      <TableRow key={i}>
                        <TableCell>
                          {m.vm_name ? (
                            <span className="flex flex-col">
                              <Link
                                to={`/vms/${encodeURIComponent(m.vm_name)}`}
                                className="underline"
                              >
                                {m.vm_name}
                              </Link>
                              <span className="font-mono text-xs text-muted-foreground">
                                {m.mac}
                              </span>
                            </span>
                          ) : m.vm || !m.ip ? (
                            // 읽을 수 없는 VM의 NIC 멤버는 서버가 `vm`·`mac`·`ip`를 비워 보낸다(리뷰 2 서버 m1).
                            <span className="text-muted-foreground">{t("lb.hiddenVm")}</span>
                          ) : (
                            t("lb.addressMember")
                          )}
                          {m.warning && (
                            <p className="mt-1 text-xs text-warning-fg">
                              {/* 서버는 한 멤버의 경고 여럿을 "; "로 잇는다(리뷰 1 M7) */}
                              {m.warning
                                .split("; ")
                                .map((w) => serverText(t, w))
                                .join("; ")}
                            </p>
                          )}
                        </TableCell>
                        <TableCell className="font-mono text-xs">{m.ip ?? "—"}</TableCell>
                        <TableCell>{m.port}</TableCell>
                        <TableCell>
                          <span className="flex flex-col items-start gap-0.5">
                            <StatusChip tone={HEALTH_TONE[m.health] ?? "neutral"}>
                              {t(`lb.healthState.${m.health}`)}
                            </StatusChip>
                            {m.reason && (
                              <span className="text-xs text-muted-foreground">
                                {serverText(t, m.reason)}
                              </span>
                            )}
                          </span>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </section>
            );
          })}
        </div>
      </SheetContent>
    </Sheet>
  );
}

/** NIC 후보의 이름 `web #1 (tenant-a, 10.0.0.5)` */
function choiceLabel(
  t: (key: string, values?: Record<string, unknown>) => string,
  c: MemberChoice,
) {
  return t("lb.nicLabel", { vm: c.vm.name, n: c.index + 1, network: c.network, ip: c.ip ?? "—" });
}

/**
 * 만들기(`lb`가 null)·고치기 대화상자: 이름·폴더·VIP 네트워크(만들 때만), VIP(비면 자동), 바깥 VIP, 출발지 고정, 리스너마다
 * 프로토콜·포트·건강 검사·멤버(NIC 고르기 — 막힌 NIC는 까닭과 함께 고를 수 없게, 또는 주소). 고치기는 바꾼 칸만 `PATCH`
 * (`If-Match`)한다.
 */
function LbDialog({ lb, onClose }: { lb: LoadBalancer | null; onClose(): void }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const folder = useNewFolder("network", "network.config");
  const networks = useList("network");
  const vms = useList("vm");
  const routed = useRouted();
  const routers = useRouters();
  const candidates = vipNetworks(networks.data ?? [], routed.data ?? []).filter(
    (n) => lb !== null || perms.has("network.assign", objectTarget("network", n)),
  );
  const [name, setName] = useState(lb?.name ?? "");
  const [description, setDescription] = useState(lb?.spec.description ?? "");
  const [network, setNetwork] = useState(lb?.spec.network ?? "");
  const vipNetwork = network || candidates[0]?.name || "";
  const [vip, setVip] = useState(lb?.spec.vip ?? "");
  const routedExternal = routedOf(routed.data, vipNetwork)?.external;
  // 라우터의 외부 네트워크를 읽을 수 없으면 서버가 빈 글을 준다. 고칠 때는 LB가 이미 가진 외부 네트워크 이름을 쓴다.
  const externalOf = routedExternal || lb?.spec.external?.network;
  const [external, setExternal] = useState(lb?.spec.external !== undefined);
  const [externalIp, setExternalIp] = useState(lb?.spec.external?.ip ?? "");
  // 고칠 때는 바깥 VIP 칸을 사용자가 바꿨을 때만 보낸다(리뷰 2 웹 #1 — 그대로 보내면 읽을 수 없거나 바뀐 외부 네트워크
  // 때문에 저장이 막히거나 VIP가 말없이 옮겨진다).
  const externalChanged =
    lb === null ||
    external !== (lb.spec.external !== undefined) ||
    (external && externalIp.trim() !== (lb.spec.external?.ip ?? ""));
  const [affinity, setAffinity] = useState(lb?.spec.affinity_secs?.toString() ?? "");
  const [rows, setRows] = useState<ListenerRow[]>(() =>
    lb ? listenerRows(lb.spec.listeners) : [newListener()],
  );
  const [errors, setErrors] = useState<Record<string, string>>({});
  const reachable = memberNetworks(vipNetwork, routers.data, routed.data);
  const choices = memberChoices(reachable, vms.data ?? []).map((c) =>
    c.reason || perms.has("vm.config", objectTarget("vm", c.vm))
      ? c
      : { ...c, reason: { key: "lb.why.denied" } },
  );

  const setRow = (i: number, patch: Partial<ListenerRow>) =>
    setRows(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));

  async function submit() {
    const found: Record<string, string> = {};
    if (!lb && !validName(name)) {
      found.name = "forms.name";
    }
    if (vipNetwork === "") {
      found.network = "lb.errors.network";
    }
    if (vip.trim() !== "" && ipFamily(vip.trim()) !== 4) {
      found.vip = "lb.errors.vip";
    }
    if (externalChanged && external && !externalOf) {
      found.external = "lb.errors.noExternal";
    }
    if (
      externalChanged &&
      external &&
      externalIp.trim() !== "" &&
      ipFamily(externalIp.trim()) !== 4
    ) {
      found.externalIp = "lb.errors.vip";
    }
    const aff = affinity.trim();
    if (aff !== "" && !(/^\d+$/.test(aff) && Number(aff) >= 1 && Number(aff) <= 65535)) {
      found.affinity = "lb.errors.affinity";
    }
    const checked = checkListeners(rows);
    Object.assign(found, checked.errors);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const externalSpec =
      external && externalOf
        ? {
            network: externalOf,
            ...(externalIp.trim() !== "" && { ip: externalIp.trim() }),
          }
        : undefined;
    let warning: string | null;
    if (lb) {
      ({ warning } = await callWarned(
        api.PATCH("/load-balancers/{key}", {
          params: { path: { key: lb.id }, header: { "If-Match": lb.revision } },
          body: {
            description: description.trim(),
            ...(externalChanged && { external: externalSpec ?? null }),
            listeners: checked.listeners,
            affinity_secs: aff === "" ? null : Number(aff),
          },
        }),
      ));
      toast.success(t("lb.saved", { name: lb.name }));
    } else {
      const created = await callWarned(
        api.POST("/load-balancers", {
          body: {
            name,
            spec: {
              network: vipNetwork,
              listeners: checked.listeners,
              ...(folder.value && { folder: folder.value }),
              ...(description.trim() !== "" && { description: description.trim() }),
              ...(vip.trim() !== "" && { vip: vip.trim() }),
              ...(externalSpec && { external: externalSpec }),
              ...(aff !== "" && { affinity_secs: Number(aff) }),
            },
          },
        }),
      );
      warning = created.warning;
      toast.success(t("lb.created", { name, vip: created.data.spec.vip ?? "" }));
    }
    if (warning) {
      // 경고는 `; `로 이은 여러 문장이다(멤버 보안 그룹·바깥 VIP의 게이트웨이).
      for (const w of warning.split("; ")) {
        toast.warning(serverText(t, w));
      }
    }
    await refreshLbs(queryClient);
  }

  return (
    <FormDialog
      title={lb ? t("lb.editTitle", { name: lb.name }) : t("lb.createTitle")}
      description={t("lb.createDescription")}
      submitLabel={lb ? t("common.save") : t("common.create")}
      className="sm:max-w-2xl"
      onClose={onClose}
      onSubmit={submit}
    >
      <div className="-mx-1 flex max-h-[65svh] flex-col gap-4 overflow-y-auto px-1">
        {!lb && (
          <Field id="lb-name" label={t("common.name")} error={errors.name}>
            {(a11y) => (
              <Input
                id="lb-name"
                autoComplete="off"
                value={name}
                onChange={(e) => setName(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        )}
        {!lb && <FolderSelect id="lb-folder" state={folder} />}
        <Field id="lb-description" label={t("lb.descriptionOptional")}>
          {(a11y) => (
            <Input
              id="lb-description"
              maxLength={200}
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
        <Field
          id="lb-network"
          label={t("lb.vipNetwork")}
          error={errors.network}
          hint={t("lb.vipNetworkHint")}
        >
          {(a11y) => (
            <NativeSelect
              id="lb-network"
              className="w-full"
              disabled={lb !== null}
              value={vipNetwork}
              onChange={(e) => {
                setNetwork(e.target.value);
                setVip("");
              }}
              {...a11y}
            >
              {candidates.length === 0 && (
                <NativeSelectOption value="">{t("lb.noVipNetwork")}</NativeSelectOption>
              )}
              {lb && !candidates.some((n) => n.name === lb.spec.network) && (
                <NativeSelectOption value={lb.spec.network}>{lb.spec.network}</NativeSelectOption>
              )}
              {candidates.map((n) => (
                <NativeSelectOption key={n.id} value={n.name}>
                  {n.name}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )}
        </Field>
        <Field id="lb-vip" label={t("lb.vipOptional")} error={errors.vip} hint={t("lb.vipHint")}>
          {(a11y) => (
            <Input
              id="lb-vip"
              placeholder="10.0.0.50"
              disabled={lb !== null}
              value={vip}
              onChange={(e) => setVip(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-2">
            <Checkbox
              id="lb-external"
              checked={external}
              disabled={!externalOf && !external}
              onCheckedChange={(on) => setExternal(on === true)}
            />
            <Label htmlFor="lb-external">
              {externalOf
                ? t("lb.externalOn", { network: externalOf })
                : routedExternal === ""
                  ? t("lb.externalHidden")
                  : t("lb.externalNone")}
            </Label>
          </div>
          {errors.external && <p className="text-xs text-destructive">{t(errors.external)}</p>}
          {external && externalOf && (
            <Field
              id="lb-external-ip"
              label={t("lb.externalIpOptional")}
              error={errors.externalIp}
              hint={t("lb.externalIpHint")}
            >
              {(a11y) => (
                <Input
                  id="lb-external-ip"
                  placeholder="203.0.113.130"
                  value={externalIp}
                  onChange={(e) => setExternalIp(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
          )}
        </div>
        <Field
          id="lb-affinity"
          label={t("lb.affinityOptional")}
          error={errors.affinity}
          hint={t("lb.affinityHint")}
        >
          {(a11y) => (
            <Input
              id="lb-affinity"
              inputMode="numeric"
              value={affinity}
              onChange={(e) => setAffinity(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
        <fieldset className="flex flex-col gap-3">
          <legend className="mb-1 text-sm font-medium">{t("lb.listeners")}</legend>
          {errors.listeners && <p className="text-xs text-destructive">{t(errors.listeners)}</p>}
          {rows.map((row, i) => (
            <ListenerEditor
              key={i}
              index={i}
              row={row}
              errors={errors}
              choices={choices}
              onChange={(patch) => setRow(i, patch)}
              onRemove={rows.length > 1 ? () => setRows(rows.filter((_, j) => j !== i)) : undefined}
            />
          ))}
          {rows.length < MAX_LISTENERS && (
            <div>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => setRows([...rows, { ...newListener(), port: "" }])}
              >
                <Plus />
                {t("lb.addListener")}
              </Button>
            </div>
          )}
        </fieldset>
      </div>
    </FormDialog>
  );
}

function ListenerEditor({
  index: i,
  row,
  errors,
  choices,
  onChange,
  onRemove,
}: {
  index: number;
  row: ListenerRow;
  errors: Record<string, string>;
  choices: MemberChoice[];
  onChange(patch: Partial<ListenerRow>): void;
  onRemove?: () => void;
}) {
  const { t } = useTranslation();
  const [pick, setPick] = useState("");
  const setMember = (j: number, patch: Partial<MemberRow>) =>
    onChange({ members: row.members.map((m, k) => (k === j ? { ...m, ...patch } : m)) });
  const usable = choices.some((c) => !c.reason);
  const memberLabel = (m: MemberRow) => {
    if (!m.vm) {
      return null;
    }
    const c = choices.find((x) => x.vm.id === m.vm && x.mac === m.mac?.toLowerCase());
    return c ? choiceLabel(t, c) : t("lb.nicMac", { mac: m.mac ?? "" });
  };
  return (
    <div
      role="group"
      aria-label={t("lb.listenerN", { n: i + 1 })}
      className="flex flex-col gap-3 rounded-md border p-3"
    >
      <div className="grid grid-cols-[auto_1fr_auto] items-end gap-2">
        <Field id={`lb-l${i}-protocol`} label={t("lb.protocol")}>
          <NativeSelect
            id={`lb-l${i}-protocol`}
            value={row.protocol}
            onChange={(e) => onChange({ protocol: e.target.value as ListenerRow["protocol"] })}
          >
            <NativeSelectOption value="tcp">TCP</NativeSelectOption>
            <NativeSelectOption value="udp">UDP</NativeSelectOption>
          </NativeSelect>
        </Field>
        <Field id={`lb-l${i}-port`} label={t("lb.vipPort")} error={errors[`l${i}.port`]}>
          {(a11y) => (
            <Input
              id={`lb-l${i}-port`}
              inputMode="numeric"
              value={row.port}
              onChange={(e) => onChange({ port: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          aria-label={t("lb.removeListener", { n: i + 1 })}
          disabled={!onRemove}
          onClick={onRemove}
        >
          <X />
        </Button>
      </div>
      <div className="flex items-center gap-2">
        <Checkbox
          id={`lb-l${i}-health`}
          checked={row.health}
          onCheckedChange={(on) => onChange({ health: on === true })}
        />
        <Label htmlFor={`lb-l${i}-health`}>{t("lb.healthOn")}</Label>
      </div>
      {row.health && (
        <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
          {(
            [
              ["interval", "lb.interval"],
              ["timeout", "lb.timeout"],
              ["rise", "lb.rise"],
              ["fall", "lb.fall"],
            ] as const
          ).map(([key, label]) => (
            <Field key={key} id={`lb-l${i}-${key}`} label={t(label)}>
              {(a11y) => (
                <Input
                  id={`lb-l${i}-${key}`}
                  inputMode="numeric"
                  value={row[key]}
                  onChange={(e) => onChange({ [key]: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
          ))}
          {errors[`l${i}.health`] && (
            <p className="col-span-full text-xs text-destructive">{t(errors[`l${i}.health`]!)}</p>
          )}
        </div>
      )}
      <div className="flex flex-col gap-2">
        <span className="text-sm font-medium">{t("lb.members")}</span>
        {row.members.length === 0 && (
          <p className="text-xs text-muted-foreground">{t("lb.noMembersReject")}</p>
        )}
        {errors[`l${i}.members`] && (
          <p className="text-xs text-destructive">{t(errors[`l${i}.members`]!)}</p>
        )}
        {row.members.map((m, j) => (
          <div key={j} className="grid grid-cols-[1fr_6rem_auto] items-end gap-2">
            {m.vm ? (
              <p className="pb-2 text-sm">{memberLabel(m)}</p>
            ) : (
              <Field
                id={`lb-l${i}-m${j}-ip`}
                label={t("lb.address")}
                error={errors[`l${i}.m${j}.ip`]}
              >
                {(a11y) => (
                  <Input
                    id={`lb-l${i}-m${j}-ip`}
                    value={m.ip ?? ""}
                    onChange={(e) => setMember(j, { ip: e.target.value })}
                    {...a11y}
                  />
                )}
              </Field>
            )}
            <Field
              id={`lb-l${i}-m${j}-port`}
              label={t("lb.port")}
              error={errors[`l${i}.m${j}.port`]}
            >
              {(a11y) => (
                <Input
                  id={`lb-l${i}-m${j}-port`}
                  inputMode="numeric"
                  value={m.port}
                  onChange={(e) => setMember(j, { port: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label={t("lb.removeMember", { n: j + 1 })}
              onClick={() => onChange({ members: row.members.filter((_, k) => k !== j) })}
            >
              <X />
            </Button>
          </div>
        ))}
        <div className="flex flex-wrap items-end gap-2">
          <Field
            id={`lb-l${i}-pick`}
            label={t("lb.addNicMember")}
            hint={usable ? undefined : t("lb.noUsableNic")}
          >
            {(a11y) => (
              <NativeSelect
                id={`lb-l${i}-pick`}
                value={pick}
                onChange={(e) => setPick(e.target.value)}
                {...a11y}
              >
                <NativeSelectOption value="">{t("lb.pickNic")}</NativeSelectOption>
                {choices.map((c) => (
                  <NativeSelectOption
                    key={`${c.vm.id}/${c.mac}/${c.index}`}
                    value={`${c.vm.id}/${c.mac}`}
                    disabled={!!c.reason}
                  >
                    {c.reason
                      ? t("lb.blockedNic", {
                          nic: choiceLabel(t, c),
                          reason: t(c.reason.key, c.reason.values),
                        })
                      : choiceLabel(t, c)}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={pick === ""}
            onClick={() => {
              const at = pick.indexOf("/");
              onChange({
                members: [
                  ...row.members,
                  { vm: pick.slice(0, at), mac: pick.slice(at + 1), port: row.port },
                ],
              });
              setPick("");
            }}
          >
            {t("lb.addMember")}
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => onChange({ members: [...row.members, { ip: "", port: row.port }] })}
          >
            {t("lb.addAddressMember")}
          </Button>
        </div>
      </div>
    </div>
  );
}
