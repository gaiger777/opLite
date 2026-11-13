import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useSettings } from "@/api/live";
import type { Settings, SettingsPatch } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { patchSettings, settingsError } from "@/settings/save";

type ReserveKind = "none" | "nodes" | "percent";

/** 서버의 범위: 노드 1~8대, 1~90%, 동시 시작 1~32(기본 4) */
const RESERVE_MAX: Record<Exclude<ReserveKind, "none">, number> = { nodes: 8, percent: 90 };
const MAX_CONCURRENCY = 32;
const DEFAULT_CONCURRENCY = 4;

interface HaValues {
  kind: ReserveKind;
  amount: string;
  concurrency: string;
}

/** 저장된 설정의 폼 값. 예약·동시 시작은 기본이면 저장하지 않으므로 없으면 기본이다. */
function fromSpec(spec: Settings["spec"]): HaValues {
  const reserve = spec.ha_reserve;
  const kind: ReserveKind = !reserve ? "none" : "nodes" in reserve ? "nodes" : "percent";
  const amount = !reserve ? "" : "nodes" in reserve ? reserve.nodes : reserve.percent;
  return {
    kind,
    amount: String(amount),
    concurrency: String(spec.ha_start_concurrency ?? DEFAULT_CONCURRENCY),
  };
}

function intIn(value: string, min: number, max: number): boolean {
  const n = Number(value);
  return /^\d+$/.test(value) && n >= min && n <= max;
}

/** 입력 오류(번역 키). 없으면 빈 객체 */
export function haErrors(v: HaValues): Partial<Record<"amount" | "concurrency", string>> {
  return {
    ...(v.kind !== "none" &&
      !intIn(v.amount, 1, RESERVE_MAX[v.kind]) && { amount: `settings.ha.errors.${v.kind}` }),
    ...(!intIn(v.concurrency, 1, MAX_CONCURRENCY) && {
      concurrency: "settings.ha.errors.concurrency",
    }),
  };
}

/** 폼 값의 설정 바꾸기 */
export function haPatch(v: HaValues): SettingsPatch {
  const amount = Number(v.amount);
  return {
    ha_reserve:
      v.kind === "none" ? "none" : v.kind === "nodes" ? { nodes: amount } : { percent: amount },
    ha_start_concurrency: Number(v.concurrency),
  };
}

/** HA 용량 예약과 대상 노드당 동시 시작 수. 리더가 쓰는 클러스터 설정이다. */
export function HaSection({ write }: { write: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const settings = useSettings();
  // 고치기 전에는 서버의 값을 보인다(watch로 바뀌어도 따라간다).
  const [edited, setEdited] = useState<HaValues | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shown, setShown] = useState<ReturnType<typeof haErrors>>({});

  if (!settings.data) {
    return null;
  }
  const current = fromSpec(settings.data.spec);
  const values = edited ?? current;
  const revision = settings.data.revision;
  const change = (patch: Partial<HaValues>) => setEdited({ ...values, ...patch });
  const unchanged =
    values.kind === current.kind &&
    (values.kind === "none" || values.amount === current.amount) &&
    values.concurrency === current.concurrency;

  async function save(e: React.FormEvent) {
    e.preventDefault();
    const errors = haErrors(values);
    setShown(errors);
    if (Object.keys(errors).length > 0) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await patchSettings(queryClient, revision, haPatch(values));
      setEdited(null);
      toast.success(t("settings.saved"));
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form noValidate onSubmit={(e) => void save(e)} className="flex flex-col gap-4">
      <h2 className="font-medium">{t("settings.ha.title")}</h2>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <Field
        id="settings-ha-reserve"
        label={t("settings.ha.reserve")}
        hint={t("settings.ha.reserveHint")}
      >
        {(a11y) => (
          <NativeSelect
            id="settings-ha-reserve"
            value={values.kind}
            disabled={!write || busy}
            onChange={(e) => {
              const kind = e.target.value as ReserveKind;
              change({ kind, amount: kind === "none" ? "" : kind === "nodes" ? "1" : "25" });
            }}
            {...a11y}
          >
            {(["none", "nodes", "percent"] as const).map((k) => (
              <NativeSelectOption key={k} value={k}>
                {t(`settings.ha.reserveKind.${k}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {values.kind !== "none" && (
        <Field
          id="settings-ha-amount"
          label={t(`settings.ha.amount.${values.kind}`)}
          error={shown.amount}
        >
          {(a11y) => (
            <Input
              id="settings-ha-amount"
              type="number"
              min={1}
              max={RESERVE_MAX[values.kind as Exclude<ReserveKind, "none">]}
              value={values.amount}
              disabled={!write || busy}
              onChange={(e) => change({ amount: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      <Field
        id="settings-ha-concurrency"
        label={t("settings.ha.concurrency")}
        error={shown.concurrency}
        hint={t("settings.ha.concurrencyHint")}
      >
        {(a11y) => (
          <Input
            id="settings-ha-concurrency"
            type="number"
            min={1}
            max={MAX_CONCURRENCY}
            value={values.concurrency}
            disabled={!write || busy}
            onChange={(e) => change({ concurrency: e.target.value })}
            {...a11y}
          />
        )}
      </Field>
      {write && (
        <div>
          <Button type="submit" variant="outline" disabled={busy || unchanged}>
            {t("settings.ha.save")}
          </Button>
        </div>
      )}
    </form>
  );
}
