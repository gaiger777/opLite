// SDN NIC 주소와 네트워크 종류의 순수 도움 함수(계획 65). VM 폼·복제·배포·가져오기가 처음 화면에 함께 실리므로 API·
// 쿼리를 들이지 않는다.
import type { Network } from "@/api/types";
import { validIpv6 } from "@/storage/pools";
import { validIpv4 } from "@/vms/cloudInit";

export function isSdnNetwork(network: Network | undefined): boolean {
  // 시험의 목록처럼 칸이 빠진 네트워크도 단순으로 본다.
  return network?.spec?.sdn != null;
}

/**
 * 서브넷이 있는 SDN 네트워크에서 주소가 하나도 없는 NIC: OVN 포트가 꺼져 아무것도 오가지 않는다(서버 리뷰 1 I-4 — MAC만인
 * 포트 보안은 어느 IP든 보내게 두므로). 서브넷이 없는 SDN 네트워크(L2만)는 MAC만으로 켠다.
 */
export function portOff(network: Network | undefined, ips: readonly string[] | undefined): boolean {
  return isSdnNetwork(network) && (network?.spec.subnets ?? []).length > 0 && !ips?.length;
}

/** 이름이 SDN 네트워크인지(목록에서 찾음) */
export function sdnNetworkNamed(networks: readonly Network[] | undefined, name: string): boolean {
  return isSdnNetwork(networks?.find((n) => n.name === name));
}

/** IP 주소(앞자리 0 없는 IPv4 또는 IPv6)의 체계. 아니면 undefined */
export function ipFamily(text: string): 4 | 6 | undefined {
  if (validIpv4(text)) {
    return 4;
  }
  return text.includes(":") && validIpv6(text) ? 6 : undefined;
}

/** 쉼표·공백·`+`로 나눈 목록 */
export function splitList(text: string): string[] {
  return text.split(/[\s,+]+/).filter((s) => s !== "");
}

/**
 * NIC 주소 입력(`10.0.0.5`, `10.0.0.5, fd00::5`)을 읽는다. 빈 입력은 자동(서브넷에서 받음)이다. 주소 체계마다
 * 하나까지(서버의 `NicIps`). 틀리면 번역 키.
 */
export function parseIps(text: string): { ips: string[] } | { error: string } {
  const ips = splitList(text);
  const families = ips.map(ipFamily);
  if (families.some((f) => f === undefined)) {
    return { error: "sdn.errors.ip" };
  }
  if (new Set(families).size !== families.length) {
    return { error: "sdn.errors.ipFamily" };
  }
  return { ips };
}

/** NIC 주소를 한 줄로(없으면 "") */
export function formatIps(ips: readonly string[] | undefined): string {
  return (ips ?? []).join(", ");
}

/**
 * NIC마다의 주소 입력을 요청의 `ips`로(NIC 순번별, 빈 목록은 자동). 모두 비었으면 undefined(보내지 않음 — 옛 서버도
 * 받는다). 틀린 입력은 `errors[<prefix><i>]`에 적는다.
 */
export function ipsOverride(
  texts: readonly string[],
  errors: Record<string, string>,
  prefix: string,
): string[][] | undefined {
  const out = texts.map((text, i) => {
    const parsed = parseIps(text);
    if ("error" in parsed) {
      errors[`${prefix}${i}`] = parsed.error;
      return [];
    }
    return parsed.ips;
  });
  return out.some((ips) => ips.length > 0) ? out : undefined;
}
