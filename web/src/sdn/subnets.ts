import { cidrFamily } from "@/security-groups/rules";
import { ipFamily, splitList } from "@/sdn/ips";
import { MAX_DNS_SERVERS, MAX_POOLS, type Subnet } from "@/sdn/sdn";

/** 서버의 `MAX_SUBNET_DESCRIPTION` */
const MAX_DESCRIPTION = 200;

/** 서브넷 표의 행. 칸은 입력 그대로의 문자열이다. */
export interface SubnetRow {
  cidr: string;
  /** 비면 첫 호스트 주소(서버가 정함) */
  gateway: string;
  /** IPv4만 뜻이 있다(IPv6 DHCP는 아직 없다). */
  dhcp: boolean;
  /** 쉼표·공백으로 나눈 DNS 서버 */
  dns: string;
  /** `시작-끝`을 쉼표·공백으로 나눈 자동 할당 범위 */
  pools: string;
  description: string;
  /** 저장된 서브넷: `cidr`·`gateway`를 바꿀 수 없다(빼고 다시). */
  saved: boolean;
}

export function emptySubnetRow(): SubnetRow {
  return { cidr: "", gateway: "", dhcp: true, dns: "", pools: "", description: "", saved: false };
}

export function subnetRow(s: Subnet): SubnetRow {
  return {
    cidr: s.cidr,
    gateway: s.gateway ?? "",
    dhcp: s.dhcp ?? !s.cidr.includes(":"),
    dns: (s.dns_servers ?? []).join(", "),
    pools: (s.pools ?? []).map((p) => `${p.start}-${p.end}`).join(", "),
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
export function subnetErrors(rows: readonly SubnetRow[]): Record<string, string> {
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

/** 행을 서버의 `Subnet`으로. 빈 칸은 보내지 않는다(서버 기본). 검사를 지난 행에만 부른다. */
export function toSubnet(row: SubnetRow): Subnet {
  const gateway = row.gateway.trim();
  const dns = splitList(row.dns);
  const pools = splitPools(row.pools) ?? [];
  const description = row.description.trim();
  return {
    cidr: row.cidr.trim(),
    ...(gateway !== "" && { gateway }),
    // IPv4의 기본은 켬, IPv6는 끔만 받는다.
    ...(rowFamily(row) === 4 && !row.dhcp && { dhcp: false }),
    ...(dns.length > 0 && { dns_servers: dns }),
    ...(pools.length > 0 && { pools }),
    ...(description !== "" && { description }),
  };
}
