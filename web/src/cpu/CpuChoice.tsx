import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { components } from "@/api/schema";
import { CpuFlagsInput } from "@/cpu/CpuFlagsInput";
import { CpuModelPicker } from "@/cpu/CpuModelPicker";

/** 고른 CPU 모델과 플래그(`+aes`·`-avx512vbmi`) */
export interface CpuPick {
  model: string;
  flags: string[];
}

/** 요청에 넣을 CPU 덮어쓰기: `from`(원본)과 다른 칸만이다(바꾸지 않았으면 빈 객체 — 서버가 원본 그대로 둔다). */
export function cpuOverride(
  pick: CpuPick | null,
  from: CpuPick,
): { cpu_model?: string; cpu_flags?: string[] } {
  if (!pick) {
    return {};
  }
  return {
    ...(pick.model !== from.model && { cpu_model: pick.model }),
    ...(pick.flags.join(" ") !== from.flags.join(" ") && { cpu_flags: pick.flags }),
  };
}

/**
 * 복제·템플릿 배포·가져오기의 CPU 칸(리뷰 2 웹 #3): VM 폼과 같은 모델 고르기와 플래그 입력. `collapsed`면 접힌 "CPU
 * 모델과 플래그"(요약에 지금 값) 안에 두고 펼칠 때만 목록을 읽는다.
 */
export function CpuChoice({
  id,
  arch,
  value,
  onChange,
  collapsed = false,
  disabled = false,
}: {
  id: string;
  arch: components["schemas"]["Arch"] | undefined;
  value: CpuPick;
  onChange(pick: CpuPick): void;
  collapsed?: boolean;
  disabled?: boolean;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const fields = (
    <div className="flex flex-col gap-3">
      <CpuModelPicker
        id={`${id}-model`}
        label={t("vmForm.cpuModel")}
        value={value.model}
        arch={arch}
        disabled={disabled}
        onChange={(model) => onChange({ ...value, model })}
      />
      <CpuFlagsInput
        id={`${id}-flags`}
        label={t("cpu.flags.label")}
        flags={value.flags}
        arches={[arch]}
        signed
        disabled={disabled}
        onChange={(flags) => onChange({ ...value, flags })}
      />
    </div>
  );
  if (!collapsed) {
    return fields;
  }
  return (
    <details
      className="rounded-md border px-3 py-2"
      open={open}
      onToggle={(e) => setOpen(e.currentTarget.open)}
    >
      <summary className="cursor-pointer text-sm font-medium">
        {t("cpu.choice.title")}{" "}
        <span className="font-mono font-normal text-muted-foreground">
          {[value.model, ...value.flags].join(" ")}
        </span>
      </summary>
      {open && <div className="pt-3">{fields}</div>}
    </details>
  );
}
