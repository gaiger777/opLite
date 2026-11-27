import { useQueryClient } from "@tanstack/react-query";
import { cn } from "cn";
import { RefreshCw } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { folderPath, folderTree } from "@/folders/folders";
import {
  fetchInventory,
  inventoryKey,
  type InvVm,
  type MigrationItem,
  type PreflightVm,
  refreshVmware,
  useInventory,
  useMigrations,
  useSources,
  vmwareErrorText,
} from "@/vmware/api";
import {
  checkWizardStep,
  createRequestOf,
  emptyWizard,
  proposeNetworks,
  rulesOf,
  usedDatastores,
  usedNetworks,
  WIZARD_STEPS,
  type WizardForm,
  type WizardStep,
} from "@/vmware/inventory";
import { levelCounts, PreflightTable } from "@/vmware/PreflightTable";
import { VmPicker } from "@/vmware/VmPicker";

/** 만든 초안(마법사가 미리 확인 단계에서 만들고, 뒤로 갔다 오면 바꾼다) */
interface Draft {
  id: string;
  name: string;
  /** 초안에 든 VM moref → 항목 이름 */
  items: Map<string, string>;
}

function Check({
  id,
  label,
  help,
  checked,
  onChange,
}: {
  id: string;
  label: string;
  help?: string;
  checked: boolean;
  onChange(on: boolean): void;
}) {
  return (
    <div className="flex items-start gap-2">
      <Checkbox
        id={id}
        checked={checked}
        onCheckedChange={(c) => onChange(c === true)}
        aria-describedby={help ? `${id}-hint` : undefined}
      />
      <div className="flex flex-col gap-0.5">
        <Label htmlFor={id}>{label}</Label>
        {help && (
          <p id={`${id}-hint`} className="text-xs text-muted-foreground">
            {help}
          </p>
        )}
      </div>
    </div>
  );
}

/**
 * 새 VMware 이전 계획(계획 72 §13): ① 원본·이름·방식 ② 인벤토리 나무 + 표에서 VM 고르기 ③ 매핑(고른 VM이 쓰는 네트워크·
 * 데이터스토어만, 폴더·노드·장치·드라이버 ISO) ④ 한도·동기화·전환 정책 ⑤ 미리 확인(초안을 만들어 VM마다 칩 — 막힌 VM은
 * 빼고 시작). 늦게 읽는 조각이다.
 */
export function MigrationWizard() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const sources = useSources(perms.reads("vmware-source"));
  const migrations = useMigrations(perms.reads("vmware-migration"));
  const networks = useList("network");
  const pools = useList("storage-pool");
  const nodes = useList("node");
  const folders = useList("folder");
  const volumes = useList("volume");
  const [step, setStep] = useState<WizardStep>("source");
  const [form, setForm] = useState<WizardForm>(() => emptyWizard());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [preflight, setPreflight] = useState<PreflightVm[] | null>(null);
  const inventory = useInventory(step === "source" ? undefined : form.source || undefined);
  const set = (patch: Partial<WizardForm>) => setForm((f) => ({ ...f, ...patch }));
  const vms: InvVm[] = useMemo(() => inventory.data?.inventory.vms ?? [], [inventory.data]);
  const chosen = vms.filter((vm) => form.selected.includes(vm.moref));
  const ourNetworks = (networks.data ?? []).map((n) => n.name).sort();
  const ourPools = (pools.data ?? []).map((p) => p.name).sort();
  const vmFolders = useMemo(() => {
    const tree = folderTree(folders.data, "vm");
    return tree.folders
      .filter((f) => perms.has("vm.create", { type: "object", kind: "vm", folder: f.id }))
      .map((f) => ({ id: f.id, path: folderPath(tree, f.id) }))
      .sort((a, b) => a.path.localeCompare(b.path));
  }, [folders.data, perms]);
  const isos = (volumes.data ?? [])
    .filter((v) => v.spec.content === "iso")
    .map((v) => v.name)
    .sort();
  const index = WIZARD_STEPS.indexOf(step);
  const taken = (migrations.data ?? []).map((m) => m.name).filter((n) => n !== draft?.name);

  function go(next: WizardStep) {
    setError(null);
    setErrors({});
    if (next === "mapping") {
      // 같은 이름의 우리 네트워크를 미리 고른다(서버 제안과 같다 — 사용자가 바꿀 수 있다).
      setForm((f) => ({
        ...f,
        networks: proposeNetworks(usedNetworks(chosen), ourNetworks, f.networks),
      }));
    }
    setStep(next);
    if (next === "check") {
      void check();
    }
  }

  function next() {
    const found = checkWizardStep(step, form, vms, taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    const following = WIZARD_STEPS[index + 1];
    if (following) {
      go(following);
    }
  }

  /** 초안을 만들거나(처음) 지금 폼에 맞춘 뒤(뒤로 갔다 왔으면) 미리 확인한다. */
  async function check(refresh = false) {
    setBusy(true);
    setError(null);
    setPreflight(null);
    try {
      let current = draft;
      if (!current) {
        const view = await call(
          api.POST("/vmware-migrations", { body: createRequestOf(form, vms) }),
        );
        current = draftOf(view.migration.id, view.migration.name, view.vms);
      } else {
        const add = form.selected.filter((ref) => !current!.items.has(ref));
        const remove = [...current.items.keys()].filter((ref) => !form.selected.includes(ref));
        const body = createRequestOf(form, vms);
        await call(
          api.PATCH("/vmware-migrations/{key}", {
            params: { path: { key: current.id } },
            body: {
              rules: rulesOf(form, vms),
              limits: body.limits,
              sync: body.sync,
              cutover: body.cutover,
              ...(add.length ? { add_vms: add.map((ref) => ({ ref })) } : {}),
              ...(remove.length ? { remove_vms: remove } : {}),
            },
          }),
        );
        const items = await call(
          api.GET("/vmware-vms", { params: { query: { migration: current.id } } }),
        );
        current = draftOf(current.id, current.name, items.items);
      }
      setDraft(current);
      const answer = await call(
        api.POST("/vmware-migrations/{key}/preflight", {
          params: { path: { key: current.id } },
          body: refresh ? { refresh: true } : {},
        }),
      );
      setPreflight(answer.vms);
      await refreshVmware(queryClient);
    } catch (err) {
      setError(vmwareErrorText(t, err));
    } finally {
      setBusy(false);
    }
  }

  /** 막힌 VM을 계획에서 빼고 시작한다(설계 §13 ⑤ — 뺀 VM은 다른 계획에 넣을 수 있다). */
  async function start() {
    if (!draft || !preflight) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const blocked = preflight.filter((v) => v.level === "blocked").map((v) => v.item);
      if (blocked.length > 0) {
        await call(
          api.PATCH("/vmware-migrations/{key}", {
            params: { path: { key: draft.id } },
            body: { remove_vms: blocked },
          }),
        );
        // 뺀 VM은 초안·확인 결과·고른 것에서도 뺀다 — 시작이 실패해 다시 눌러도 이미 뺀 것을 또 빼지 않고, 다시 확인해도
        // 도로 넣지 않는다.
        const gone = [...draft.items]
          .filter(([, item]) => blocked.includes(item))
          .map(([ref]) => ref);
        setDraft({
          ...draft,
          items: new Map([...draft.items].filter(([ref]) => !gone.includes(ref))),
        });
        setPreflight(preflight.filter((v) => v.level !== "blocked"));
        setForm((f) => ({ ...f, selected: f.selected.filter((ref) => !gone.includes(ref)) }));
      }
      await call(
        api.POST("/vmware-migrations/{key}/actions/start", {
          params: { path: { key: draft.id } },
        }),
      );
      await refreshVmware(queryClient);
      toast.success(t("vmware.wizard.started", { name: draft.name }));
      void navigate(`/vmware-migrations/${encodeURIComponent(draft.name)}`);
    } catch (err) {
      setError(vmwareErrorText(t, err));
    } finally {
      setBusy(false);
    }
  }

  async function reload() {
    try {
      const fresh = await fetchInventory(form.source, true);
      queryClient.setQueryData(inventoryKey(form.source), fresh);
      // 원본에서 사라진 VM은 표에 없어 풀 수 없다 — 고른 것에서 빼고 알린다.
      const there = new Set(fresh.inventory.vms.map((vm) => vm.moref));
      const gone = form.selected.filter((ref) => !there.has(ref));
      if (gone.length > 0) {
        set({ selected: form.selected.filter((ref) => there.has(ref)) });
        toast.info(t("vmware.wizard.vanished", { count: gone.length }));
      }
    } catch (err) {
      toast.error(vmwareErrorText(t, err));
    }
  }

  const counts = preflight ? levelCounts(preflight) : null;
  const startable = counts ? counts.ok + counts.warn : 0;

  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t("vmware.wizard.title")} description={t("vmware.wizard.hint")} />
      <ol aria-label={t("vmware.wizard.steps")} className="flex flex-wrap gap-2 text-sm">
        {WIZARD_STEPS.map((s, i) => (
          <li
            key={s}
            aria-current={s === step ? "step" : undefined}
            className={cn(
              "rounded-md border px-2 py-1",
              s === step ? "border-brand font-medium" : "text-muted-foreground",
            )}
          >
            {i + 1}. {t(`vmware.wizard.step.${s}`)}
          </li>
        ))}
      </ol>
      <section aria-labelledby="vmware-wizard-step" className="flex min-w-0 flex-col gap-4">
        <h2 id="vmware-wizard-step" className="text-lg font-medium">
          {t(`vmware.wizard.step.${step}`)}
        </h2>
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}

        {step === "source" && (
          <div className="flex max-w-xl flex-col gap-4">
            <Field id="vmware-wizard-name" label={t("common.name")} error={errors.name}>
              {(a11y) => (
                <Input
                  id="vmware-wizard-name"
                  value={form.name}
                  disabled={draft !== null}
                  onChange={(e) => set({ name: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id="vmware-wizard-source"
              label={t("vmware.migration.source")}
              error={errors.source}
            >
              {(a11y) => (
                <NativeSelect
                  id="vmware-wizard-source"
                  className="w-full"
                  value={form.source}
                  disabled={draft !== null}
                  onChange={(e) => set({ source: e.target.value, selected: [] })}
                  {...a11y}
                >
                  <NativeSelectOption value="">{t("vmware.wizard.pickSource")}</NativeSelectOption>
                  {(sources.data ?? []).map((s) => (
                    <NativeSelectOption key={s.id} value={s.id}>
                      {s.name} ({s.spec.host})
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              )}
            </Field>
            {sources.isSuccess && sources.data.length === 0 && (
              <p className="text-sm text-muted-foreground">
                {t("vmware.wizard.noSources")}{" "}
                <Link to="/vmware-sources" className="underline underline-offset-4">
                  {t("nav.vmwareSources")}
                </Link>
              </p>
            )}
            <fieldset className="flex flex-col gap-2">
              <legend className="mb-1 text-sm font-medium">{t("vmware.migration.mode")}</legend>
              {(["warm", "cold"] as const).map((mode) => (
                <label key={mode} className="flex items-start gap-2 text-sm">
                  <input
                    type="radio"
                    name="vmware-wizard-mode"
                    className="mt-1"
                    checked={form.mode === mode}
                    disabled={draft !== null}
                    onChange={() => set({ mode })}
                  />
                  <span>
                    <span className="font-medium">{t(`vmware.mode.${mode}`)}</span>
                    <span className="block text-xs text-muted-foreground">
                      {t(`vmware.mode.${mode}Hint`)}
                    </span>
                  </span>
                </label>
              ))}
            </fieldset>
            {draft && (
              <p className="text-xs text-muted-foreground">
                {t("vmware.wizard.draftLocked", { name: draft.name })}
              </p>
            )}
          </div>
        )}

        {step === "vms" && (
          <div className="flex min-w-0 flex-col gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={inventory.isFetching}
                onClick={() => void reload()}
              >
                <RefreshCw />
                {t("vmware.wizard.reload")}
              </Button>
              {inventory.data && (
                <span className="text-xs text-muted-foreground">
                  {t("vmware.wizard.fetched", {
                    time: new Date(inventory.data.fetched_at * 1000).toLocaleTimeString(),
                  })}
                </span>
              )}
            </div>
            {inventory.isError ? (
              <p role="alert" className="text-sm text-destructive">
                {vmwareErrorText(t, inventory.error)}
              </p>
            ) : !inventory.data ? (
              <p role="status" className="text-sm text-muted-foreground">
                {t("vmware.wizard.loadingInventory")}
              </p>
            ) : (
              <VmPicker
                inventory={inventory.data}
                draft={draft?.name}
                selected={form.selected}
                onChange={(selected) => set({ selected })}
              />
            )}
            {errors.vms && (
              <p role="alert" className="text-sm text-destructive">
                {t(errors.vms)}
              </p>
            )}
          </div>
        )}

        {step === "mapping" && (
          <div className="flex max-w-3xl flex-col gap-6">
            <div className="flex flex-col gap-2">
              <h3 className="font-medium">{t("vmware.mapping.networks")}</h3>
              <p className="text-xs text-muted-foreground">{t("vmware.mapping.networksHint")}</p>
              <div className="overflow-x-auto">
                <Table aria-label={t("vmware.mapping.networks")}>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t("vmware.mapping.sourceNetwork")}</TableHead>
                      <TableHead>{t("vmware.mapping.ourNetwork")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {usedNetworks(chosen).map((from, i) => (
                      <TableRow key={from}>
                        <TableCell className="font-mono text-xs">
                          <label htmlFor={`vmware-net-${i}`}>{from}</label>
                        </TableCell>
                        <TableCell>
                          <NativeSelect
                            id={`vmware-net-${i}`}
                            value={form.networks[from] ?? ""}
                            onChange={(e) =>
                              set({ networks: { ...form.networks, [from]: e.target.value } })
                            }
                          >
                            <NativeSelectOption value="">
                              {t("vmware.mapping.unmapped")}
                            </NativeSelectOption>
                            {ourNetworks.map((n) => (
                              <NativeSelectOption key={n} value={n}>
                                {n}
                              </NativeSelectOption>
                            ))}
                          </NativeSelect>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            </div>
            <div className="flex flex-col gap-2">
              <h3 className="font-medium">{t("vmware.mapping.datastores")}</h3>
              <div className="overflow-x-auto">
                <Table aria-label={t("vmware.mapping.datastores")}>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t("vmware.mapping.datastore")}</TableHead>
                      <TableHead>{t("vmware.mapping.pool")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {usedDatastores(chosen).map((ds, i) => (
                      <TableRow key={ds}>
                        <TableCell className="font-mono text-xs">
                          <label htmlFor={`vmware-ds-${i}`}>{ds}</label>
                        </TableCell>
                        <TableCell>
                          <NativeSelect
                            id={`vmware-ds-${i}`}
                            value={form.datastores[ds] ?? ""}
                            aria-invalid={errors[`ds:${ds}`] ? true : undefined}
                            aria-describedby={
                              errors[`ds:${ds}`] ? `vmware-ds-${i}-error` : undefined
                            }
                            onChange={(e) =>
                              set({ datastores: { ...form.datastores, [ds]: e.target.value } })
                            }
                          >
                            <NativeSelectOption value="">
                              {t("vmware.mapping.useDefault")}
                            </NativeSelectOption>
                            {ourPools.map((p) => (
                              <NativeSelectOption key={p} value={p}>
                                {p}
                              </NativeSelectOption>
                            ))}
                          </NativeSelect>
                          {errors[`ds:${ds}`] && (
                            <p
                              id={`vmware-ds-${i}-error`}
                              className="mt-1 text-xs text-destructive"
                            >
                              {t(errors[`ds:${ds}`]!)}
                            </p>
                          )}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
              <Field id="vmware-default-pool" label={t("vmware.mapping.defaultPool")}>
                <NativeSelect
                  id="vmware-default-pool"
                  className="w-full"
                  value={form.defaultPool}
                  onChange={(e) => set({ defaultPool: e.target.value })}
                >
                  <NativeSelectOption value="">{t("vmware.mapping.none")}</NativeSelectOption>
                  {ourPools.map((p) => (
                    <NativeSelectOption key={p} value={p}>
                      {p}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </Field>
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field id="vmware-folder-kind" label={t("vmware.mapping.folders")}>
                <NativeSelect
                  id="vmware-folder-kind"
                  className="w-full"
                  value={form.folderKind}
                  onChange={(e) => set({ folderKind: e.target.value as WizardForm["folderKind"] })}
                >
                  <NativeSelectOption value="flat">{t("vmware.mapping.flat")}</NativeSelectOption>
                  <NativeSelectOption value="mirror">
                    {t("vmware.mapping.mirror")}
                  </NativeSelectOption>
                </NativeSelect>
              </Field>
              <Field
                id="vmware-folder"
                label={
                  form.folderKind === "flat" ? t("vmware.mapping.into") : t("vmware.mapping.under")
                }
              >
                <NativeSelect
                  id="vmware-folder"
                  className="w-full"
                  value={form.folder}
                  onChange={(e) => set({ folder: e.target.value })}
                >
                  <NativeSelectOption value="">{t("folders.root")}</NativeSelectOption>
                  {vmFolders.map((f) => (
                    <NativeSelectOption key={f.id} value={f.id}>
                      {f.path}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </Field>
              <Field id="vmware-node" label={t("common.node")}>
                <NativeSelect
                  id="vmware-node"
                  className="w-full"
                  value={form.node}
                  onChange={(e) => set({ node: e.target.value })}
                >
                  <NativeSelectOption value="">{t("vmware.mapping.autoNode")}</NativeSelectOption>
                  {(nodes.data ?? []).map((n) => (
                    <NativeSelectOption key={n.id} value={n.id}>
                      {n.name}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </Field>
              <Field id="vmware-driver-iso" label={t("vmware.mapping.driverIso")}>
                <NativeSelect
                  id="vmware-driver-iso"
                  className="w-full"
                  value={form.driverIso}
                  onChange={(e) => set({ driverIso: e.target.value })}
                >
                  <NativeSelectOption value="">{t("vmware.mapping.none")}</NativeSelectOption>
                  {isos.map((iso) => (
                    <NativeSelectOption key={iso} value={iso}>
                      {iso}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </Field>
              <Field id="vmware-devices" label={t("vmware.mapping.devices")}>
                <NativeSelect
                  id="vmware-devices"
                  className="w-full"
                  value={form.devices}
                  onChange={(e) => set({ devices: e.target.value as WizardForm["devices"] })}
                >
                  <NativeSelectOption value="auto">
                    {t("vmware.mapping.devicesAuto")}
                  </NativeSelectOption>
                  <NativeSelectOption value="fixed">
                    {t("vmware.mapping.devicesFixed")}
                  </NativeSelectOption>
                </NativeSelect>
              </Field>
              {form.devices === "fixed" && (
                <div className="grid grid-cols-2 gap-2">
                  <Field id="vmware-disk-bus" label={t("vmware.mapping.diskBus")}>
                    <NativeSelect
                      id="vmware-disk-bus"
                      className="w-full"
                      value={form.diskBus}
                      onChange={(e) => set({ diskBus: e.target.value as WizardForm["diskBus"] })}
                    >
                      <NativeSelectOption value="virtio-blk">virtio-blk</NativeSelectOption>
                      <NativeSelectOption value="sata">SATA</NativeSelectOption>
                    </NativeSelect>
                  </Field>
                  <Field id="vmware-nic-model" label={t("vmware.mapping.nicModel")}>
                    <NativeSelect
                      id="vmware-nic-model"
                      className="w-full"
                      value={form.nicModel}
                      onChange={(e) => set({ nicModel: e.target.value as WizardForm["nicModel"] })}
                    >
                      <NativeSelectOption value="virtio">virtio</NativeSelectOption>
                      <NativeSelectOption value="e1000e">e1000e</NativeSelectOption>
                    </NativeSelect>
                  </Field>
                </div>
              )}
            </div>
            <div className="flex flex-col gap-3">
              <Check
                id="vmware-keep-macs"
                label={t("vmware.mapping.keepMacs")}
                help={t("vmware.mapping.keepMacsHint")}
                checked={form.keepMacs}
                onChange={(keepMacs) => set({ keepMacs })}
              />
              <Check
                id="vmware-reserve-ips"
                label={t("vmware.mapping.reserveIps")}
                help={t("vmware.mapping.reserveIpsHint")}
                checked={form.reserveIps}
                onChange={(reserveIps) => set({ reserveIps })}
              />
              <Check
                id="vmware-start-after"
                label={t("vmware.mapping.startAfter")}
                checked={form.startAfter}
                onChange={(startAfter) => set({ startAfter })}
              />
            </div>
          </div>
        )}

        {step === "policy" && (
          <div className="flex max-w-3xl flex-col gap-6">
            <div className="grid gap-4 sm:grid-cols-2">
              {(
                [
                  ["maxParallel", "vmware.policy.maxParallel"],
                  ["maxPerHost", "vmware.policy.maxPerHost"],
                  ["maxPerNode", "vmware.policy.maxPerNode"],
                  ["bandwidth", "vmware.policy.bandwidth"],
                  ...(form.mode === "warm"
                    ? ([
                        ["intervalMin", "vmware.policy.interval"],
                        ["stopAfterDays", "vmware.policy.stopAfter"],
                      ] as const)
                    : []),
                  ["shutdownTimeout", "vmware.policy.shutdownTimeout"],
                  ["maxFinalDelta", "vmware.policy.maxFinalDelta"],
                ] as const
              ).map(([key, label]) => (
                <Field
                  key={key}
                  id={`vmware-${key}`}
                  label={t(label)}
                  error={errors[key]}
                  help={t(`${label}Hint`)}
                >
                  {(a11y) => (
                    <Input
                      id={`vmware-${key}`}
                      inputMode="numeric"
                      value={form[key]}
                      onChange={(e) => set({ [key]: e.target.value })}
                      {...a11y}
                    />
                  )}
                </Field>
              ))}
              <Field id="vmware-shutdown" label={t("vmware.policy.shutdown")}>
                <NativeSelect
                  id="vmware-shutdown"
                  className="w-full"
                  value={form.shutdown}
                  onChange={(e) => set({ shutdown: e.target.value as WizardForm["shutdown"] })}
                >
                  <NativeSelectOption value="guest">
                    {t("vmware.shutdown.guest")}
                  </NativeSelectOption>
                  <NativeSelectOption value="power-off">
                    {t("vmware.shutdown.power-off")}
                  </NativeSelectOption>
                </NativeSelect>
              </Field>
              <Field
                id="vmware-cutover-at"
                label={t("vmware.policy.cutoverAt")}
                error={errors.cutoverAt}
                help={t("vmware.policy.cutoverAtHint")}
              >
                {(a11y) => (
                  <Input
                    id="vmware-cutover-at"
                    type="datetime-local"
                    value={form.cutoverAt}
                    onChange={(e) => set({ cutoverAt: e.target.value })}
                    {...a11y}
                  />
                )}
              </Field>
            </div>
            <Check
              id="vmware-protect-source"
              label={t("vmware.policy.protectSource")}
              help={t("vmware.policy.protectSourceHint")}
              checked={form.protectSource}
              onChange={(protectSource) => set({ protectSource })}
            />
          </div>
        )}

        {step === "check" && (
          <div className="flex min-w-0 flex-col gap-3">
            {busy && !preflight && (
              <p role="status" className="text-sm text-muted-foreground">
                {t("vmware.preflight.running")}
              </p>
            )}
            {preflight && <PreflightTable vms={preflight} />}
            {counts && counts.blocked > 0 && (
              <p className="text-sm text-muted-foreground">
                {t("vmware.wizard.blockedLeftOut", { count: counts.blocked })}
              </p>
            )}
          </div>
        )}
      </section>

      <div className="flex flex-wrap gap-2 border-t pt-4">
        {index > 0 && (
          <Button
            type="button"
            variant="outline"
            disabled={busy}
            onClick={() => go(WIZARD_STEPS[index - 1]!)}
          >
            {t("vmware.back")}
          </Button>
        )}
        {step !== "check" ? (
          <Button type="button" onClick={next}>
            {t("vmware.next")}
          </Button>
        ) : (
          <>
            <Button type="button" disabled={busy || startable === 0} onClick={() => void start()}>
              {t("vmware.wizard.start", { count: startable })}
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() => void check(true)}
            >
              <RefreshCw />
              {t("vmware.wizard.recheck")}
            </Button>
            {draft && (
              <Button type="button" variant="ghost" asChild>
                <Link to={`/vmware-migrations/${encodeURIComponent(draft.name)}`}>
                  {t("vmware.wizard.keepDraft")}
                </Link>
              </Button>
            )}
          </>
        )}
      </div>
    </div>
  );
}

function draftOf(id: string, name: string, items: readonly MigrationItem[]): Draft {
  return { id, name, items: new Map(items.map((i) => [i.spec.source_ref, i.name])) };
}
