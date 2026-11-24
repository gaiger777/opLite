// 부품 상자의 부품(계획 71 설계 §7.2): 지금 목록(watch)에서 매번 셈한다 — 다른 사람이 풀을 지우면 부품이 사라지고
// 이미 놓은 블록은 문제(`problems`)가 된다. 쓸 수 없는 부품도 보이고 `blocked`에 까닭이 있다(숨기면 "왜 없지?").
import type { Node, StoragePool, Volume } from "@/api/types";
import { objectTarget } from "@/auth/permissions";
import { archOf } from "@/cpu/cpu";
import { folderPath, folderTree } from "@/folders/folders";
import { choosableIsos } from "@/isos/isos";
import { isSdnNetwork } from "@/sdn/ips";
import { isShared, poolCapacity, usableOn } from "@/storage/pools";
import { isTemplate } from "@/templates/templates";
import type { BuilderCtx, BuilderState, Reason } from "@/vmBuilder/model/state";
import { firmwareUnavailable, haActive, haUnavailable } from "@/vms/form";

export type PartGroup =
  | "size"
  | "os"
  | "cpu"
  | "disk"
  | "media"
  | "template"
  | "network"
  | "cloudInit"
  | "placement"
  | "protection"
  | "folder";

/** 크기 프리셋(설계 §7.2): 작게 1 vCPU·1 GiB / 보통 2·4 / 크게 4·8 / 아주 크게 8·16 */
export const SIZES = {
  s: { cores: 1, memory: 1024 },
  m: { cores: 2, memory: 4096 },
  l: { cores: 4, memory: 8192 },
  xl: { cores: 8, memory: 16384 },
} as const;
export type Size = keyof typeof SIZES;

/** 풀에 놓은 새 디스크의 처음 크기(GiB) */
export const NEW_DISK_GIB = "20";

/** cloud-init 조각(사용자·SSH 키, 비밀번호, DNS, 패키지) */
export type Snippet = "user" | "password" | "dns" | "packages";

interface PartBase {
  /** 부품 id(`<종류>:<이름>`) — 끌기 틀의 항목 id */
  id: string;
  group: PartGroup;
  /** 리소스 이름(정해진 부품은 빈 글 — 화면이 종류로 이름을 붙인다) */
  name: string;
  /** 어디에도 놓을 수 없는 까닭(흐리게 보임) */
  blocked?: Reason;
}

export type Part = PartBase &
  (
    | { kind: "size"; size: Size; fit?: "fits" | "tight" | "over" }
    | { kind: "os"; os: "linux" | "windows" }
    | { kind: "cpu"; model: string; usable?: number }
    | { kind: "pool"; pool: string; shared: boolean; free?: number }
    | { kind: "volume"; volume: string; pool: string; size: number }
    | { kind: "iso"; iso: string; pool: string; shared: boolean }
    | { kind: "template"; vm: string; vcpus: number; memory: number; disks: number }
    | { kind: "network"; network: string; sdn: boolean }
    | { kind: "sg"; sg: string; rules: number }
    | { kind: "ci"; snippet: Snippet }
    | { kind: "addr"; mode: "dhcp" | "static" }
    | { kind: "auto" }
    | { kind: "node"; node: string; free?: number }
    | { kind: "zone"; value: string; nodes: number }
    | { kind: "ha" }
    | { kind: "haGroup"; haGroup: string }
    | { kind: "folder"; folder: string }
  );

export type PartOf<K extends Part["kind"]> = Extract<Part, { kind: K }>;

/** 사용자 모드 NAT 네트워크(목록에 없는 내장 네트워크) */
export const USER_NETWORK = "user";

/** 노드의 남은 메모리(MiB, `/placement`의 `mem_total_mib − mem_allocated_mib`). 모르면 undefined */
export function freeMemory(ctx: BuilderCtx, node: string): number | undefined {
  const load = ctx.loads?.find((l) => l.node === node);
  return load && load.mem_total_mib - load.mem_allocated_mib;
}

/** 놓을 수 있는 노드 가운데 가장 많이 남은 메모리(노드를 고르지 않았을 때) */
function bestFreeMemory(ctx: BuilderCtx): number | undefined {
  const free = (ctx.nodes ?? [])
    .filter((n) => !n.spec.witness && n.status.online && !n.spec.maintenance)
    .flatMap((n) => freeMemory(ctx, n.id) ?? []);
  return free.length ? Math.max(...free) : undefined;
}

/** 메모리 `mib`가 고른 노드(없으면 가장 여유 있는 노드)에 들어가는지: 남은 것의 3/4까지 들어감, 남은 것까지 빠듯함 */
export function memoryFit(
  ctx: BuilderCtx,
  node: string,
  mib: number,
): "fits" | "tight" | "over" | undefined {
  const free = node ? freeMemory(ctx, node) : bestFreeMemory(ctx);
  if (free === undefined) {
    return undefined;
  }
  return mib > free ? "over" : mib > free * 0.75 ? "tight" : "fits";
}

/** 풀의 남은 용량(바이트): 공유 풀은 하나, 로컬 풀은 고른 노드(없으면 가장 많이 남은 노드). 모르면 undefined */
export function poolFree(pool: StoragePool, ctx: BuilderCtx, node: string): number | undefined {
  const caps = poolCapacity(pool, ctx.nodes ?? []);
  if (!isShared(pool) && node) {
    const name = ctx.nodes?.find((n) => n.id === node)?.name;
    return caps.find((c) => c.node === name)?.free;
  }
  return caps.length ? Math.max(...caps.map((c) => c.free)) : undefined;
}

/** 볼륨을 만들 풀의 `storage.allocate`(서버 `require_pool`) */
export function canAllocate(ctx: BuilderCtx, pool: StoragePool | undefined): boolean {
  return !pool || ctx.perms.has("storage.allocate", objectTarget("storage-pool", pool));
}

/** NIC를 붙일 네트워크의 `network.assign`(서버 `require_networks` — `user`는 보지 않는다) */
export function canAssign(ctx: BuilderCtx, network: string): boolean {
  const found = ctx.networks.find((n) => n.name === network);
  return (
    network === USER_NETWORK ||
    !found ||
    ctx.perms.has("network.assign", objectTarget("network", found))
  );
}

/** 이 폴더에 VM을 만들 수 있는지(`vm.create`, 루트는 빈 글) */
export function canCreateIn(ctx: BuilderCtx, folder: string): boolean {
  return ctx.perms.has("vm.create", { type: "object", kind: "vm", folder: folder || null });
}

/** 붙일 수 있는 남는 디스크 볼륨(폼의 `freeVolumes` — 준비됨·어디에도 안 붙음, ISO 아님)에서 이미 놓은 것을 뺀 것 */
export function freeVolumes(ctx: BuilderCtx, s: BuilderState): Volume[] {
  const used = new Set(s.values.disks.map((d) => d.volume));
  return ctx.volumes.filter(
    (v) =>
      v.spec.content !== "iso" &&
      v.status.phase === "ready" &&
      !v.status.attached_to &&
      !used.has(v.name),
  );
}

/** 노드의 `zone` 레이블 값마다 노드 수(값 순) */
export function zones(nodes: readonly Node[], key = "zone"): { value: string; nodes: number }[] {
  const count = new Map<string, number>();
  for (const n of nodes) {
    const v = n.labels?.[key];
    if (v !== undefined && !n.spec.witness) {
      count.set(v, (count.get(v) ?? 0) + 1);
    }
  }
  return [...count]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([value, nodes]) => ({ value, nodes }));
}

/** 템플릿 VM의 아키텍처가 고른 노드와 다른지 */
function archOff(ctx: BuilderCtx, node: string, machine: Parameters<typeof archOf>[0]) {
  const chosen = ctx.nodes?.find((n) => n.id === node);
  return !!chosen?.status.arch && chosen.status.arch !== archOf(machine);
}

/** 부품 상자의 부품(묶음 차례대로) */
export function parts(s: BuilderState, ctx: BuilderCtx): Part[] {
  const v = s.values;
  const out: Part[] = [];
  for (const size of Object.keys(SIZES) as Size[]) {
    out.push({
      id: `size:${size}`,
      group: "size",
      name: "",
      kind: "size",
      size,
      fit: memoryFit(ctx, v.node, SIZES[size].memory),
    });
  }
  const tpm = firmwareUnavailable(ctx.nodes ?? undefined, v.node, "tpm");
  out.push({ id: "os:linux", group: "os", name: "", kind: "os", os: "linux" });
  out.push({
    id: "os:windows",
    group: "os",
    name: "",
    kind: "os",
    os: "windows",
    ...(tpm && { blocked: { key: tpm.key, values: tpm.values } }),
  });
  for (const m of ctx.cpuModels ?? []) {
    // 기본·클러스터·수준 모델과, 켤 수 있는 노드가 있는 이름 모델(별칭·폐기 모델은 뺀다)
    if (m.alias_of || m.deprecated || (m.kind === "qemu" && m.usable === 0)) {
      continue;
    }
    out.push({
      id: `cpu:${m.name}`,
      group: "cpu",
      name: m.name,
      kind: "cpu",
      model: m.name,
      usable: m.usable,
      ...(m.usable === 0 && { blocked: { key: "vmBuilder.reason.cpuNowhere" } }),
    });
  }
  for (const p of ctx.pools) {
    const nowhere = ctx.nodes !== null && !ctx.nodes.some((n) => usableOn(p, n));
    out.push({
      id: `pool:${p.name}`,
      group: "disk",
      name: p.name,
      kind: "pool",
      pool: p.name,
      shared: isShared(p),
      free: poolFree(p, ctx, v.node),
      ...(!canAllocate(ctx, p)
        ? { blocked: { key: "vmBuilder.reason.permPool", values: { name: p.name } } }
        : nowhere && {
            blocked: { key: "vmBuilder.reason.poolNowhere", values: { name: p.name } },
          }),
    });
  }
  for (const vol of freeVolumes(ctx, s)) {
    out.push({
      id: `volume:${vol.name}`,
      group: "disk",
      name: vol.name,
      kind: "volume",
      volume: vol.name,
      pool: vol.spec.pool,
      size: vol.spec.size_bytes,
    });
  }
  for (const iso of choosableIsos(ctx.volumes, ctx.pools, false)) {
    const pool = ctx.pools.find((p) => p.name === iso.spec.pool);
    const shared = !pool || isShared(pool);
    out.push({
      id: `iso:${iso.name}`,
      group: "media",
      name: iso.name,
      kind: "iso",
      iso: iso.name,
      pool: iso.spec.pool,
      shared,
      ...(!shared &&
        haActive(v) && {
          blocked: { key: "vmBuilder.reason.isoLocalHa", values: { name: iso.name } },
        }),
    });
  }
  for (const t of ctx.vms.filter(isTemplate)) {
    out.push({
      id: `template:${t.id}`,
      group: "template",
      name: t.name,
      kind: "template",
      vm: t.id,
      vcpus: (t.spec.cpu.sockets ?? 1) * t.spec.cpu.cores,
      memory: t.spec.memory_mib,
      disks: (t.spec.disks ?? []).length,
      ...(archOff(ctx, v.node, t.spec.machine) && {
        blocked: { key: "vmBuilder.reason.arch", values: { name: t.name } },
      }),
    });
  }
  out.push({
    id: `network:${USER_NETWORK}`,
    group: "network",
    name: USER_NETWORK,
    kind: "network",
    network: USER_NETWORK,
    sdn: false,
  });
  for (const n of ctx.networks) {
    out.push({
      id: `network:${n.name}`,
      group: "network",
      name: n.name,
      kind: "network",
      network: n.name,
      sdn: isSdnNetwork(n),
      ...(!canAssign(ctx, n.name) && {
        blocked: { key: "vmBuilder.reason.permNetwork", values: { name: n.name } },
      }),
    });
  }
  for (const g of ctx.securityGroups) {
    out.push({
      id: `sg:${g.name}`,
      group: "network",
      name: g.name,
      kind: "sg",
      sg: g.name,
      rules: (g.spec.rules ?? []).length,
    });
  }
  for (const snippet of ["user", "password", "dns", "packages"] as const) {
    out.push({ id: `ci:${snippet}`, group: "cloudInit", name: "", kind: "ci", snippet });
  }
  out.push({ id: "addr:dhcp", group: "cloudInit", name: "", kind: "addr", mode: "dhcp" });
  out.push({ id: "addr:static", group: "cloudInit", name: "", kind: "addr", mode: "static" });
  // 노드를 읽지 못하는 사용자는 배치 묶음이 없다(서버가 고른다).
  if (ctx.nodes !== null) {
    out.push({ id: "auto", group: "placement", name: "", kind: "auto" });
    for (const n of ctx.nodes.filter((n) => !n.spec.witness)) {
      const blocked = nodeBlocked(n);
      out.push({
        id: `node:${n.id}`,
        group: "placement",
        name: n.name,
        kind: "node",
        node: n.id,
        free: freeMemory(ctx, n.id),
        ...(blocked && { blocked }),
      });
    }
    const write = ctx.perms.can("write", "placement-rule");
    for (const z of zones(ctx.nodes)) {
      out.push({
        id: `zone:${z.value}`,
        group: "placement",
        name: z.value,
        kind: "zone",
        value: z.value,
        nodes: z.nodes,
        ...(!write && { blocked: { key: "vmBuilder.reason.permRules" } }),
      });
    }
  }
  const ha = haUnavailable(ctx.nodes ?? undefined, v.node);
  out.push({
    id: "ha",
    group: "protection",
    name: "",
    kind: "ha",
    ...(ha && { blocked: { key: ha.key, values: ha.values } }),
  });
  for (const g of ctx.haGroups) {
    out.push({
      id: `haGroup:${g.name}`,
      group: "protection",
      name: g.name,
      kind: "haGroup",
      haGroup: g.name,
    });
  }
  if (canCreateIn(ctx, "")) {
    out.push({ id: "folder:", group: "folder", name: "/", kind: "folder", folder: "" });
  }
  const tree = folderTree(ctx.folders, "vm");
  const folders = tree.folders
    .filter((f) => canCreateIn(ctx, f.id))
    .map((f) => ({ id: f.id, path: folderPath(tree, f.id) }))
    .sort((a, b) => a.path.localeCompare(b.path));
  for (const f of folders) {
    out.push({ id: `folder:${f.id}`, group: "folder", name: f.path, kind: "folder", folder: f.id });
  }
  return out;
}

/** 노드에 놓을 수 없는 까닭(오프라인·유지보수·위트니스) */
export function nodeBlocked(n: Node): Reason | undefined {
  const values = { name: n.name };
  if (n.spec.witness) {
    return { key: "vmBuilder.reason.nodeWitness", values };
  }
  if (!n.status.online) {
    return { key: "vmBuilder.reason.nodeOffline", values };
  }
  if (n.spec.maintenance) {
    return { key: "vmBuilder.reason.nodeMaintenance", values };
  }
  return undefined;
}
