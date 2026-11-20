// 라우터·유동 IP·공급자 네트워크(계획 66)의 질의와 순수 도움 함수. 라우터·유동 IP는 watch로 나가지 않아(서버
// `WATCHABLE`에 없다) 목록을 주기적으로 다시 읽고, 바꾼 뒤에는 캐시를 버린다.
import { type QueryClient, useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Network, Node, SecurityGroup, Vm } from "@/api/types";
import { ipFamily, isSdnNetwork } from "@/sdn/ips";

type Schemas = components["schemas"];
export type Router = Schemas["RouterView"];
export type RouterGateway = Schemas["RouterGateway"];
export type RoutedNetwork = Schemas["RoutedNetwork"];
export type RouterPatch = Schemas["RouterPatch"];
export type FloatingIp = Schemas["FloatingIpView"];
export type FipState = Schemas["FipState"];
export type NodeSdnConfig = Schemas["NodeSdnConfig"];

export const ROUTERS_KEY = ["routers"] as const;
export const FIPS_KEY = ["floating-ips"] as const;
/** 라우터의 지금 게이트웨이(NB — 연 서랍 하나만)와 유동 IP 상태를 다시 읽는 주기 */
export const ROUTING_REFRESH_MS = 10_000;
/** NB를 읽지 않는 라우터 목록과 VM 상세의 유동 IP를 다시 읽는 주기(리뷰 1 웹 I1 — 열어 둔 탭이 서버를 덜 부른다) */
export const ROUTING_SLOW_REFRESH_MS = 30_000;
/** 서버의 `DEFAULT_PROVIDER_MTU`: 물리 망 */
export const DEFAULT_PROVIDER_MTU = 1500;
/** 서버의 `MAX_INTERFACES`·`MAX_ROUTES`·`MAX_PHYSNETS` */
export const MAX_INTERFACES = 16;
export const MAX_ROUTES = 64;
export const MAX_PHYSNETS = 8;

/**
 * 라우터 목록(`GET /routers?live=false`). NB를 읽지 않으므로 지금 게이트웨이(`hosting`·`active`)가 빈다 — 그것은
 * 연 라우터 하나만 `useRouter`로 읽는다(리뷰 1 웹 I1: 목록마다 NB 전체를 읽지 않게).
 */
export function useRouters(enabled = true) {
  return useQuery({
    queryKey: [...ROUTERS_KEY, "spec"],
    queryFn: fetchRouters,
    select: (list) => list.items,
    refetchInterval: ROUTING_SLOW_REFRESH_MS,
    enabled,
  });
}

function fetchRouters() {
  return call(api.GET("/routers", { params: { query: { live: false } } }));
}

/**
 * 라우터 인터페이스인 네트워크(`GET /routers`의 `routed` — 읽을 수 없는 라우터의 것도, 라우터 이름 없이). 유동 IP를
 * 붙일 NIC와 라우터에 이을 수 있는 네트워크를 서버와 같게 고른다(리뷰 2 웹 I1). 라우터 목록과 같은 질의(캐시)다.
 */
export function useRouted(enabled = true) {
  return useQuery({
    queryKey: [...ROUTERS_KEY, "spec"],
    queryFn: fetchRouters,
    select: (list) => list.routed ?? [],
    refetchInterval: ROUTING_SLOW_REFRESH_MS,
    enabled,
  });
}

/** 라우터 하나와 NB의 지금 게이트웨이(`GET /routers/{key}`) — 연 서랍만 읽는다. */
export function useRouter(id: string | null) {
  return useQuery({
    queryKey: [...ROUTERS_KEY, "live", id],
    queryFn: () => call(api.GET("/routers/{key}", { params: { path: { key: id! } } })),
    refetchInterval: ROUTING_REFRESH_MS,
    enabled: id !== null,
  });
}

/** VM 상세의 유동 IP 다시 읽기: 하나라도 있을 때만 30초, 없거나 아직 모르면 다시 읽지 않는다. */
export function slowFipRefresh(count: number | undefined): number | false {
  return (count ?? 0) > 0 ? ROUTING_SLOW_REFRESH_MS : false;
}

/**
 * 유동 IP 목록(`GET /floating-ips`, 상태는 서버가 Raft에서 셈). `slow`면(VM 상세) 유동 IP가 하나라도 있을 때만 30초마다
 * 다시 읽는다 — 유동 IP가 없는 클러스터에서 VM 상세가 서버를 부르지 않는다.
 */
export function useFloatingIps(enabled = true, slow = false) {
  return useQuery({
    queryKey: FIPS_KEY,
    queryFn: () => call(api.GET("/floating-ips")),
    select: (list) => list.items,
    refetchInterval: slow
      ? (query) => slowFipRefresh(query.state.data?.items.length)
      : ROUTING_REFRESH_MS,
    enabled,
  });
}

/** 라우터·유동 IP를 바꾼 뒤(둘은 서로의 상태를 바꾼다 — 인터페이스를 빼면 유동 IP가 라우팅되지 않는다) */
export async function refreshRouting(queryClient: QueryClient) {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: ROUTERS_KEY }),
    queryClient.invalidateQueries({ queryKey: FIPS_KEY }),
    queryClient.invalidateQueries({ queryKey: ["network-ips"] }),
  ]);
}

/** 공급자(외부) 네트워크인지(`sdn.provider`가 있음) */
export function isProvider(network: Network | undefined): boolean {
  return network?.spec?.sdn?.provider != null;
}

/** 라우터 인터페이스가 될 수 있는 네트워크: 공급자가 아닌 SDN 네트워크이고 서브넷이 있다(서버 `check_router`). */
export function isOverlay(network: Network): boolean {
  return isSdnNetwork(network) && !isProvider(network);
}

/** 네트워크 목록의 VLAN/VNI 칸에 쓰는 공급자 표시 `ext:100`(flat이면 `ext`) */
export function providerLabel(network: Network): string {
  const p = network.spec.sdn?.provider;
  return p ? (p.vlan != null ? `${p.physnet}:${p.vlan}` : p.physnet) : "";
}

/** 노드들이 설정했거나 알린 physnet 이름(이름 순). 공급자 네트워크 만들기의 고르기다. */
export function knownPhysnets(nodes: readonly Node[]): string[] {
  const names = new Set<string>();
  for (const n of nodes) {
    Object.keys(n.spec.sdn?.physnets ?? {}).forEach((p) => names.add(p));
    (n.status.sdn?.physnets ?? []).forEach((p) => names.add(p.name));
  }
  return [...names].sort();
}

/** 그 physnet이 준비된 노드(서버 `NodeSdn::physnet_ready`) */
export function physnetNodes(nodes: readonly Node[], physnet: string): Node[] {
  return nodes.filter((n) =>
    (n.status.sdn?.physnets ?? []).some((p) => p.name === physnet && p.ready),
  );
}

/** 서버의 `valid_physnet`: `[a-z][a-z0-9-]{0,30}` */
export function validPhysnet(name: string): boolean {
  return /^[a-z][a-z0-9-]{0,30}$/.test(name);
}

/** 그 네트워크를 인터페이스로 가진 라우터(한 네트워크는 라우터 하나에만, 읽을 수 있는 것 가운데) */
export function routerOf(routers: readonly Router[] | undefined, network: string) {
  return routers?.find((r) => r.spec.interfaces?.some((i) => i.network === network));
}

/** 그 네트워크가 어떤 라우터(읽을 수 없는 것도)의 인터페이스인지와 그 외부(`routed` — 리뷰 2 웹 I1) */
export function routedOf(routed: readonly RoutedNetwork[] | undefined, network: string) {
  return routed?.find((r) => r.network === network);
}

/**
 * 네트워크 `network`를 쓰는 라우터(인터페이스 — 읽을 수 없는 라우터도 `routed`로, 외부)와 유동 IP 수(서버
 * `check_network_unused`와 같은 셈 — 리뷰 2 웹 m2). 지우기 단추를 막고 영향 줄에 쓴다.
 */
export function routingUsers(
  network: string,
  routers: readonly Router[] | undefined,
  routed: readonly RoutedNetwork[] | undefined,
  fips: readonly FloatingIp[] | undefined,
): { routers: number; fips: number } {
  const interfaceOf = routedOf(routed, network) ? 1 : 0;
  const externalOf = (routers ?? []).filter((r) => r.spec.external?.network === network).length;
  return {
    routers: interfaceOf + externalOf,
    fips: (fips ?? []).filter((f) => f.spec.network === network).length,
  };
}

/** VM NIC(MAC)에 붙은 유동 IP */
export function fipOn(
  fips: readonly FloatingIp[] | undefined,
  vm: string,
  mac: string | null | undefined,
): FloatingIp | undefined {
  if (!mac) {
    return undefined;
  }
  const wanted = mac.toLowerCase();
  return fips?.find((f) => f.spec.target?.vm === vm && f.spec.target.mac.toLowerCase() === wanted);
}

/** 유동 IP를 붙일 NIC 후보 하나. `reason`이 있으면 막힌 까닭(번역 키와 값)이다. */
export interface NicChoice {
  vm: Vm;
  index: number;
  mac: string;
  network: string;
  reason?: { key: string; values?: Record<string, string> };
}

/**
 * 유동 IP(공급자 네트워크 `fipNetwork`)를 붙일 수 있는 NIC: 서버 `routable`·`check_target`과 같은 차례로 막힌 까닭을
 * 붙인다 — 템플릿, MAC 없음, NIC의 네트워크가 외부가 `fipNetwork`인 라우터의 인터페이스가 아님(`routed` — 읽을 수 없는
 * 라우터도 서버가 알린다, 리뷰 2 웹 I1), NIC에 IPv4 주소가 없음, 다른 유동 IP가 붙음(`self`는 빼고). 오버레이 SDN NIC만
 * 든다(단순 NIC와 공급자 네트워크에 곧바로 붙은 NIC는 라우터 인터페이스가 될 수 없다 — 리뷰 2 웹 m3).
 */
export function nicChoices(
  fipNetwork: string,
  vms: readonly Vm[],
  networks: readonly Network[],
  routed: readonly RoutedNetwork[],
  fips: readonly FloatingIp[],
  self?: string,
): NicChoice[] {
  const out: NicChoice[] = [];
  const sdn = new Set(networks.filter(isOverlay).map((n) => n.name));
  for (const vm of [...vms].sort((a, b) => a.name.localeCompare(b.name))) {
    (vm.spec.nics ?? []).forEach((nic, index) => {
      if (!sdn.has(nic.network) || !nic.mac) {
        return;
      }
      const choice: NicChoice = { vm, index, mac: nic.mac.toLowerCase(), network: nic.network };
      const router = routedOf(routed, nic.network)?.external === fipNetwork;
      const taken = fipOn(fips, vm.id, nic.mac);
      if (vm.spec.template) {
        choice.reason = { key: "fip.why.template" };
      } else if (!router) {
        choice.reason = {
          key: "fip.why.noRouter",
          values: { network: nic.network, external: fipNetwork },
        };
      } else if (!(nic.ips ?? []).some((ip) => ipFamily(ip) === 4)) {
        choice.reason = { key: "fip.why.noIpv4", values: { network: nic.network } };
      } else if (taken && taken.id !== self) {
        choice.reason = { key: "fip.why.taken", values: { fip: taken.spec.address ?? taken.name } };
      }
      out.push(choice);
    });
  }
  return out;
}

/**
 * 보안 그룹을 건 NIC 수(서버 `count_nics`와 같은 셈 — 템플릿 포함): 단순 NIC(nftables)와 SDN NIC(OVN ACL). 서버는
 * 목록 응답의 `status.nics`에만 이 수를 넣고 watch 사건에는 없으므로 화면이 VM·네트워크 목록으로 센다.
 */
export function groupNics(
  group: SecurityGroup,
  vms: readonly Vm[],
  networks: readonly Network[],
): { simple: number; sdn: number } {
  const sdn = new Set(networks.filter(isSdnNetwork).map((n) => n.name));
  const out = { simple: 0, sdn: 0 };
  for (const nic of vms.flatMap((vm) => vm.spec.nics ?? [])) {
    if (nic.security_groups?.includes(group.name)) {
      if (sdn.has(nic.network)) {
        out.sdn += 1;
      } else {
        out.simple += 1;
      }
    }
  }
  return out;
}

type Translate = (key: string, values?: Record<string, unknown>) => string;

/** 서버가 보내는 영어 까닭·경고 가운데 웹이 아는 꼴(정규식 → 번역 키, 잡은 묶음의 이름) */
const SERVER_TEXTS: readonly [RegExp, string, readonly string[]][] = [
  [/^no gateway node has physnet (\S+) ready$/, "routing.server.noGateway", ["physnet"]],
  [
    /^no gateway node has physnet (\S+) ready; the router's external gateway stays off until one has it/,
    "routing.server.noGatewayYet",
    ["physnet"],
  ],
  [/^no node has physnet (\S+) ready yet\b/, "routing.server.noPhysnetYet", ["physnet"]],
  [/^the VM is not running$/, "routing.server.vmStopped", []],
  [/^the VM no longer exists$/, "routing.server.vmGone", []],
  [
    /^node (.+) does not have physnet (\S+) ready$/,
    "routing.server.nodeNoPhysnet",
    ["node", "physnet"],
  ],
  [
    /^a node of the VM does not have physnet (\S+) ready$/,
    "routing.server.someNodeNoPhysnet",
    ["physnet"],
  ],
  [/^(?:VM "(.+)"|the target VM) is a template$/, "routing.server.template", ["vm"]],
  [
    /^(?:VM "(.+)"|the target VM) has no NIC with MAC (\S+)$/,
    "routing.server.noNic",
    ["vm", "mac"],
  ],
  [
    /^network "(.+)" of the NIC is not an interface of a router whose external network is "(.+)"$/,
    "routing.server.noRouter",
    ["network", "external"],
  ],
  [/^the NIC has no IPv4 address on network "(.+)"$/, "routing.server.noIpv4", ["network"]],
  [/^network "(.+)" not found$/, "routing.server.networkGone", ["network"]],
];

/**
 * 서버의 영어 까닭(유동 IP 상태의 `reason`, 라우터의 `gateway_error`)과 경고(`orbivisor-warning`)를 아는 꼴이면 번역한다
 * (리뷰 2 웹 m7 — 한국어 화면에 영어 문장이 섞이지 않게). 모르는 꼴은 그대로다. `the target VM`(읽을 수 없는 VM)은
 * VM 이름 없이 옮긴다.
 */
export function serverText(t: Translate, text: string): string {
  for (const [pattern, key, names] of SERVER_TEXTS) {
    const m = pattern.exec(text);
    if (m) {
      const values: Record<string, string> = {};
      names.forEach((name, i) => {
        values[name] = m[i + 1] ?? "";
      });
      if ("vm" in values && values.vm === "") {
        return t(`${key}Hidden`, values);
      }
      return t(key, values);
    }
  }
  return text;
}
