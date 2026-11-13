import type { components } from "@/api/schema";
import type { VmHa, VmSpec } from "@/api/types";

/** 서버의 기본 정책(`VmHa::default`). 옛 `ha: true`와 같다. */
export const DEFAULT_HA: VmHa = { state: "started", max_restart: 1, max_relocate: 1, priority: 50 };
/** 서버의 `MAX_HA_COUNT`: `max_restart`·`max_relocate`의 최댓값 */
export const MAX_HA_COUNT = 10;
/** 서버의 `MAX_HA_PRIORITY`: 복구 우선순위의 최댓값 */
export const MAX_HA_PRIORITY = 100;
/** 서버의 게스트 감시 시간 범위와 기본값(초) */
export const MIN_GUEST_WATCH_SECS = 30;
export const MAX_GUEST_WATCH_SECS = 3600;
export const DEFAULT_GUEST_WATCH_SECS = 60;

/** 서버의 `VmSpec::ha_active`: HA 정책이 있고(옛 `true`나 객체) `ignored`가 아닌지. */
export function haEnabled(spec: Pick<VmSpec, "ha">): boolean {
  const ha = spec.ha;
  if (typeof ha === "boolean" || ha === undefined) {
    return ha ?? false;
  }
  return ha.state !== "ignored";
}

/** `ha`(옛 bool이나 객체)의 정책. 없으면(`false`) null이고, 객체의 빠진 값은 기본이다. */
export function haPolicy(spec: Pick<VmSpec, "ha">): VmHa | null {
  const ha = spec.ha;
  if (typeof ha === "boolean" || ha === undefined) {
    return ha ? DEFAULT_HA : null;
  }
  return { ...DEFAULT_HA, ...ha };
}

/**
 * 보낼 `ha` 값: 정책이 없으면 `false`, 기본 정책이면 옛 모양 `true`, 아니면 객체. 서버도 이렇게 쓰므로
 * 단순 HA VM은 옛 노드가 남은 클러스터에서도 저장된다(기본이 아닌 객체는 모든 노드가 업그레이드돼야 한다).
 */
export function haSetting(policy: VmHa | null): components["schemas"]["HaSetting"] {
  if (!policy) {
    return false;
  }
  const isDefault =
    policy.state === DEFAULT_HA.state &&
    !policy.group &&
    policy.max_restart === DEFAULT_HA.max_restart &&
    policy.max_relocate === DEFAULT_HA.max_relocate &&
    policy.priority === DEFAULT_HA.priority &&
    !policy.guest_watch;
  if (isDefault) {
    return true;
  }
  const { group, guest_watch, ...rest } = policy;
  return { ...rest, ...(group && { group }), ...(guest_watch && { guest_watch }) };
}
