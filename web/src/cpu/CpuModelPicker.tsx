import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { components } from "@/api/schema";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  NativeSelect,
  NativeSelectOptGroup,
  NativeSelectOption,
} from "@/components/ui/native-select";
import { type CpuModelKind, modelGroups, useCpuModels } from "@/cpu/cpu";

/**
 * CPU 모델 고르기(설계 §13): 찾기 상자와 묶음 목록 — 기본(`host`·`max`), 클러스터 기준(`cluster`: 지금 모델), 호환
 * 수준(`x86-64-v*`), QEMU 모델(버전). 별칭은 보이지 않고 가리키는 버전 모델로 찾는다(서버도 버전 이름으로 고정한다).
 * 항목마다 켤 수 있는 노드 수, 고르면 못 켜는 노드와 빠진 플래그(루트 `cluster.read`가 있을 때)를 보인다.
 * 목록을 읽지 못하면(권한·옛 서버) `host`·`max`만 고른다.
 */
export function CpuModelPicker({
  id,
  label,
  value,
  onChange,
  arch,
  kinds,
  disabled = false,
}: {
  id: string;
  label: string;
  value: string;
  onChange(model: string): void;
  arch: components["schemas"]["Arch"] | undefined;
  /** 고를 수 있는 묶음(고정 기준은 `level`·`qemu`만). 없으면 모두 */
  kinds?: readonly CpuModelKind[];
  disabled?: boolean;
}) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const models = useCpuModels(arch);
  const all = (models.data?.models ?? []).filter((m) => !kinds || kinds.includes(m.kind));
  const groups = modelGroups(all, query, value);
  const chosen = all.find((m) => m.name === value);
  const total = models.data?.nodes ?? 0;
  const known = groups.some((g) => g.models.some((m) => m.name === value));
  const blocked = (chosen?.nodes ?? []).filter((n) => !n.can_run);

  const optionLabel = (m: (typeof all)[number]) => {
    const name = m.kind === "cluster" && m.current ? `cluster (${m.current})` : m.name;
    const extra = [
      total > 0 ? t("cpu.model.usable", { usable: m.usable, total }) : "",
      m.deprecated ? t("cpu.model.deprecated") : "",
    ].filter(Boolean);
    return extra.length > 0 ? `${name} — ${extra.join(", ")}` : name;
  };

  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      {all.length > 8 && (
        <Input
          type="search"
          aria-label={t("cpu.model.search")}
          placeholder={t("cpu.model.search")}
          value={query}
          disabled={disabled}
          onChange={(e) => setQuery(e.target.value)}
        />
      )}
      <NativeSelect
        id={id}
        className="w-full"
        value={value}
        disabled={disabled}
        aria-describedby={`${id}-detail`}
        onChange={(e) => onChange(e.target.value)}
      >
        {/* 목록에 없는 지금 값(읽기 전·모르는 모델)도 남긴다. */}
        {!known && (
          <NativeSelectOption value={value}>{value || t("cpu.model.choose")}</NativeSelectOption>
        )}
        {models.data ? (
          groups.map((g) => (
            <NativeSelectOptGroup key={g.kind} label={t(`cpu.kind.${g.kind}`)}>
              {g.models.map((m) => (
                <NativeSelectOption key={m.name} value={m.name}>
                  {optionLabel(m)}
                </NativeSelectOption>
              ))}
            </NativeSelectOptGroup>
          ))
        ) : (
          <>
            {value !== "host" && <NativeSelectOption value="host">host</NativeSelectOption>}
            {value !== "max" && <NativeSelectOption value="max">max</NativeSelectOption>}
          </>
        )}
      </NativeSelect>
      <div id={`${id}-detail`} className="text-xs text-muted-foreground">
        {chosen?.kind === "cluster" &&
          (chosen.current
            ? t("cpu.model.clusterNow", { model: chosen.current })
            : t("cpu.model.clusterOff"))}
        {chosen && chosen.kind !== "basic" && total > 0 && chosen.usable < total && (
          <p className="text-warning-fg">
            {t("cpu.model.usable", { usable: chosen.usable, total })}
          </p>
        )}
        {blocked.length > 0 && (
          <ul className="text-warning-fg">
            {blocked.map((n) => (
              <li key={n.node}>
                {(n.missing ?? []).length > 0
                  ? t("cpu.model.missing", { node: n.node, flags: (n.missing ?? []).join(", ") })
                  : t("cpu.model.cannot", { node: n.node, reason: n.reason ?? "" })}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
