import { useQueryClient } from "@tanstack/react-query";
import { Globe, Link2, Plus, Trash2, Unlink } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
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
import { FolderSelect, useNewFolder } from "@/folders/FolderSelect";
import { useFolderPaths } from "@/folders/useFolderPaths";
import { PermissionsButton, PermissionsSheet } from "@/permissions/PermissionsSheet";
import {
  type FipState,
  type FloatingIp,
  isProvider,
  type NicChoice,
  nicChoices,
  refreshRouting,
  serverText,
  useFloatingIps,
  useRouted,
} from "@/routing/routing";
import { ipFamily } from "@/sdn/ips";
import { sdnReady, useSdn } from "@/sdn/sdn";
import { validName } from "@/vms/form";

const STATE_TONE: Record<FipState["kind"], StatusTone> = {
  unattached: "neutral",
  distributed: "success",
  centralized: "info",
  unrouted: "warning",
};

/** 상태 칩(분산이면 노드, 중앙·라우팅 안 됨이면 까닭을 아래 줄에) */
export function FipStateChip({ state }: { state: FipState }) {
  const { t } = useTranslation();
  return (
    <span className="flex flex-col items-start gap-0.5">
      <StatusChip tone={STATE_TONE[state.kind]}>
        {state.kind === "distributed" && state.node
          ? t("fip.state.distributedOn", { node: state.node })
          : t(`fip.state.${state.kind}`)}
      </StatusChip>
      {"reason" in state && (
        <span className="text-xs text-muted-foreground">{serverText(t, state.reason)}</span>
      )}
    </span>
  );
}

/** NIC 후보의 이름 `web #1 (tenant-a)` */
function nicLabel(t: (key: string, values?: Record<string, unknown>) => string, c: NicChoice) {
  return t("fip.nicLabel", { vm: c.vm.name, n: c.index + 1, network: c.network });
}

/**
 * 인프라 › 네트워크 › 유동 IP(계획 66): 목록(주소·대상 VM·상태 칩)과 만들기·붙이기 대화상자. 붙이기는 라우팅되는 NIC만
 * 고를 수 있고 막힌 NIC는 까닭과 함께 보인다.
 */
export function FloatingIpsPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const fips = useFloatingIps();
  const folderPath = useFolderPaths();
  const readsSdn = perms.reads("sdn");
  const sdn = useSdn(false, readsSdn);
  const blocked = sdn.isSuccess && !sdnReady(sdn.data);
  const queryClient = useQueryClient();
  const [creating, setCreating] = useState(false);
  const [attaching, setAttaching] = useState<FloatingIp | null>(null);
  const [detaching, setDetaching] = useState<FloatingIp | null>(null);
  const [deleting, setDeleting] = useState<FloatingIp | null>(null);
  const [sharing, setSharing] = useState<FloatingIp | null>(null);
  const write = perms.anywhere("network.config");
  const writes = (f: FloatingIp) => perms.has("network.config", objectTarget("floating-ip", f));
  const delegates = (f: FloatingIp) =>
    perms.has("permission.modify", objectTarget("floating-ip", f));
  const items = fips.data ?? [];

  const createButton = write && (
    <Button disabled={blocked} onClick={() => setCreating(true)}>
      <Plus />
      {t("fip.create")}
    </Button>
  );

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="fips-title"
        title={t("nav.floatingIps")}
        description={t("fip.pageHint")}
        actions={items.length > 0 && createButton}
      />
      {blocked && (
        <p role="status" className="text-sm text-warning-fg">
          {t("router.sdnNotReady")}
        </p>
      )}
      {fips.data === undefined ? (
        <ListPending error={fips.error} />
      ) : items.length === 0 ? (
        <EmptyState
          icon={Globe}
          title={t("fip.empty.title")}
          description={t("fip.empty.description")}
          action={createButton}
        />
      ) : (
        <Table aria-labelledby="fips-title">
          <TableHeader>
            <TableRow>
              <TableHead>{t("fip.address")}</TableHead>
              <TableHead>{t("vms.network")}</TableHead>
              <TableHead>{t("fip.target")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
              <TableHead>{t("folders.folder")}</TableHead>
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {items.map((f) => (
              <TableRow key={f.id}>
                <TableCell>
                  <span className="flex flex-col">
                    <span className="font-mono">{f.spec.address ?? "—"}</span>
                    {f.name !== f.spec.address?.replaceAll(".", "-") && (
                      <span className="text-xs text-muted-foreground">{f.name}</span>
                    )}
                  </span>
                </TableCell>
                <TableCell>{f.spec.network}</TableCell>
                <TableCell>
                  <TargetCell fip={f} />
                </TableCell>
                <TableCell>
                  <FipStateChip state={f.state} />
                </TableCell>
                <TableCell className="text-sm">
                  {f.spec.folder ? folderPath(f.spec.folder) : "/"}
                </TableCell>
                <TableCell className="text-right whitespace-nowrap">
                  {writes(f) &&
                    (f.spec.target ? (
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("fip.detachNamed", { name: f.name })}
                        onClick={() => setDetaching(f)}
                      >
                        <Unlink />
                      </Button>
                    ) : (
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("fip.attachNamed", { name: f.name })}
                        onClick={() => setAttaching(f)}
                      >
                        <Link2 />
                      </Button>
                    ))}
                  {delegates(f) && (
                    <PermissionsButton name={f.name} onClick={() => setSharing(f)} />
                  )}
                  {writes(f) && (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: f.name })}
                      onClick={() => setDeleting(f)}
                    >
                      <Trash2 />
                    </Button>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {creating && <CreateFipDialog onClose={() => setCreating(false)} />}
      {attaching && <AttachDialog fip={attaching} onClose={() => setAttaching(null)} />}
      {sharing && (
        <PermissionsSheet
          target={objectTarget("floating-ip", sharing)}
          label={sharing.name}
          onClose={() => setSharing(null)}
        />
      )}
      <ConfirmDialog
        open={detaching !== null}
        onOpenChange={(open) => !open && setDetaching(null)}
        title={t("fip.detachTitle", { name: detaching?.name })}
        description={t("fip.detachDescription")}
        confirmLabel={t("fip.detach")}
        onConfirm={async () => {
          const fip = detaching!;
          await call(
            api.PATCH("/floating-ips/{key}", {
              params: { path: { key: fip.id }, header: { "If-Match": fip.revision } },
              body: { target: null },
            }),
          );
          await refreshRouting(queryClient);
          toast.success(t("fip.detached", { name: fip.name }));
        }}
      />
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("fip.deleteTitle", { name: deleting?.name })}
        description={t(deleting?.spec.target ? "fip.deleteAttached" : "fip.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const fip = deleting!;
          // 확인을 연 사이 누가 붙였으면(412) 붙은 NAT를 몰래 지우지 않는다(리뷰 1 웹 M7).
          await call(
            api.DELETE("/floating-ips/{key}", {
              params: { path: { key: fip.id }, header: { "If-Match": fip.revision } },
            }),
          );
          await refreshRouting(queryClient);
          toast.success(t("fip.deleted", { name: fip.name }));
        }}
      />
    </div>
  );
}

/** 대상: VM 링크(읽을 수 있을 때 — 서버가 `vm_name`을 줌)와 NIC MAC */
function TargetCell({ fip }: { fip: FloatingIp }) {
  const { t } = useTranslation();
  const target = fip.spec.target;
  if (!target) {
    return <span className="text-muted-foreground">{t("fip.unattached")}</span>;
  }
  return (
    <span className="flex flex-col">
      {fip.vm_name ? (
        <Link to={`/vms/${encodeURIComponent(fip.vm_name)}`} className="underline">
          {fip.vm_name}
        </Link>
      ) : (
        <span className="text-muted-foreground">{t("fip.hiddenVm")}</span>
      )}
      <span className="font-mono text-xs text-muted-foreground">{target.mac}</span>
    </span>
  );
}

/**
 * NIC 후보 고르기: 라우팅되는 NIC만 고를 수 있다. 막힌 NIC는 까닭을 붙여 고를 수 없게 보인다(서버의 409
 * `fip_not_routable`·`fip_in_use`를 미리 알린다). `vm.config`가 없는 VM은 서버가 403이므로 막는다.
 */
function useNicChoices(network: string, self?: string) {
  const perms = usePermissions();
  const vms = useList("vm");
  const networks = useList("network");
  // 읽을 수 없는 라우터 뒤의 NIC도 서버가 알리는 `routed`로 고른다(리뷰 2 웹 I1).
  const routed = useRouted();
  const fips = useFloatingIps();
  const ready = vms.data && networks.data && routed.data && fips.data;
  const choices = ready
    ? nicChoices(network, vms.data!, networks.data!, routed.data!, fips.data!, self).map((c) =>
        c.reason || perms.has("vm.config", objectTarget("vm", c.vm))
          ? c
          : { ...c, reason: { key: "fip.why.denied" } },
      )
    : [];
  return { ready: !!ready, choices };
}

function NicSelect({
  id,
  choices,
  value,
  optional,
  error,
  onChange,
}: {
  id: string;
  choices: NicChoice[];
  value: string;
  optional?: boolean;
  error?: string;
  onChange(value: string): void;
}) {
  const { t } = useTranslation();
  const usable = choices.some((c) => !c.reason);
  return (
    <>
      <Field
        id={id}
        label={t(optional ? "fip.attachOptional" : "fip.nic")}
        error={error}
        help={t("fip.nicHint")}
      >
        {(a11y) => (
          <NativeSelect
            id={id}
            className="w-full"
            value={value}
            onChange={(e) => onChange(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">
              {t(optional ? "fip.noAttach" : "fip.pickNic")}
            </NativeSelectOption>
            {choices.map((c) => (
              <NativeSelectOption
                key={`${c.vm.id}/${c.mac}`}
                value={`${c.vm.id}/${c.mac}`}
                disabled={!!c.reason}
              >
                {c.reason
                  ? t("fip.blockedNic", {
                      nic: nicLabel(t, c),
                      reason: t(c.reason.key, c.reason.values),
                    })
                  : nicLabel(t, c)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {!usable && <p className="text-xs text-muted-foreground">{t("fip.noRoutableNic")}</p>}
    </>
  );
}

/** `vmId/mac` → 붙일 대상 */
function targetOf(value: string): { vm: string; mac: string } | null {
  const at = value.indexOf("/");
  return at > 0 ? { vm: value.slice(0, at), mac: value.slice(at + 1) } : null;
}

function CreateFipDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const folder = useNewFolder("network", "network.config");
  const networks = useList("network");
  const providers = (networks.data ?? []).filter(
    (n) => isProvider(n) && perms.has("network.assign", objectTarget("network", n)),
  );
  const [network, setNetwork] = useState("");
  const chosenNetwork = network || providers[0]?.name || "";
  const [address, setAddress] = useState("");
  const [name, setName] = useState("");
  const [nic, setNic] = useState("");
  const { choices } = useNicChoices(chosenNetwork);
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (chosenNetwork === "") {
      found.network = "fip.errors.network";
    }
    if (address.trim() !== "" && ipFamily(address.trim()) !== 4) {
      found.address = "fip.errors.address";
    }
    if (name !== "" && !validName(name)) {
      found.name = "forms.name";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const target = targetOf(nic);
    const created = await call(
      api.POST("/floating-ips", {
        body: {
          ...(name !== "" && { name }),
          spec: {
            network: chosenNetwork,
            ...(address.trim() !== "" && { address: address.trim() }),
            ...(folder.value && { folder: folder.value }),
            ...(target && { target }),
          },
        },
      }),
    );
    await refreshRouting(queryClient);
    toast.success(t("fip.created", { address: created.spec.address ?? created.name }));
  }

  return (
    <FormDialog
      title={t("fip.createTitle")}
      description={t("fip.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <FolderSelect id="fip-folder" state={folder} />
      <Field id="fip-network" label={t("fip.network")} error={errors.network}>
        {(a11y) => (
          <NativeSelect
            id="fip-network"
            className="w-full"
            value={chosenNetwork}
            onChange={(e) => {
              setNetwork(e.target.value);
              setNic("");
            }}
            {...a11y}
          >
            {providers.length === 0 && (
              <NativeSelectOption value="">{t("fip.noProvider")}</NativeSelectOption>
            )}
            {providers.map((n) => (
              <NativeSelectOption key={n.id} value={n.name}>
                {n.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <Field
        id="fip-address"
        label={t("fip.addressOptional")}
        error={errors.address}
        help={t("fip.addressHint")}
      >
        {(a11y) => (
          <Input
            id="fip-address"
            placeholder="203.0.113.120"
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="fip-name"
        label={t("fip.nameOptional")}
        error={errors.name}
        help={t("fip.nameHint")}
      >
        {(a11y) => (
          <Input id="fip-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      {chosenNetwork !== "" && (
        <NicSelect id="fip-nic" choices={choices} value={nic} optional onChange={setNic} />
      )}
    </FormDialog>
  );
}

function AttachDialog({ fip, onClose }: { fip: FloatingIp; onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { choices } = useNicChoices(fip.spec.network, fip.id);
  const [nic, setNic] = useState("");
  const [error, setError] = useState<string | undefined>();

  async function submit() {
    const target = targetOf(nic);
    if (!target) {
      setError("fip.errors.nic");
      return false;
    }
    await call(
      api.PATCH("/floating-ips/{key}", {
        params: { path: { key: fip.id }, header: { "If-Match": fip.revision } },
        body: { target },
      }),
    );
    await refreshRouting(queryClient);
    const vm: Vm | undefined = choices.find((c) => c.vm.id === target.vm)?.vm;
    toast.success(t("fip.attached", { name: fip.name, vm: vm?.name ?? target.vm }));
  }

  return (
    <FormDialog
      title={t("fip.attachTitle", { name: fip.spec.address ?? fip.name })}
      description={t("fip.attachDescription", { network: fip.spec.network })}
      submitLabel={t("fip.attach")}
      onClose={onClose}
      onSubmit={submit}
    >
      <NicSelect
        id="fip-attach-nic"
        choices={choices}
        value={nic}
        error={error}
        onChange={setNic}
      />
    </FormDialog>
  );
}
