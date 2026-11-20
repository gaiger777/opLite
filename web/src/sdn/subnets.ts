import { cidrFamily } from "@/security-groups/rules";
import { ipFamily, splitList } from "@/sdn/ips";
import { MAX_DNS_SERVERS, MAX_POOLS, type Subnet } from "@/sdn/sdn";

/** 서버의 `MAX_FLOATING_POOLS` */
const MAX_FLOATING_POOLS = 16;

/** IPv6 주소 알리기(계획 66, 서버 `Ipv6Mode`). ""는 없음(cloud-init 고정 주소). */
export type Ipv6ModeValue = "" | NonNullable<Subnet["ipv6_mode"]>;

/** 서버의 `MAX_SUBNET_DESCRIPTION` */
const MAX_DESCRIPTION = 200;

/** 서브넷 표의 행. 칸은 입력 그대로의 문자열이다. */
export interface SubnetRow {
  cidr: string;
  /** 비면 첫 호스트 주소(서버가 정함) */
  gateway: string;
  /** IPv4만 뜻이 있다(IPv6 DHCP는 아직 없다). */
  dhcp: boolean;
  /** 공급자 네트워크의 IPv4 서브넷만: `시작-끝` 목록, 라우터 외부 주소·유동 IP를 고르는 범위(계획 66) */
  floating: string;
  /** IPv6 서브넷만(계획 66) */
  ipv6Mode: Ipv6ModeValue;
  /** 쉼표·공백으로 나눈 DNS 서버 */
  dns: string;
  /** `시작-끝`을 쉼표·공백으로 나눈 자동 할당 범위 */
  pools: string;
  description: string;
  /** 저장된 서브넷: `cidr`·`gateway`를 바꿀 수 없다(빼고 다시). */
  saved: boolean;
}

/** 새 행. 공급자 네트워크의 DHCP는 기본 끔이다(물리 망의 DHCP 서버와 다투지 않게 — 서버 `subnet_dhcp`). */
export function emptySubnetRow(provider = false): SubnetRow {
  return {
    cidr: "",
    gateway: "",
    dhcp: !provider,
    floating: "",
    ipv6Mode: "",
    dns: "",
    pools: "",
    description: "",
    saved: false,
  };
}

function formatRanges(ranges: Subnet["pools"]): string {
  return (ranges ?? []).map((p) => `${p.start}-${p.end}`).join(", ");
}

export function subnetRow(s: Subnet, provider = false): SubnetRow {
  return {
    cidr: s.cidr,
    gateway: s.gateway ?? "",
    dhcp: s.dhcp ?? (!s.cidr.includes(":") && !provider),
    floating: formatRanges(s.floating_pools),
    ipv6Mode: s.ipv6_mode ?? "",
    dns: (s.dns_servers ?? []).join(", "),
    pools: formatRanges(s.pools),
    description: s.description ?? "",
    saved: true,
  };
}

/** 행의 주소 체계(CIDR 모양으로) */
export function rowFamily(row: SubnetRow): 4 | 6 | undefined {
  return cidrFamily(row.cidr.trim());
}

/**
 * 서브넷 표의 입력 오류: `subnets.<i>.<칸>` → 번역 키. 서버의 `Subnet::validate`의 흔한 것만 보고(접두사 길이, 주소
 * 모양, 체계마다 하나, 개수) 범위 안팎·겹침은 서버가 400 `invalid_subnet`으로 알린다.
 */
export function subnetErrors(rows: readonly SubnetRow[], provider = false): Record<string, string> {
  const errors: Record<string, string> = {};
  const seen = new Set<number>();
  rows.forEach((row, i) => {
    const at = (field: string) => `subnets.${i}.${field}`;
    const cidr = row.cidr.trim();
    const [, prefix] = cidr.split("/");
    const family = cidrFamily(cidr);
    const bits = Number(prefix);
    if (
      family === undefined ||
      prefix === undefined ||
      (family === 4 ? bits < 8 || bits > 29 : bits < 48 || bits > 124)
    ) {
      errors[at("cidr")] = "sdn.errors.cidr";
      return;
    }
    if (seen.has(family)) {
      errors[at("cidr")] = "sdn.errors.family";
    }
    seen.add(family);
    const gateway = row.gateway.trim();
    if (gateway !== "" && ipFamily(gateway) !== family) {
      errors[at("gateway")] = "sdn.errors.gateway";
    }
    const dns = splitList(row.dns);
    if (dns.length > MAX_DNS_SERVERS || dns.some((d) => ipFamily(d) === undefined)) {
      errors[at("dns")] = "sdn.errors.dns";
    }
    const pools = splitPools(row.pools);
    if (
      pools === null ||
      pools.length > MAX_POOLS ||
      pools.some((p) => ipFamily(p.start) !== family || ipFamily(p.end) !== family)
    ) {
      errors[at("pools")] = "sdn.errors.pools";
    }
    if (family === 4 && provider) {
      const floating = splitPools(row.floating);
      if (
        floating === null ||
        floating.length > MAX_FLOATING_POOLS ||
        floating.some((p) => ipFamily(p.start) !== 4 || ipFamily(p.end) !== 4)
      ) {
        errors[at("floating")] = "sdn.errors.floating";
      }
    }
    // SLAAC 계열은 EUI-64라 /64만(서버 `SlaacNeeds64`)
    if (
      family === 6 &&
      (row.ipv6Mode === "slaac" || row.ipv6Mode === "dhcpv6-stateless") &&
      bits !== 64
    ) {
      errors[at("ipv6Mode")] = "sdn.errors.slaac64";
    }
    if (row.description.length > MAX_DESCRIPTION) {
      errors[at("description")] = "sdn.errors.description";
    }
  });
  return errors;
}

/** `시작-끝` 목록. 모양이 틀리면 null */
function splitPools(text: string): { start: string; end: string }[] | null {
  const out: { start: string; end: string }[] = [];
  for (const part of text.split(/[\s,]+/).filter((s) => s !== "")) {
    const [start, end, ...rest] = part.split("-");
    if (!start || !end || rest.length > 0) {
      return null;
    }
    out.push({ start, end });
  }
  return out;
}

/**
 * 행을 서버의 `Subnet`으로. 빈 칸은 보내지 않는다(서버 기본). 검사를 지난 행에만 부른다. `provider`면 공급자
 * 네트워크의 서브넷이다(DHCP 기본 끔, 유동 IP 풀).
 */
export function toSubnet(row: SubnetRow, provider = false): Subnet {
  const gateway = row.gateway.trim();
  const dns = splitList(row.dns);
  const pools = splitPools(row.pools) ?? [];
  const family = rowFamily(row);
  const floating = family === 4 && provider ? (splitPools(row.floating) ?? []) : [];
  const description = row.description.trim();
  return {
    cidr: row.cidr.trim(),
    ...(gateway !== "" && { gateway }),
    // IPv4의 기본은 켬(공급자는 끔), IPv6는 끔만 받는다. 기본과 다를 때만 보낸다.
    ...(family === 4 && row.dhcp === provider && { dhcp: row.dhcp }),
    ...(dns.length > 0 && { dns_servers: dns }),
    ...(pools.length > 0 && { pools }),
    ...(floating.length > 0 && { floating_pools: floating }),
    ...(family === 6 && row.ipv6Mode !== "" && { ipv6_mode: row.ipv6Mode }),
    ...(description !== "" && { description }),
  };
}
