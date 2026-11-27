import { useQueryClient } from "@tanstack/react-query";
import { Plus, X } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api } from "@/api/client";
import { putResource } from "@/api/optimistic";
import type { Node } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { InfoButton } from "@/components/InfoButton";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { managedName, validBridge } from "@/networks/NetworksPage";
import { MAX_PHYSNETS, type NodeSdnConfig, validPhysnet } from "@/routing/routing";
import { callWarned } from "@/sdn/sdn";

/** macOS 노드의 하이퍼바이저(OVN chassis가 될 수 없다) */
const HVF = "qemu-hvf";
/** 게이트웨이 우선순위의 화면 기본값(설계 §6) */
const DEFAULT_GATEWAY_PRIORITY = 100;

/** physnet 한 줄(입력 그대로) */
interface PhysnetRow {
  name: string;
  bridge: string;
}

interface SdnValues {
  rows: PhysnetRow[];
  gateway: boolean;
  priority: string;
}

function valuesOf(sdn: NodeSdnConfig | undefined): SdnValues {
  return {
    rows: Object.entries(sdn?.physnets ?? {}).map(([name, bridge]) => ({ name, bridge })),
    gateway: sdn?.gateway != null,
    priority: String(sdn?.gateway ?? DEFAULT_GATEWAY_PRIORITY),
  };
}

/**
 * 폼 값의 입력 오류(번역 키)와 보낼 `NodeSdnConfig`. 빈 설정은 `{}`(서버가 지운다 — 칸을 빼면 그대로 둔다).
 */
export function sdnConfigOf(values: SdnValues): {
  errors: Record<string, string>;
  config: NodeSdnConfig;
} {
  const errors: Record<string, string> = {};
  const physnets: Record<string, string> = {};
  values.rows.forEach((row, i) => {
    const name = row.name.trim();
    const bridge = row.bridge.trim();
    if (!validPhysnet(name)) {
      errors[`physnets.${i}.name`] = "sdn.physnet.errors.name";
    } else if (name in physnets) {
      errors[`physnets.${i}.name`] = "sdn.physnet.errors.duplicate";
    }
    if (!validBridge(bridge) || bridge === "br-int") {
      errors[`physnets.${i}.bridge`] = "sdn.physnet.errors.bridge";
    }
    physnets[name] = bridge;
  });
  if (values.rows.length > MAX_PHYSNETS) {
    errors.physnets = "sdn.physnet.errors.tooMany";
  }
  const priority = values.priority.trim();
  if (values.gateway && !(/^\d+$/.test(priority) && +priority >= 1 && +priority <= 1000)) {
    errors.priority = "sdn.physnet.errors.priority";
  }
  return {
    errors,
    config: {
      ...(values.rows.length > 0 && { physnets }),
      ...(values.gateway && { gateway: Number(priority) }),
    },
  };
}

/**
 * 노드 상세의 "SDN" 카드(계획 66 §7): physnet 표(이름 → 브리지, 준비·오류)와 게이트웨이 후보·우선순위. 고치기는 루트
 * `cluster.admin`(`PUT /nodes/{key}`의 `spec.sdn`). 위트니스·macOS 노드는 chassis가 될 수 없어 카드가 없다.
 */
export function NodeSdnSection({ node }: { node: Node }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const [editing, setEditing] = useState(false);
  if (node.spec.witness || node.status.hypervisor === HVF) {
    return null;
  }
  const config = node.spec.sdn;
  const states = node.status.sdn?.physnets ?? [];
  const names = [
    ...new Set([...Object.keys(config?.physnets ?? {}), ...states.map((s) => s.name)]),
  ].sort();
  const admin = perms.has("cluster.admin");
  if (names.length === 0 && config?.gateway == null && !admin) {
    return null;
  }
  return (
    <section aria-labelledby="node-sdn" className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="node-sdn" className="font-medium">
          {t("sdn.physnet.title")}
        </h2>
        {admin && (
          <Button variant="outline" size="sm" onClick={() => setEditing(true)}>
            {t("sdn.physnet.edit")}
          </Button>
        )}
      </div>
      <p className="text-sm">
        {config?.gateway != null
          ? t("sdn.physnet.gatewayOn", { priority: config.gateway })
          : t("sdn.physnet.gatewayOff")}
        {/* 에이전트가 `ovn-cms-options`에 적었는지(chassis일 때만 알린다) */}
        {config?.gateway != null &&
          node.status.sdn?.chassis === "ready" &&
          !node.status.sdn.gateway && (
            <span className="text-muted-foreground"> {t("sdn.physnet.gatewayPending")}</span>
          )}
      </p>
      {names.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("sdn.physnet.none")}</p>
      ) : (
        <Table aria-labelledby="node-sdn">
          <TableHeader>
            <TableRow>
              <TableHead>{t("sdn.physnet.name")}</TableHead>
              <TableHead>{t("sdn.physnet.bridge")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {names.map((name) => {
              const state = states.find((s) => s.name === name);
              const wanted = config?.physnets?.[name];
              return (
                <TableRow key={name}>
                  <TableCell>{name}</TableCell>
                  <TableCell className="font-mono text-xs">
                    {wanted ?? state?.bridge ?? "—"}
                  </TableCell>
                  <TableCell>
                    <span className="flex flex-col items-start gap-0.5">
                      {state === undefined ? (
                        <StatusChip tone="neutral">
                          {t(wanted ? "sdn.physnet.pending" : "sdn.physnet.removing")}
                        </StatusChip>
                      ) : state.ready ? (
                        <StatusChip tone="success">{t("sdn.physnet.ready")}</StatusChip>
                      ) : (
                        <StatusChip tone="danger">{t("sdn.physnet.notReady")}</StatusChip>
                      )}
                      {state?.error && (
                        <span className="text-xs text-muted-foreground">{state.error}</span>
                      )}
                    </span>
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      {editing && <NodeSdnDialog node={node} onClose={() => setEditing(false)} />}
    </section>
  );
}

function NodeSdnDialog({ node, onClose }: { node: Node; onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 연 때의 노드(판·스펙·레이블): 그사이 다른 곳에서 바꿨으면 서버가 412로 알린다(호스트 자원 대화상자와 같다).
  const [opened] = useState(node);
  const [values, setValues] = useState<SdnValues>(() => valuesOf(opened.spec.sdn));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const bridgesId = useId();
  // Orbivisor가 만든 브리지(VXLAN `oplv<vni>`, physnet `oplp…`)와 OVN 통합 브리지는 physnet에 쓸 수 없어 제안하지 않는다.
  const bridges = opened.status.bridges
    .map((b) => b.name)
    .filter((b) => !managedName(b) && b !== "br-int")
    .sort();
  const setRow = (i: number, patch: Partial<PhysnetRow>) =>
    setValues({ ...values, rows: values.rows.map((r, j) => (j === i ? { ...r, ...patch } : r)) });

  async function submit() {
    const { errors: found, config } = sdnConfigOf(values);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    // 409 physnet_in_use: 빠지는 physnet을 쓰는 VM이 이 노드에서 돈다(서버가 VM 이름을 말한다).
    const { data, warning } = await callWarned(
      api.PUT("/nodes/{key}", {
        params: { path: { key: opened.id }, header: { "If-Match": opened.revision } },
        body: { labels: opened.labels, spec: { ...opened.spec, sdn: config } },
      }),
    );
    putResource(queryClient, "node", data);
    toast.success(t("sdn.physnet.saved", { name: opened.name }));
    if (warning) {
      toast.warning(warning);
    }
  }

  return (
    <FormDialog
      title={t("sdn.physnet.editTitle", { name: opened.name })}
      description={t("sdn.physnet.editDescription")}
      submitLabel={t("common.save")}
      className="sm:max-w-2xl"
      onClose={onClose}
      onSubmit={submit}
    >
      <div role="group" aria-label={t("sdn.physnet.rows")} className="flex flex-col gap-2">
        <span className="text-sm font-medium">{t("sdn.physnet.rows")}</span>
        {values.rows.length === 0 && (
          <p className="text-xs text-muted-foreground">{t("sdn.physnet.none")}</p>
        )}
        {values.rows.map((row, i) => (
          <div key={i} className="grid grid-cols-[1fr_1fr_auto] items-start gap-2">
            <Field
              id={`physnet-${i}-name`}
              label={t("sdn.physnet.nameN", { n: i + 1 })}
              error={errors[`physnets.${i}.name`]}
              help={t("help.sdn.physnetName")}
              info="networks.physnet"
            >
              {(a11y) => (
                <Input
                  id={`physnet-${i}-name`}
                  placeholder="ext"
                  value={row.name}
                  onChange={(e) => setRow(i, { name: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id={`physnet-${i}-bridge`}
              label={t("sdn.physnet.bridgeN", { n: i + 1 })}
              error={errors[`physnets.${i}.bridge`]}
              help={t("help.sdn.physnetBridge")}
            >
              {(a11y) => (
                <Input
                  id={`physnet-${i}-bridge`}
                  list={bridgesId}
                  placeholder="vmbr1"
                  value={row.bridge}
                  onChange={(e) => setRow(i, { bridge: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="mt-6"
              aria-label={t("sdn.physnet.remove", { n: i + 1 })}
              onClick={() => setValues({ ...values, rows: values.rows.filter((_, j) => j !== i) })}
            >
              <X />
            </Button>
          </div>
        ))}
        <datalist id={bridgesId}>
          {bridges.map((b) => (
            <option key={b} value={b} />
          ))}
        </datalist>
        {errors.physnets && <p className="text-xs text-destructive">{t(errors.physnets)}</p>}
        {values.rows.length < MAX_PHYSNETS && (
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                setValues({ ...values, rows: [...values.rows, { name: "", bridge: "" }] })
              }
            >
              <Plus />
              {t("sdn.physnet.add")}
            </Button>
          </div>
        )}
        <p className="text-xs text-muted-foreground">{t("sdn.physnet.bridgeHint")}</p>
      </div>
      <div className="flex items-center gap-2">
        <Checkbox
          id="node-sdn-gateway"
          checked={values.gateway}
          onCheckedChange={(on) => setValues({ ...values, gateway: on === true })}
        />
        <Label htmlFor="node-sdn-gateway">{t("sdn.physnet.gateway")}</Label>
        <InfoButton
          topic="sdn.gateway"
          label={t("sdn.physnet.gateway")}
          line={t("help.sdn.gateway")}
        />
      </div>
      {values.gateway && (
        <Field
          id="node-sdn-priority"
          label={t("sdn.physnet.priority")}
          error={errors.priority}
          help={t("sdn.physnet.priorityHint")}
          info="sdn.gateway"
        >
          {(a11y) => (
            <Input
              id="node-sdn-priority"
              inputMode="numeric"
              value={values.priority}
              onChange={(e) => setValues({ ...values, priority: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
    </FormDialog>
  );
}
