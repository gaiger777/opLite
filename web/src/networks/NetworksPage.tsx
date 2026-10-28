import { Plus, Trash2 } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Network, Node } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { validName } from "@/vms/form";

/** QEMU user 모드 NAT. 서버가 이 이름을 예약해 둔다. */
const USER_NETWORK = "user";

/** 서버의 `valid_ifname`: 1–15바이트의 보이는 ASCII, `/`·`:` 없음, `.`·`..`가 아님 */
export function validBridge(name: string): boolean {
  return /^[\x21-\x7e]{1,15}$/.test(name) && !/[/:]/.test(name) && name !== "." && name !== "..";
}

/** 그 브리지가 있는 노드와, VLAN을 쓰는데 VLAN 필터링이 꺼진 노드 */
export function bridgeNodes(network: Network, nodes: Node[]) {
  const having = nodes.filter((n) => n.status.bridges.some((b) => b.name === network.spec.bridge));
  const noVlan =
    network.spec.vlan == null
      ? []
      : having.filter(
          (n) => !n.status.bridges.find((b) => b.name === network.spec.bridge)?.vlan_filtering,
        );
  return { having, noVlan };
}

export function NetworksPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const networks = useList("network");
  const nodes = useList("node");
  const vms = useList("vm");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<Network | null>(null);
  const write = can(me.role, "write", "network");
  // VM 목록을 다 읽기 전에는 모른다(undefined).
  const users = (network: Network) =>
    vms.isSuccess
      ? vms.data.filter((vm) => vm.spec.nics?.some((nic) => nic.network === network.name)).length
      : undefined;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="networks-title" className="text-2xl font-semibold">
          {t("nav.networks")}
        </h1>
        {write && (
          <Button onClick={() => setCreating(true)}>
            <Plus />
            {t("common.create")}
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">{t("networks.userNote")}</p>
      <Table aria-labelledby="networks-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("networks.bridge")}</TableHead>
            <TableHead>{t("networks.vlan")}</TableHead>
            <TableHead>{t("networks.nodes")}</TableHead>
            <TableHead>{t("networks.vms")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(networks.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 6 : 5} query={networks} empty={t("networks.empty")} />
          )}
          {(networks.data ?? []).map((network) => {
            const { having, noVlan } = bridgeNodes(network, nodes.data ?? []);
            const used = users(network);
            return (
              <TableRow key={network.id}>
                <TableCell className="font-medium">{network.name}</TableCell>
                <TableCell className="font-mono text-xs">{network.spec.bridge}</TableCell>
                <TableCell>{network.spec.vlan ?? "—"}</TableCell>
                <TableCell>
                  {!nodes.isSuccess ? (
                    "—"
                  ) : having.length === 0 ? (
                    <span className="text-destructive">{t("networks.noNode")}</span>
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
                  {write && used !== undefined && used > 0 && (
                    <div className="text-xs text-muted-foreground">{t("networks.inUse")}</div>
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: network.name })}
                      // VM이 쓰는 네트워크는 서버가 지우지 않는다.
                      disabled={used !== 0}
                      onClick={() => setDeleting(network)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreateNetworkDialog onClose={() => setCreating(false)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("networks.deleteTitle", { name: deleting?.name })}
        description={t("networks.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const network = deleting!;
          await call(api.DELETE("/networks/{key}", { params: { path: { key: network.id } } }));
          toast.success(t("networks.deleted", { name: network.name }));
        }}
      />
    </div>
  );
}

function CreateNetworkDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const bridgesId = useId();
  const [name, setName] = useState("");
  const [bridge, setBridge] = useState("");
  const [vlan, setVlan] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 노드가 알린 브리지를 제안한다. 아직 브리지를 만들지 않은 노드도 있으므로 직접 쓸 수도 있다.
  const known = [
    ...new Set((nodes.data ?? []).flatMap((n) => n.status.bridges.map((b) => b.name))),
  ].sort();

  async function submit() {
    const found: Record<string, string> = {};
    if (name === USER_NETWORK) {
      found.name = "networks.errors.reserved";
    } else if (!validName(name)) {
      found.name = "forms.name";
    }
    if (!validBridge(bridge)) {
      found.bridge = "networks.errors.bridge";
    }
    // 숫자만 받는다(`Number`는 "0x10"·"1e3"도 읽는다).
    const tag = Number(vlan);
    if (vlan !== "" && !(/^\d+$/.test(vlan) && tag >= 1 && tag <= 4094)) {
      found.vlan = "networks.errors.vlan";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(
      api.POST("/networks", {
        body: { name, spec: { bridge, vlan: vlan === "" ? null : tag } },
      }),
    );
    toast.success(t("networks.created", { name }));
  }

  return (
    <FormDialog
      title={t("networks.createTitle")}
      description={t("networks.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="network-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="network-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="network-bridge" label={t("networks.bridge")} error={errors.bridge}>
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
        hint={t("networks.vlanHint")}
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
    </FormDialog>
  );
}
