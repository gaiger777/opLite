// 부하 분산기(계획 67 §7)의 질의와 순수 도움 함수. 목록·보기는 watch(`load-balancer`)가 오면 다시 읽고(`LiveProvider`),
// 멤버 건강(SB `Service_Monitor`)만 따로 30초마다 다시 읽어 얹는다.
import { type QueryClient, useQuery } from "@tanstack/react-query";
import { useMemo } from "react";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Network, Vm } from "@/api/types";
import { isOverlay, type RoutedNetwork, type Router, routedOf } from "@/routing/routing";
import { ipFamily } from "@/sdn/ips";

type Schemas = components["schemas"];
export type LoadBalancer = Schemas["LoadBalancerView"];
export type LbSpec = Schemas["LoadBalancerSpec"];
export type LbListener = Schemas["LbListener"];
export type LbMember = Schemas["LbMember"];
export type LbHealth = Schemas["LbHealth"];
export type LbMemberView = Schemas["LbMemberView"];
export type LbState = Schemas["LbState"];
export type MemberHealth = Schemas["MemberHealth"];
export type LbProtocol = Schemas["LbProtocol"];

export const LBS_KEY = ["load-balancers"] as const;
/** 멤버 건강(SB)을 다시 읽는 주기 */
export const LB_LIVE_MS = 30_000;
/** 서버 `LbHealth` 기본값 */
export const DEFAULT_HEALTH: LbHealth = { interval_secs: 5, timeout_secs: 3, rise: 2, fall: 3 };
/** 서버의 한도: 리스너 1~16, 리스너마다 멤버 0~64 */
export const MAX_LISTENERS = 16;
export const MAX_MEMBERS = 64;

/** 멤버 건강(SB)을 함께 읽은 목록의 키 — watch 사건이 버리지 않는다(`REFRESH_ON`의 `load-balancers`와 다른 앞부분) */
export const LBS_LIVE_KEY = ["load-balancers-live"] as const;

/** 멤버 하나를 가리키는 열쇠(리스너·주소 또는 MAC·포트) */
function memberKey(m: Pick<LbMemberView, "listener" | "ip" | "mac" | "vm" | "port">): string {
  return `${m.listener}|${m.vm ?? ""}|${m.ip ?? m.mac ?? ""}|${m.port}`;
}

/**
 * 스펙 목록(`live=false`)에 살아 있는 목록의 멤버 건강을 얹는다(같은 LB·같은 멤버만). 살아 있는 목록이 아직 없거나 그
 * 뒤에 생긴 멤버는 스펙 목록의 값(검사 안 함) 그대로다.
 */
export function withHealth(
  spec: readonly LoadBalancer[],
  live: readonly LoadBalancer[] | undefined,
): LoadBalancer[] {
  if (!live) {
    return [...spec];
  }
  const byId = new Map(live.map((l) => [l.id, l]));
  return spec.map((lb) => {
    const fresh = byId.get(lb.id);
    if (!fresh) {
      return lb;
    }
    const health = new Map(fresh.members.map((m) => [memberKey(m), m]));
    return {
      ...lb,
      ...(fresh.live_error !== undefined && { live_error: fresh.live_error }),
      members: lb.members.map((m) => {
        const h = health.get(memberKey(m));
        return h ? { ...m, health: h.health, reason: h.reason } : m;
      }),
    };
  });
}

/**
 * 부하 분산기 목록. 목록 자체는 OVN을 읽지 않고(`live=false`) watch 사건으로만 다시 읽는다(리뷰 1 I3 — 사건마다 SB를 읽지
 * 않게). `live`면 멤버 건강(SB)을 따로 30초마다 읽어 얹는다. 폴더 보기의 객체 목록은 `live` 없이 부른다.
 */
export function useLoadBalancers(enabled = true, live = true) {
  const spec = useQuery({
    queryKey: [...LBS_KEY, "spec"],
    queryFn: () => call(api.GET("/load-balancers", { params: { query: { live: false } } })),
    select: (list) => [...list.items].sort((a, b) => a.name.localeCompare(b.name)),
    enabled,
  });
  const health = useQuery({
    queryKey: LBS_LIVE_KEY,
    queryFn: () => call(api.GET("/load-balancers")),
    select: (list) => list.items,
    refetchInterval: LB_LIVE_MS,
    enabled: enabled && live,
  });
  const data = useMemo(
    () => (spec.data ? withHealth(spec.data, live ? health.data : undefined) : undefined),
    [spec.data, health.data, live],
  );
  return { data, error: spec.error };
}

export async function refreshLbs(queryClient: QueryClient) {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: LBS_KEY }),
    queryClient.invalidateQueries({ queryKey: LBS_LIVE_KEY }),
    queryClient.invalidateQueries({ queryKey: ["network-ips"] }),
  ]);
}

/** 리스너 이름 `tcp/80` */
export function listenerName(l: Pick<LbListener, "protocol" | "port">): string {
  return `${l.protocol}/${l.port}`;
}

/** 건강 막대의 몫: 살아 있음·죽음(offline·error)·검사 없음·닿지 않음 */
export function healthCounts(members: readonly Pick<LbMemberView, "health">[]) {
  const out = { online: 0, offline: 0, unchecked: 0, missing: 0 };
  for (const m of members) {
    if (m.health === "online") {
      out.online += 1;
    } else if (m.health === "offline" || m.health === "error") {
      out.offline += 1;
    } else if (m.health === "missing") {
      out.missing += 1;
    } else {
      out.unchecked += 1;
    }
  }
  return out;
}

/**
 * VIP 네트워크가 될 수 있는 네트워크(서버 `vip_router`): IPv4 서브넷이 있는 오버레이 SDN 네트워크이고 어떤 라우터의
 * 인터페이스(`routed` — 읽을 수 없는 라우터도 서버가 알린다).
 */
export function vipNetworks(
  networks: readonly Network[],
  routed: readonly RoutedNetwork[],
): Network[] {
  return networks
    .filter(
      (n) =>
        isOverlay(n) &&
        (n.spec.subnets ?? []).some((s) => ipFamily(s.cidr.split("/")[0] ?? "") === 4) &&
        routedOf(routed, n.name) !== undefined,
    )
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** 멤버가 닿을 수 있는 네트워크: VIP 네트워크와 같은 라우터의 다른 인터페이스(라우터를 읽을 수 있을 때) */
export function memberNetworks(
  vipNetwork: string,
  routers: readonly Router[] | undefined,
): string[] {
  const router = routers?.find((r) => r.spec.interfaces?.some((i) => i.network === vipNetwork));
  return router ? (router.spec.interfaces ?? []).map((i) => i.network) : [vipNetwork];
}

/** NIC 멤버 후보 하나. `reason`이 있으면 막힌 까닭(번역 키와 값). */
export interface MemberChoice {
  vm: Vm;
  index: number;
  mac: string;
  network: string;
  ip?: string;
  reason?: { key: string; values?: Record<string, string> };
}

/**
 * NIC 멤버로 고를 수 있는 NIC(서버 `member_target`과 같은 차례): 템플릿, MAC 없음, NIC가 닿는 네트워크가 아님, IPv4 없음.
 * 닿는 네트워크의 NIC만 든다(다른 NIC는 목록을 길게 할 뿐). `vm.config`가 없으면 쓰는 쪽이 막는다.
 */
export function memberChoices(reachable: readonly string[], vms: readonly Vm[]): MemberChoice[] {
  const out: MemberChoice[] = [];
  const nets = new Set(reachable);
  for (const vm of [...vms].sort((a, b) => a.name.localeCompare(b.name))) {
    (vm.spec.nics ?? []).forEach((nic, index) => {
      if (!nets.has(nic.network)) {
        return;
      }
      const ip = (nic.ips ?? []).find((a) => ipFamily(a) === 4);
      const choice: MemberChoice = {
        vm,
        index,
        mac: (nic.mac ?? "").toLowerCase(),
        network: nic.network,
        ...(ip && { ip }),
      };
      if (vm.spec.template) {
        choice.reason = { key: "lb.why.template" };
      } else if (!nic.mac) {
        choice.reason = { key: "lb.why.noMac" };
      } else if (!ip) {
        choice.reason = { key: "lb.why.noIpv4", values: { network: nic.network } };
      }
      out.push(choice);
    });
  }
  return out;
}

/** 폼의 리스너 한 줄(글 입력 그대로) */
export interface ListenerRow {
  protocol: LbProtocol;
  port: string;
  health: boolean;
  interval: string;
  timeout: string;
  rise: string;
  fall: string;
  members: MemberRow[];
}

/** 멤버 한 줄: NIC(`vm`·`mac`) 또는 주소(`ip`), 포트 */
export interface MemberRow {
  vm?: string;
  mac?: string;
  ip?: string;
  port: string;
}

export function listenerRows(listeners: readonly LbListener[]): ListenerRow[] {
  return listeners.map((l) => ({
    protocol: l.protocol,
    port: String(l.port),
    health: l.health !== undefined,
    interval: String(l.health?.interval_secs ?? DEFAULT_HEALTH.interval_secs),
    timeout: String(l.health?.timeout_secs ?? DEFAULT_HEALTH.timeout_secs),
    rise: String(l.health?.rise ?? DEFAULT_HEALTH.rise),
    fall: String(l.health?.fall ?? DEFAULT_HEALTH.fall),
    members: (l.members ?? []).map((m) => ({
      ...(m.vm && { vm: m.vm }),
      ...(m.mac && { mac: m.mac }),
      ...(m.ip && { ip: m.ip }),
      port: String(m.port),
    })),
  }));
}

export function newListener(): ListenerRow {
  return {
    protocol: "tcp",
    port: "80",
    health: true,
    interval: String(DEFAULT_HEALTH.interval_secs),
    timeout: String(DEFAULT_HEALTH.timeout_secs),
    rise: String(DEFAULT_HEALTH.rise),
    fall: String(DEFAULT_HEALTH.fall),
    members: [],
  };
}

function int(text: string, min: number, max: number): number | null {
  const v = text.trim();
  if (!/^\d+$/.test(v)) {
    return null;
  }
  const n = Number(v);
  return n >= min && n <= max ? n : null;
}

/**
 * 리스너 줄의 오류(칸 열쇠 → 번역 키)와 보낼 리스너(서버 `LoadBalancerSpec::validate`와 같은 검사: 1~16개, 포트 1~65535,
 * 같은 프로토콜·포트 두 번, 멤버 0~64·같은 멤버 두 번, 건강 검사 범위).
 */
export function checkListeners(rows: readonly ListenerRow[]): {
  errors: Record<string, string>;
  listeners: LbListener[];
} {
  const errors: Record<string, string> = {};
  if (rows.length === 0 || rows.length > MAX_LISTENERS) {
    errors.listeners = "lb.errors.listeners";
  }
  const seen = new Set<string>();
  const listeners: LbListener[] = rows.map((row, i) => {
    const port = int(row.port, 1, 65535);
    if (port === null) {
      errors[`l${i}.port`] = "lb.errors.port";
    } else if (seen.has(`${row.protocol}/${port}`)) {
      errors[`l${i}.port`] = "lb.errors.listenerTaken";
    }
    seen.add(`${row.protocol}/${port}`);
    let health: LbHealth | undefined;
    if (row.health) {
      const interval = int(row.interval, 1, 300);
      const timeout = interval === null ? null : int(row.timeout, 1, interval);
      const rise = int(row.rise, 1, 10);
      const fall = int(row.fall, 1, 10);
      if (interval === null || timeout === null || rise === null || fall === null) {
        errors[`l${i}.health`] = "lb.errors.health";
      } else {
        health = { interval_secs: interval, timeout_secs: timeout, rise, fall };
      }
    }
    if (row.members.length > MAX_MEMBERS) {
      errors[`l${i}.members`] = "lb.errors.members";
    }
    const members = new Set<string>();
    const out: LbMember[] = row.members.map((m, j) => {
      const mport = int(m.port, 1, 65535);
      if (mport === null) {
        errors[`l${i}.m${j}.port`] = "lb.errors.port";
      }
      if (!m.vm && (!m.ip || ipFamily(m.ip) !== 4)) {
        errors[`l${i}.m${j}.ip`] = "lb.errors.memberIp";
      }
      const id = `${m.vm ?? ""}/${m.mac ?? ""}/${m.ip ?? ""}/${mport}`;
      if (members.has(id)) {
        errors[`l${i}.m${j}.port`] = "lb.errors.memberTaken";
      }
      members.add(id);
      return {
        ...(m.vm && { vm: m.vm, mac: m.mac }),
        ...(!m.vm && m.ip && { ip: m.ip.trim() }),
        port: mport ?? 0,
      };
    });
    return {
      protocol: row.protocol,
      port: port ?? 0,
      ...(out.length > 0 && { members: out }),
      ...(health && { health }),
    };
  });
  return { errors, listeners };
}
