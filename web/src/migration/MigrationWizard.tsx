import { useQueryClient } from "@tanstack/react-query";
import { AlertTriangle } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Network } from "@/api/types";
import { objectTarget } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import {
  isInstanceIdWarning,
  MAX_PARALLEL,
  type Migration,
  MIGRATIONS_KEY,
  type MigrationMode,
  type MigrationPlan,
  type MigrationSpec,
  MODE_TONE,
  MODES,
  PHASE_TONE,
  type PlanChange,
  planChanges,
  planKey,
  planSummary,
  planText,
  planWarnings,
  progress,
  STATE_TONE,
  unfinished,
  useMigrations,
} from "@/migration/migration";
import { bridgeName } from "@/networks/NetworksPage";
import { isOverlay, isProvider, useRouters } from "@/routing/routing";
import { isSdnNetwork, splitList } from "@/sdn/ips";
import { emptySubnetRow, subnetErrors, toSubnet } from "@/sdn/subnets";
import { trackTask } from "@/tasks/tasks";
import { validName } from "@/vms/form";

type Step = "target" | "plan" | "progress" | "finish";

/**
 * "SDN으로 옮기기" 마법사(계획 67 §9, §14 웹): ① 대상 고르기(또는 같은 CIDR로 새 SDN 네트워크), 방식·다리 노드 ② 미리 보기
 * 계획 표(VM마다 방식·주소·출처·막힘·경고 — cloud-init instance-id 바뀜은 눈에 띄게, 주소 고쳐 넣기·VM 빼기·방식 바꾸고 다시
 * 계획) ③ 시작 → 진행(NIC마다 상태, 멈춤·이어 하기·되돌리기) ④ 끝내기(라우터 고르기). 끝나지 않은 옮기기가 있으면 ③에서 연다.
 */
export function MigrationWizard({ source, onClose }: { source: Network; onClose(): void }) {
  const { t } = useTranslation();
  const migrations = useMigrations();
  const current = migrations.data?.find((m) => m.spec.source === source.name && unfinished(m));
  // 연 옮기기를 id로 붙잡는다(리뷰 1 I1 — 끝내거나 모두 되돌려도 그 기록을 계속 보인다).
  const [pinned, setPinned] = useState<string | null>(null);
  if (pinned === null && current) {
    setPinned(current.id);
  }
  const shown = pinned ? migrations.data?.find((m) => m.id === pinned) : undefined;
  const [step, setStep] = useState<Step | null>(null);
  const at: Step = step ?? (shown ? "progress" : "target");
  const [spec, setSpec] = useState<MigrationSpec>({ source: source.name, target: "" });
  const [plan, setPlan] = useState<MigrationPlan | null>(null);

  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-4xl">
        <SheetTitle className="px-4 pt-4 text-lg">
          {t("migration.title", { name: source.name })}
        </SheetTitle>
        <SheetDescription className="px-4 text-sm text-muted-foreground">
          {t("migration.description")}
        </SheetDescription>
        <ol
          className="flex flex-wrap gap-x-4 gap-y-1 px-4 pt-2 text-sm"
          aria-label={t("migration.steps")}
        >
          {(["target", "plan", "progress", "finish"] as const).map((s, i) => (
            <li
              key={s}
              aria-current={at === s ? "step" : undefined}
              className={at === s ? "font-medium text-foreground" : "text-muted-foreground"}
            >
              {i + 1}. {t(`migration.step.${s}`)}
            </li>
          ))}
        </ol>
        <div className="flex flex-col gap-6 p-4">
          {at === "target" && (
            <TargetStep
              source={source}
              spec={spec}
              onPlanned={(next, p) => {
                setSpec(next);
                setPlan(p);
                setStep("plan");
              }}
            />
          )}
          {at === "plan" && plan && (
            <PlanStep
              spec={spec}
              plan={plan}
              onBack={() => setStep("target")}
              onReplan={(next, p) => {
                setSpec(next);
                setPlan(p);
              }}
              onCreated={setPinned}
              onStarted={(id) => {
                setPinned(id);
                setStep("progress");
              }}
            />
          )}
          {at === "progress" && shown && (
            <ProgressStep
              source={source}
              migration={shown}
              onFinish={() => setStep("finish")}
              onClose={onClose}
            />
          )}
          {at === "progress" && !shown && (
            <p className="text-muted-foreground">
              {migrations.data && pinned ? t("migration.gone") : t("common.loading")}
            </p>
          )}
          {at === "finish" && shown && (
            <FinishStep
              migration={shown}
              onBack={() => setStep("progress")}
              onDone={() => setStep("progress")}
            />
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}

/**
 * 계획만 받는다(`?dry_run=true`, 200 `MigrationPlan`). 원본의 모든 VM을 계획한다(`vms` 없이 — 뺀 VM은 화면에서만 흐리게 두어
 * 다시 넣을 수 있다, 리뷰 1 I6). 이름은 보내지 않는다(서버가 `<원본>-to-<대상>`, 쓰였으면 `-2`… — 리뷰 1 I2).
 */
async function dryRun(spec: MigrationSpec): Promise<MigrationPlan> {
  const all: MigrationSpec = { ...spec };
  delete all.vms;
  const plan = await call(
    api.POST("/network-migrations", {
      params: { query: { dry_run: true } },
      body: { spec: all },
    }),
  );
  return plan as MigrationPlan;
}

/** ① 대상과 방식 */
function TargetStep({
  source,
  spec,
  onPlanned,
}: {
  source: Network;
  spec: MigrationSpec;
  onPlanned(spec: MigrationSpec, plan: MigrationPlan): void;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const networks = useList("network");
  const nodes = useList("node");
  // 대상: 서브넷이 있는 SDN 네트워크(오버레이·공급자) 가운데 `network.assign`이 있는 것
  const targets = (networks.data ?? [])
    .filter(
      (n) =>
        isSdnNetwork(n) &&
        (n.spec.subnets ?? []).length > 0 &&
        perms.has("network.assign", objectTarget("network", n)),
    )
    .sort((a, b) => a.name.localeCompare(b.name));
  // "있는/새 네트워크"는 고르기 전에는 목록을 읽은 뒤에 정한다(리뷰 2 웹 m5 — 목록이 오기 전에 정해 "새"에 머물지 않게).
  const [choice, setCreate] = useState<boolean | null>(null);
  const create = choice ?? (networks.data !== undefined && targets.length === 0);
  const [target, setTarget] = useState(spec.target);
  const chosen = target || targets[0]?.name || "";
  const [newName, setNewName] = useState(`${source.name}-sdn`.slice(0, 63));
  const [cidr, setCidr] = useState("");
  const [gateway, setGateway] = useState("");
  const [mode, setMode] = useState<MigrationMode>(spec.mode ?? "auto");
  const [bridge, setBridge] = useState(spec.bridge !== false);
  const [bridgeNode, setBridgeNode] = useState(spec.bridge_node ?? "");
  const [shrink, setShrink] = useState(spec.allow_mtu_shrink ?? false);
  const [parallel, setParallel] = useState(String(spec.parallel ?? 1));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const targetNet = create ? undefined : networks.data?.find((n) => n.name === chosen);
  const overlay = create || (targetNet !== undefined && isOverlay(targetNet));
  // 공급자 대상에는 다리가 없다: `bridge: false`를 보내 대상 네트워크의 겹침 키를 잡지 않는다(리뷰 2 웹 #2·서버 m3).
  const provider = targetNet !== undefined && !isOverlay(targetNet);
  // 다리 노드 후보: 원본 브리지가 있고 chassis가 준비된 노드(서버 `plan_bridge`)
  const bridgeNodes = (nodes.data ?? [])
    .filter(
      (n) =>
        n.status.sdn?.chassis === "ready" &&
        n.status.bridges.some((b) => b.name === bridgeName(source.spec)),
    )
    .sort((a, b) => a.name.localeCompare(b.name));

  async function next() {
    const found: Record<string, string> = {};
    if (create) {
      if (!validName(newName)) {
        found.name = "forms.name";
      }
      const rows = [{ ...emptySubnetRow(), cidr, gateway }];
      const subnet = subnetErrors(rows, false);
      if (cidr.trim() === "") {
        found.cidr = "migration.errors.cidr";
      } else if (Object.keys(subnet).length > 0) {
        found.cidr = Object.values(subnet)[0]!;
      }
    } else if (chosen === "") {
      found.target = "migration.errors.target";
    }
    if (!/^\d+$/.test(parallel) || Number(parallel) < 1 || Number(parallel) > MAX_PARALLEL) {
      found.parallel = "migration.errors.parallel";
    }
    setErrors(found);
    setFailure(null);
    if (Object.keys(found).length > 0) {
      return;
    }
    setBusy(true);
    try {
      let targetName = chosen;
      if (create) {
        await call(
          api.POST("/networks", {
            body: {
              name: newName,
              spec: {
                sdn: {},
                subnets: [toSubnet({ ...emptySubnetRow(), cidr, gateway })],
                ...(source.spec.folder && { folder: source.spec.folder }),
              },
            },
          }),
        );
        targetName = newName;
        toast.success(t("networks.created", { name: newName }));
        // 만든 네트워크를 고른 것으로 둔다(리뷰 1 M2 — 계획이 실패해도 다시 누르면 같은 이름을 또 만들지 않는다).
        await queryClient.invalidateQueries({ queryKey: ["list", "network"] });
        setCreate(false);
        setTarget(newName);
      }
      const nextSpec: MigrationSpec = {
        source: source.name,
        target: targetName,
        ...(mode !== "auto" && { mode }),
        ...((provider || (overlay && !bridge)) && { bridge: false }),
        ...(overlay && bridge && bridgeNode && { bridge_node: bridgeNode }),
        ...(shrink && { allow_mtu_shrink: true }),
        ...(parallel !== "1" && { parallel: Number(parallel) }),
        ...(spec.vms && { vms: spec.vms }),
        ...(spec.addresses && { addresses: spec.addresses }),
      };
      const plan = await dryRun(nextSpec);
      onPlanned(nextSpec, plan);
    } catch (err) {
      setFailure(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1 text-sm font-medium">{t("migration.target")}</legend>
        <div className="flex items-center gap-2">
          <input
            type="radio"
            id="mig-existing"
            name="mig-target"
            checked={!create}
            disabled={targets.length === 0}
            onChange={() => setCreate(false)}
          />
          <Label htmlFor="mig-existing">{t("migration.existing")}</Label>
        </div>
        {!create && (
          <Field id="mig-target" label={t("migration.targetNetwork")} error={errors.target}>
            {(a11y) => (
              <NativeSelect
                id="mig-target"
                className="w-full"
                value={chosen}
                onChange={(e) => setTarget(e.target.value)}
                {...a11y}
              >
                {targets.map((n) => (
                  <NativeSelectOption key={n.id} value={n.name}>
                    {n.name} ({(n.spec.subnets ?? []).map((s) => s.cidr).join(", ")}
                    {isProvider(n) ? `, ${t("networks.kinds.provider")}` : ""})
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
        )}
        <div className="flex items-center gap-2">
          <input
            type="radio"
            id="mig-create"
            name="mig-target"
            checked={create}
            onChange={() => setCreate(true)}
          />
          <Label htmlFor="mig-create">{t("migration.createNew")}</Label>
        </div>
        {create && (
          <div className="grid gap-2 sm:grid-cols-3">
            <Field id="mig-new-name" label={t("common.name")} error={errors.name}>
              {(a11y) => (
                <Input
                  id="mig-new-name"
                  value={newName}
                  onChange={(e) => setNewName(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id="mig-new-cidr"
              label={t("migration.cidr")}
              error={errors.cidr}
              hint={t("migration.cidrHint")}
            >
              {(a11y) => (
                <Input
                  id="mig-new-cidr"
                  placeholder="192.168.10.0/24"
                  value={cidr}
                  onChange={(e) => setCidr(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id="mig-new-gateway"
              label={t("migration.gateway")}
              hint={t("migration.gatewayHint")}
            >
              {(a11y) => (
                <Input
                  id="mig-new-gateway"
                  placeholder="192.168.10.1"
                  value={gateway}
                  onChange={(e) => setGateway(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
          </div>
        )}
      </fieldset>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field id="mig-mode" label={t("migration.mode")} hint={t(`migration.modeHint.${mode}`)}>
          {(a11y) => (
            <NativeSelect
              id="mig-mode"
              className="w-full"
              value={mode}
              onChange={(e) => setMode(e.target.value as MigrationMode)}
              {...a11y}
            >
              {MODES.map((m) => (
                <NativeSelectOption key={m} value={m}>
                  {t(`migration.modes.${m}`)}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )}
        </Field>
        <Field id="mig-parallel" label={t("migration.parallel")} error={errors.parallel}>
          {(a11y) => (
            <Input
              id="mig-parallel"
              inputMode="numeric"
              value={parallel}
              onChange={(e) => setParallel(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      </div>
      <div className="flex items-center gap-2">
        <Checkbox
          id="mig-shrink"
          checked={shrink}
          onCheckedChange={(on) => setShrink(on === true)}
        />
        <Label htmlFor="mig-shrink">{t("migration.allowShrink")}</Label>
      </div>
      {overlay && (
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-2">
            <Checkbox
              id="mig-bridge"
              checked={bridge}
              onCheckedChange={(on) => setBridge(on === true)}
            />
            <Label htmlFor="mig-bridge">{t("migration.bridge")}</Label>
          </div>
          <p className="text-xs text-muted-foreground">{t("migration.bridgeHint")}</p>
          {bridge && (
            <Field id="mig-bridge-node" label={t("migration.bridgeNode")}>
              {(a11y) => (
                <NativeSelect
                  id="mig-bridge-node"
                  className="w-full"
                  value={bridgeNode}
                  onChange={(e) => setBridgeNode(e.target.value)}
                  {...a11y}
                >
                  <NativeSelectOption value="">{t("migration.bridgeNodeAuto")}</NativeSelectOption>
                  {bridgeNodes.map((n) => (
                    <NativeSelectOption key={n.id} value={n.id}>
                      {n.name}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              )}
            </Field>
          )}
        </div>
      )}
      {failure && (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      <div>
        <Button disabled={busy || networks.data === undefined} onClick={() => void next()}>
          {t("migration.makePlan")}
        </Button>
      </div>
    </div>
  );
}

/** ② 계획 표 */
function PlanStep({
  spec,
  plan,
  onBack,
  onReplan,
  onCreated,
  onStarted,
}: {
  spec: MigrationSpec;
  plan: MigrationPlan;
  onBack(): void;
  onReplan(spec: MigrationSpec, plan: MigrationPlan): void;
  /** 리소스를 만들었다(`planned` — 시작 전) */
  onCreated(id: string): void;
  onStarted(id: string): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const [mode, setMode] = useState<MigrationMode>(spec.mode ?? "auto");
  // VM마다 넣기(빼면 만들 때 `vms`에서 뺀다) — 계획은 늘 원본의 모든 VM이다(리뷰 1 I6).
  const allVms = [...new Set(plan.nics.map((n) => n.vm))];
  const [excluded, setExcluded] = useState<Set<string>>(
    () => new Set(spec.vms ? allVms.filter((v) => !spec.vms!.includes(v)) : []),
  );
  // 고쳐 넣은 주소(`vm/mac` → 글)
  const [addresses, setAddresses] = useState<Record<string, string>>(() =>
    Object.fromEntries((spec.addresses ?? []).map((a) => [`${a.vm}/${a.mac}`, a.ip.join(", ")])),
  );
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // 만들 때 서버가 다시 한 계획이 화면과 다르면 시작하기 전에 묻는다(리뷰 1 C1).
  const [changed, setChanged] = useState<{ migration: Migration; changes: PlanChange[] } | null>(
    null,
  );
  const instanceId = plan.nics.some((n) => n.warnings.some(isInstanceIdWarning));
  const movable = plan.nics.filter((n) => n.mode !== "blocked" && !excluded.has(n.vm)).length;
  const blockedBridge = plan.bridge.error != null;
  const summary = planSummary(plan, excluded);
  const summaryText = (["restart", "live", "offline"] as const)
    .filter((k) => summary[k] > 0)
    .map((k) => t(`migration.summary.${k}`, { count: summary[k] }));

  // 화면의 계획을 만든 스펙과 지금 고른 방식·주소가 다르면 다시 계획해야 시작한다(리뷰 1 C1).
  const stale = planKey(edited()) !== planKey(spec);

  function edited(): MigrationSpec {
    // 계획 표가 정하는 칸(VM·주소·방식)은 새로 쓴다.
    const rest: MigrationSpec = { ...spec };
    delete rest.vms;
    delete rest.addresses;
    delete rest.mode;
    const given = Object.entries(addresses)
      .map(([key, text]) => {
        const [vm, mac] = key.split("/");
        return { vm: vm!, mac: mac!, ip: splitList(text) };
      })
      .filter((a) => a.ip.length > 0);
    return {
      ...rest,
      ...(mode !== "auto" && { mode }),
      ...(excluded.size > 0 && { vms: allVms.filter((v) => !excluded.has(v)) }),
      ...(given.length > 0 && { addresses: given }),
    };
  }

  async function replan() {
    setBusy(true);
    setFailure(null);
    try {
      const next = edited();
      onReplan(next, await dryRun(next));
    } catch (err) {
      setFailure(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  /** 만든 옮기기의 `start` Task를 시작한다. 실패해도 리소스는 `planned`로 남으니 진행 단계로 간다(리뷰 1 M3). */
  async function startTask(m: Migration) {
    try {
      const task = await call(
        api.POST("/network-migrations/{key}/start", {
          params: { path: { key: m.id } },
          body: {},
        }),
      );
      trackTask(
        task,
        t("tasks.op.network-migrate", {
          target: m.name,
          step: t("migration.taskStep.start"),
        }),
      );
    } catch (err) {
      toast.error(errorMessage(t, err));
    }
    await queryClient.invalidateQueries({ queryKey: MIGRATIONS_KEY });
    onStarted(m.id);
  }

  async function start() {
    setBusy(true);
    setFailure(null);
    try {
      const next = edited();
      const created = (await call(
        api.POST("/network-migrations", { body: { spec: next } }),
      )) as Migration & { plan: MigrationPlan };
      onCreated(created.id);
      // 경고 수는 서버 계획에서 센다(머리의 글은 문장이다 — 리뷰 1 M1).
      const warnings = planWarnings(created.plan);
      if (warnings > 0) {
        toast.warning(t("migration.planWarnings", { count: warnings }));
      }
      const changes = planChanges(plan, created.plan, excluded);
      if (changes.length > 0) {
        await queryClient.invalidateQueries({ queryKey: MIGRATIONS_KEY });
        setChanged({ migration: created, changes });
        return;
      }
      await startTask(created);
    } catch (err) {
      setFailure(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  const changeText = (c: PlanChange) => {
    const who = t("migration.change.who", { name: c.name, n: c.nic + 1 });
    const mode = (m: string | undefined) => t(`migration.planned.${m ?? "blocked"}`);
    const text =
      c.what === "new"
        ? t("migration.change.new", { who, mode: mode(c.after) })
        : c.what === "mode"
          ? t("migration.change.mode", { who, before: mode(c.before), after: mode(c.after) })
          : c.what === "ips"
            ? t("migration.change.ips", {
                who,
                before: c.before || t("migration.newAddress"),
                after: c.after || t("migration.newAddress"),
              })
            : c.after
              ? t("migration.change.blockers", {
                  who,
                  after: c.after
                    .split("; ")
                    .map((b) => planText(t, b))
                    .join("; "),
                })
              : t("migration.change.unblocked", { who });
    return c.disrupts ? `${text} — ${t(`migration.change.disrupt.${c.after}`)}` : text;
  };

  return (
    <div className="flex flex-col gap-4">
      {instanceId && (
        <p
          role="alert"
          className="flex gap-2 rounded-md bg-warning-soft px-3 py-2 text-sm text-warning-fg"
        >
          <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />
          {t("migration.instanceIdBanner")}
        </p>
      )}
      <div className="flex flex-wrap items-end gap-3">
        <Field id="mig-plan-mode" label={t("migration.mode")}>
          {(a11y) => (
            <NativeSelect
              id="mig-plan-mode"
              value={mode}
              onChange={(e) => setMode(e.target.value as MigrationMode)}
              {...a11y}
            >
              {MODES.map((m) => (
                <NativeSelectOption key={m} value={m}>
                  {t(`migration.modes.${m}`)}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )}
        </Field>
        <Button variant="outline" disabled={busy} onClick={() => void replan()}>
          {t("migration.replan")}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">{t("migration.planHint")}</p>
      <p className="text-xs text-muted-foreground">{t("migration.oneMode")}</p>
      {(plan.hidden ?? 0) > 0 && (
        <p role="status" className="text-sm text-muted-foreground">
          {t("migration.hiddenVms", { count: plan.hidden })}
        </p>
      )}
      <div className="overflow-x-auto">
        <Table aria-label={t("migration.planTable")}>
          <TableHeader>
            <TableRow>
              <TableHead>{t("migration.include")}</TableHead>
              <TableHead>{t("migration.vmNic")}</TableHead>
              <TableHead>{t("migration.mode")}</TableHead>
              <TableHead>{t("migration.address")}</TableHead>
              <TableHead>{t("migration.notes")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {plan.nics.length === 0 && (
              <TableRow>
                <TableCell colSpan={5} className="text-muted-foreground">
                  {t("migration.noNics")}
                </TableCell>
              </TableRow>
            )}
            {plan.nics.map((n) => {
              const key = `${n.vm}/${n.mac ?? ""}`;
              const out = excluded.has(n.vm);
              return (
                <TableRow key={`${n.vm}-${n.nic}`} className={out ? "opacity-60" : undefined}>
                  <TableCell>
                    <Checkbox
                      aria-label={t("migration.includeVm", { name: n.name })}
                      checked={!out}
                      onCheckedChange={(on) => {
                        const next = new Set(excluded);
                        if (on === true) {
                          next.delete(n.vm);
                        } else {
                          next.add(n.vm);
                        }
                        setExcluded(next);
                      }}
                    />
                  </TableCell>
                  <TableCell>
                    <span className="flex flex-col">
                      <span className="font-medium">{n.name}</span>
                      <span className="text-xs text-muted-foreground">
                        {t("migration.nicN", { n: n.nic + 1 })} ·{" "}
                        <span className="font-mono">{n.mac ?? "—"}</span>
                      </span>
                    </span>
                  </TableCell>
                  <TableCell>
                    <StatusChip tone={MODE_TONE[n.mode]}>
                      {t(`migration.planned.${n.mode}`)}
                    </StatusChip>
                  </TableCell>
                  <TableCell>
                    <span className="flex flex-col gap-1">
                      <span className="font-mono text-xs">
                        {n.ips.join(", ") || t("migration.newAddress")}
                      </span>
                      <span
                        className={
                          n.address_source === "ipam"
                            ? "text-xs text-warning-fg"
                            : "text-xs text-muted-foreground"
                        }
                      >
                        {t(`migration.source.${n.address_source}`)}
                      </span>
                      {n.mac && (
                        <Input
                          aria-label={t("migration.fixAddress", { name: n.name, n: n.nic + 1 })}
                          className="h-7 w-40 text-xs"
                          placeholder={t("migration.fixAddressPlaceholder")}
                          value={addresses[key] ?? ""}
                          onChange={(e) => setAddresses({ ...addresses, [key]: e.target.value })}
                        />
                      )}
                    </span>
                  </TableCell>
                  <TableCell>
                    <ul className="flex flex-col gap-1 text-xs">
                      {n.blockers.map((b) => (
                        <li key={b} className="text-destructive">
                          {planText(t, b)}
                        </li>
                      ))}
                      {n.warnings.map((w) => (
                        <li
                          key={w}
                          className={
                            isInstanceIdWarning(w) || w.startsWith("the address changes")
                              ? "font-medium text-warning-fg"
                              : "text-muted-foreground"
                          }
                        >
                          {planText(t, w)}
                        </li>
                      ))}
                    </ul>
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>
      <section aria-labelledby="mig-bridge-title" className="flex flex-col gap-1 text-sm">
        <h3 id="mig-bridge-title" className="font-medium">
          {t("migration.bridgeTitle")}
        </h3>
        <p>
          {plan.bridge.needed
            ? plan.bridge.node
              ? t("migration.bridgeOn", {
                  node:
                    nodes.data?.find((n) => n.id === plan.bridge.node)?.name ?? plan.bridge.node,
                })
              : t("migration.bridgeNeeded")
            : t("migration.bridgeNone")}
        </p>
        {plan.bridge.error && <p className="text-destructive">{planText(t, plan.bridge.error)}</p>}
        {plan.bridge.warnings.map((w) => (
          <p key={w} className="text-muted-foreground">
            {planText(t, w)}
          </p>
        ))}
      </section>
      {failure && (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      {stale && (
        <p role="status" className="text-sm text-warning-fg">
          {t("migration.replanFirst")}
        </p>
      )}
      {summaryText.length > 0 && (
        <p
          className={
            summary.restart > 0 ? "text-sm font-medium text-warning-fg" : "text-sm text-foreground"
          }
        >
          {summaryText.join(", ")}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button variant="outline" disabled={busy} onClick={onBack}>
          {t("common.back")}
        </Button>
        <Button
          disabled={busy || stale || movable === 0 || blockedBridge}
          onClick={() => void start()}
        >
          {t("migration.start", { count: movable })}
        </Button>
      </div>
      {changed && (
        <ConfirmDialog
          open
          onOpenChange={(open) => {
            if (!open) {
              // 시작하지 않으면 `planned`로 남은 옮기기를 진행 단계에서 보인다(시작·지우기).
              const id = changed.migration.id;
              setChanged(null);
              onStarted(id);
            }
          }}
          title={t("migration.changedTitle", { name: changed.migration.name })}
          description={t("migration.changedDescription")}
          confirmLabel={t("migration.changedStart")}
          impact={[
            ...changed.changes.filter((c) => c.disrupts),
            ...changed.changes.filter((c) => !c.disrupts),
          ].map(changeText)}
          onConfirm={() => startTask(changed.migration)}
        >
          {changed.changes.some((c) => c.disrupts) && (
            <p role="alert" className="text-sm font-medium text-warning-fg">
              {t("migration.changedDisrupts", {
                count: new Set(changed.changes.filter((c) => c.disrupts).map((c) => c.vm)).size,
              })}
            </p>
          )}
        </ConfirmDialog>
      )}
    </div>
  );
}

/** ③ 진행: NIC마다 상태, 멈춤·이어 하기(다리 노드 바꾸기)·되돌리기 */
function ProgressStep({
  source,
  migration: m,
  onFinish,
  onClose,
}: {
  source: Network;
  migration: Migration;
  onFinish(): void;
  onClose(): void;
}) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const vms = useList("vm");
  const nodes = useList("node");
  const [chosen, setChosen] = useState<Set<string>>(new Set());
  const [bridgeNode, setBridgeNode] = useState("");
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [rollingBack, setRollingBack] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const phase = m.status.phase;
  const { moved, total } = progress(m);
  const moves = m.status.vms ?? [];
  const vmName = (id: string) => vms.data?.find((v) => v.id === id)?.name ?? id.slice(0, 8);
  const running = phase === "running" || phase === "rolling-back";
  const done = phase === "finished" || phase === "rolled-back";
  const canResume = phase === "planned" || phase === "paused" || phase === "failed";
  const rollbackable = moves.filter((v) => v.state === "moved" || v.state === "failed");
  // 다리 노드를 클러스터에서 지워 멈췄다(서버 `drop_bridge_node`): 이어 할 때 다른 노드를 고른다.
  const bridgeGone =
    m.status.bridge === undefined && (m.status.error ?? "").startsWith("the bridge node");
  // 놓인 다리의 노드가 꺼졌다(지우지 않음): 서버는 `resume --bridge-node`로 바꾸게 한다(리뷰 1 서버 M5, 리뷰 2 웹 #3).
  const bridgeHost = m.status.bridge
    ? nodes.data?.find((n) => n.id === m.status.bridge?.node)
    : undefined;
  const bridgeOffline = bridgeHost !== undefined && !bridgeHost.status.online;
  const nodeName = (id: string) => nodes.data?.find((n) => n.id === id)?.name ?? id;
  // 서버 `plan_bridge`와 같게: chassis가 준비되고 원본 브리지가 있는 노드(리뷰 1 M5). 꺼진 다리 노드는 뺀다.
  const bridgeCandidates = (nodes.data ?? []).filter(
    (n) =>
      n.id !== m.status.bridge?.node &&
      n.status.sdn?.chassis === "ready" &&
      n.status.bridges.some((b) => b.name === bridgeName(source.spec)),
  );

  async function act(
    label: string,
    run: () => Promise<{ id: string; revision: number } | null>,
    task?: string,
  ) {
    setBusy(true);
    setFailure(null);
    try {
      const result = await run();
      if (task && result) {
        trackTask(
          result as never,
          t("tasks.op.network-migrate", { target: m.name, step: t(`migration.taskStep.${task}`) }),
        );
      }
      toast.success(t(label, { name: m.name }));
      await queryClient.invalidateQueries({ queryKey: MIGRATIONS_KEY });
    } catch (err) {
      setFailure(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-3 text-sm">
        <StatusChip tone={PHASE_TONE[phase]} busy={running}>
          {t(`migration.phase.${phase}`)}
        </StatusChip>
        <span>{t("migration.progress", { moved, total })}</span>
        <span className="text-muted-foreground">
          {m.spec.source} → {m.spec.target}
        </span>
        {m.status.bridge && (
          <span className="text-muted-foreground">
            {t("migration.bridgeOn", {
              node:
                nodes.data?.find((n) => n.id === m.status.bridge?.node)?.name ??
                m.status.bridge.node,
            })}
          </span>
        )}
      </div>
      <div
        role="progressbar"
        aria-label={t("migration.progressLabel")}
        aria-valuemin={0}
        aria-valuemax={total}
        aria-valuenow={moved}
        className="h-2 w-full overflow-hidden rounded-full bg-muted"
      >
        <div
          className="h-full bg-success motion-safe:transition-all"
          style={{ width: `${total > 0 ? (moved / total) * 100 : 0}%` }}
        />
      </div>
      {m.status.error && (
        <p role="alert" className="text-sm text-destructive">
          {planText(t, m.status.error, nodeName)}
        </p>
      )}
      <div className="overflow-x-auto">
        <Table aria-label={t("migration.progressTable")}>
          <TableHeader>
            <TableRow>
              <TableHead>
                <span className="sr-only">{t("migration.pick")}</span>
              </TableHead>
              <TableHead>{t("migration.vmNic")}</TableHead>
              <TableHead>{t("migration.mode")}</TableHead>
              <TableHead>{t("migration.address")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {moves.map((v) => {
              const pickable = v.state === "moved" || v.state === "failed";
              return (
                <TableRow key={`${v.vm}-${v.nic}`}>
                  <TableCell>
                    {pickable && !done && (
                      <Checkbox
                        aria-label={t("migration.pickVm", { name: vmName(v.vm) })}
                        checked={chosen.has(v.vm)}
                        onCheckedChange={(on) => {
                          const next = new Set(chosen);
                          if (on === true) {
                            next.add(v.vm);
                          } else {
                            next.delete(v.vm);
                          }
                          setChosen(next);
                        }}
                      />
                    )}
                  </TableCell>
                  <TableCell>
                    <span className="flex flex-col">
                      <span className="font-medium">{vmName(v.vm)}</span>
                      <span className="text-xs text-muted-foreground">
                        {t("migration.nicN", { n: v.nic + 1 })} · {v.from.network}
                      </span>
                    </span>
                  </TableCell>
                  <TableCell>
                    <StatusChip tone={MODE_TONE[v.mode]}>
                      {t(`migration.planned.${v.mode}`)}
                    </StatusChip>
                  </TableCell>
                  <TableCell className="font-mono text-xs">
                    {(v.ips ?? []).join(", ") || "—"}
                  </TableCell>
                  <TableCell>
                    <span className="flex flex-col items-start gap-0.5">
                      <StatusChip
                        tone={STATE_TONE[v.state ?? "pending"]}
                        busy={v.state === "moving" || v.state === "rolling-back"}
                      >
                        {t(`migration.state.${v.state ?? "pending"}`)}
                      </StatusChip>
                      {v.error && (
                        <span className="text-xs text-destructive">
                          {planText(t, v.error, nodeName)}
                        </span>
                      )}
                      {v.at && (
                        <span className="text-xs text-muted-foreground">
                          {new Date(v.at * 1000).toLocaleTimeString(i18n.language)}
                        </span>
                      )}
                    </span>
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>
      {canResume && (bridgeGone || bridgeOffline) && (
        <Field
          id="mig-resume-node"
          label={t("migration.newBridgeNode")}
          hint={
            bridgeOffline
              ? t("migration.newBridgeNodeOffline", { node: bridgeHost.name })
              : t("migration.newBridgeNodeHint")
          }
        >
          {(a11y) => (
            <NativeSelect
              id="mig-resume-node"
              value={bridgeNode}
              onChange={(e) => setBridgeNode(e.target.value)}
              {...a11y}
            >
              <NativeSelectOption value="">{t("migration.bridgeNodeAuto")}</NativeSelectOption>
              {bridgeCandidates.map((n) => (
                <NativeSelectOption key={n.id} value={n.id}>
                  {n.name}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )}
        </Field>
      )}
      {failure && (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        {phase === "running" && (
          <Button
            variant="outline"
            disabled={busy}
            onClick={() =>
              void act("migration.paused", () =>
                call(
                  api.POST("/network-migrations/{key}/pause", { params: { path: { key: m.id } } }),
                ),
              )
            }
          >
            {t("migration.pause")}
          </Button>
        )}
        {canResume && (
          <Button
            disabled={busy}
            onClick={() =>
              void act(
                phase === "planned" ? "migration.startedNow" : "migration.resumed",
                () =>
                  call(
                    api.POST(
                      phase === "planned"
                        ? "/network-migrations/{key}/start"
                        : "/network-migrations/{key}/resume",
                      {
                        params: { path: { key: m.id } },
                        body: bridgeNode ? { bridge_node: bridgeNode } : {},
                      },
                    ),
                  ),
                phase === "planned" ? "start" : "resume",
              )
            }
          >
            {t(phase === "planned" ? "migration.startNow" : "migration.resume")}
          </Button>
        )}
        {!done && !running && rollbackable.length > 0 && (
          <Button variant="outline" disabled={busy} onClick={() => setRollingBack(true)}>
            {chosen.size > 0
              ? t("migration.rollbackChosen", { count: chosen.size })
              : t("migration.rollbackAll")}
          </Button>
        )}
        {!done && !running && (
          <Button variant="outline" disabled={busy} onClick={onFinish}>
            {t("migration.finish")}
          </Button>
        )}
        {!running && (
          <Button variant="ghost" disabled={busy} onClick={() => setDeleting(true)}>
            {t(done ? "migration.deleteRecord" : "migration.delete")}
          </Button>
        )}
        {done && (
          <Button variant="outline" onClick={onClose}>
            {t("migration.close")}
          </Button>
        )}
      </div>
      <ConfirmDialog
        open={rollingBack}
        onOpenChange={setRollingBack}
        title={t("migration.rollbackTitle", { name: m.name })}
        description={t("migration.rollbackDescription")}
        confirmLabel={t("migration.rollback")}
        onConfirm={async () => {
          const names = [...chosen].map(vmName);
          const task = await call(
            api.POST("/network-migrations/{key}/rollback", {
              params: { path: { key: m.id } },
              body: names.length > 0 ? { vms: [...chosen] } : {},
            }),
          );
          trackTask(
            task,
            t("tasks.op.network-migrate", {
              target: m.name,
              step: t("migration.taskStep.rollback"),
            }),
          );
          setChosen(new Set());
          await queryClient.invalidateQueries({ queryKey: MIGRATIONS_KEY });
        }}
      />
      <ConfirmDialog
        open={deleting}
        onOpenChange={setDeleting}
        title={t("migration.deleteTitle", { name: m.name })}
        description={t(done ? "migration.deleteRecordDescription" : "migration.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          await call(api.DELETE("/network-migrations/{key}", { params: { path: { key: m.id } } }));
          await queryClient.invalidateQueries({ queryKey: MIGRATIONS_KEY });
          toast.success(t("migration.deleted", { name: m.name }));
          onClose();
        }}
      />
    </div>
  );
}

/** ④ 끝내기: 다리를 걷고 라우터를 붙일지 고른다 */
function FinishStep({
  migration: m,
  onBack,
  onDone,
}: {
  migration: Migration;
  onBack(): void;
  onDone(): void;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const routers = useRouters();
  const [router, setRouter] = useState("");
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const left = (m.status.vms ?? []).filter(
    (v) => v.state !== "moved" && v.mode !== "blocked",
  ).length;
  const choices = (routers.data ?? []).filter((r) =>
    perms.has("network.config", objectTarget("router", r)),
  );

  async function finish() {
    setBusy(true);
    setFailure(null);
    try {
      const task = await call(
        api.POST("/network-migrations/{key}/finish", {
          params: { path: { key: m.id } },
          body: router ? { router } : {},
        }),
      );
      trackTask(
        task,
        t("tasks.op.network-migrate", { target: m.name, step: t("migration.taskStep.finish") }),
      );
      await queryClient.invalidateQueries({ queryKey: MIGRATIONS_KEY });
      onDone();
    } catch (err) {
      setFailure(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm">{t("migration.finishHint", { target: m.spec.target })}</p>
      {left > 0 && (
        <p role="status" className="text-sm text-warning-fg">
          {t("migration.finishLeft", { count: left })}
        </p>
      )}
      <Field id="mig-router" label={t("migration.router")} hint={t("migration.routerHint")}>
        {(a11y) => (
          <NativeSelect
            id="mig-router"
            className="w-full"
            value={router}
            onChange={(e) => setRouter(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">{t("migration.noRouter")}</NativeSelectOption>
            {choices.map((r) => (
              <NativeSelectOption key={r.id} value={r.name}>
                {r.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {failure && (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button variant="outline" disabled={busy} onClick={onBack}>
          {t("common.back")}
        </Button>
        <Button disabled={busy} onClick={() => void finish()}>
          {t("migration.finishNow")}
        </Button>
      </div>
    </div>
  );
}
