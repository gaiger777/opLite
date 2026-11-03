import type { components } from "@/api/schema";
import type { Rule } from "@/api/types";

type Schemas = components["schemas"];

/** 서버의 `MAX_RULES` */
export const MAX_RULES = 200;
/** 서버의 `MAX_DESCRIPTION` */
export const MAX_DESCRIPTION = 200;
/** 서버의 `MAX_NIC_SECURITY_GROUPS` */
export const MAX_NIC_SECURITY_GROUPS = 8;

/** 규칙 표의 한 행. 프로토콜이 없으면 `any`, 포트와 원격 주소가 없으면 빈 글이다. */
export interface RuleRow {
  direction: Schemas["Direction"];
  action: Schemas["Action"];
  protocol: Schemas["Protocol"] | "any";
  ports: string;
  remote: string;
}

export function emptyRule(): RuleRow {
  return { direction: "in", action: "accept", protocol: "tcp", ports: "", remote: "" };
}

export function fromRule(rule: Rule): RuleRow {
  return {
    direction: rule.direction,
    action: rule.action,
    protocol: rule.protocol ?? "any",
    ports: rule.ports ?? "",
    remote: rule.remote ?? "",
  };
}

/** 포트는 tcp·udp에서만 쓴다. */
export function hasPorts(protocol: RuleRow["protocol"]): boolean {
  return protocol === "tcp" || protocol === "udp";
}

/** 행을 API 규칙으로. 빈 값은 보내지 않는다. */
export function toRule(row: RuleRow): Rule {
  const rule: Rule = { direction: row.direction, action: row.action };
  if (row.protocol !== "any") {
    rule.protocol = row.protocol;
  }
  if (hasPorts(row.protocol) && row.ports.trim() !== "") {
    rule.ports = row.ports.trim();
  }
  if (row.remote.trim() !== "") {
    rule.remote = row.remote.trim();
  }
  return rule;
}

/** 서버의 `PortRange`: `22` 또는 `8000-8080`(1–65535, 시작 ≤ 끝) */
export function validPorts(text: string): boolean {
  const m = /^(\d{1,5})(?:-(\d{1,5}))?$/.exec(text);
  if (!m) {
    return false;
  }
  const start = Number(m[1]);
  const end = m[2] === undefined ? start : Number(m[2]);
  return start >= 1 && end <= 65535 && start <= end;
}

/**
 * CIDR(`10.0.0.0/8`, `2001:db8::/32`, 접두사 생략 가능)의 주소 종류. 모양이 틀리면 undefined. IPv6
 * 주소의 자세한 문법과 호스트 비트는 서버가 본다.
 */
export function cidrFamily(text: string): 4 | 6 | undefined {
  const [addr = "", prefix, ...rest] = text.split("/");
  if (rest.length > 0 || (prefix !== undefined && !/^\d{1,3}$/.test(prefix))) {
    return undefined;
  }
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(addr);
  if (v4) {
    const octets = v4.slice(1).map(Number);
    return octets.every((o) => o <= 255) && (prefix === undefined || Number(prefix) <= 32)
      ? 4
      : undefined;
  }
  const v6 =
    addr.includes(":") &&
    /^[0-9a-f:.]+$/i.test(addr) &&
    addr.split("::").length <= 2 &&
    (prefix === undefined || Number(prefix) <= 128);
  return v6 ? 6 : undefined;
}

/** 규칙 행의 입력 오류: `rules.<i>.ports`·`rules.<i>.remote` → 번역 키. 나머지 검사는 서버가 한다. */
export function validateRules(rows: RuleRow[]): Record<string, string> {
  const errors: Record<string, string> = {};
  rows.forEach((row, i) => {
    if (hasPorts(row.protocol) && row.ports.trim() !== "" && !validPorts(row.ports.trim())) {
      errors[`rules.${i}.ports`] = "securityGroups.errors.ports";
    }
    const remote = row.remote.trim();
    if (remote === "") {
      return;
    }
    const family = cidrFamily(remote);
    if (family === undefined) {
      errors[`rules.${i}.remote`] = "securityGroups.errors.remote";
    } else if (row.protocol === "icmp" && family === 6) {
      errors[`rules.${i}.remote`] = "securityGroups.errors.icmpRemote";
    } else if (row.protocol === "icmpv6" && family === 4) {
      errors[`rules.${i}.remote`] = "securityGroups.errors.icmpv6Remote";
    }
  });
  return errors;
}

/** 규칙 `i`를 `by`(−1 위, +1 아래)만큼 옮긴 목록. 끝을 넘으면 그대로다. */
export function moveRule<T>(rows: T[], i: number, by: -1 | 1): T[] {
  const j = i + by;
  if (j < 0 || j >= rows.length) {
    return rows;
  }
  // 이웃한 두 행을 맞바꾼다.
  const lo = Math.min(i, j);
  return [...rows.slice(0, lo), ...rows.slice(lo, lo + 2).reverse(), ...rows.slice(lo + 2)];
}
