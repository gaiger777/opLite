import type { VmSpec } from "@/api/types";

/** 서버의 `VmSpec::ha_active`: HA 정책이 있고(옛 `true`나 객체) `ignored`가 아닌지. */
export function haEnabled(spec: Pick<VmSpec, "ha">): boolean {
  const ha = spec.ha;
  if (typeof ha === "boolean" || ha === undefined) {
    return ha ?? false;
  }
  return ha.state !== "ignored";
}
