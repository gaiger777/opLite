import type { PendingChange, PendingReason, RunningHardware, Vm } from "@/api/types";
import { isStopped } from "@/vms/actions";

/**
 * 폼이 디스크·NIC를 바꾸는 방식. `free`: 만들기·멈춘 VM — 무엇이든. `live`: 핫플러그 배치로 켠 실행 중 VM —
 * 끝에 붙이기와 실행 중 붙인 것을 끝에서부터 떼기만(서버의 핫플러그 Task). `restart`: 배치 없이 켠 실행 중 VM —
 * 한 번 다시 켜야 붙이고 뗄 수 있다. `locked`: 하드웨어 기록이 없는 실행 중 VM(옛 서버, 켜는 중) — 지금처럼 잠근다.
 */
export type DeviceMode = "free" | "live" | "restart" | "locked";

export function deviceMode(vm: Vm | undefined): DeviceMode {
  if (vm === undefined || isStopped(vm)) {
    return "free";
  }
  const hw = vm.status.hardware;
  if (!hw) {
    return "locked";
  }
  return hw.layout ? "live" : "restart";
}

/** 실행 중에 붙인 장치인지(켤 때 단 장치 수 `boot_*` 뒤의 순번). 배치가 없으면 모두 켤 때 단 것이다. */
export function isHotAdded(vm: Vm, kind: "disk" | "nic", index: number): boolean {
  const layout = vm.status.hardware?.layout;
  if (!layout) {
    return false;
  }
  return index >= (kind === "disk" ? layout.boot_disks : layout.boot_nics);
}

/** 지금 값과 상한(지금 값보다 클 때만) */
export interface WithMax {
  now: number;
  max: number | null;
}

/** 도는 vCPU 수(배치가 없으면 소켓 × 코어)와 켤 때 정한 상한 */
export function runningVcpus(hw: RunningHardware): WithMax {
  const boot = (hw.cpu.sockets ?? 1) * hw.cpu.cores;
  const now = hw.layout?.vcpus ?? boot;
  const max = hw.cpu.max_vcpus ?? null;
  return { now, max: max !== null && max > now ? max : null };
}

/** 도는 메모리(켤 때 크기 + virtio-mem에 꽂은 크기, MiB)와 상한 */
export function runningMemory(hw: RunningHardware): WithMax {
  const now = hw.memory_mib + (hw.layout?.plugged_mib ?? 0);
  const max = hw.layout?.max_memory_mib ?? null;
  return { now, max: max !== null && max > hw.memory_mib ? max : null };
}

/** 실행 중 붙인 장치가 쓰는 루트 포트 수와 모든 포트 수. 배치가 없으면 null */
export function hotplugPorts(hw: RunningHardware): { used: number; ports: number } | null {
  const layout = hw.layout;
  if (!layout) {
    return null;
  }
  const used = (layout.disk_ports ?? []).length + (layout.nic_ports ?? []).length;
  return { used, ports: layout.ports };
}

/**
 * 폼 행 가운데 저장된 장치의 수. 실행 중에는 끝에서부터만 떼므로 저장된 행은 늘 `saved`의 앞부분이고 새 행은
 * 그 뒤에 붙는다.
 */
export function savedCount<R, S>(
  rows: readonly R[],
  saved: readonly S[],
  same: (r: R, s: S) => boolean,
): number {
  const n = rows.findIndex((r, i) => i >= saved.length || !same(r, saved[i]!));
  return n === -1 ? rows.length : n;
}

/**
 * 실행 중(`live`) 행 `index`를 뺄 수 있는지: 폼에서 더한 행(`index ≥ saved`)이거나, 저장된 행 가운데 마지막이면서
 * 실행 중 붙인 것(`index ≥ boot`). 서버는 가운데 떼기와 켤 때 단 장치 떼기를 받지 않는다.
 */
export function liveRemovable(index: number, saved: number, boot: number): boolean {
  return index >= saved || (index === saved - 1 && index >= boot);
}

/**
 * 메모리 스냅샷을 담을 수 있는 모양인지(서버의 `matches_boot`): 실행 중에 바꾼 vCPU·메모리·장치와 다음 재시작 때
 * 적용할 변경이 없다. 아니면 서버는 디스크만 담는다(메모리 스냅샷은 409 `snapshot_unsupported`).
 */
export function memoryMatchesBoot(vm: Vm): boolean {
  const hw = vm.status.hardware;
  if (!hw) {
    return true;
  }
  if ((hw.pending ?? []).length > 0) {
    return false;
  }
  const layout = hw.layout;
  if (!layout) {
    return true;
  }
  const boot = (hw.cpu.sockets ?? 1) * hw.cpu.cores;
  return (
    layout.vcpus === boot &&
    layout.plugged_mib === 0 &&
    (layout.disk_ports ?? []).length === 0 &&
    (layout.nic_ports ?? []).length === 0
  );
}

/** 다음 재시작 때 적용할 변경이 있다. */
export function needsRestart(vm: Vm): boolean {
  return (vm.status.hardware?.pending ?? []).length > 0;
}

/** 해 봤으나 실패한 변경(다시 적용할 수 있다) */
export function failedChanges(vm: Vm): PendingChange[] {
  return (vm.status.hardware?.pending ?? []).filter((c) => c.reason === "failed");
}

/** 보류 이유의 번역 키 */
export function pendingReasonKey(reason: PendingReason): string {
  return `vms.pendingReason.${reason}`;
}
