import { cn } from "cn";
import { ArrowDown, ArrowUp, CircleHelp, Plus, Trash2, X } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  type Draft,
  type GroupDraft,
  type IpDraft,
  moveGroup,
  newGroup,
  newIp,
  newVm,
  ON_TIMEOUT,
  type Pair,
  testNetworkIssues,
  type VmDraft,
  WAITS,
} from "@/dr/model";
import { VmPickerDialog } from "@/dr/VmPickerDialog";
import type { RemoteView } from "@/remotes/remotes";
import { useRouted } from "@/routing/routing";

type Errors = Record<string, string>;

/**
 * 복구 계획 편집(계획 76 §4): 기본(원천·풀·폴더·RPO·`on_timeout`), 부팅 그룹(차례는 위·아래 단추 — 끌어 놓기 없음, 기다림·
 * 시간 초과·지연, VM 고르기와 VM별 풀·노드·IP 규칙), 네트워크·보안 그룹·시험 네트워크 맵. `readOnly`면 고칠 수 없다(viewer).
 */
export function PlanEditor({
  draft,
  onChange,
  isNew,
  readOnly,
  errors,
  remotes,
}: {
  draft: Draft;
  onChange(draft: Draft): void;
  isNew: boolean;
  readOnly: boolean;
  errors: Errors;
  remotes: readonly RemoteView[];
}) {
  const { t } = useTranslation();
  const id = useId();
  const pools = useList("storage-pool");
  const networks = useList("network");
  const groups = useList("security-group");
  const routed = useRouted();
  const [picking, setPicking] = useState<number | null>(null);
  const set = (patch: Partial<Draft>) => onChange({ ...draft, ...patch });
  const setGroup = (i: number, patch: Partial<GroupDraft>) =>
    set({ groups: draft.groups.map((g, j) => (j === i ? { ...g, ...patch } : g)) });
  const source = remotes.find((r) => r.name === draft.source);
  const poolNames = (pools.data ?? []).map((p) => p.name).sort();
  const networkNames = (networks.data ?? []).map((n) => n.name).sort();
  const groupNames = (groups.data ?? []).map((g) => g.name).sort();
  const rowErrors = (at: string) =>
    Object.fromEntries(
      Object.entries(errors).flatMap(([k, v]) => {
        const m = new RegExp(`^${at}(\\d+)$`).exec(k);
        return m ? [[Number(m[1]), t(v)]] : [];
      }),
    );
  const isolation = Object.fromEntries(
    Object.entries(
      testNetworkIssues(draft, networks.data, new Set((routed.data ?? []).map((r) => r.network))),
    ).map(([i, issue]) => [Number(i), t(`dr.edit.notIsolated.${issue}`)]),
  );

  return (
    <fieldset disabled={readOnly} className="flex min-w-0 flex-col gap-6">
      <section aria-labelledby={`${id}-basics`} className="flex flex-col gap-3">
        <h2 id={`${id}-basics`} className="text-base font-semibold">
          {t("dr.edit.basics")}
        </h2>
        <div className="grid gap-3 sm:grid-cols-2">
          {isNew && (
            <Field id={`${id}-name`} label={t("common.name")} error={errors.name}>
              {(a11y) => (
                <Input
                  id={`${id}-name`}
                  value={draft.name}
                  autoComplete="off"
                  onChange={(e) => set({ name: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
          )}
          <Field
            id={`${id}-source`}
            label={t("dr.plan.source")}
            error={errors.source}
            help={t("dr.edit.sourceHint")}
          >
            {(a11y) => (
              <NativeSelect
                id={`${id}-source`}
                className="w-full"
                value={draft.source}
                onChange={(e) => set({ source: e.target.value })}
                {...a11y}
              >
                <NativeSelectOption value="">{t("dr.edit.choose")}</NativeSelectOption>
                {[...new Set([draft.source, ...remotes.map((r) => r.name)])]
                  .filter(Boolean)
                  .map((name) => (
                    <NativeSelectOption key={name} value={name}>
                      {name}
                    </NativeSelectOption>
                  ))}
              </NativeSelect>
            )}
          </Field>
          <Field id={`${id}-pool`} label={t("dr.edit.pool")} error={errors.pool}>
            {(a11y) => (
              <NativeSelect
                id={`${id}-pool`}
                className="w-full"
                value={draft.pool}
                onChange={(e) => set({ pool: e.target.value })}
                {...a11y}
              >
                <NativeSelectOption value="">{t("dr.edit.choose")}</NativeSelectOption>
                {[...new Set([draft.pool, ...poolNames])].filter(Boolean).map((name) => (
                  <NativeSelectOption key={name} value={name}>
                    {name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          <Field id={`${id}-folder`} label={t("dr.edit.folder")} help={t("dr.edit.folderHint")}>
            {(a11y) => (
              <Input
                id={`${id}-folder`}
                value={draft.folder}
                autoComplete="off"
                onChange={(e) => set({ folder: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id={`${id}-sbpool`}
            label={t("dr.edit.sourceBackupPool")}
            error={errors.sourceBackupPool}
            help={t("dr.edit.sourceBackupPoolHint")}
          >
            {(a11y) => (
              <Input
                id={`${id}-sbpool`}
                value={draft.sourceBackupPool}
                autoComplete="off"
                onChange={(e) => set({ sourceBackupPool: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id={`${id}-rpo`}
            label={t("dr.edit.rpo")}
            error={errors.rpo}
            help={t("dr.edit.rpoHint")}
          >
            {(a11y) => (
              <Input
                id={`${id}-rpo`}
                inputMode="decimal"
                value={draft.rpoHours}
                onChange={(e) => set({ rpoHours: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field id={`${id}-timeout`} label={t("dr.edit.onTimeout")}>
            <NativeSelect
              id={`${id}-timeout`}
              className="w-full"
              value={draft.onTimeout}
              onChange={(e) => set({ onTimeout: e.target.value as Draft["onTimeout"] })}
            >
              {ON_TIMEOUT.map((v) => (
                <NativeSelectOption key={v} value={v}>
                  {t(`dr.onTimeout.${v}`)}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          </Field>
        </div>
      </section>

      <section aria-labelledby={`${id}-groups`} className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 id={`${id}-groups`} className="text-base font-semibold">
            {t("dr.edit.groups")}
          </h2>
          {!readOnly && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                set({ groups: [...draft.groups, newGroup(`group-${draft.groups.length + 1}`)] })
              }
            >
              <Plus />
              {t("dr.edit.addGroup")}
            </Button>
          )}
        </div>
        {(errors.groups || errors.vms) && (
          <p role="alert" className="text-sm text-destructive">
            {t(errors.groups ?? errors.vms!)}
          </p>
        )}
        <ol className="flex flex-col gap-3">
          {draft.groups.map((g, i) => (
            <GroupCard
              key={g.key}
              group={g}
              index={i}
              count={draft.groups.length}
              readOnly={readOnly}
              errors={errors}
              pools={poolNames}
              onChange={(patch) => setGroup(i, patch)}
              onMove={(by) => set({ groups: moveGroup(draft.groups, i, by) })}
              onRemove={() => set({ groups: draft.groups.filter((_, j) => j !== i) })}
              onPick={() => setPicking(i)}
            />
          ))}
        </ol>
      </section>

      <MapTable
        title={t("dr.edit.networks")}
        help={t("dr.edit.networksHint")}
        pairs={draft.networks}
        targets={networkNames}
        errors={rowErrors("net")}
        readOnly={readOnly}
        onChange={(networks) => set({ networks })}
      />
      <MapTable
        title={t("dr.edit.securityGroups")}
        help={t("dr.edit.securityGroupsHint")}
        pairs={draft.securityGroups}
        targets={groupNames}
        errors={rowErrors("sg")}
        readOnly={readOnly}
        onChange={(securityGroups) => set({ securityGroups })}
      />
      <MapTable
        title={t("dr.edit.testNetworks")}
        help={t("dr.edit.testNetworksHint")}
        pairs={draft.testNetworks}
        targets={networkNames}
        errors={rowErrors("tnet")}
        warnings={isolation}
        readOnly={readOnly}
        onChange={(testNetworks) => set({ testNetworks })}
      />

      {picking !== null && (
        <VmPickerDialog
          source={source}
          taken={new Set(draft.groups.flatMap((g) => g.vms.map((v) => v.vm)))}
          group={draft.groups[picking]?.name ?? ""}
          onClose={() => setPicking(null)}
          onPick={(names) => {
            const g = draft.groups[picking];
            if (g) {
              setGroup(picking, { vms: [...g.vms, ...names.map(newVm)] });
            }
            setPicking(null);
          }}
        />
      )}
    </fieldset>
  );
}

function GroupCard({
  group,
  index,
  count,
  readOnly,
  errors,
  pools,
  onChange,
  onMove,
  onRemove,
  onPick,
}: {
  group: GroupDraft;
  index: number;
  count: number;
  readOnly: boolean;
  errors: Errors;
  pools: string[];
  onChange(patch: Partial<GroupDraft>): void;
  onMove(by: -1 | 1): void;
  onRemove(): void;
  onPick(): void;
}) {
  const { t } = useTranslation();
  const id = useId();
  const at = `g${index}`;
  const label = group.name || t("dr.edit.groupN", { n: index + 1 });
  const setVm = (j: number, patch: Partial<VmDraft>) =>
    onChange({ vms: group.vms.map((v, k) => (k === j ? { ...v, ...patch } : v)) });
  return (
    <li
      aria-label={t("dr.edit.groupLabel", { n: index + 1, name: label })}
      className="rounded-lg border p-3"
    >
      <div className="flex flex-wrap items-start justify-between gap-2">
        <span className="text-sm font-medium text-muted-foreground">
          {t("dr.edit.groupN", { n: index + 1 })}
        </span>
        {!readOnly && (
          <div className="flex gap-1">
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={t("dr.edit.moveUp", { name: label })}
              disabled={index === 0}
              onClick={() => onMove(-1)}
            >
              <ArrowUp />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={t("dr.edit.moveDown", { name: label })}
              disabled={index === count - 1}
              onClick={() => onMove(1)}
            >
              <ArrowDown />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={t("dr.edit.removeGroup", { name: label })}
              disabled={count === 1}
              onClick={onRemove}
            >
              <Trash2 />
            </Button>
          </div>
        )}
      </div>
      <div className="mt-2 grid gap-3 sm:grid-cols-4">
        <Field id={`${id}-name`} label={t("dr.edit.groupName")} error={errors[`${at}.name`]}>
          {(a11y) => (
            <Input
              id={`${id}-name`}
              value={group.name}
              autoComplete="off"
              onChange={(e) => onChange({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        <Field id={`${id}-wait`} label={t("dr.edit.wait")}>
          <NativeSelect
            id={`${id}-wait`}
            className="w-full"
            value={group.wait}
            onChange={(e) => onChange({ wait: e.target.value as GroupDraft["wait"] })}
          >
            {WAITS.map((w) => (
              <NativeSelectOption key={w} value={w}>
                {t(`dr.wait.${w}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
        <Field id={`${id}-timeout`} label={t("dr.edit.timeout")} error={errors[`${at}.timeout`]}>
          {(a11y) => (
            <Input
              id={`${id}-timeout`}
              inputMode="numeric"
              value={group.timeout}
              onChange={(e) => onChange({ timeout: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        <Field id={`${id}-delay`} label={t("dr.edit.delay")} error={errors[`${at}.delay`]}>
          {(a11y) => (
            <Input
              id={`${id}-delay`}
              inputMode="numeric"
              value={group.delay}
              onChange={(e) => onChange({ delay: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      </div>
      <div className="mt-3 flex flex-col gap-2">
        {group.vms.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("dr.edit.noVms")}</p>
        ) : (
          <ul aria-label={t("dr.edit.vmsOf", { name: label })} className="flex flex-col gap-2">
            {group.vms.map((v, j) => (
              <VmRow
                key={v.vm}
                vm={v}
                at={`${at}.v${j}`}
                errors={errors}
                pools={pools}
                readOnly={readOnly}
                onChange={(patch) => setVm(j, patch)}
                onRemove={() => onChange({ vms: group.vms.filter((_, k) => k !== j) })}
              />
            ))}
          </ul>
        )}
        {errors[`${at}.vms`] && (
          <p className="text-xs text-destructive">{t(errors[`${at}.vms`]!)}</p>
        )}
        {!readOnly && (
          <Button type="button" variant="outline" size="sm" className="w-fit" onClick={onPick}>
            <Plus />
            {t("dr.edit.addVms")}
          </Button>
        )}
      </div>
    </li>
  );
}

function VmRow({
  vm,
  at,
  errors,
  pools,
  readOnly,
  onChange,
  onRemove,
}: {
  vm: VmDraft;
  at: string;
  errors: Errors;
  pools: string[];
  readOnly: boolean;
  onChange(patch: Partial<VmDraft>): void;
  onRemove(): void;
}) {
  const { t } = useTranslation();
  const id = useId();
  const setIp = (k: number, patch: Partial<IpDraft>) =>
    onChange({ ip: vm.ip.map((r, l) => (l === k ? { ...r, ...patch } : r)) });
  return (
    <li className="flex flex-col gap-2 rounded-md bg-surface-1 p-2">
      <div className="flex flex-wrap items-end gap-2">
        <span className="min-w-24 self-center font-medium">{vm.vm}</span>
        <div className="flex flex-col gap-1">
          <Label htmlFor={`${id}-pool`} className="text-xs">
            {t("dr.edit.vmPool")}
          </Label>
          <NativeSelect
            id={`${id}-pool`}
            size="sm"
            value={vm.pool}
            onChange={(e) => onChange({ pool: e.target.value })}
          >
            <NativeSelectOption value="">{t("dr.edit.planPool")}</NativeSelectOption>
            {[...new Set([vm.pool, ...pools])].filter(Boolean).map((p) => (
              <NativeSelectOption key={p} value={p}>
                {p}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor={`${id}-node`} className="text-xs">
            {t("common.node")}
          </Label>
          <Input
            id={`${id}-node`}
            className="h-7 w-32"
            placeholder={t("dr.edit.anyNode")}
            value={vm.node}
            onChange={(e) => onChange({ node: e.target.value })}
          />
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor={`${id}-sbpool`} className="text-xs">
            {t("dr.edit.vmSourceBackupPool")}
          </Label>
          <Input
            id={`${id}-sbpool`}
            className="h-7 w-32"
            placeholder={t("dr.edit.planSourceBackupPool")}
            value={vm.sourceBackupPool}
            aria-invalid={errors[`${at}.sourceBackupPool`] ? true : undefined}
            onChange={(e) => onChange({ sourceBackupPool: e.target.value })}
          />
        </div>
        <div className="flex items-center gap-2 self-center">
          <Checkbox
            id={`${id}-live`}
            checked={vm.live}
            onCheckedChange={(v) => onChange({ live: v === true })}
          />
          <Label htmlFor={`${id}-live`} className="text-xs">
            {t("dr.edit.live")}
          </Label>
        </div>
        {!readOnly && (
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            className="ml-auto"
            aria-label={t("dr.edit.removeVm", { name: vm.vm })}
            onClick={onRemove}
          >
            <X />
          </Button>
        )}
      </div>
      {vm.ip.length > 0 && (
        <ul aria-label={t("dr.edit.ipRulesOf", { name: vm.vm })} className="flex flex-col gap-1">
          {vm.ip.map((r, k) => (
            <li key={k} className="flex flex-wrap items-center gap-2 text-sm">
              <Label htmlFor={`${id}-nic${k}`} className="text-xs">
                {t("dr.edit.nic")}
              </Label>
              <Input
                id={`${id}-nic${k}`}
                className="h-7 w-14"
                inputMode="numeric"
                value={r.nic}
                onChange={(e) => setIp(k, { nic: e.target.value })}
              />
              <span className="flex items-center gap-1">
                <Checkbox
                  id={`${id}-dhcp${k}`}
                  checked={r.dhcp}
                  onCheckedChange={(v) => setIp(k, { dhcp: v === true })}
                />
                <Label htmlFor={`${id}-dhcp${k}`} className="text-xs">
                  DHCP
                </Label>
              </span>
              {!r.dhcp && (
                <>
                  <Input
                    aria-label={t("dr.edit.address")}
                    className="h-7 w-36"
                    placeholder="10.20.0.5/24"
                    value={r.address}
                    onChange={(e) => setIp(k, { address: e.target.value })}
                  />
                  <Input
                    aria-label={t("dr.edit.gateway")}
                    className="h-7 w-28"
                    placeholder={t("dr.edit.gateway")}
                    value={r.gateway}
                    onChange={(e) => setIp(k, { gateway: e.target.value })}
                  />
                  <Input
                    aria-label={t("dr.edit.dns")}
                    className="h-7 w-36"
                    placeholder={t("dr.edit.dns")}
                    value={r.dns}
                    onChange={(e) => setIp(k, { dns: e.target.value })}
                  />
                </>
              )}
              {!readOnly && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("dr.edit.removeIp", { n: k + 1 })}
                  onClick={() => onChange({ ip: vm.ip.filter((_, l) => l !== k) })}
                >
                  <X />
                </Button>
              )}
              {errors[`${at}.ip${k}`] && (
                <span className="basis-full text-xs text-destructive">
                  {t(errors[`${at}.ip${k}`]!)}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
      {(errors[`${at}.pool`] ?? errors[`${at}.sourceBackupPool`]) && (
        <p className="text-xs text-destructive">
          {t((errors[`${at}.pool`] ?? errors[`${at}.sourceBackupPool`])!)}
        </p>
      )}
      {!readOnly && (
        <div className="flex items-center gap-1">
          <Button
            type="button"
            variant="link"
            size="sm"
            className="h-auto w-fit p-0"
            onClick={() => onChange({ ip: [...vm.ip, newIp(vm.ip.length)] })}
          >
            {t("dr.edit.addIp")}
          </Button>
          <IpHelp />
        </div>
      )}
    </li>
  );
}

/** IP 규칙 옆 "?": 되살린 VM과 cloud-init(문서 "한계" 절을 짧게) */
function IpHelp() {
  const { t } = useTranslation();
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button type="button" variant="ghost" size="icon-xs" aria-label={t("dr.edit.ipHelpLabel")}>
          <CircleHelp />
        </Button>
      </PopoverTrigger>
      <PopoverContent className="text-xs">
        <p>{t("dr.edit.ipHelpInstance")}</p>
        <p>{t("dr.edit.ipHelpNoCloudInit")}</p>
      </PopoverContent>
    </Popover>
  );
}

/**
 * 원본 이름 → DR 이름 맵 표. 대상 칸은 이 클러스터의 이름을 제안한다(`datalist`). 줄마다 오류(반쯤 쓴 줄·겹친 원본·이름 모양)와
 * 경고(시험망 격리)를 그 줄 밑에 글로 보인다.
 */
function MapTable({
  title,
  help,
  pairs,
  targets,
  errors,
  warnings = {},
  readOnly,
  onChange,
}: {
  title: string;
  help: string;
  pairs: Pair[];
  targets: string[];
  /** 줄 번호 → 번역한 오류 */
  errors: Record<number, string>;
  /** 줄 번호 → 번역한 경고 */
  warnings?: Record<number, string>;
  readOnly: boolean;
  onChange(pairs: Pair[]): void;
}) {
  const { t } = useTranslation();
  const id = useId();
  const set = (i: number, patch: Partial<Pair>) =>
    onChange(pairs.map((p, j) => (j === i ? { ...p, ...patch } : p)));
  return (
    <section aria-labelledby={`${id}-title`} className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id={`${id}-title`} className="text-base font-semibold">
          {title}
        </h2>
        {!readOnly && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => onChange([...pairs, { from: "", to: "" }])}
          >
            <Plus />
            {t("dr.edit.addMapping")}
          </Button>
        )}
      </div>
      <p className="text-xs text-muted-foreground">{help}</p>
      <datalist id={`${id}-targets`}>
        {targets.map((n) => (
          <option key={n} value={n} />
        ))}
      </datalist>
      {pairs.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("dr.edit.noMappings")}</p>
      ) : (
        <ul aria-label={title} className="flex flex-col gap-1">
          {pairs.map((p, i) => (
            <li key={i} className="flex flex-wrap items-center gap-2">
              <Input
                aria-label={t("dr.edit.mapFrom", { n: i + 1 })}
                className="h-8 w-40"
                value={p.from}
                aria-invalid={errors[i] ? true : undefined}
                aria-describedby={errors[i] || warnings[i] ? `${id}-note${i}` : undefined}
                onChange={(e) => set(i, { from: e.target.value })}
              />
              <span aria-hidden>→</span>
              <Input
                aria-label={t("dr.edit.mapTo", { n: i + 1 })}
                className="h-8 w-40"
                list={`${id}-targets`}
                value={p.to}
                aria-invalid={errors[i] ? true : undefined}
                aria-describedby={errors[i] || warnings[i] ? `${id}-note${i}` : undefined}
                onChange={(e) => set(i, { to: e.target.value })}
              />
              {!readOnly && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("dr.edit.removeMapping", { n: i + 1 })}
                  onClick={() => onChange(pairs.filter((_, j) => j !== i))}
                >
                  <X />
                </Button>
              )}
              {(errors[i] || warnings[i]) && (
                <span
                  id={`${id}-note${i}`}
                  className={cn(
                    "basis-full text-xs",
                    errors[i] ? "text-destructive" : "text-warning-fg",
                  )}
                >
                  {errors[i] ?? warnings[i]}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
