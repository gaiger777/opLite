// 놓기 규칙(계획 71 설계 §7.3): 부품·블록을 이 칸에 놓을 수 있는지. 위에서부터 첫 것으로 막는다 — 종류가 맞지 않음
// (까닭 없음 — 그 칸은 밝히지 않음), 템플릿 바탕의 개수 바꾸기, 개수, 권한, 자리. 용량(풀·노드 메모리·할당량)은 막지
// 않고 경고로 놓게 둔다(서버가 정한다). 같은 검사를 이미 놓은 블록에 다시 물어 문제(`problems`)를 만든다.
import type { Node } from "@/api/types";
import type { Reason, Verdict } from "@/dnd/types";
import { hardViolations, type RuleSubject } from "@/placement/hard";
import { MAX_NIC_SECURITY_GROUPS } from "@/security-groups/rules";
import { isShared, usableOn } from "@/storage/pools";
import {
  canAllocate,
  canAssign,
  freeMemory,
  memoryFit,
  NEW_DISK_GIB,
  nodeBlocked,
  type Part,
  poolFree,
  SIZES,
  USER_NETWORK,
} from "@/vmBuilder/model/parts";
import { type Block, listOf, slotOf } from "@/vmBuilder/model/slots";
import type { BuilderCtx, BuilderState } from "@/vmBuilder/model/state";
import {
  firmwareUnavailable,
  gibToBytes,
  haActive,
  haCapable,
  MAX_CDROMS,
  MAX_NICS,
  Q35_SATA_PORTS,
  type VmFormValues,
} from "@/vms/form";

export type Thing = Part | Block;

const NO: Verdict = { ok: false };
const OK: Verdict = { ok: true };
const no = (key: string, values?: Reason["values"]): Verdict => ({
  ok: false,
  reason: { key: `vmBuilder.reason.${key}`, values },
});
const warn = (key: string, values?: Reason["values"]): Verdict => ({
  ok: true,
  warn: { key: `vmBuilder.warn.${key}`, values },
});

/** 부품 종류마다 붙는 칸 종류 */
const FITS: Record<Part["kind"], readonly string[]> = {
  size: ["compute"],
  cpu: ["compute"],
  os: ["vm"],
  template: ["vm"],
  pool: ["disks", "disk"],
  volume: ["disks"],
  iso: ["cdroms", "cdrom"],
  network: ["nics", "nic"],
  sg: ["nic"],
  addr: ["nic"],
  ci: ["cloudInit"],
  auto: ["placement"],
  node: ["placement"],
  zone: ["placement"],
  ha: ["protection"],
  haGroup: ["protection"],
  folder: ["folder"],
};

const GIB = 1024 ** 3;

/** 규칙에 비춰 볼 만들 VM(이름·HA). 레이블은 없다(빌더는 레이블을 주지 않는다). */
export function subject(v: VmFormValues): RuleSubject {
  return { name: v.name, haActive: haActive(v), haGroup: v.haGroup || undefined };
}

/** q35 내장 AHCI 포트를 SATA 디스크·CD·cloud-init이 나눠 쓴다(폼 `cdromErrors`와 같은 셈) — 더한 뒤 넘는지 */
function sataOver(
  v: VmFormValues,
  add: { sata?: number; cdroms?: number; seed?: boolean },
): boolean {
  const seed = v.cloudInit.enabled || add.seed ? 1 : 0;
  const cdroms = v.cdroms.length + (add.cdroms ?? 0);
  const sata = v.disks.filter((d) => d.bus === "sata").length + (add.sata ?? 0);
  return v.machine === "q35" && (cdroms > 0 || seed > 0) && sata + cdroms + seed > Q35_SATA_PORTS;
}

/** VM이 쓰는 양(할당량 어림): vCPU, 메모리, 새 디스크 바이트 */
export function totals(v: VmFormValues): { vcpus: number; memory_mib: number; disk_bytes: number } {
  return {
    vcpus: v.sockets * (Number(v.cores) || 0),
    memory_mib: Number(v.memory) || 0,
    disk_bytes: v.disks.reduce(
      (sum, d) => sum + (d.kind === "new" ? (gibToBytes(d.sizeGib) ?? 0) : 0),
      0,
    ),
  };
}

/** 할당량을 넘는 칸(폴더 사슬의 첫 넘음): 이 VM을 더한 사용량이 한도를 넘음. 사용량을 모르면 null */
export function quotaOver(
  ctx: BuilderCtx,
  v: VmFormValues,
  extraDisk = 0,
): { what: "vcpus" | "memory_mib" | "disk_bytes"; path: string } | null {
  const mine = totals(v);
  mine.disk_bytes += extraDisk;
  for (const limit of ctx.usage?.limits ?? []) {
    for (const what of ["vcpus", "memory_mib", "disk_bytes"] as const) {
      const max = limit.quota[what];
      if (max != null && limit.usage[what] + mine[what] > max) {
        return { what, path: limit.path };
      }
    }
  }
  return null;
}

/** 할당량 넘음 경고 열쇠(칸마다) */
export const QUOTA_KEY = {
  vcpus: "quotaVcpus",
  memory_mib: "quotaMemory",
  disk_bytes: "quotaDisk",
} as const;

function quotaWarn(ctx: BuilderCtx, v: VmFormValues, extraDisk = 0): Verdict | null {
  const over = quotaOver(ctx, v, extraDisk);
  return over ? warn(QUOTA_KEY[over.what], { path: over.path }) : null;
}

/** 새 디스크를 둘 풀: 있음·권한·고른 노드에서 씀 */
export function poolCheck(pool: string, ctx: BuilderCtx, node: string): Verdict {
  const found = ctx.pools.find((p) => p.name === pool);
  if (!found) {
    return no("gone", { name: pool });
  }
  if (!canAllocate(ctx, found)) {
    return no("permPool", { name: pool });
  }
  const chosen = ctx.nodes?.find((n) => n.id === node);
  if (chosen && !usableOn(found, chosen)) {
    return no("poolNotOnNode", { name: pool, node: chosen.name });
  }
  return OK;
}

/** 새 디스크 크기가 풀 남은 용량보다 크면 경고 */
function poolSpace(pool: string, sizeGib: string, ctx: BuilderCtx, node: string): Verdict | null {
  const found = ctx.pools.find((p) => p.name === pool);
  const free = found && poolFree(found, ctx, node);
  const bytes = gibToBytes(sizeGib);
  return free !== undefined && bytes !== undefined && bytes > free
    ? warn("poolFull", { name: pool, free: Math.floor(free / GIB) })
    : null;
}

/** 붙일 있는 볼륨: 있음·권한(볼륨 풀의 `storage.allocate`)·로컬 풀이면 고른 노드에 있음 */
export function volumeCheck(volume: string, ctx: BuilderCtx, node: string): Verdict {
  const found = ctx.volumes.find((v) => v.name === volume);
  if (!found) {
    return no("gone", { name: volume });
  }
  const pool = ctx.pools.find((p) => p.name === found.spec.pool);
  if (!canAllocate(ctx, pool)) {
    return no("permVolume", { name: volume });
  }
  const chosen = ctx.nodes?.find((n) => n.id === node);
  if (chosen && pool && !isShared(pool) && found.status.node && found.status.node !== chosen.id) {
    return no("volumeElsewhere", { name: volume, node: chosen.name });
  }
  return OK;
}

/** NIC 네트워크: 있음(`user` 말고)·권한 */
export function networkCheck(network: string, ctx: BuilderCtx): Verdict {
  if (network !== USER_NETWORK && !ctx.networks.some((n) => n.name === network)) {
    return no("gone", { name: network });
  }
  return canAssign(ctx, network) ? OK : no("permNetwork", { name: network });
}

/** CD에 넣을 ISO: 있음, HA면 공유 풀 */
export function isoCheck(iso: string, ctx: BuilderCtx, v: VmFormValues): Verdict {
  if (iso === "") {
    return OK;
  }
  const found = ctx.volumes.find((x) => x.name === iso);
  if (!found) {
    return no("gone", { name: iso });
  }
  const pool = ctx.pools.find((p) => p.name === found.spec.pool);
  return haActive(v) && pool && !isShared(pool) ? no("isoLocalHa", { name: iso }) : OK;
}

/** 머신의 아키텍처(`q35` → x86_64)와 노드가 다르면 참 */
function archMismatch(node: Node, machine: VmFormValues["machine"]): boolean {
  const arch = machine === "q35" ? "x86_64" : machine === "virt" ? "aarch64" : null;
  return !!arch && !!node.status.arch && node.status.arch !== arch;
}

/**
 * 노드에 놓기: 있음·온라인·유지보수·위트니스, 머신이 정해졌으면(Windows·템플릿) 아키텍처, HA면 펜싱, TPM이면 swtpm,
 * hard 배치 규칙(예상), 그리고 메모리가 남은 것보다 크면 경고.
 */
export function nodeCheck(
  id: string,
  s: BuilderState,
  ctx: BuilderCtx,
  fixedMachine: boolean,
): Verdict {
  const node = ctx.nodes?.find((n) => n.id === id);
  if (!ctx.nodes) {
    return OK;
  }
  if (!node) {
    return no("gone", { name: id });
  }
  const v = s.values;
  const blocked = nodeBlocked(node);
  if (blocked) {
    return { ok: false, reason: blocked };
  }
  if (fixedMachine && archMismatch(node, v.machine)) {
    return no("arch", { name: node.name });
  }
  if (haActive(v) && !haCapable(node)) {
    return { ok: false, reason: { key: "vmForm.haNodeOff", values: { node: node.name } } };
  }
  for (const cap of ["tpm", "secure_boot"] as const) {
    const off = (cap === "tpm" ? v.tpm : v.secureBoot) && firmwareUnavailable(ctx.nodes, id, cap);
    if (off) {
      return { ok: false, reason: off };
    }
  }
  const rules = hardViolations(subject(v), node, ctx.rules, ctx.vms, ctx.haGroups);
  if (rules.length > 0) {
    return no("hardRule", { name: node.name, rules: rules.join(", ") });
  }
  const free = freeMemory(ctx, id);
  return free !== undefined && Number(v.memory) > free
    ? warn("nodeMemory", { name: node.name, free })
    : OK;
}

/** CPU 모델을 고른 노드에서 켤 수 없음(`nodes[].can_run`) */
export function cpuCheck(model: string, ctx: BuilderCtx, node: string): Verdict {
  const fit = ctx.cpuModels?.find((m) => m.name === model)?.nodes?.find((f) => f.node === node);
  if (fit && !fit.can_run) {
    const name = ctx.nodes?.find((n) => n.id === node)?.name ?? node;
    return no("cpuCannotRun", {
      model,
      name,
      why: fit.reason ?? (fit.missing ?? []).join(" "),
    });
  }
  return OK;
}

/** 블록(디스크·CD·NIC 줄)을 놓기: 같은 목록 안 다른 자리(순서 바꾸기)·목록 끝·빼기 */
function acceptsBlock(b: Block, at: string, s: BuilderState): Verdict {
  const slot = slotOf(at);
  if (!slot) {
    return NO;
  }
  const list = listOf(slot);
  const fixed = s.base.kind === "template" && b.list !== "cdroms";
  if (slot.kind === "trash") {
    return fixed ? no("templateFixed") : OK;
  }
  if (!list || list.list !== b.list) {
    return NO;
  }
  const last = s.values[b.list].length - 1;
  const to = list.index ?? last;
  if (to === b.index) {
    return NO;
  }
  return fixed ? no("templateFixed") : OK;
}

/** 부품·블록을 칸 `at`에 놓을 수 있는지 */
export function accepts(t: Thing, at: string, s: BuilderState, ctx: BuilderCtx): Verdict {
  if (t.kind === "block") {
    return acceptsBlock(t, at, s);
  }
  const slot = slotOf(at);
  if (!slot || !FITS[t.kind].includes(slot.kind)) {
    return NO;
  }
  const v = s.values;
  const index = "index" in slot ? slot.index : null;
  // 새 디스크 블록에만 풀을 바꿔 놓는다(있는 볼륨은 풀이 정해짐).
  if (t.kind === "pool" && index !== null && v.disks[index]?.kind !== "new") {
    return NO;
  }
  if (t.blocked) {
    return { ok: false, reason: t.blocked };
  }
  const template = s.base.kind === "template";
  // 배포는 디스크·NIC·CD 수와 장치를 바꾸지 못한다("만든 뒤 고치기에서").
  if (
    template &&
    (t.kind === "os" ||
      t.kind === "sg" ||
      t.kind === "volume" ||
      t.kind === "iso" ||
      (t.kind === "pool" && index === null) ||
      (t.kind === "network" && index === null))
  ) {
    return no("templateFixed");
  }
  const windows = s.preset === "windows";
  switch (t.kind) {
    case "size": {
      const next = {
        ...v,
        sockets: 1,
        cores: String(SIZES[t.size].cores),
        memory: String(SIZES[t.size].memory),
      };
      if (memoryFit(ctx, v.node, SIZES[t.size].memory) === "over") {
        return warn("sizeMemory", { memory: SIZES[t.size].memory });
      }
      return quotaWarn(ctx, next) ?? OK;
    }
    case "cpu":
      return cpuCheck(t.model, ctx, v.node);
    case "os":
      // Windows 프리셋은 q35다.
      if (t.os === "windows" && ctx.nodes) {
        const chosen = ctx.nodes.find((n) => n.id === v.node);
        if (chosen ? archMismatch(chosen, "q35") : v.machine === "virt") {
          return no("windowsArch");
        }
      }
      return OK;
    case "template": {
      const vm = ctx.vms.find((x) => x.id === t.vm);
      return vm ? OK : no("gone", { name: t.name });
    }
    case "pool": {
      if (index === null && windows && sataOver(v, { sata: 1 })) {
        return no("sataPorts", { max: Q35_SATA_PORTS });
      }
      const check = poolCheck(t.pool, ctx, v.node);
      if (!check.ok) {
        return check;
      }
      const size = index === null ? NEW_DISK_GIB : v.disks[index]!.sizeGib;
      return (
        poolSpace(t.pool, size, ctx, v.node) ??
        (index === null ? quotaWarn(ctx, v, gibToBytes(size) ?? 0) : null) ??
        OK
      );
    }
    case "volume":
      if (windows && sataOver(v, { sata: 1 })) {
        return no("sataPorts", { max: Q35_SATA_PORTS });
      }
      return volumeCheck(t.volume, ctx, v.node);
    case "iso":
      if (index === null) {
        if (v.cdroms.length >= MAX_CDROMS) {
          return no("maxCdroms", { max: MAX_CDROMS });
        }
        if (sataOver(v, { cdroms: 1 })) {
          return no("sataPorts", { max: Q35_SATA_PORTS });
        }
      }
      return isoCheck(t.iso, ctx, v);
    case "network":
      if (index === null && v.nics.length >= MAX_NICS) {
        return no("maxNics", { max: MAX_NICS });
      }
      return networkCheck(t.network, ctx);
    case "sg": {
      const nic = v.nics[index!]!;
      if (nic.network === USER_NETWORK) {
        return no("userNoSg");
      }
      if (
        !nic.securityGroups.includes(t.sg) &&
        nic.securityGroups.length >= MAX_NIC_SECURITY_GROUPS
      ) {
        return no("maxSgs", { max: MAX_NIC_SECURITY_GROUPS });
      }
      return OK;
    }
    case "addr": {
      const sdn = ctx.networks.find((n) => n.name === v.nics[index!]!.network)?.spec.sdn != null;
      return t.mode === "static" && sdn ? no("sdnStatic") : OK;
    }
    case "ci":
      if (!v.cloudInit.enabled && sataOver(v, { seed: true })) {
        return no("sataPorts", { max: Q35_SATA_PORTS });
      }
      return t.snippet === "packages" && v.cloudInit.userData !== "" ? warn("userData") : OK;
    case "auto":
      return OK;
    case "node":
      return nodeCheck(t.node, s, ctx, windows || template);
    case "zone":
      return OK;
    case "ha": {
      const local = v.cdroms.find((c) => {
        const pool = ctx.pools.find(
          (p) => p.name === ctx.volumes.find((x) => x.name === c.iso)?.spec.pool,
        );
        return pool !== undefined && !isShared(pool);
      });
      return local ? no("isoLocalHa", { name: local.iso }) : OK;
    }
    case "haGroup":
    case "folder":
      return OK;
  }
}
