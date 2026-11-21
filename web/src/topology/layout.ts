// 토폴로지 배치(계획 67 §12.1): 순수 함수 `layoutTopology(graph, opts) → Scene`. 라우터를 x축에 늘어놓고 그 인터페이스
// 네트워크를 라우터 앞(y)으로 펼치며, 공급자 네트워크는 뒤(먼 y) 줄, 라우터 없는 오버레이와 단순 네트워크는 옆 열이다.
// 같은 입력이면 같은 그림(이름 순 — 새로 읽어도 흔들리지 않음). 그리기 차례는 깊이(x + y, z)로 정한다.
import { type Box, boxBounds, iso, type Point, topCenter } from "@/components/iso/iso";
import type { Topology, TopoNetwork, TopoVm } from "@/topology/topology";

/** 원소 종류 */
export type ItemKind =
  "provider" | "network" | "simple" | "router" | "vm" | "more" | "lb" | "fip" | "vip";

/** 색의 뜻(그리는 쪽이 디자인 토큰으로 바꾼다) */
export type Tone =
  | "overlay"
  | "provider"
  | "simple"
  | "router"
  | "running"
  | "stopped"
  | "failed"
  | "busy"
  | "online"
  | "offline"
  | "unchecked"
  | "missing"
  | "warning"
  | "fip"
  | "lb";

export interface SceneItem {
  /** 그림 안에서 하나뿐인 열쇠(`vm:<네트워크>:<VM>`처럼) */
  key: string;
  kind: ItemKind;
  /** 가리키는 리소스 id(네트워크·라우터·VM·LB·유동 IP), "+N"은 그 네트워크 id */
  id: string;
  label: string;
  /** 둘째 줄(CIDR·VIP·주소) */
  detail?: string;
  tone: Tone;
  /** 세계 좌표의 상자(깊이·그리기) */
  box: Box;
  /** 그리기 층: 0 바닥 판, 1 판 위의 것 */
  layer: number;
  /** 키보드 층: 0 공급자, 1 꼬리표, 2 라우터, 3 네트워크, 4 VM·LB */
  row: number;
  /** 화면에서의 가운데(초점·이웃 찾기·글) */
  center: Point;
  /** "+N"의 N */
  count?: number;
  /** 판 위의 것이면 그 판의 열쇠 */
  parent?: string;
  /** 경보 색을 쓸 까닭(게이트웨이 후보 없음 따위) */
  warn?: boolean;
}

export type LinkKind = "interface" | "external" | "nic" | "member" | "fip" | "vip" | "migration";

export interface SceneLink {
  key: string;
  kind: LinkKind;
  from: Point;
  to: Point;
  /** 이어진 두 원소의 열쇠 */
  ends: [string, string];
  tone: Tone | "link";
  dashed: boolean;
  /** 휜 선의 높이(0이면 곧은 선) */
  lift: number;
  /** 선 위의 글(옮기기 진행 수) */
  label?: string;
}

export interface Scene {
  /** 깊이 순(먼 것부터) */
  items: SceneItem[];
  links: SceneLink[];
  bounds: { minX: number; minY: number; maxX: number; maxY: number };
  /** 판 열쇠 → 접어 둔 VM(서랍의 목록) */
  folded: Record<string, TopoVm[]>;
}

export interface LayoutOptions {
  /** 단순 네트워크(브리지·VLAN·VXLAN)를 그리지 않는다 */
  hideSimple?: boolean;
  /** VM을 그리지 않는다(큰 클러스터) */
  hideVms?: boolean;
}

/** 판 하나에 그리는 VM 수. 넘으면 앞의 것과 "+N" 블록 하나 */
export const MAX_VMS_PER_SLAB = 30;
/** 한 열에 쌓는 판 수(라우터 없는 오버레이·단순 네트워크) */
const PER_COLUMN = 6;
const CELL = 34;
const SLAB_COLS = 6;
const PAD = 16;
const SLAB_W = SLAB_COLS * CELL + PAD * 2;
const SLAB_H = 8;
const MIN_SLAB_D = 96;
const COL_GAP = 90;
const ROW_GAP = 44;
const PROVIDER_D = 96;
const ROUTER_Y = PROVIDER_D + 70;
const ROUTER_SIZE = 72;
const ROUTER_Z = 46;
const ROUTER_H = 34;
const NET_Y0 = ROUTER_Y + ROUTER_SIZE + 70;
const VM_SIZE = 22;
const VM_H = 18;
const TAG_CELL = 30;

const byName = <T extends { name: string }>(a: T, b: T) =>
  a.name < b.name ? -1 : a.name > b.name ? 1 : 0;

/** VM 전원 → 색 */
export function powerTone(power: TopoVm["power"]): Tone {
  switch (power) {
    case "running":
      return "running";
    case "stopped":
      return "stopped";
    case "failed":
      return "failed";
    default:
      return "busy";
  }
}

function slabDepth(cells: number): number {
  return Math.max(MIN_SLAB_D, PAD * 2 + Math.ceil(Math.max(cells, 1) / SLAB_COLS) * CELL);
}

/** 판 안 `i`번째 칸의 상자(앞 왼쪽부터 줄마다) */
function cellBox(slab: Box, i: number, size: number, h: number): Box {
  const col = i % SLAB_COLS;
  const row = Math.floor(i / SLAB_COLS);
  return {
    x: slab.x + PAD + col * CELL + (CELL - size) / 2,
    y: slab.y + PAD + row * CELL + (CELL - size) / 2,
    z: slab.z + slab.h,
    w: size,
    d: size,
    h,
  };
}

/**
 * 토폴로지 응답 → 그림. 단순 네트워크·VM 감추기를 받는다.
 */
export function layoutTopology(graph: Topology, opts: LayoutOptions = {}): Scene {
  const items: SceneItem[] = [];
  const links: SceneLink[] = [];
  const folded: Record<string, TopoVm[]> = {};
  const networks = [...graph.networks].sort(byName);
  const netById = new Map(networks.map((n) => [n.id, n]));
  const routers = [...graph.routers].sort(byName);
  const vms = opts.hideVms ? [] : [...graph.vms].sort(byName);
  const lbs = [...graph.load_balancers].sort(byName);
  const slabOf = new Map<string, SceneItem>();
  const byKey = new Map<string, SceneItem>();
  const put = (item: SceneItem) => {
    items.push(item);
    byKey.set(item.key, item);
  };

  const isSimple = (n: TopoNetwork) => n.kind !== "sdn" && n.kind !== "provider";
  const shownNet = (n: TopoNetwork | undefined) =>
    n !== undefined && !(opts.hideSimple && isSimple(n));

  // VM은 그림에 있는 첫 NIC의 네트워크 판에 한 번 놓고, 다른 NIC의 네트워크로는 선을 긋는다. 공급자 네트워크에만 NIC가
  // 있는 VM은 그 공급자 판(꼬리표 뒤)에 놓는다(리뷰 1 I4).
  const homeOf = new Map<string, string>();
  const onSlab = new Map<string, TopoVm[]>();
  for (const vm of vms) {
    const shown = vm.nics.filter((nic) => shownNet(netById.get(nic.network)));
    const home = (shown.find((nic) => netById.get(nic.network)!.kind !== "provider") ?? shown[0])
      ?.network;
    if (home) {
      homeOf.set(vm.id, home);
      onSlab.set(home, [...(onSlab.get(home) ?? []), vm]);
    }
  }
  const lbsOn = new Map<string, typeof lbs>();
  for (const lb of lbs) {
    lbsOn.set(lb.network, [...(lbsOn.get(lb.network) ?? []), lb]);
  }
  const cellsOf = (n: TopoNetwork) =>
    (lbsOn.get(n.id)?.length ?? 0) + Math.min(onSlab.get(n.id)?.length ?? 0, MAX_VMS_PER_SLAB);

  const addSlab = (n: TopoNetwork, x: number, y: number): SceneItem => {
    const box: Box = { x, y, z: 0, w: SLAB_W, d: slabDepth(cellsOf(n)), h: SLAB_H };
    const kind: ItemKind = n.kind === "provider" ? "provider" : isSimple(n) ? "simple" : "network";
    const item: SceneItem = {
      key: `net:${n.id}`,
      kind,
      id: n.id,
      label: n.name,
      detail: networkDetail(n),
      tone: kind === "provider" ? "provider" : kind === "simple" ? "simple" : "overlay",
      box,
      layer: 0,
      row: kind === "provider" ? 0 : 3,
      center: topCenter(box),
    };
    put(item);
    slabOf.set(n.id, item);
    return item;
  };

  // 1) 라우터 열과 그 인터페이스 네트워크
  let x = 0;
  const routerX = new Map<string, number>();
  const placed = new Set<string>();
  for (const r of routers) {
    const ifaces = r.interfaces
      .map((id) => netById.get(id))
      .filter((n): n is TopoNetwork => shownNet(n) && !placed.has(n!.id))
      .sort(byName);
    let y = NET_Y0;
    for (const n of ifaces) {
      const slab = addSlab(n, x, y);
      placed.add(n.id);
      y += slab.box.d + ROW_GAP;
    }
    const box: Box = {
      x: x + (SLAB_W - ROUTER_SIZE) / 2,
      y: ROUTER_Y,
      z: ROUTER_Z,
      w: ROUTER_SIZE,
      d: ROUTER_SIZE,
      h: ROUTER_H,
    };
    const warn = r.gateway.state === "no-candidate";
    put({
      key: `router:${r.id}`,
      kind: "router",
      id: r.id,
      label: r.name,
      detail: r.gateway.state === "active" && r.gateway.node ? r.gateway.node : undefined,
      tone: warn ? "warning" : "router",
      box,
      layer: 1,
      row: 2,
      center: topCenter(box),
      warn,
    });
    routerX.set(r.id, x);
    for (const n of ifaces) {
      const slab = slabOf.get(n.id)!;
      links.push({
        key: `iface:${r.id}:${n.id}`,
        kind: "interface",
        from: iso(box.x + box.w / 2, box.y + box.d / 2, box.z),
        to: iso(slab.box.x + slab.box.w / 2, slab.box.y + 6, slab.box.h),
        ends: [`router:${r.id}`, slab.key],
        tone: "router",
        dashed: false,
        lift: 0,
      });
    }
    x += SLAB_W + COL_GAP;
  }

  // 2) 라우터 없는 오버레이, 3) 단순 네트워크 — 옆 열(한 열에 PER_COLUMN개)
  const column = (list: TopoNetwork[]) => {
    list.forEach((n, i) => {
      if (i > 0 && i % PER_COLUMN === 0) {
        x += SLAB_W + COL_GAP;
      }
      const above = items.filter(
        (it) => it.layer === 0 && it.box.x === x && it.box.y >= NET_Y0 && it.kind !== "provider",
      );
      const y = above.reduce((m, it) => Math.max(m, it.box.y + it.box.d + ROW_GAP), NET_Y0);
      addSlab(n, x, y);
      placed.add(n.id);
    });
    if (list.length > 0) {
      x += SLAB_W + COL_GAP;
    }
  };
  column(networks.filter((n) => n.kind === "sdn" && !placed.has(n.id)));
  column(networks.filter((n) => isSimple(n) && shownNet(n) && !placed.has(n.id)));

  // 4) 공급자 네트워크 — 뒤 줄. 쓰는 첫 라우터의 열에 맞추고 겹치면 오른쪽으로
  const providers = networks.filter((n) => n.kind === "provider");
  const preferred = (n: TopoNetwork) => {
    const r = routers.find((r) => r.external?.network === n.id);
    return r ? routerX.get(r.id)! : Number.POSITIVE_INFINITY;
  };
  const fipsOn = new Map<string, typeof graph.floating_ips>();
  for (const f of [...graph.floating_ips].sort((a, b) =>
    (a.address ?? a.id).localeCompare(b.address ?? b.id, undefined, { numeric: true }),
  )) {
    fipsOn.set(f.network, [...(fipsOn.get(f.network) ?? []), f]);
  }
  let free = 0;
  for (const n of [...providers].sort((a, b) => preferred(a) - preferred(b) || byName(a, b))) {
    const want = preferred(n);
    const px = Number.isFinite(want) ? Math.max(want, free) : free;
    const tags =
      (fipsOn.get(n.id)?.length ?? 0) + lbs.filter((l) => externalOf(graph, l.id) === n.id).length;
    const vmCells = Math.min(onSlab.get(n.id)?.length ?? 0, MAX_VMS_PER_SLAB);
    const width = Math.max(
      SLAB_W,
      PAD * 2 + Math.ceil(tags / 2) * TAG_CELL + Math.ceil(vmCells / 2) * CELL,
    );
    const slab = addSlab(n, px, 0);
    slab.box = { ...slab.box, w: width, d: PROVIDER_D };
    slab.center = topCenter(slab.box);
    free = px + width + COL_GAP;
  }
  for (const r of routers) {
    if (!r.external) {
      continue;
    }
    const slab = slabOf.get(r.external.network);
    const router = byKey.get(`router:${r.id}`)!;
    if (!slab) {
      continue;
    }
    links.push({
      key: `ext:${r.id}`,
      kind: "external",
      from: iso(router.box.x + router.box.w / 2, router.box.y, router.box.z + router.box.h),
      to: iso(
        Math.min(
          Math.max(router.box.x + router.box.w / 2, slab.box.x + PAD),
          slab.box.x + slab.box.w - PAD,
        ),
        slab.box.y + slab.box.d,
        slab.box.h,
      ),
      ends: [router.key, slab.key],
      tone: "provider",
      dashed: false,
      lift: 0,
    });
  }

  // 5) 판 위: LB(육각 기둥) 먼저, VM(블록), 넘친 것은 "+N". 공급자 판은 꼬리표 열 뒤에 두 줄로.
  const vmKey = new Map<string, string>();
  for (const [netId, slab] of slabOf) {
    const provider = slab.kind === "provider";
    const tagCols = provider
      ? Math.ceil(
          ((fipsOn.get(netId)?.length ?? 0) +
            lbs.filter((l) => externalOf(graph, l.id) === netId).length) /
            2,
        )
      : 0;
    const cellBoxOn = (i: number, size: number, h: number): Box =>
      provider
        ? {
            x: slab.box.x + PAD + tagCols * TAG_CELL + Math.floor(i / 2) * CELL + (CELL - size) / 2,
            y: slab.box.y + PAD + (i % 2) * CELL + (CELL - size) / 2,
            z: slab.box.z + slab.box.h,
            w: size,
            d: size,
            h,
          }
        : cellBox(slab.box, i, size, h);
    let cell = 0;
    for (const lb of lbsOn.get(netId) ?? []) {
      const box = cellBox(slab.box, cell++, 24, 28);
      put({
        key: `lb:${lb.id}`,
        kind: "lb",
        id: lb.id,
        label: lb.name,
        detail: lb.vip,
        tone: lbTone(lb.members.map((m) => m.health)),
        box,
        layer: 1,
        row: 4,
        center: topCenter(box),
        parent: slab.key,
      });
    }
    const here = onSlab.get(netId) ?? [];
    const overflow = here.length > MAX_VMS_PER_SLAB;
    const shown = overflow ? here.slice(0, MAX_VMS_PER_SLAB - 1) : here;
    for (const vm of shown) {
      const box = cellBoxOn(cell++, VM_SIZE, VM_H);
      const key = `vm:${netId}:${vm.id}`;
      vmKey.set(vm.id, key);
      put({
        key,
        kind: "vm",
        id: vm.id,
        label: vm.name,
        detail: vm.nics.find((n) => n.network === netId)?.ips.join(", ") || undefined,
        tone: powerTone(vm.power),
        box,
        layer: 1,
        row: 4,
        center: topCenter(box),
        parent: slab.key,
      });
    }
    if (overflow) {
      const rest = here.slice(MAX_VMS_PER_SLAB - 1);
      const box = cellBoxOn(cell, VM_SIZE, VM_H);
      put({
        key: `more:${netId}`,
        kind: "more",
        id: netId,
        label: `+${rest.length}`,
        tone: "stopped",
        box,
        layer: 1,
        row: 4,
        center: topCenter(box),
        parent: slab.key,
        count: rest.length,
      });
      folded[slab.key] = rest;
      for (const vm of rest) {
        vmKey.set(vm.id, `more:${netId}`);
      }
    }
  }

  // 다른 NIC의 네트워크로 가는 선
  for (const vm of vms) {
    const from = vmKey.get(vm.id);
    const home = homeOf.get(vm.id);
    if (!from || from.startsWith("more:")) {
      continue;
    }
    const vmItem = byKey.get(from)!;
    for (const nic of vm.nics) {
      const slab = slabOf.get(nic.network);
      if (!slab || nic.network === home) {
        continue;
      }
      links.push({
        key: `nic:${vm.id}:${nic.network}`,
        kind: "nic",
        from: vmItem.center,
        to: slab.center,
        ends: [from, slab.key],
        tone: "link",
        dashed: true,
        lift: 24,
      });
    }
  }

  // LB 멤버 선(멤버 건강 색)
  for (const lb of lbs) {
    const lbItem = byKey.get(`lb:${lb.id}`);
    if (!lbItem) {
      continue;
    }
    const seen = new Set<string>();
    for (const m of lb.members) {
      const target = m.vm ? vmKey.get(m.vm) : undefined;
      if (!target || seen.has(target)) {
        continue;
      }
      seen.add(target);
      const vmItem = byKey.get(target)!;
      links.push({
        key: `member:${lb.id}:${target}`,
        kind: "member",
        from: lbItem.center,
        to: vmItem.center,
        ends: [lbItem.key, target],
        tone: memberTone(m.health),
        dashed: true,
        lift: 14,
      });
    }
  }

  // 6) 공급자 판 위의 꼬리표: 유동 IP와 바깥 VIP
  for (const [netId, slab] of slabOf) {
    if (slab.kind !== "provider") {
      continue;
    }
    const tags: {
      key: string;
      kind: "fip" | "vip";
      id: string;
      label: string;
      target?: string;
      tone: Tone;
      dashed: boolean;
    }[] = [];
    for (const f of fipsOn.get(netId) ?? []) {
      const target = f.vm ? vmKey.get(f.vm) : undefined;
      tags.push({
        key: `fip:${f.id}`,
        kind: "fip",
        id: f.id,
        label: f.address ?? f.id.slice(0, 8),
        target,
        tone: f.state === "unrouted" ? "warning" : "fip",
        // 분산(그 VM의 chassis에서 NAT)은 실선, 중앙(게이트웨이를 지남)·라우팅 안 됨은 점선
        dashed: f.state !== "distributed",
      });
    }
    for (const lb of lbs.filter((l) => externalOf(graph, l.id) === netId)) {
      tags.push({
        key: `vip:${lb.id}`,
        kind: "vip",
        id: lb.id,
        label: lb.external_ip ?? lb.name,
        target: byKey.get(`lb:${lb.id}`)?.key,
        tone: "lb",
        dashed: true,
      });
    }
    tags.forEach((tag, i) => {
      const box: Box = {
        x: slab.box.x + PAD + Math.floor(i / 2) * TAG_CELL,
        y: slab.box.y + PAD + (i % 2) * (TAG_CELL + 8),
        z: slab.box.h,
        w: 8,
        d: 8,
        h: 26,
      };
      const item: SceneItem = {
        key: tag.key,
        kind: tag.kind,
        id: tag.id,
        label: tag.label,
        tone: tag.tone,
        box,
        layer: 1,
        row: 1,
        center: iso(box.x, box.y, box.z + 30),
        parent: slab.key,
        warn: tag.tone === "warning",
      };
      put(item);
      const target = tag.target ? byKey.get(tag.target) : undefined;
      if (target) {
        links.push({
          key: `${tag.kind}-to:${tag.id}`,
          kind: tag.kind,
          from: item.center,
          to: target.center,
          ends: [item.key, target.key],
          tone: tag.tone,
          dashed: tag.dashed,
          lift: 60,
        });
      }
    });
  }

  // 7) 옮기기 중인 단순 네트워크 → 대상 판(화살표와 진행 수)
  for (const n of networks) {
    const m = n.migration;
    const from = slabOf.get(n.id);
    const to = m?.target ? slabOf.get(m.target) : undefined;
    if (!m || !from || !to) {
      continue;
    }
    links.push({
      key: `migration:${m.id}`,
      kind: "migration",
      from: iso(from.box.x, from.box.y + from.box.d / 2, from.box.h),
      to: iso(to.box.x + to.box.w, to.box.y + to.box.d / 2, to.box.h),
      ends: [from.key, to.key],
      tone: "warning",
      dashed: true,
      lift: 50,
      label: `${m.moved}/${m.total}`,
    });
  }

  const ordered = depthSorted(items);
  return { items: ordered, links, bounds: sceneBounds(ordered), folded };
}

function externalOf(graph: Topology, lbId: string): string | undefined {
  const lb = graph.load_balancers.find((l) => l.id === lbId);
  if (!lb?.external_ip) {
    return undefined;
  }
  // 바깥 VIP의 공급자 네트워크 = VIP 네트워크 라우터의 바깥
  const router = graph.routers.find((r) => r.interfaces.includes(lb.network));
  return router?.external?.network;
}

/** 네트워크 판의 둘째 줄: CIDR, 아니면 종류(VLAN·VXLAN·physnet) */
function networkDetail(n: TopoNetwork): string | undefined {
  if (n.cidrs.length > 0) {
    return n.cidrs.join(", ");
  }
  if (n.kind === "provider") {
    return n.physnet ? (n.vlan != null ? `${n.physnet}:${n.vlan}` : n.physnet) : undefined;
  }
  if (n.kind === "vlan" && n.vlan != null) {
    return `VLAN ${n.vlan}`;
  }
  return undefined;
}

export function memberTone(health: string): Tone {
  switch (health) {
    case "online":
      return "online";
    case "offline":
    case "error":
      return "offline";
    case "missing":
      return "missing";
    default:
      return "unchecked";
  }
}

/** LB 기둥 색: 죽은 멤버가 있으면 그 색, 모두 살았으면 살아 있음, 검사가 없으면 LB 색 */
function lbTone(health: readonly string[]): Tone {
  if (health.some((h) => h === "offline" || h === "error")) {
    return "offline";
  }
  if (health.some((h) => h === "missing")) {
    return "missing";
  }
  if (health.length > 0 && health.every((h) => h === "online")) {
    return "online";
  }
  return "lb";
}

/** 바닥 판 먼저(깊이 순), 그 위의 것(깊이 순) */
function depthSorted(items: SceneItem[]): SceneItem[] {
  const depth = (b: Box) => b.x + b.w / 2 + b.y + b.d / 2 + b.z * 1e-3;
  return [...items].sort((a, b) => a.layer - b.layer || depth(a.box) - depth(b.box));
}

function sceneBounds(items: readonly SceneItem[]) {
  if (items.length === 0) {
    return { minX: 0, minY: 0, maxX: 400, maxY: 240 };
  }
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const it of items) {
    const b = boxBounds(it.box);
    minX = Math.min(minX, b.minX);
    minY = Math.min(minY, b.minY - 24);
    maxX = Math.max(maxX, b.maxX);
    maxY = Math.max(maxY, b.maxY + 34);
  }
  return { minX, minY, maxX, maxY };
}

/** 키보드 이동 방향 */
export type Direction = "left" | "right" | "up" | "down";

/**
 * 화살표 키의 이웃: 왼오른은 같은 층에서 화면 x 차례, 위아래는 이웃한(빈 층은 건너뛴) 층에서 화면 x가 가장 가까운 것.
 * 없으면 null.
 */
export function neighbor(items: readonly SceneItem[], key: string, dir: Direction): string | null {
  const at = items.find((it) => it.key === key);
  if (!at) {
    return items[0]?.key ?? null;
  }
  if (dir === "left" || dir === "right") {
    const row = items
      .filter((it) => it.row === at.row)
      .sort((a, b) => a.center.x - b.center.x || a.center.y - b.center.y);
    const i = row.findIndex((it) => it.key === key);
    const next = row[dir === "left" ? i - 1 : i + 1];
    return next?.key ?? null;
  }
  const rows = [...new Set(items.map((it) => it.row))].sort((a, b) => a - b);
  const r = rows.indexOf(at.row);
  const target = rows[dir === "up" ? r - 1 : r + 1];
  if (target === undefined) {
    return null;
  }
  let best: SceneItem | null = null;
  for (const it of items) {
    if (it.row !== target) {
      continue;
    }
    const d = Math.abs(it.center.x - at.center.x) + Math.abs(it.center.y - at.center.y) * 0.25;
    const bd = best
      ? Math.abs(best.center.x - at.center.x) + Math.abs(best.center.y - at.center.y) * 0.25
      : Infinity;
    if (d < bd) {
      best = it;
    }
  }
  return best?.key ?? null;
}

/**
 * 가리키거나 초점이 간 원소와 이어진 것(밝힐 원소·선 열쇠): 그 원소, 위로 오르는 길(VM → 네트워크 → 라우터 → 공급자),
 * 바로 이어진 선의 반대쪽과 그 위의 길, 판이면 그 위의 것.
 */
export function related(scene: Scene, key: string): Set<string> {
  const up = new Map<string, string[]>();
  const side = new Map<string, string[]>();
  const add = (map: Map<string, string[]>, a: string, b: string) =>
    map.set(a, [...(map.get(a) ?? []), b]);
  for (const it of scene.items) {
    if (it.parent) {
      add(up, it.key, it.parent);
    }
  }
  for (const l of scene.links) {
    if (l.kind === "interface") {
      add(up, l.ends[1], l.ends[0]);
    } else if (l.kind === "external") {
      add(up, l.ends[0], l.ends[1]);
    } else {
      add(side, l.ends[0], l.ends[1]);
      add(side, l.ends[1], l.ends[0]);
    }
  }
  const out = new Set<string>([key]);
  const climb = (k: string) => {
    for (const p of up.get(k) ?? []) {
      if (!out.has(p)) {
        out.add(p);
        climb(p);
      }
    }
  };
  climb(key);
  for (const s of side.get(key) ?? []) {
    out.add(s);
    climb(s);
  }
  // 판·라우터·공급자면 바로 아래의 것(그 판의 VM·LB, 라우터의 네트워크)
  for (const [child, parents] of up) {
    if (parents.includes(key)) {
      out.add(child);
    }
  }
  for (const l of scene.links) {
    if (out.has(l.ends[0]) && out.has(l.ends[1])) {
      out.add(l.key);
    }
  }
  return out;
}
