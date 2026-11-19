import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { VmSpec } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  coresFor,
  type CpuPolicy,
  describeNuma,
  type EmulatorPolicy,
  memoryHotplug,
  type MemoryPolicy,
  nodeCounts,
  type NumaMode,
  numaSpec,
  type NumaValues,
  type OwnUse,
  type PageSize,
  roomOn,
  usesHostResources,
  type ThreadPolicy,
  useTopologyUsage,
} from "@/numa/numa";

const THREADS: ThreadPolicy[] = ["prefer", "isolate", "require"];
const PAGES: PageSize[] = ["2M", "1G", "any"];

/**
 * 정책·스레드·에뮬레이터·vNUMA·메모리 묶기·hugepages 칸(VM 폼과 복제·배포·가져오기가 함께 쓴다). `canDedicate`가
 * 거짓이면(`vm.dedicated` 없음) 전용 정책과 hugepages를 새로 고를 수 없고 까닭을 보인다 — 이미 그 값이면 그대로 둔다.
 * `sockets`의 약수만 노드 수로 고를 수 있다(게스트 노드는 소켓 단위, 설계 §4). `list`가 없으면 직접 나누기를 고르지
 * 않는다(원본이 직접 나누기면 그대로 보인다).
 */
function NumaFields({
  id,
  value,
  onChange,
  sockets,
  canDedicate,
  errors,
  list,
  hotplug,
}: {
  id: string;
  value: NumaValues;
  onChange(value: NumaValues): void;
  sockets: number;
  canDedicate: boolean;
  errors: Record<string, string>;
  list: boolean;
  /** 메모리 핫플러그 VM(상한 > 메모리): `any`는 2 MiB 페이지다. */
  hotplug: boolean;
}) {
  const { t } = useTranslation();
  const set = (patch: Partial<NumaValues>) => onChange({ ...value, ...patch });
  const dedicated = value.policy === "dedicated";
  const counts = nodeCounts(sockets);
  // 지금 값이 고를 수 없는 수(소켓을 줄임)여도 보이게 둔다(검증이 알린다).
  const shownCounts =
    value.mode === "count" && !counts.includes(Number(value.count))
      ? [...counts, Number(value.count)]
      : counts;
  const modeValue = value.mode === "count" ? `count:${value.count}` : value.mode;
  return (
    <>
      <Field
        id={`${id}-policy`}
        label={t("numa.policy.label")}
        hint={t(canDedicate ? "numa.policy.hint" : "numa.needsPrivilege")}
      >
        {(a11y) => (
          <NativeSelect
            id={`${id}-policy`}
            className="w-full"
            value={value.policy}
            onChange={(e) => {
              const policy = e.target.value as CpuPolicy;
              // 공유로 바꾸면 스레드·에뮬레이터를 비운다(서버가 공유 VM의 두 칸을 400으로 거절한다).
              set(policy === "shared" ? { policy, threadPolicy: "", emulator: "" } : { policy });
            }}
            {...a11y}
          >
            <NativeSelectOption value="shared">{t("numa.policy.shared")}</NativeSelectOption>
            <NativeSelectOption value="dedicated" disabled={!canDedicate && !dedicated}>
              {t("numa.policy.dedicated")}
            </NativeSelectOption>
          </NativeSelect>
        )}
      </Field>
      {dedicated && (
        <>
          <Field
            id={`${id}-threads`}
            label={t("numa.threads.label")}
            error={errors["numa.threads"]}
            hint={t(`numa.threads.hint.${value.threadPolicy || "prefer"}`)}
          >
            {(a11y) => (
              <NativeSelect
                id={`${id}-threads`}
                className="w-full"
                value={value.threadPolicy || "prefer"}
                onChange={(e) => {
                  const p = e.target.value as ThreadPolicy;
                  set({ threadPolicy: p === "prefer" ? "" : p });
                }}
                {...a11y}
              >
                {THREADS.map((p) => (
                  <NativeSelectOption key={p} value={p}>
                    {t(`numa.threads.${p}`)}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          <Field
            id={`${id}-emulator`}
            label={t("numa.emulator.label")}
            hint={t("numa.emulator.hint")}
          >
            {(a11y) => (
              <NativeSelect
                id={`${id}-emulator`}
                className="w-full"
                value={value.emulator || "shared"}
                onChange={(e) => {
                  const p = e.target.value as EmulatorPolicy;
                  set({ emulator: p === "shared" ? "" : p });
                }}
                {...a11y}
              >
                <NativeSelectOption value="shared">{t("numa.emulator.shared")}</NativeSelectOption>
                <NativeSelectOption value="isolate">
                  {t("numa.emulator.isolate")}
                </NativeSelectOption>
              </NativeSelect>
            )}
          </Field>
        </>
      )}
      <Field
        id={`${id}-nodes`}
        label={t("numa.nodes.label")}
        error={errors["numa.count"]}
        hint={
          sockets === 1
            ? t("numa.nodes.oneSocket")
            : value.mode === "off" && (dedicated || value.hugepages !== "")
              ? t("numa.nodes.implicitAuto")
              : t("numa.nodes.hint")
        }
      >
        {(a11y) => (
          <NativeSelect
            id={`${id}-nodes`}
            className="w-full"
            value={modeValue}
            onChange={(e) => {
              const v = e.target.value;
              if (v.startsWith("count:")) {
                set({ mode: "count", count: v.slice(6) });
              } else {
                set({ mode: v as NumaMode, ...(v === "off" && { memory: "" }) });
              }
            }}
            {...a11y}
          >
            <NativeSelectOption value="off">{t("numa.nodes.off")}</NativeSelectOption>
            <NativeSelectOption value="auto">{t("numa.nodes.auto")}</NativeSelectOption>
            {shownCounts.map((n) => (
              <NativeSelectOption key={n} value={`count:${n}`}>
                {t("numa.summary.nodes", { count: n })}
              </NativeSelectOption>
            ))}
            {(list || value.mode === "list") && (
              <NativeSelectOption value="list">{t("numa.nodes.list")}</NativeSelectOption>
            )}
          </NativeSelect>
        )}
      </Field>
      <Field id={`${id}-memory`} label={t("numa.memory.label")} hint={t("numa.memory.hint")}>
        {(a11y) => (
          <NativeSelect
            id={`${id}-memory`}
            className="w-full"
            disabled={value.mode === "off"}
            value={value.memory}
            onChange={(e) => set({ memory: e.target.value as MemoryPolicy | "" })}
            {...a11y}
          >
            <NativeSelectOption value="">{t("numa.memory.default")}</NativeSelectOption>
            <NativeSelectOption value="strict">{t("numa.memory.strict")}</NativeSelectOption>
            <NativeSelectOption value="preferred">{t("numa.memory.preferred")}</NativeSelectOption>
          </NativeSelect>
        )}
      </Field>
      <Field
        id={`${id}-hugepages`}
        label={t("numa.hugepages.label")}
        error={errors["numa.hugepages"]}
        hint={
          !canDedicate
            ? t("numa.needsPrivilege")
            : value.hugepages === "any" && hotplug
              ? t("numa.hugepages.anyHotplug")
              : t("numa.hugepages.hint")
        }
      >
        {(a11y) => (
          <NativeSelect
            id={`${id}-hugepages`}
            className="w-full"
            value={value.hugepages}
            onChange={(e) => set({ hugepages: e.target.value as PageSize | "" })}
            {...a11y}
          >
            <NativeSelectOption value="">{t("numa.hugepages.none")}</NativeSelectOption>
            {PAGES.map((p) => (
              <NativeSelectOption
                key={p}
                value={p}
                disabled={!canDedicate && value.hugepages !== p}
              >
                {t(`numa.hugepages.${p}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
    </>
  );
}

/** 직접 나누기의 노드 표(vCPU 자리 수·MiB) */
function NodeList({
  value,
  onChange,
  errors,
  hotplug,
}: {
  value: NumaValues;
  onChange(value: NumaValues): void;
  errors: Record<string, string>;
  /** 메모리 상한이 있는 VM: 목록은 켤 때 나눔이고 그 위의 메모리는 노드마다 고르게 더한다(리뷰 2). */
  hotplug: boolean;
}) {
  const { t } = useTranslation();
  const rows = value.list;
  const setRow = (i: number, patch: Partial<NumaValues["list"][number]>) =>
    onChange({ ...value, list: rows.map((r, j) => (j === i ? { ...r, ...patch } : r)) });
  return (
    <fieldset className="flex flex-col gap-2 sm:col-span-2">
      <legend className="mb-1 text-sm font-medium">{t("numa.list.title")}</legend>
      {hotplug && <p className="text-xs text-muted-foreground">{t("numa.list.hotplugHint")}</p>}
      {errors["numa.list"] && (
        <p role="alert" className="text-xs text-destructive">
          {t(errors["numa.list"])}
        </p>
      )}
      {rows.map((row, i) => (
        <div
          key={i}
          role="group"
          aria-label={t("numa.list.node", { n: i })}
          className="flex items-end gap-2"
        >
          <Field
            id={`vm-numa-list-${i}-cpus`}
            label={t("numa.list.cpus", { n: i })}
            error={errors[`numa.list.${i}.cpus`]}
          >
            {(a11y) => (
              <Input
                id={`vm-numa-list-${i}-cpus`}
                inputMode="numeric"
                value={row.cpus}
                onChange={(e) => setRow(i, { cpus: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id={`vm-numa-list-${i}-memory`}
            label={t("numa.list.memory", { n: i })}
            error={errors[`numa.list.${i}.memory`]}
          >
            {(a11y) => (
              <Input
                id={`vm-numa-list-${i}-memory`}
                inputMode="numeric"
                value={row.memory}
                onChange={(e) => setRow(i, { memory: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            aria-label={t("numa.list.remove", { n: i })}
            onClick={() => onChange({ ...value, list: rows.filter((_, j) => j !== i) })}
          >
            <Trash2 />
          </Button>
        </div>
      ))}
      <div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => onChange({ ...value, list: [...rows, { cpus: "", memory: "" }] })}
        >
          <Plus />
          {t("numa.list.add")}
        </Button>
      </div>
    </fieldset>
  );
}

/**
 * VM 폼 › 일반의 접힌 "CPU 배치와 NUMA"(설계 §13): 소켓 수, 정책 칸, 직접 나누기, 들어갈 노드 어림(`/topology/usage`,
 * 루트 `cluster.read`), 실행 중이면 "다음 전원 껐다 켜기 때". 바꾼 값이 있거나 입력 오류가 있으면 펼쳐 둔다.
 */
export function NumaSection({
  sockets,
  cores,
  maxVcpus,
  value,
  onChange,
  onSockets,
  errors,
  canDedicate,
  spec,
  running,
  own,
}: {
  sockets: number;
  /** 코어 칸(소켓당). 소켓을 바꿔도 vCPU 수가 같게 나눌 수 있는 소켓 수만 고르게 한다. */
  cores: string;
  /** 핫플러그 vCPU 상한 칸(비면 없음). 상한도 소켓 수로 나뉘어야 한다(리뷰 2 웹). */
  maxVcpus: string;
  value: NumaValues;
  onChange(value: NumaValues): void;
  onSockets(sockets: number): void;
  errors: Record<string, string>;
  canDedicate: boolean;
  /** 폼 값의 스펙(들어갈 노드 어림) */
  spec: VmSpec;
  /** 멈추지 않은 VM: 바꾸면 보류 `numa`(전원 껐다 켜기) */
  running: boolean;
  /** 고치는 VM이 지금 잡은 전용 CPU·hugepages(들어갈 노드 어림에서 되돌려 셈) */
  own?: OwnUse;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const invalid = Object.keys(errors).some((k) => k.startsWith("numa."));
  const [open, setOpen] = useState(
    () =>
      sockets > 1 ||
      numaSpec(value).numa !== undefined ||
      value.policy !== "shared" ||
      value.hugepages !== "",
  );
  const bound = value.policy === "dedicated" || value.hugepages !== "";
  const usage = useTopologyUsage(bound && perms.has("cluster.read"));
  const room = usage.data ? roomOn(usage.data.nodes, spec, own) : undefined;
  // vCPU 수(소켓 × 코어)를 그대로 두고 vCPU 상한도 나누는 소켓 수만(지금 값은 늘 보인다 — CLI로 만든 VM)
  const socketChoices = [...new Set([...SOCKET_CHOICES, sockets])]
    .filter(
      (n) =>
        n === sockets ||
        (coresFor(sockets, cores, n) !== null && (maxVcpus === "" || Number(maxVcpus) % n === 0)),
    )
    .sort((a, b) => a - b);
  return (
    <details
      className="rounded-md border px-3 py-2 sm:col-span-2"
      open={open || invalid}
      onToggle={(e) => setOpen(e.currentTarget.open)}
    >
      <summary className="cursor-pointer text-sm font-medium">
        {t("numa.form.title")}{" "}
        <span className="font-normal text-muted-foreground">{describeNuma(t, spec)}</span>
      </summary>
      <div className="grid grid-cols-1 gap-4 pt-3 sm:grid-cols-2">
        {running && (
          <p className="text-sm text-muted-foreground sm:col-span-2">{t("numa.form.powerCycle")}</p>
        )}
        <Field
          id="vm-sockets"
          label={t("numa.form.sockets")}
          hint={t("numa.form.socketsHint", {
            count: sockets * (Number(cores) || 0),
          })}
        >
          {(a11y) => (
            <NativeSelect
              id="vm-sockets"
              className="w-full"
              value={sockets}
              onChange={(e) => onSockets(Number(e.target.value))}
              {...a11y}
            >
              {socketChoices.map((n) => (
                <NativeSelectOption key={n} value={n}>
                  {n}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )}
        </Field>
        <NumaFields
          id="vm-numa"
          value={value}
          onChange={onChange}
          sockets={sockets}
          canDedicate={canDedicate}
          errors={errors}
          list
          hotplug={memoryHotplug(spec)}
        />
        {value.mode === "list" && (
          <NodeList
            value={value}
            onChange={onChange}
            errors={errors}
            hotplug={spec.max_memory_mib != null}
          />
        )}
        {room !== undefined && (
          <p
            role="status"
            className={
              room.length > 0
                ? "text-sm text-muted-foreground sm:col-span-2"
                : "text-sm text-warning-fg sm:col-span-2"
            }
          >
            {room.length > 0
              ? t("numa.form.room", { nodes: room.join(", ") })
              : t("numa.form.noRoom")}
          </p>
        )}
      </div>
    </details>
  );
}

/** 고를 수 있는 소켓 수(게스트 NUMA 노드는 8개까지라 그 배수 몇 개) */
const SOCKET_CHOICES = [1, 2, 3, 4, 6, 8, 12, 16];

/**
 * 복제·템플릿 배포·가져오기의 접힌 "CPU 배치와 NUMA"(계획 63 `CpuChoice`와 같은 꼴). 요약에 지금 값을 보이고 펼칠
 * 때만 칸을 그린다. 권한 `vm.dedicated`는 어딘가에 있으면 고르게 하고 폴더는 서버가 본다(403).
 */
export function NumaChoice({
  id,
  sockets,
  value,
  onChange,
  errors = {},
  hotplug = false,
}: {
  id: string;
  /** 새 VM의 소켓 수(노드 수 고르기). 덮어쓴 vCPU 수가 있으면 1이다(서버의 `apply_overrides`). */
  sockets: number;
  value: NumaValues;
  onChange(value: NumaValues): void;
  /** 덮어쓴 CPU·메모리로 다시 본 입력 오류([`numaErrors`], 리뷰 2 웹). 있으면 칸을 펼친다. */
  errors?: Record<string, string>;
  /** 새 VM이 메모리 핫플러그 VM이다(`any`는 2 MiB). */
  hotplug?: boolean;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const [open, setOpen] = useState(false);
  const n = numaSpec(value);
  // 요약은 배치 칸만 본다(CPU 수·메모리와 상관없다).
  const shown = { cpu: { cores: 1, ...n.cpu }, numa: n.numa, hugepages: n.hugepages } as VmSpec;
  // 전용·hugepages 원본을 권한 없이 복제하면 서버가 403이다: 미리 말하고 칸을 펼친다(리뷰 1 웹).
  const blocked = usesHostResources(shown) && !perms.anywhere("vm.dedicated");
  const invalid = Object.keys(errors).length > 0;
  // 직접 나누기는 여기서 고칠 수 없다(노드 수를 다시 고른다): 그 오류는 한 줄로 보인다.
  const listErrors = [
    ...new Set(
      Object.entries(errors)
        .filter(([k]) => k.startsWith("numa.list"))
        .map(([, v]) => v),
    ),
  ];
  return (
    <details
      className="rounded-md border px-3 py-2"
      open={open || blocked || invalid}
      onToggle={(e) => setOpen(e.currentTarget.open)}
    >
      <summary className="cursor-pointer text-sm font-medium">
        {t("numa.form.title")}{" "}
        <span className="font-normal text-muted-foreground">{describeNuma(t, shown)}</span>
      </summary>
      {blocked && (
        <p role="status" className="pt-2 text-sm text-warning-fg">
          {t("numa.form.needsPrivilege")}
        </p>
      )}
      {listErrors.length > 0 && (
        <p role="alert" className="pt-2 text-sm text-destructive">
          {listErrors.map((k) => t(k)).join(" ")}
        </p>
      )}
      {(open || blocked || invalid) && (
        <div className="flex flex-col gap-3 pt-3">
          <NumaFields
            id={id}
            value={value}
            onChange={onChange}
            sockets={sockets}
            canDedicate={perms.anywhere("vm.dedicated")}
            errors={errors}
            list={false}
            hotplug={hotplug}
          />
        </div>
      )}
    </details>
  );
}
