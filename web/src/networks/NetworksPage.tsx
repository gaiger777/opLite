import { ArrowRightLeft, Plus, Trash2 } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { RowConfigMenu } from "@/declarative/ConfigMenu";
import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Network, Node } from "@/api/types";
import { objectTarget } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { FolderSelect, useNewFolder } from "@/folders/FolderSelect";
import { PermissionsButton, PermissionsSheet } from "@/permissions/PermissionsSheet";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { EmptyState } from "@/components/EmptyState";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { PageHeader } from "@/components/PageHeader";
import { ViewToggle } from "@/layout/PageTabs";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
import { useCreateParam } from "@/lib/createParam";
import { migrationOf, progress, useMigrations } from "@/migration/migration";
import { MigrationWizard } from "@/migration/MigrationWizard";
import { StatusChip } from "@/components/StatusChip";
import {
  addressCount,
  hasUsageBar,
  NetworkSheet,
  UsageBar,
  useNetworkIps,
} from "@/sdn/NetworkSheet";
import {
  DEFAULT_PROVIDER_MTU,
  isProvider,
  knownPhysnets,
  physnetNodes,
  providerLabel,
  routingUsers,
  serverText,
  useFloatingIps,
  useRouted,
  useRouters,
  validPhysnet,
} from "@/routing/routing";
import { isSdnNetwork } from "@/sdn/ips";
import { callWarned, DEFAULT_SDN_MTU, sdnReady, useSdn } from "@/sdn/sdn";
import { SubnetsEditor } from "@/sdn/SubnetsEditor";
import { subnetErrors, type SubnetRow, toSubnet } from "@/sdn/subnets";
import { validName } from "@/vms/form";

/** QEMU user 모드 NAT. 서버가 이 이름을 예약해 둔다. */
const USER_NETWORK = "user";

/** 서버의 `DEFAULT_VXLAN_MTU`: underlay 1500에서 VXLAN 머리 50바이트를 뺀 값 */
const DEFAULT_VXLAN_MTU = 1450;

/**
 * 서버의 `is_managed_ifname`: opld가 VXLAN 네트워크(`oplv<vni>`·`oplx<vni>`)와 physnet 연결(`oplp<h8>[obx]`)에 쓰는
 * 이름 모양이라 브리지로 쓸 수 없다.
 */
export function managedName(name: string): boolean {
  return /^opl[vx][0-9]+$/.test(name) || /^oplp[0-9a-f]{8}[obx]$/.test(name);
}

/** 서버의 `valid_ifname`(1–15바이트의 보이는 ASCII, `/`·`:` 없음, `.`·`..`가 아님)이고 관리 이름이 아님 */
export function validBridge(name: string): boolean {
  return (
    /^[\x21-\x7e]{1,15}$/.test(name) &&
    !/[/:]/.test(name) &&
    name !== "." &&
    name !== ".." &&
    !managedName(name)
  );
}

/** OVN 통합 브리지(서버의 `OVN_BRIDGE`) */
const OVN_BRIDGE = "br-int";

/** VM NIC가 붙는 브리지: `bridge`, opld가 만드는 `oplv<vni>`, SDN이면 `br-int`(서버의 `bridge_name`) */
export function bridgeName(spec: Network["spec"]): string {
  return spec.sdn ? OVN_BRIDGE : spec.vxlan ? `oplv${spec.vxlan.vni}` : (spec.bridge ?? "");
}

/** `provider`는 공급자(외부) SDN 네트워크(계획 66) */
type Kind = "bridge" | "vxlan" | "sdn" | "provider";

function kindOf(spec: Network["spec"]): Kind {
  return spec.sdn?.provider ? "provider" : spec.sdn ? "sdn" : spec.vxlan ? "vxlan" : "bridge";
}

/** 목록의 MTU 칸: 정하지 않았으면 종류마다 서버의 기본 */
function mtuOf(spec: Network["spec"]): number | string {
  if (spec.sdn) {
    return spec.mtu ?? (spec.sdn.provider ? DEFAULT_PROVIDER_MTU : DEFAULT_SDN_MTU);
  }
  return spec.vxlan ? (spec.mtu ?? DEFAULT_VXLAN_MTU) : "—";
}

/**
 * 그 브리지가 있는 노드와, VLAN을 쓰는데 VLAN 필터링이 꺼진 노드. SDN 네트워크는 준비된 OVN chassis다(`br-int`는 OVS
 * 브리지라 노드의 브리지 목록에 없다 — 서버 스케줄러의 `has_sdn`). 공급자 네트워크는 그 physnet이 준비된 chassis다
 * (스케줄러 `missing_physnet`).
 */
export function bridgeNodes(network: Network, nodes: Node[]) {
  const physnet = network.spec.sdn?.provider?.physnet;
  if (physnet !== undefined) {
    return {
      having: physnetNodes(nodes, physnet).filter((n) => n.status.sdn?.chassis === "ready"),
      noVlan: [],
    };
  }
  if (isSdnNetwork(network)) {
    return { having: nodes.filter((n) => n.status.sdn?.chassis === "ready"), noVlan: [] };
  }
  const bridge = bridgeName(network.spec);
  const having = nodes.filter((n) => n.status.bridges.some((b) => b.name === bridge));
  const noVlan =
    network.spec.vlan == null
      ? []
      : having.filter((n) => !n.status.bridges.find((b) => b.name === bridge)?.vlan_filtering);
  return { having, noVlan };
}

export function NetworksPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const networks = useList("network");
  const nodes = useList("node");
  const vms = useList("vm");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<Network | null>(null);
  // 계획 62: 네트워크마다 폴더가 다르다. 어딘가에 만들 수 있으면 만들기를, 행마다 그 네트워크의 권한으로 단추를
  // 보인다.
  const write = perms.anywhere("network.config");
  // 브리지 네트워크(노드의 브리지 이름)는 루트 `cluster.admin`만 만든다. VXLAN 네트워크는 폴더의 `network.config`로
  // 만든다(설계 §18 리뷰 1).
  const bridges = perms.has("cluster.admin");
  // 공급자 네트워크의 바꾸기·지우기는 서버가 루트 `cluster.admin`도 본다(리뷰 1 웹 M1 — 폴더의 위임자에게 403 단추를
  // 보이지 않는다).
  const writes = (network: Network) =>
    perms.has("network.config", objectTarget("network", network)) &&
    (!isProvider(network) || bridges);
  const delegates = (network: Network) =>
    perms.has("permission.modify", objectTarget("network", network));
  const actions = write || perms.anywhere("permission.modify");
  const [sharingPerms, setSharingPerms] = useState<Network | null>(null);
  // SDN 네트워크의 서랍(서브넷·주소). 목록이 바뀌면 새 것을 보인다.
  const [opened, setOpened] = useState<string | null>(null);
  const openedNetwork = networks.data?.find((n) => n.id === opened);
  useCreateParam(() => setCreating(true), write || bridges);
  // VM 목록을 다 읽기 전에는 모른다(undefined).
  const users = (network: Network) =>
    vms.isSuccess
      ? vms.data.filter((vm) => vm.spec.nics?.some((nic) => nic.network === network.name)).length
      : undefined;
  // 라우터(인터페이스·외부)와 유동 IP가 쓰는 네트워크도 서버가 지우지 않는다(409 `network_in_use` — 리뷰 2 웹 m2). SDN
  // 네트워크가 있을 때만 읽는다.
  const sdnAny = (networks.data ?? []).some(isSdnNetwork);
  const routers = useRouters(sdnAny);
  const routed = useRouted(sdnAny);
  const fips = useFloatingIps(sdnAny);
  const routing = (network: Network) =>
    routingUsers(network.name, routers.data, routed.data, fips.data);
  // 단순 → SDN 옮기기(계획 67): 단순 네트워크가 있을 때만 읽는다. 옮기기는 SDN이 세워진 뒤에만 시작할 수 있다.
  const simpleAny = (networks.data ?? []).some((n) => !isSdnNetwork(n));
  const migrations = useMigrations(simpleAny);
  const readsSdn = perms.reads("sdn");
  const sdn = useSdn(false, readsSdn && simpleAny);
  const canMigrate = sdnReady(sdn.data);
  const [migrating, setMigrating] = useState<string | null>(null);
  const migratingNetwork = networks.data?.find((n) => n.id === migrating);

  const deleteImpact = (network: Network) => {
    const lines: string[] = [];
    const vmCount = users(network);
    if (vmCount !== undefined) {
      lines.push(t("impact.vmsUsing", { count: vmCount }));
    }
    const r = routing(network);
    if (r.routers > 0) {
      lines.push(t("impact.routersUsing", { count: r.routers }));
    }
    if (r.fips > 0) {
      lines.push(t("impact.fipsUsing", { count: r.fips }));
    }
    return lines.length > 0 ? lines : undefined;
  };

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="networks-title"
        title={t("nav.networks")}
        help="networks"
        intro={t("help.networks.intro")}
        actions={
          <>
            <ViewToggle />
            {(write || bridges) && (
              <Button onClick={() => setCreating(true)}>
                <Plus />
                {t("common.create")}
              </Button>
            )}
          </>
        }
      />
      <p className="text-sm text-muted-foreground">{t("networks.userNote")}</p>
      {networks.isSuccess && networks.data.length === 0 ? (
        <EmptyState
          title={t("networks.empty")}
          description={t("help.networks.empty")}
          diagram="sdn"
          help="networks.page"
          action={
            (write || bridges) && (
              <Button onClick={() => setCreating(true)}>
                <Plus />
                {t("networks.createTitle")}
              </Button>
            )
          }
        />
      ) : (
        <Table aria-labelledby="networks-title">
          <TableHeader>
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("networks.kind")}</TableHead>
              <TableHead>{t("networks.bridge")}</TableHead>
              <TableHead>{t("networks.vlanVni")}</TableHead>
              <TableHead>{t("networks.mtu")}</TableHead>
              <TableHead>{t("sdn.subnets.title")}</TableHead>
              <TableHead>{t("networks.nodes")}</TableHead>
              <TableHead>{t("networks.vms")}</TableHead>
              {actions && (
                <TableHead>
                  <span className="sr-only">{t("vms.actions")}</span>
                </TableHead>
              )}
            </TableRow>
          </TableHeader>
          <TableBody>
            {(networks.data ?? []).length === 0 && (
              <TableMessage
                colSpan={actions ? 9 : 8}
                query={networks}
                empty={t("networks.empty")}
              />
            )}
            {(networks.data ?? []).map((network) => {
              const { having, noVlan } = bridgeNodes(network, nodes.data ?? []);
              const used = users(network);
              const routingUse = routing(network);
              return (
                <TableRow key={network.id}>
                  <TableCell className="font-medium">
                    {isSdnNetwork(network) ? (
                      <button
                        type="button"
                        className="underline-offset-4 hover:underline"
                        aria-label={t("sdn.sheet.open", { name: network.name })}
                        onClick={() => setOpened(network.id)}
                      >
                        {network.name}
                      </button>
                    ) : (
                      network.name
                    )}
                    <MigrationChip
                      migration={migrationOf(migrations.data, network.name)}
                      onOpen={() => setMigrating(network.id)}
                    />
                  </TableCell>
                  <TableCell>{t(`networks.kinds.${kindOf(network.spec)}`)}</TableCell>
                  <TableCell className="font-mono text-xs">{bridgeName(network.spec)}</TableCell>
                  <TableCell>
                    {isProvider(network)
                      ? providerLabel(network)
                      : (network.spec.vxlan?.vni ?? network.spec.vlan ?? "—")}
                  </TableCell>
                  <TableCell>{mtuOf(network.spec)}</TableCell>
                  <TableCell>
                    {isSdnNetwork(network) ? <SubnetsCell network={network} /> : "—"}
                  </TableCell>
                  <TableCell>
                    {!nodes.isSuccess ? (
                      "—"
                    ) : having.length === 0 ? (
                      <span className="text-destructive">
                        {isProvider(network)
                          ? t("sdn.physnet.noNode", {
                              physnet: network.spec.sdn?.provider?.physnet,
                            })
                          : t(isSdnNetwork(network) ? "sdn.noChassis" : "networks.noNode")}
                      </span>
                    ) : (
                      having.map((n) => n.name).join(", ")
                    )}
                    {noVlan.length > 0 && (
                      <div className="text-xs text-destructive">
                        {t("networks.noVlanFiltering", {
                          nodes: noVlan.map((n) => n.name).join(", "),
                        })}
                      </div>
                    )}
                  </TableCell>
                  <TableCell>
                    {used ?? "—"}
                    {writes(network) && used !== undefined && used > 0 && (
                      <div className="text-xs text-muted-foreground">{t("networks.inUse")}</div>
                    )}
                  </TableCell>
                  {actions && (
                    <TableCell className="text-right whitespace-nowrap">
                      {writes(network) &&
                        canMigrate &&
                        !isSdnNetwork(network) &&
                        !migrationOf(migrations.data, network.name) && (
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            aria-label={t("migration.openNamed", { name: network.name })}
                            onClick={() => setMigrating(network.id)}
                          >
                            <ArrowRightLeft />
                          </Button>
                        )}
                      {delegates(network) && (
                        <PermissionsButton
                          name={network.name}
                          onClick={() => setSharingPerms(network)}
                        />
                      )}
                      {writes(network) && (
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t("common.deleteNamed", { name: network.name })}
                          // VM·라우터·유동 IP가 쓰는 네트워크는 서버가 지우지 않는다.
                          disabled={used !== 0 || routingUse.routers + routingUse.fips > 0}
                          onClick={() => setDeleting(network)}
                        >
                          <Trash2 />
                        </Button>
                      )}
                      <RowConfigMenu
                        target={{ kind: "network", id: network.id, name: network.name }}
                      />
                    </TableCell>
                  )}
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      {creating && <CreateNetworkDialog bridges={bridges} onClose={() => setCreating(false)} />}
      {migratingNetwork && (
        <MigrationWizard source={migratingNetwork} onClose={() => setMigrating(null)} />
      )}
      {openedNetwork && (
        <NetworkSheet
          network={openedNetwork}
          writes={writes(openedNetwork)}
          onClose={() => setOpened(null)}
        />
      )}
      {sharingPerms && (
        <PermissionsSheet
          target={objectTarget("network", sharingPerms)}
          label={sharingPerms.name}
          onClose={() => setSharingPerms(null)}
        />
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("networks.deleteTitle", { name: deleting?.name })}
        description={t("networks.deleteDescription")}
        confirmLabel={t("common.delete")}
        impact={deleting ? deleteImpact(deleting) : undefined}
        onConfirm={async () => {
          const network = deleting!;
          await call(api.DELETE("/networks/{key}", { params: { path: { key: network.id } } }));
          toast.success(t("networks.deleted", { name: network.name }));
        }}
      />
    </div>
  );
}

/** 진행 중인 옮기기 칩(누르면 마법사의 진행 단계) */
function MigrationChip({
  migration,
  onOpen,
}: {
  migration: ReturnType<typeof migrationOf>;
  onOpen(): void;
}) {
  const { t } = useTranslation();
  if (!migration) {
    return null;
  }
  const { moved, total } = progress(migration);
  return (
    <button
      type="button"
      className="ml-2 align-middle"
      aria-label={t("migration.chipLabel", { name: migration.name, moved, total })}
      onClick={onOpen}
    >
      <StatusChip tone="info" busy={migration.status.phase === "running"}>
        {t("migration.chip", { moved, total })}
      </StatusChip>
    </button>
  );
}

/** SDN 네트워크 행의 서브넷과 사용률 막대(`GET /networks/{key}/ips`) */
function SubnetsCell({ network }: { network: Network }) {
  const { t } = useTranslation();
  const ips = useNetworkIps(network, true, false);
  const subnets = network.spec.subnets ?? [];
  if (subnets.length === 0) {
    return <span className="text-muted-foreground">{t("sdn.subnets.none")}</span>;
  }
  return (
    <ul className="flex flex-col gap-1">
      {subnets.map((s) => {
        const usage = ips.data?.subnets.find((u) => u.cidr === s.cidr);
        return (
          <li key={s.cidr} className="flex flex-col gap-0.5">
            <span className="font-mono text-xs">{s.cidr}</span>
            {usage && (
              <span className="flex items-center gap-2 text-xs text-muted-foreground">
                {hasUsageBar(usage.size) && <UsageBar used={usage.used} size={usage.size} />}
                {t("sdn.ips.usedShort", { used: usage.used, size: addressCount(usage.size) })}
              </span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

/**
 * `bridges`가 거짓이면(폴더 위임자) VXLAN·SDN만 만든다. 브리지와 공급자(외부, 계획 66 — 노드의 물리 망) 네트워크는 루트
 * `cluster.admin`이다. SDN·공급자는 OVN이 세워졌을 때만 고를 수 있다(서버 409 `sdn_not_ready`). SDN 상태를 읽을 수 없는
 * 사용자(루트 `cluster.read`가 없는 폴더 위임자)에게는 막지 않고 서버에 맡긴다.
 */
function CreateNetworkDialog({ bridges, onClose }: { bridges: boolean; onClose(): void }) {
  const { t } = useTranslation();
  const folder = useNewFolder("network", "network.config");
  const nodes = useList("node");
  // 루트 cluster.read가 없는 폴더 위임자는 `GET /sdn`을 읽지 못한다(403) — 묻지 않고 서버에 맡긴다(리뷰 1 #5).
  const readsSdn = usePermissions().reads("sdn");
  const sdn = useSdn(false, readsSdn);
  const sdnBlocked = sdn.isSuccess && !sdnReady(sdn.data);
  const bridgesId = useId();
  const [name, setName] = useState("");
  const [kind, setKind] = useState<Kind>(bridges ? "bridge" : "vxlan");
  const [bridge, setBridge] = useState("");
  const [vlan, setVlan] = useState("");
  const [vni, setVni] = useState("");
  const [physnet, setPhysnet] = useState("");
  const physnetsId = useId();
  const physnets = knownPhysnets(nodes.data ?? []);
  const sdnKind = kind === "sdn" || kind === "provider";
  const [mtu, setMtu] = useState("");
  const [subnets, setSubnets] = useState<SubnetRow[]>([]);
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 노드가 알린 브리지를 제안한다. 아직 브리지를 만들지 않은 노드도 있으므로 직접 쓸 수도 있다.
  // opld가 VXLAN 네트워크에 만든 브리지(관리 이름)는 쓸 수 없으므로 뺀다.
  const known = [...new Set((nodes.data ?? []).flatMap((n) => n.status.bridges.map((b) => b.name)))]
    .filter((b) => !managedName(b))
    .sort();

  async function submit() {
    const found: Record<string, string> = {};
    if (name === USER_NETWORK) {
      found.name = "networks.errors.reserved";
    } else if (!validName(name)) {
      found.name = "forms.name";
    }
    // 숫자만 받는다(`Number`는 "0x10"·"1e3"도 읽는다).
    const inRange = (value: string, min: number, max: number) =>
      /^\d+$/.test(value) && Number(value) >= min && Number(value) <= max;
    if (kind === "bridge") {
      if (managedName(bridge)) {
        found.bridge = "networks.errors.managed";
      } else if (!validBridge(bridge)) {
        found.bridge = "networks.errors.bridge";
      }
      if (vlan !== "" && !inRange(vlan, 1, 4094)) {
        found.vlan = "networks.errors.vlan";
      }
    } else {
      if (kind === "vxlan" && !inRange(vni, 1, 16777215)) {
        found.vni = "networks.errors.vni";
      }
      if (mtu !== "" && !inRange(mtu, 1280, 9000)) {
        found.mtu = "networks.errors.mtu";
      }
      if (kind === "provider") {
        if (!validPhysnet(physnet)) {
          found.physnet = "sdn.physnet.errors.name";
        }
        if (vlan !== "" && !inRange(vlan, 1, 4094)) {
          found.vlan = "networks.errors.vlan";
        }
      }
      if (sdnKind) {
        if (sdnBlocked) {
          found.kind = "sdn.notReadyHint";
        }
        Object.assign(found, subnetErrors(subnets, kind === "provider"));
      }
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const withMtu = mtu !== "" ? { mtu: Number(mtu) } : {};
    const provider = kind === "provider";
    const spec =
      kind === "bridge"
        ? { bridge, vlan: vlan === "" ? null : Number(vlan) }
        : kind === "vxlan"
          ? { vxlan: { vni: Number(vni) }, ...withMtu }
          : {
              sdn: provider
                ? { provider: { physnet, ...(vlan !== "" && { vlan: Number(vlan) }) } }
                : {},
              ...withMtu,
              ...(subnets.length > 0 && {
                subnets: subnets.map((row) => toSubnet(row, provider)),
              }),
            };
    // 공급자 네트워크: physnet이 준비된 노드가 없어도 만들고 `orbivisor-warning`으로 알린다.
    const { warning } = await callWarned(
      api.POST("/networks", {
        body: { name, spec: { ...spec, ...(folder.value && { folder: folder.value }) } },
      }),
    );
    toast.success(t("networks.created", { name }));
    if (warning) {
      toast.warning(serverText(t, warning));
    }
  }

  return (
    <FormDialog
      title={t("networks.createTitle")}
      description={t("networks.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <FolderSelect id="network-folder" state={folder} />
      <Field
        id="network-name"
        label={t("common.name")}
        error={errors.name}
        help={t("help.networks.name")}
      >
        {(a11y) => (
          <Input
            id="network-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="network-kind"
        label={t("networks.kind")}
        error={errors.kind}
        help={t(`networks.kindHint.${kind}`)}
        info="networks.kind"
      >
        {(a11y) => (
          <NativeSelect
            id="network-kind"
            className="w-full"
            value={kind}
            onChange={(e) => {
              const next = e.target.value as Kind;
              // 공급자와 오버레이의 DHCP 기본이 달라 서브넷 행을 새로 쓴다.
              if ((next === "provider") !== (kind === "provider")) {
                setSubnets([]);
              }
              setKind(next);
            }}
            {...a11y}
          >
            {bridges && (
              <NativeSelectOption value="bridge">{t("networks.kinds.bridge")}</NativeSelectOption>
            )}
            <NativeSelectOption value="vxlan">{t("networks.kinds.vxlan")}</NativeSelectOption>
            {/* OVN이 세워지기 전에는 고를 수 없다(까닭은 아래 글). */}
            <NativeSelectOption value="sdn" disabled={sdnBlocked}>
              {t(sdnBlocked ? "sdn.kindNotReady" : "networks.kinds.sdn")}
            </NativeSelectOption>
            {bridges && (
              <NativeSelectOption value="provider" disabled={sdnBlocked}>
                {t(sdnBlocked ? "sdn.providerNotReady" : "networks.kinds.provider")}
              </NativeSelectOption>
            )}
          </NativeSelect>
        )}
      </Field>
      {sdnBlocked && <p className="text-xs text-muted-foreground">{t("sdn.notReadyHint")}</p>}
      {!readsSdn && sdnKind && (
        <p className="text-xs text-muted-foreground">{t("sdn.serverChecks")}</p>
      )}
      {sdn.isError && sdnKind && (
        <p className="text-xs text-muted-foreground">
          {t("sdn.statusUnknown", { error: errorMessage(t, sdn.error) })}
        </p>
      )}
      {kind === "bridge" ? (
        <>
          <Field
            id="network-bridge"
            label={t("networks.bridge")}
            error={errors.bridge}
            help={t("help.networks.bridge")}
          >
            {(a11y) => (
              <Input
                id="network-bridge"
                list={bridgesId}
                placeholder="vmbr0"
                value={bridge}
                onChange={(e) => setBridge(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <datalist id={bridgesId}>
            {known.map((b) => (
              <option key={b} value={b} />
            ))}
          </datalist>
          <Field
            id="network-vlan"
            label={t("networks.vlanOptional")}
            error={errors.vlan}
            help={t("networks.vlanHint")}
            info="networks.vlan"
          >
            {(a11y) => (
              <Input
                id="network-vlan"
                inputMode="numeric"
                value={vlan}
                onChange={(e) => setVlan(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </>
      ) : (
        <>
          {kind === "provider" && (
            <>
              <Field
                id="network-physnet"
                label={t("sdn.physnet.name")}
                error={errors.physnet}
                help={t(physnets.length > 0 ? "sdn.physnet.pickHint" : "sdn.physnet.noneHint")}
                info="networks.physnet"
              >
                {(a11y) => (
                  <Input
                    id="network-physnet"
                    list={physnetsId}
                    placeholder="ext"
                    value={physnet}
                    onChange={(e) => setPhysnet(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
              <datalist id={physnetsId}>
                {physnets.map((p) => (
                  <option key={p} value={p} />
                ))}
              </datalist>
              <Field
                id="network-vlan"
                label={t("networks.vlanOptional")}
                error={errors.vlan}
                help={t("sdn.physnet.vlanHint")}
                info="networks.vlan"
              >
                {(a11y) => (
                  <Input
                    id="network-vlan"
                    inputMode="numeric"
                    value={vlan}
                    onChange={(e) => setVlan(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
            </>
          )}
          {kind === "vxlan" && (
            <Field
              id="network-vni"
              label={t("networks.vni")}
              error={errors.vni}
              help={t("networks.vniHint")}
            >
              {(a11y) => (
                <Input
                  id="network-vni"
                  inputMode="numeric"
                  value={vni}
                  onChange={(e) => setVni(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
          )}
          <Field
            id="network-mtu"
            label={t("networks.mtuOptional")}
            error={errors.mtu}
            help={t(
              kind === "provider"
                ? "sdn.physnet.mtuHint"
                : kind === "sdn"
                  ? "sdn.mtuHint"
                  : "networks.mtuHint",
            )}
            info="networks.mtu"
          >
            {(a11y) => (
              <Input
                id="network-mtu"
                inputMode="numeric"
                placeholder={String(
                  kind === "provider"
                    ? DEFAULT_PROVIDER_MTU
                    : kind === "sdn"
                      ? DEFAULT_SDN_MTU
                      : DEFAULT_VXLAN_MTU,
                )}
                value={mtu}
                onChange={(e) => setMtu(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          {sdnKind && (
            <SubnetsEditor
              id="network-subnet"
              rows={subnets}
              errors={errors}
              provider={kind === "provider"}
              onChange={setSubnets}
            />
          )}
        </>
      )}
    </FormDialog>
  );
}
