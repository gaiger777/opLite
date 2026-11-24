// 문제(계획 71 설계 §7.5): 브라우저 문제는 폼의 `validate()` 오류 열쇠를 칸으로 옮긴 것 + 놓은 뒤 바뀐 세상(지운 풀·
// 네트워크, 못 쓰게 된 노드 — 놓기 규칙의 검사를 놓인 블록에 다시 물음) + 용량·할당량 경고. 서버 문제는 미리 확인의
// `problems[]` 그대로다. 칸마다 가장 나쁜 수준이 블록 테두리가 된다.
import type { Reason, Verdict } from "@/dnd/types";
import {
  cpuCheck,
  isoCheck,
  networkCheck,
  nodeCheck,
  poolCheck,
  QUOTA_KEY,
  quotaOver,
  volumeCheck,
} from "@/vmBuilder/model/accepts";
import { canCreateIn, memoryFit, poolFree, USER_NETWORK, zones } from "@/vmBuilder/model/parts";
import { sdnOf, type VmCheck } from "@/vmBuilder/model/requests";
import type { BuilderCtx, BuilderState } from "@/vmBuilder/model/state";
import { userDataWarning } from "@/vms/cloudInit";
import { gibToBytes, haUnavailable, validate } from "@/vms/form";

export interface Problem {
  /** 칸 id, 또는 `name`(이름 칸)·`quota`·`spec`(그 밖) */
  at: string;
  /** 번역 키(브라우저) 또는 서버 오류 `code` */
  key: string;
  values?: Reason["values"];
  /** 서버가 준 설명(서버 문제만) */
  message?: string;
  level: "error" | "warn";
  source: "browser" | "server";
}

/** 폼 오류 열쇠(`validate()`)의 칸 */
export function areaOf(key: string): string {
  const [head = "", n] = key.split(".");
  switch (head) {
    case "name":
      return "name";
    case "disks":
    case "cdroms":
    case "nics":
      return n !== undefined && /^\d+$/.test(n) ? `${head}.${n}` : head;
    case "ci":
      return "cloudInit";
    case "tpm":
    case "secureBoot":
    case "schedule":
    case "replication":
      return "vm";
    case "maxRestart":
    case "maxRelocate":
    case "haPriority":
    case "guestWatchSecs":
      return "protection";
    default:
      return "compute";
  }
}

const GIB = 1024 ** 3;

/** 겹침을 가리는 열쇠: 서버 문제는 같은 자리·같은 코드를 하나로(설계 §9.2), 브라우저 문제는 값까지 같을 때만 */
const keyOf = (p: Problem) =>
  p.source === "server"
    ? `s\u0000${p.at}\u0000${p.key}`
    : `b\u0000${p.at}\u0000${p.key}\u0000${JSON.stringify(p.values ?? null)}`;

function fromVerdict(at: string, v: Verdict): Problem | null {
  if (!v.ok) {
    return v.reason
      ? { at, key: v.reason.key, values: v.reason.values, level: "error", source: "browser" }
      : null;
  }
  return v.warn
    ? { at, key: v.warn.key, values: v.warn.values, level: "warn", source: "browser" }
    : null;
}

/** 브라우저 문제(즉시) */
export function browserProblems(s: BuilderState, ctx: BuilderCtx): Problem[] {
  const v = s.values;
  const out: Problem[] = [];
  const add = (p: Problem | null) => {
    if (p) {
      out.push(p);
    }
  };
  // 템플릿 바탕은 배포가 디스크를 만들므로 폼의 볼륨 검사(이름·풀)를 하지 않는다.
  const template = s.base.kind === "template";
  const checked = template ? { ...v, disks: [] } : v;
  const errors = validate(
    checked,
    true,
    ctx.volumes.map((x) => x.name),
    sdnOf(s, ctx),
  );
  for (const [key, msg] of Object.entries(errors)) {
    out.push({ at: areaOf(key), key: msg, level: "error", source: "browser" });
  }
  if (v.name !== "" && ctx.vms.some((x) => x.name === v.name)) {
    add({
      at: "name",
      key: "vmBuilder.problem.nameTaken",
      values: { name: v.name },
      level: "error",
      source: "browser",
    });
  }
  if (template && !ctx.vms.some((x) => x.id === (s.base.kind === "template" ? s.base.id : ""))) {
    add({
      at: "vm",
      key: "vmBuilder.reason.gone",
      values: { name: s.base.kind === "template" ? s.base.id : "" },
      level: "error",
      source: "browser",
    });
  }
  // 놓은 뒤 바뀐 세상
  v.disks.forEach((d, i) => {
    if (template) {
      // 배포의 디스크 크기는 늘리기만 된다(배포 대화상자와 같은 검사).
      if (d.sizeGib.trim() !== "") {
        const bytes = gibToBytes(d.sizeGib.trim());
        const base = ctx.volumes.find((x) => x.name === d.volume)?.spec.size_bytes ?? 0;
        const key =
          bytes === undefined ? "vmForm.errors.size" : bytes <= base ? "volumes.errors.grow" : null;
        if (key) {
          add({ at: `disks.${i}`, key, level: "error", source: "browser" });
        }
      }
      return;
    }
    const at = `disks.${i}`;
    if (d.kind === "new") {
      if (d.pool !== "") {
        const check = poolCheck(d.pool, ctx, v.node);
        add(fromVerdict(at, check));
        const pool = ctx.pools.find((p) => p.name === d.pool);
        const free = pool && poolFree(pool, ctx, v.node);
        const bytes = gibToBytes(d.sizeGib);
        if (check.ok && free !== undefined && bytes !== undefined && bytes > free) {
          add({
            at,
            key: "vmBuilder.warn.poolFull",
            values: { name: d.pool, free: Math.floor(free / GIB) },
            level: "warn",
            source: "browser",
          });
        }
      }
    } else if (d.volume !== "") {
      add(fromVerdict(at, volumeCheck(d.volume, ctx, v.node)));
    }
  });
  v.cdroms.forEach((c, i) => add(fromVerdict(`cdroms.${i}`, isoCheck(c.iso, ctx, v))));
  v.nics.forEach((n, i) => {
    const at = `nics.${i}`;
    add(fromVerdict(at, networkCheck(n.network, ctx)));
    for (const g of n.securityGroups) {
      if (!ctx.securityGroups.some((x) => x.name === g)) {
        add({
          at,
          key: "vmBuilder.reason.gone",
          values: { name: g },
          level: "error",
          source: "browser",
        });
      }
    }
    if (n.network === USER_NETWORK && n.securityGroups.length > 0) {
      add({ at, key: "vmBuilder.reason.userNoSg", level: "error", source: "browser" });
    }
  });
  add(fromVerdict("compute", cpuCheck(v.model, ctx, v.node)));
  if (v.node !== "") {
    add(fromVerdict("placement", nodeCheck(v.node, s, ctx, s.preset === "windows" || template)));
  } else if (memoryFit(ctx, "", Number(v.memory)) === "over") {
    add({
      at: "compute",
      key: "vmBuilder.warn.sizeMemory",
      values: { memory: Number(v.memory) },
      level: "warn",
      source: "browser",
    });
  }
  if (s.zone && ctx.nodes && !zones(ctx.nodes, s.zone.key).some((z) => z.value === s.zone!.value)) {
    add({
      at: "placement",
      key: "vmBuilder.reason.zoneEmpty",
      values: { name: s.zone.value },
      level: "error",
      source: "browser",
    });
  }
  if (v.ha) {
    const off = haUnavailable(ctx.nodes ?? undefined, v.node);
    if (off) {
      add({
        at: "protection",
        key: off.key,
        values: off.values,
        level: "error",
        source: "browser",
      });
    }
    if (v.haGroup !== "" && !ctx.haGroups.some((g) => g.name === v.haGroup)) {
      add({
        at: "protection",
        key: "vmBuilder.reason.gone",
        values: { name: v.haGroup },
        level: "error",
        source: "browser",
      });
    }
  }
  if (v.folder !== "" && !ctx.folders.some((f) => f.id === v.folder)) {
    add({
      at: "folder",
      key: "vmBuilder.reason.gone",
      values: { name: v.folder },
      level: "error",
      source: "browser",
    });
  } else if (!canCreateIn(ctx, v.folder)) {
    add({ at: "folder", key: "vmBuilder.reason.permFolder", level: "error", source: "browser" });
  }
  const quota = quotaOver(ctx, v);
  if (quota) {
    add({
      at: "quota",
      key: `vmBuilder.warn.${QUOTA_KEY[quota.what]}`,
      values: { path: quota.path },
      level: "warn",
      source: "browser",
    });
  }
  if (userDataWarning(v.cloudInit)) {
    add({ at: "cloudInit", key: "vmBuilder.warn.userData", level: "warn", source: "browser" });
  }
  if (v.disks.length === 0 && v.cdroms.length === 0) {
    add({ at: "disks", key: "vmBuilder.warn.noBoot", level: "warn", source: "browser" });
  }
  return out;
}

/** 서버 미리 확인의 문제(그대로 — 수준이 없으면 오류) */
export function serverProblems(check: VmCheck | null | undefined): Problem[] {
  return (check?.problems ?? []).map((p) => ({
    at: p.at,
    key: p.code,
    message: p.message,
    level: p.level ?? "error",
    source: "server" as const,
  }));
}

/** 모든 문제: 브라우저 + 서버, 겹친 것은 하나로(먼저 것) */
export function problems(s: BuilderState, ctx: BuilderCtx, check?: VmCheck | null): Problem[] {
  const seen = new Set<string>();
  return [...browserProblems(s, ctx), ...serverProblems(check)].filter((p) => {
    const k = keyOf(p);
    if (seen.has(k)) {
      return false;
    }
    seen.add(k);
    return true;
  });
}

/**
 * 칸의 가장 나쁜 수준(블록 테두리). 목록 칸(`disks`)은 그 줄들(`disks.0`)의 문제도 담는다. 문제가 없으면 null
 */
export function worst(list: readonly Problem[], at: string): "error" | "warn" | null {
  const mine = list.filter((p) => p.at === at || p.at.startsWith(`${at}.`));
  return mine.some((p) => p.level === "error") ? "error" : mine.length > 0 ? "warn" : null;
}

/** 만들기를 막는 첫 문제(오류) — 단추 옆 까닭. 없으면 null */
export function blocking(list: readonly Problem[]): Problem | null {
  return list.find((p) => p.level === "error") ?? null;
}
