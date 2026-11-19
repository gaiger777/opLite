import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { api, ApiError, call } from "@/api/client";
import { SETTINGS_KEY, useList, useSettings } from "@/api/live";
import type { components } from "@/api/schema";
import type { Settings } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import { CPU_KEY, describeBaseline, useCpuBaseline } from "@/cpu/cpu";
import { CpuFlagsInput } from "@/cpu/CpuFlagsInput";
import { CpuModelPicker } from "@/cpu/CpuModelPicker";
import { settingsError, storeSettings } from "@/settings/save";

type Schemas = components["schemas"];
type Arch = Schemas["Arch"];
export type CpuMode = "off" | "auto" | "pinned";

export interface CpuValues {
  mode: CpuMode;
  /** 아키텍처마다 고정 모델과 플래그(모델이 비면 고정하지 않음) */
  pinned: Partial<Record<Arch, { model: string; flags: string[] }>>;
  autoFlags: boolean;
  mask: string[];
  /** 저장된 `baseline`(고정이 있으면 모드는 "고정"이어도 `auto`일 수 있다 — CLI로 둘 다 둔 설정) */
  stored?: "off" | "auto";
}

/** 저장된 설정의 폼 값. 고정이 하나라도 있으면 "고정"이다(서버는 `baseline: off`여도 고정을 쓴다). */
export function cpuValues(spec: Settings["spec"]): CpuValues {
  const cpu = spec.cpu;
  const pinned: CpuValues["pinned"] = {};
  for (const p of cpu?.pinned ?? []) {
    pinned[p.arch] = { model: p.model, flags: p.flags ?? [] };
  }
  return {
    mode: (cpu?.pinned ?? []).length > 0 ? "pinned" : (cpu?.baseline ?? "off"),
    pinned,
    autoFlags: cpu?.auto_flags ?? true,
    mask: cpu?.mask ?? [],
    stored: cpu?.baseline ?? "off",
  };
}

function pinnedList(v: CpuValues): Schemas["PinnedBaseline"][] {
  if (v.mode !== "pinned") {
    return [];
  }
  return (Object.entries(v.pinned) as [Arch, { model: string; flags: string[] }][])
    .filter(([, p]) => p.model !== "")
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([arch, p]) => ({ arch, model: p.model, ...(p.flags.length > 0 && { flags: p.flags }) }));
}

/**
 * 폼 값의 설정 바꾸기(`PATCH /settings`의 `cpu`). `current`는 고치기 시작할 때 읽은 설정이고 그것과 다른 칸만 보낸다 —
 * 고정을 보내면 서버가 켤 수 없는 노드를 다시 보므로 숨김 목록만 바꿀 때 409를 받지 않게 하고, 그 사이 다른 사람이 바꾼
 * 칸을 되돌리지 않는다. 바뀐 것이 없으면 빈 객체다.
 */
export function cpuPatch(
  v: CpuValues,
  current: CpuValues,
  force = false,
): Schemas["CpuSettingsPatch"] {
  const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
  const want = v.mode === "auto" ? "auto" : "off";
  // 저장된 값과 견준다: `auto` + 고정인 설정에서 "끔"을 고르면 `baseline: off`도 보내야 한다(리뷰 1 웹 #5). 모드를
  // 바꿨을 때만 보낸다 — 고정은 "고정"으로 보이지만 저장된 `baseline`은 `auto`일 수 있다(CLI로 x86_64 자동 + aarch64
  // 고정). 고정에서 고정은 `baseline`을 건드리지 않는다(리뷰 2 웹 #1).
  const had = current.stored ?? (current.mode === "auto" ? "auto" : "off");
  const pinned = pinnedList(v);
  const patch: Schemas["CpuSettingsPatch"] = {
    ...(v.mode !== current.mode && want !== had && { baseline: want }),
    ...(!same(pinned, pinnedList(current)) && { pinned }),
    ...(v.autoFlags !== current.autoFlags && { auto_flags: v.autoFlags }),
    ...(!same([...v.mask].sort(), [...current.mask].sort()) && { mask: v.mask }),
  };
  return Object.keys(patch).length > 0 && force ? { ...patch, force: true } : patch;
}

/** 409 `cpu_baseline_unsupported`의 `details` */
interface Unsupported {
  nodes: string[];
  warnings: string[];
}

function unsupported(err: unknown): Unsupported | null {
  if (!(err instanceof ApiError) || err.code !== "cpu_baseline_unsupported") {
    return null;
  }
  const d = (err.details ?? {}) as Partial<Unsupported>;
  return { nodes: d.nodes ?? [], warnings: d.warnings ?? [] };
}

/**
 * 설정 › 일반의 "CPU"(설계 §9·§11·§13): 클러스터 기준(끔·자동·고정), 고정 모델과 플래그, 자동 플래그, 모든 VM에서
 * 숨기는 플래그. 저장하면 전원 껐다 켜기까지 그대로인 실행 중 VM 수(`affected_running_vms`)와 그 목록 링크를 보인다.
 * 고정 모델을 켤 수 없는 노드가 있으면(409 `cpu_baseline_unsupported`) 노드와 까닭을 보이고 "그래도 고정"으로 넘는다.
 */
export function CpuSettingsSection({ write }: { write: boolean }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const settings = useSettings();
  const nodes = useList("node");
  const baseline = useCpuBaseline(perms.has("cluster.read"));
  // 고치기 시작할 때의 설정과 revision(리뷰 1 웹 #3): 그것과 다른 칸만 보내고 그 revision으로 쓴다 — 그 사이 다른
  // 사람이 바꿨으면 412로 알리고 조용히 되돌리지 않는다.
  const [draft, setDraft] = useState<{
    base: CpuValues;
    revision: number;
    values: CpuValues;
  } | null>(null);
  const [conflict, setConflict] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [blocked, setBlocked] = useState<Unsupported | null>(null);
  const [affected, setAffected] = useState<number | null>(null);

  if (!settings.data) {
    return null;
  }
  const current = cpuValues(settings.data.spec);
  const base = draft?.base ?? current;
  const values = draft?.values ?? current;
  // 그 사이 CPU 칸이 그대로면(같은 페이지의 다른 절, 다른 사람의 다른 설정, 원격 측정 상태 쓰기) 새 revision을 쓴다 —
  // CPU를 바꾼 때만 412로 알린다(리뷰 2 웹 #2).
  const revision =
    draft && JSON.stringify(current) !== JSON.stringify(draft.base)
      ? draft.revision
      : settings.data.revision;
  const change = (patch: Partial<CpuValues>) => {
    setDraft({ base, revision, values: { ...values, ...patch } });
    setBlocked(null);
  };
  const discard = () => {
    // 연결이 끊겨 watch가 새 설정을 밀어 주지 않았을 수 있으니 다시 읽는다(리뷰 2 웹 #4).
    void queryClient.invalidateQueries({ queryKey: SETTINGS_KEY });
    setDraft(null);
    setConflict(false);
    setError(null);
    setBlocked(null);
  };
  // 고정할 아키텍처: 노드들의 아키텍처(위트니스 제외)와 기준이 있는 아키텍처, 이미 고정한 것
  const arches = [
    ...new Set<Arch>([
      ...(nodes.data ?? []).flatMap((n) =>
        n.status.arch && !n.spec.witness ? [n.status.arch] : [],
      ),
      ...(baseline.data?.arches ?? []).map((a) => a.arch),
      ...(Object.keys(values.pinned) as Arch[]),
    ]),
  ].sort();
  const unchanged = Object.keys(cpuPatch(values, base)).length === 0;

  async function save(force: boolean) {
    setBusy(true);
    setError(null);
    try {
      const updated = await call(
        api.PATCH("/settings", {
          params: { header: { "If-Match": revision } },
          body: { cpu: cpuPatch(values, base, force) },
        }),
      );
      storeSettings(queryClient, updated);
      void queryClient.invalidateQueries({ queryKey: CPU_KEY });
      setDraft(null);
      setConflict(false);
      setBlocked(null);
      setAffected(updated.affected_running_vms ?? null);
      toast.success(t("settings.saved"));
    } catch (err) {
      const found = unsupported(err);
      setBlocked(found);
      if (!found) {
        setError(settingsError(t, err));
        setConflict(
          err instanceof ApiError && (err.status === 412 || err.code === "revision_mismatch"),
        );
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        void save(false);
      }}
      className="flex flex-col gap-4"
    >
      <h2 className="font-medium">{t("cpu.settings.title")}</h2>
      {error && (
        <div role="alert" className="flex flex-wrap items-center gap-2 text-sm text-destructive">
          <p>{error}</p>
          {conflict && (
            <Button type="button" variant="outline" size="sm" onClick={discard}>
              {t("cpu.settings.reload")}
            </Button>
          )}
        </div>
      )}
      <Field
        id="settings-cpu-mode"
        label={t("cpu.settings.mode")}
        hint={t(`cpu.settings.modeHint.${values.mode}`)}
      >
        {(a11y) => (
          <NativeSelect
            id="settings-cpu-mode"
            value={values.mode}
            disabled={!write || busy}
            onChange={(e) => {
              const mode = e.target.value as CpuMode;
              // 고정으로 바꾸면 지금 기준을 플래그까지 미리 채운다(모델만 두면 cluster VM이 자동 플래그를 잃는다).
              // 숨김 목록이 끈 이름은 기준에도 꺼져 있지만 고정에 넣지 않는다(숨김 목록을 빼도 남지 않게).
              const pinned = { ...values.pinned };
              if (mode === "pinned") {
                for (const a of baseline.data?.arches ?? []) {
                  if (!pinned[a.arch] && a.baseline) {
                    pinned[a.arch] = {
                      model: a.baseline.model,
                      flags: Object.entries(a.baseline.flags ?? {})
                        // 저장된 숨김 목록으로 거른다: 기준의 `-이름`은 저장된 목록에서 왔다(리뷰 2 웹 #7).
                        .filter(([name, on]) => on || !base.mask.includes(name))
                        .map(([name, on]) => `${on ? "+" : "-"}${name}`),
                    };
                  }
                }
              }
              change({ mode, pinned });
            }}
            {...a11y}
          >
            {(["off", "auto", "pinned"] as const).map((m) => (
              <NativeSelectOption key={m} value={m}>
                {t(`cpu.mode.${m}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {(baseline.data?.arches ?? [])
        .filter((a) => a.baseline)
        .map((a) => (
          <p key={a.arch} className="text-xs text-muted-foreground">
            {t("cpu.settings.now", { arch: a.arch, cpu: describeBaseline(a.baseline!) })}
          </p>
        ))}
      {values.mode === "pinned" &&
        arches.map((arch) => {
          const p = values.pinned[arch] ?? { model: "", flags: [] };
          const set = (patch: Partial<typeof p>) =>
            change({ pinned: { ...values.pinned, [arch]: { ...p, ...patch } } });
          return (
            <fieldset key={arch} className="flex flex-col gap-3 rounded-md border p-3">
              <legend className="px-1 text-sm font-medium">{arch}</legend>
              <CpuModelPicker
                id={`settings-cpu-pinned-${arch}`}
                label={t("cpu.settings.pinnedModel")}
                value={p.model}
                arch={arch}
                kinds={arch === "x86_64" ? ["level", "qemu"] : ["qemu"]}
                disabled={!write || busy}
                onChange={(model) => set({ model })}
              />
              <CpuFlagsInput
                id={`settings-cpu-pinned-flags-${arch}`}
                label={t("cpu.settings.pinnedFlags")}
                flags={p.flags}
                arches={[arch]}
                signed
                disabled={!write || busy}
                onChange={(flags) => set({ flags })}
              />
            </fieldset>
          );
        })}
      {values.mode === "auto" && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center gap-2">
            <Switch
              id="settings-cpu-auto-flags"
              aria-describedby="settings-cpu-auto-flags-hint"
              checked={values.autoFlags}
              disabled={!write || busy}
              onCheckedChange={(autoFlags) => change({ autoFlags })}
            />
            <Label htmlFor="settings-cpu-auto-flags">{t("cpu.settings.autoFlags")}</Label>
          </div>
          <p id="settings-cpu-auto-flags-hint" className="text-xs text-muted-foreground">
            {t("cpu.settings.autoFlagsHint")}
          </p>
        </div>
      )}
      <CpuFlagsInput
        id="settings-cpu-mask"
        label={t("cpu.settings.mask")}
        hint={t("cpu.settings.maskHint")}
        flags={values.mask}
        arches={arches.length > 0 ? arches : [undefined]}
        signed={false}
        disabled={!write || busy}
        onChange={(mask) => change({ mask })}
      />
      {blocked && (
        <div
          role="alert"
          className="flex flex-col gap-2 rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm"
        >
          <p className="font-medium">
            {t("cpu.settings.unsupported", { nodes: blocked.nodes.join(", ") })}
          </p>
          {blocked.warnings.length > 0 && (
            <ul className="list-disc pl-5">
              {blocked.warnings.map((w) => (
                <li key={w}>{w}</li>
              ))}
            </ul>
          )}
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => void save(true)}
            >
              {t("cpu.settings.force")}
            </Button>
          </div>
        </div>
      )}
      {affected !== null && affected > 0 && (
        <p role="status" className="text-sm">
          {t("cpu.settings.affected", { count: affected })}{" "}
          <Link to="/vms?cpu=pending" className="underline underline-offset-4">
            {t("cpu.settings.affectedLink")}
          </Link>{" "}
          <span className="text-muted-foreground">{t("cpu.settings.affectedDelay")}</span>
        </p>
      )}
      {write && (
        <div>
          <Button type="submit" variant="outline" disabled={busy || unchanged}>
            {t("cpu.settings.save")}
          </Button>
        </div>
      )}
    </form>
  );
}
