// VM 만들기·고치기의 단계 목록과 프리셋(설계 §5.6). 화면 없이 시험하는 순수 함수다.
import { windows11, type VmFormValues } from "@/vms/form";

/** 폼의 단계(섹션) 순서. 마지막은 요약 검토다. */
export const FORM_STEPS = [
  "general",
  "disks",
  "nics",
  "cdroms",
  "boot",
  "security",
  "agent",
  "cloudInit",
  "schedule",
  "replication",
  "review",
] as const;
export type FormStep = (typeof FORM_STEPS)[number];

/** 단계 섹션의 요소 id */
export function stepId(step: FormStep): string {
  return `vm-step-${step}`;
}

/** 입력 오류 키(`validate()`)가 있는 단계. 모르는 키는 일반이다. */
export function stepOf(key: string): FormStep {
  if (key.endsWith(".bootOrder")) {
    return "boot";
  }
  const head = key.split(".")[0];
  switch (head) {
    case "disks":
    case "nics":
    case "cdroms":
    case "replication":
      return head;
    case "ci":
      return "cloudInit";
    case "schedule":
      return "schedule";
    case "tpm":
    case "secureBoot":
      return "security";
    default:
      return "general";
  }
}

/** 단계마다 입력 오류 수 */
export function errorCounts(errors: Record<string, string>): Partial<Record<FormStep, number>> {
  const counts: Partial<Record<FormStep, number>> = {};
  for (const key of Object.keys(errors)) {
    const step = stepOf(key);
    counts[step] = (counts[step] ?? 0) + 1;
  }
  return counts;
}

/** VM 만들기의 첫 단계 "프리셋 고르기". 템플릿에서는 폼 대신 템플릿 배포로 간다. */
export const PRESETS = ["blank", "linux", "windows", "template"] as const;
export type Preset = (typeof PRESETS)[number];

/**
 * 프리셋을 폼 값에 얹는다. 프리셋이 정하는 값만 바꾸고 나머지 입력은 그대로 둔다.
 * Windows 11은 계획 59의 미리 채우기(`windows11`)이고, Linux 서버는 게스트 에이전트와 cloud-init을 켠다.
 * 빈 VM과 템플릿은 값을 바꾸지 않는다. `applied`(앞서 얹은 프리셋)가 Windows였으면
 * 그 프리셋이 바꾼 디스크 버스(SATA)와 NIC 모델(e1000e)을 처음 값으로 돌린다.
 */
export function applyPreset(preset: Preset, applied: Preset, current: VmFormValues): VmFormValues {
  if (preset === "template" || preset === applied) {
    return current;
  }
  const base: VmFormValues =
    applied === "windows"
      ? {
          ...current,
          disks: current.disks.map((d) => (d.bus === "sata" ? { ...d, bus: "virtio-blk" } : d)),
          nics: current.nics.map((n) => (n.model === "e1000e" ? { ...n, model: "virtio" } : n)),
        }
      : current;
  switch (preset) {
    case "windows":
      return windows11(base);
    case "linux":
      return {
        ...base,
        cores: "2",
        memory: "2048",
        guestAgent: true,
        cloudInit: { ...base.cloudInit, enabled: true },
      };
    default:
      return base;
  }
}
