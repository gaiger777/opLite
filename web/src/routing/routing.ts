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
export type RouterPatch = Schemas["RouterPatch"];
export type FloatingIp = Schemas["FloatingIpView"];
export type FipState = Schemas["FipState"];
export type NodeSdnConfig = Schemas["NodeSdnConfig"];

export const ROUTERS_KEY = ["routers"] as const;
export const FIPS_KEY = ["floating-ips"] as const;
/** 라우터의 지금 게이트웨이(NB)와 유동 IP 상태를 다시 읽는 주기 */
export const ROUTING_REFRESH_MS = 10_000;
/** 서버의 `DEFAULT_PROVIDER_MTU`: 물리 망 */
export const DEFAULT_PROVIDER_MTU = 1500;
/** 서버의 `MAX_INTERFACES`·`MAX_ROUTES`·`MAX_PHYSNETS` */
export const MAX_INTERFACES = 16;
export const MAX_ROUTES = 64;
export const MAX_PHYSNETS = 8;

/** 라우터 목록(`GET /routers`). `live`가 거짓이면 NB를 읽지 않는다(지금 게이트웨이가 빈다). */
export function useRouters(enabled = true, live = true) {
  return useQuery({
    queryKey: [...ROUTERS_KEY, live ? "live" : "spec"],
    queryFn: () => call(api.GET("/routers", live ? {} : { params: { query: { live: false } } })),
    select: (list) => list.items,
    refetchInterval: live ? ROUTING_REFRESH_MS : false,
    enabled,
  });
}

/** 유동 IP 목록(`GET /floating-ips`, 상태는 서버가 Raft에서 셈) */
export function useFloatingIps(enabled = true) {
  return useQuery({
    queryKey: FIPS_KEY,
    queryFn: () => call(api.GET("/floating-ips")),
    select: (list) => list.items,
    refetchInterval: ROUTING_REFRESH_MS,
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

/** 그 네트워크를 인터페이스로 가진 라우터(한 네트워크는 라우터 하나에만) */
export function routerOf(routers: readonly Router[] | undefined, network: string) {
  return routers?.find((r) => r.spec.interfaces?.some((i) => i.network === network));
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
 * 붙인다 — 템플릿, MAC 없음, NIC의 네트워크가 외부가 `fipNetwork`인 라우터의 인터페이스가 아님, NIC에 IPv4 주소가 없음,
 * 다른 유동 IP가 붙음(`self`는 빼고). SDN NIC만 든다(단순 NIC는 라우터에 닿지 않는다).
 */
export function nicChoices(
  fipNetwork: string,
  vms: readonly Vm[],
  networks: readonly Network[],
  routers: readonly Router[],
  fips: readonly FloatingIp[],
  self?: string,
): NicChoice[] {
  const out: NicChoice[] = [];
  const sdn = new Set(networks.filter(isSdnNetwork).map((n) => n.name));
  for (const vm of [...vms].sort((a, b) => a.name.localeCompare(b.name))) {
    (vm.spec.nics ?? []).forEach((nic, index) => {
      if (!sdn.has(nic.network) || !nic.mac) {
        return;
      }
      const choice: NicChoice = { vm, index, mac: nic.mac.toLowerCase(), network: nic.network };
      const router = routers.find(
        (r) =>
          r.spec.interfaces?.some((i) => i.network === nic.network) &&
          r.spec.external?.network === fipNetwork,
      );
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
