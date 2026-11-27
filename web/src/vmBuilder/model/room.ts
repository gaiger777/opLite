// 놓일 노드와 그 노드의 여유(계획 77-5 — 진단 §4.3 "요약·검사" 칸의 여유 막대, vSphere 호환성 패널): 고른 노드, 아니면
// 서버 미리 확인이 고른 노드(켜진 채 `node`, 꺼진 채 `start_node`). 서버가 답했는데 노드가 없으면 "놓을 노드 없음"(막대 대신
// 까닭)이고, 답을 기다리는 동안(묻지 않을 때도)만 메모리가 가장 많이 남은 노드(어림)다. 여유는 서버 배치와 같은 기준이다 —
// 그 노드에서 켜기로 된 VM(`spec.node`가 그 노드이고 `spec.power`가 running, 서버 `placed_on`)의 vCPU와 할당 메모리
// (`/placement`의 `mem_allocated_mib` — 같은 VM들), 새 디스크가 갈 풀의 남은 용량에 이 VM을 더한 것이다.
import type { Node } from "@/api/types";
import { isTemplate } from "@/templates/templates";
import { freeMemory, nodeBlocked, poolFree } from "@/vmBuilder/model/parts";
import type { VmCheck } from "@/vmBuilder/model/requests";
import type { BuilderCtx, BuilderState } from "@/vmBuilder/model/state";
import { gibToBytes } from "@/vms/form";

export type TargetHow = "chosen" | "server" | "guess";

export type Target =
  | { node: Node; how: TargetHow }
  /**
   * 서버가 답했지만 노드가 없다: 놓을 노드가 없음(`placement` 오류 — 그 문제, 꺼진 채면 켤 때 놓을 노드 없음) 또는 다른 오류
   * 때문에 서버가 노드를 고르지 않음(`other`)
   */
  | { node: null; how: "none"; problem?: VmCheck["problems"][number]; other?: boolean };

/**
 * 이 VM이 놓일 노드(모르면 null — 노드를 읽지 못함). `check`는 지금 값에 대한 서버 답(없거나 지난 것이면 null)이다.
 */
export function targetNode(s: BuilderState, ctx: BuilderCtx, check: VmCheck | null): Target | null {
  const nodes = ctx.nodes;
  if (!nodes) {
    return null;
  }
  const chosen = nodes.find((n) => n.id === s.values.node);
  if (chosen) {
    return { node: chosen, how: "chosen" };
  }
  const named = check?.node ?? check?.start_node;
  const server = named ? nodes.find((n) => n.name === named) : undefined;
  if (server) {
    return { node: server, how: "server" };
  }
  if (check) {
    // 노드를 읽는 사용자에게 서버는 이름을 준다 — 없으면 놓지 못했다.
    const problem = check.problems.find((p) => p.at === "placement" && p.level === "error");
    return problem || check.ok
      ? { node: null, how: "none", ...(problem && { problem }) }
      : { node: null, how: "none", other: true };
  }
  let best: { node: Node; free: number } | null = null;
  for (const n of nodes) {
    const free = nodeBlocked(n) ? undefined : freeMemory(ctx, n.id);
    if (free !== undefined && (!best || free > best.free)) {
      best = { node: n, free };
    }
  }
  return best && { node: best.node, how: "guess" as const };
}

/** 막대 하나: 이미 쓴 것, 이 VM이 더할 것, 전체(단위는 막대마다 — vCPU·MiB·바이트) */
export interface Bar {
  used: number;
  add: number;
  total: number;
}

export interface Room {
  cpu: Bar;
  memory?: Bar;
  disk?: Bar & {
    pool: string;
    /** 새 디스크가 가는 다른 풀 수(막대는 가장 많이 가는 풀 하나) */ others: number;
  };
}

const vcpus = (cores: string, sockets: number) => sockets * (Number(cores) || 0);

/** 노드 `node`에 이 VM을 놓으면 쓰는 양 */
export function nodeRoom(s: BuilderState, ctx: BuilderCtx, node: Node): Room {
  const v = s.values;
  // 서버 `placed_on`과 같다: 그 노드에서 켜기로 된 VM
  const placed = ctx.vms.filter(
    (x) => x.spec.node === node.id && x.spec.power === "running" && !isTemplate(x),
  );
  const cpu: Bar = {
    used: placed.reduce((n, x) => n + (x.spec.cpu.sockets ?? 1) * x.spec.cpu.cores, 0),
    add: vcpus(v.cores, v.sockets),
    total: node.status.cpus,
  };
  const load = ctx.loads?.find((l) => l.node === node.id);
  const memory = load && {
    used: load.mem_allocated_mib,
    add: Number(v.memory) || 0,
    total: load.mem_total_mib,
  };
  // 새 디스크가 가장 많이 가는 풀(템플릿 바탕은 배포가 정한다)
  const byPool = new Map<string, number>();
  if (s.base.kind === "blank") {
    for (const d of v.disks) {
      if (d.kind === "new" && d.pool) {
        byPool.set(d.pool, (byPool.get(d.pool) ?? 0) + (gibToBytes(d.sizeGib) ?? 0));
      }
    }
  }
  const [poolName, add] = [...byPool].sort((a, b) => b[1] - a[1])[0] ?? [];
  const pool = ctx.pools.find((p) => p.name === poolName);
  const state = pool && node.status.pools?.[pool.name];
  const free = pool && poolFree(pool, ctx, node.id);
  const disk =
    pool && state && free !== undefined && state.total_bytes > 0
      ? {
          pool: pool.name,
          others: byPool.size - 1,
          used: state.total_bytes - free,
          add: add ?? 0,
          total: state.total_bytes,
        }
      : undefined;
  return { cpu, ...(memory && { memory }), ...(disk && { disk }) };
}

/** 쓴 것 + 더할 것이 전체의 몇 %인지(전체가 0이면 0) */
export function percent(b: Bar): number {
  return b.total > 0 ? Math.round(((b.used + b.add) / b.total) * 100) : 0;
}
