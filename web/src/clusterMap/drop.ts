// 지도 끌어 옮기기의 놓을 자리 판정(계획 71 설계 §10.2, 순수 — "예상"): 켜진 VM을 이 노드에 놓으면 옮기기 대화상자를
// 열 수 있는지(ok), 열되 알릴 것이 있는지(warn), 열지 않는지(blocked)와 그 까닭. 위에서부터 첫 것이다. 최종은 서버다
// (대화상자 → `POST /vms/{key}/migrate`의 409).
import type { components } from "@/api/schema";
import type { HaGroup, Node, PlacementRule, Task, Vm } from "@/api/types";
import type { Reason, Verdict } from "@/dnd/types";
import { formatBytes, formatMib } from "@/lib/format";
import { isFrozen } from "@/nodes/NodeActions";
import { hardViolations, subjectOf } from "@/placement/hard";
import { availability } from "@/vms/actions";
import { migrationTargets } from "@/vms/migrate";

type Schemas = components["schemas"];

export type DropLevel = "ok" | "warn" | "blocked";

export interface DropCheck {
  level: DropLevel;
  reason?: Reason;
}

/** 판정에 쓰는 자료. 없는 것(읽지 못함·아직 받지 못함)은 그 줄을 판정하지 않는다. */
export interface DropCtx {
  /** 규칙의 다른 VM 자리(`hardViolations`) */
  vms: readonly Vm[];
  rules?: readonly PlacementRule[];
  groups?: readonly HaGroup[];
  /** `GET /vms/{key}/cpu`의 노드마다 맞음(노드 이름) */
  cpu?: readonly Schemas["CpuNodeFit"][];
  /** `GET /vms/{key}/numa`의 노드마다 맞음(NUMA를 쓰는 VM만) */
  numa?: readonly Schemas["NumaNodeFit"][];
  /** 노드 id → 할당하지 않은 메모리(MiB, `/placement`) */
  freeMib?: ReadonlyMap<string, number>;
  /** 대상으로 복사할 로컬 디스크 바이트(`localDisks`) */
  copyBytes?: number;
  /** hard 규칙을 넘길 수 있음(대화상자의 "규칙 넘기" — `placement-rule` 쓰기) */
  mayForce: boolean;
}

const r = (key: string, values?: Reason["values"]): Reason => ({
  key: `clusterMap.drag.${key}`,
  values,
});

/** VM을 `node`에 놓으면: 같은 노드 → 노드 상태 → CPU → NUMA → hard 규칙 → 메모리 → 디스크 복사 */
export function dropCheck(vm: Vm, node: Node, ctx: DropCtx): DropCheck {
  const blocked = (reason: Reason): DropCheck => ({ level: "blocked", reason });
  const name = node.name;
  if ((vm.spec.node ?? vm.status.node) === node.id) {
    return blocked(r("here"));
  }
  if (migrationTargets(vm, [node]).length === 0) {
    if (node.spec.witness) {
      return blocked(r("witness", { node: name }));
    }
    if (!node.status.online) {
      return blocked(r(isFrozen(node) ? "frozen" : "offline", { node: name }));
    }
    if (node.spec.maintenance) {
      return blocked(r("maintenance", { node: name }));
    }
    return blocked(r("arch", { node: name, arch: node.status.arch ?? "?" }));
  }
  const cpu = ctx.cpu?.find((f) => f.node === name);
  if (cpu?.can_run === false) {
    return blocked(r("cpu", { node: name, why: cpu.reason ?? (cpu.missing ?? []).join(", ") }));
  }
  const numa = ctx.numa?.find((f) => f.node === name);
  if (numa?.fits === false) {
    return blocked(r("numa", { node: name, why: numa.reason ?? "" }));
  }
  if (ctx.rules) {
    const broken = hardViolations(subjectOf(vm), node, ctx.rules, ctx.vms, ctx.groups ?? []);
    if (broken.length > 0) {
      const reason = r(ctx.mayForce ? "rulesForce" : "rules", { rules: broken.join(", ") });
      return ctx.mayForce ? { level: "warn", reason } : blocked(reason);
    }
  }
  const free = ctx.freeMib?.get(node.id);
  if (free !== undefined && free < vm.spec.memory_mib) {
    return {
      level: "warn",
      reason: r("memory", {
        free: formatMib(Math.max(0, free)),
        need: formatMib(vm.spec.memory_mib),
      }),
    };
  }
  if (ctx.copyBytes) {
    return { level: "warn", reason: r("copy", { size: formatBytes(ctx.copyBytes) }) };
  }
  return { level: "ok" };
}

/** 끌기 틀의 판정으로 */
export function toVerdict(c: DropCheck): Verdict {
  return c.level === "blocked" ? { ok: false, reason: c.reason } : { ok: true, warn: c.reason };
}

/**
 * 이 VM을 끌 수 있는지(설계 §10.1): 켜져 있고 지금 옮길 수 있으며(`availability`) 사용자가 `vm.migrate`를 가짐. 아니면
 * `m`이 읽을 까닭 키.
 */
export function dragBlock(
  vm: Vm,
  tasks: readonly Task[],
  mayMigrate: boolean,
): { key: string; values?: Record<string, string> } | null {
  if (vm.status.phase === "stopped") {
    return { key: "clusterMap.drag.stopped" };
  }
  if (!mayMigrate) {
    return { key: "clusterMap.drag.denied", values: { vm: vm.name } };
  }
  const able = availability(vm, "migrate", tasks);
  if (!able.ok) {
    return { key: able.reason };
  }
  return vm.status.phase === "running" ? null : { key: "vms.reason.busy" };
}
