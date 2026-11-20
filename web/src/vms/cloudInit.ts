import type { components } from "@/api/schema";
import { validIpv6 } from "@/storage/pools";

type Schemas = components["schemas"];
export type CloudInit = Schemas["CloudInit"];
export type IpConfig = Schemas["IpConfig"];

/** 서버가 응답에서 저장된 비밀번호(해시) 대신 보이는 값(`PASSWORD_MASK`). 되보내면 저장된 해시를 둔다. */
export const PASSWORD_MASK = "********";
/** 서버의 상한(`opl_types::cloud_init`) */
export const MAX_SSH_KEYS = 32;
const MAX_SSH_KEYS_BYTES = 16 * 1024;
const MAX_PASSWORD_BYTES = 256;
export const MAX_DATA_BYTES = 64 * 1024;
const MAX_DNS_SERVERS = 3;
const MAX_DNS_SEARCH = 6;
/** 서버가 받는 SSH 공개 키 종류(줄 앞, 뒤에 공백) */
const SSH_KEY_TYPES = [
  "ssh-ed25519 ",
  "ssh-rsa ",
  "ecdsa-sha2-nistp256 ",
  "ecdsa-sha2-nistp384 ",
  "ecdsa-sha2-nistp521 ",
  "sk-ssh-ed25519@openssh.com ",
  "sk-ecdsa-sha2-nistp256@openssh.com ",
];

/**
 * NIC 하나의 주소 입력. IPv4 `dhcp`와 IPv6 `none`이 기본(서버가 쓰는 것과 같은 결과)이다. IPv4 `none`은 IPv6만 쓰는
 * NIC다(서버가 DHCPv4를 넣지 않는다).
 */
export interface NetRow {
  ipv4: "none" | "dhcp" | "static";
  ipv4Address: string;
  ipv4Gateway: string;
  ipv6: "none" | "dhcp" | "slaac" | "static";
  ipv6Address: string;
  ipv6Gateway: string;
}

export const DEFAULT_NET: NetRow = {
  ipv4: "dhcp",
  ipv4Address: "",
  ipv4Gateway: "",
  ipv6: "none",
  ipv6Address: "",
  ipv6Gateway: "",
};

/** cloud-init 입력. 끄더라도 값은 남겨 다시 켤 때 쓴다. */
export interface CloudInitValues {
  enabled: boolean;
  /** 비면 VM 이름 */
  hostname: string;
  /** 비면 배포판 기본 사용자 */
  user: string;
  /** 저장된 비밀번호가 있다. 새 비밀번호를 넣지 않으면 가림 값을 되보내 그대로 둔다. */
  passwordSet: boolean;
  /** 새 비밀번호(평문이나 crypt 해시). 비면 바꾸지 않는다. */
  password: string;
  /** SSH 공개 키, 한 줄에 하나 */
  sshKeys: string;
  /** NIC 순번마다의 주소. NIC보다 짧으면 나머지는 기본이다. */
  networks: NetRow[];
  /** 저장된 설정에 `networks`가 있었다. 없었고 모두 기본이면 보내지 않는다(network-config를 만들지 않음). */
  explicitNetworks: boolean;
  /** DNS 서버(쉼표·공백으로 나눔) */
  dns: string;
  /** 검색 도메인(쉼표·공백으로 나눔) */
  search: string;
  userData: string;
  vendorData: string;
}

export function emptyCloudInit(): CloudInitValues {
  return {
    enabled: false,
    hostname: "",
    user: "",
    passwordSet: false,
    password: "",
    sshKeys: "",
    networks: [],
    explicitNetworks: false,
    dns: "",
    search: "",
    userData: "",
    vendorData: "",
  };
}

/** 주소 설정 하나. `none`(없음)은 undefined */
export function ipConfigFrom(
  mode: NetRow["ipv4"] | NetRow["ipv6"],
  address: string,
  gateway: string,
): IpConfig | undefined {
  if (mode === "none") {
    return undefined;
  }
  if (mode === "static") {
    return {
      mode,
      address: address.trim(),
      ...(gateway.trim() !== "" && { gateway: gateway.trim() }),
    };
  }
  return { mode };
}

function netRowFrom(net: Schemas["CloudInitNet"]): NetRow {
  const v4 = net.ipv4;
  const v6 = net.ipv6;
  return {
    // IPv4 칸의 slaac는 서버가 받지 않는다. IPv6만 있으면 IPv4 없음이고, 둘 다 없으면(빈 항목) dhcp와 같다.
    ipv4: v4?.mode === "static" ? "static" : !v4 && v6 ? "none" : "dhcp",
    ipv4Address: v4?.mode === "static" ? v4.address : "",
    ipv4Gateway: v4?.mode === "static" ? (v4.gateway ?? "") : "",
    ipv6: v6 ? v6.mode : "none",
    ipv6Address: v6?.mode === "static" ? v6.address : "",
    ipv6Gateway: v6?.mode === "static" ? (v6.gateway ?? "") : "",
  };
}

/** 저장된 설정(응답 — 비밀번호는 가려짐)의 입력 값 */
export function cloudInitFrom(ci: CloudInit | undefined): CloudInitValues {
  if (!ci) {
    return emptyCloudInit();
  }
  return {
    enabled: true,
    hostname: ci.hostname ?? "",
    user: ci.user ?? "",
    passwordSet: ci.password != null,
    password: "",
    sshKeys: (ci.ssh_keys ?? []).join("\n"),
    networks: (ci.networks ?? []).map(netRowFrom),
    explicitNetworks: (ci.networks ?? []).length > 0,
    dns: (ci.dns?.servers ?? []).join(", "),
    search: (ci.dns?.search ?? []).join(", "),
    userData: ci.user_data ?? "",
    vendorData: ci.vendor_data ?? "",
  };
}

/** 키 글상자의 줄(앞뒤 공백을 떼고 빈 줄은 건너뜀) */
export function keyLines(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

function list(text: string): string[] {
  return text.split(/[\s,]+/).filter((s) => s !== "");
}

function bytes(text: string): number {
  return new TextEncoder().encode(text).length;
}

function sameNet(a: NetRow, b: NetRow): boolean {
  return (Object.keys(a) as (keyof NetRow)[]).every((k) => a[k] === b[k]);
}

/**
 * 입력을 VM spec의 `cloud_init`으로. 꺼져 있으면 undefined. 빈 칸은 보내지 않는다. `sdn[i]`가 참인 NIC(SDN 네트워크,
 * 계획 65)는 주소를 IPAM이 정하므로 빈 항목 `{}`만 보낸다(서버는 그 자리에 다른 것을 받지 않는다 — 400
 * `cloud_init_sdn_nic`).
 */
export function toCloudInit(
  v: CloudInitValues,
  nics: number,
  sdn: readonly boolean[] = [],
): CloudInit | undefined {
  if (!v.enabled) {
    return undefined;
  }
  const rows = Array.from({ length: nics }, (_, i) => v.networks[i] ?? DEFAULT_NET);
  const custom = v.explicitNetworks || rows.some((r, i) => !sdn[i] && !sameNet(r, DEFAULT_NET));
  const keys = keyLines(v.sshKeys);
  const servers = list(v.dns);
  const search = list(v.search);
  const password = v.password !== "" ? v.password : v.passwordSet ? PASSWORD_MASK : undefined;
  return {
    ...(v.hostname.trim() !== "" && { hostname: v.hostname.trim() }),
    ...(v.user.trim() !== "" && { user: v.user.trim() }),
    ...(password !== undefined && { password }),
    ...(keys.length > 0 && { ssh_keys: keys }),
    ...(custom &&
      rows.length > 0 && {
        networks: rows.map((r, i) => {
          if (sdn[i]) {
            return {};
          }
          const ipv6 = ipConfigFrom(r.ipv6, r.ipv6Address, r.ipv6Gateway);
          return {
            ipv4: ipConfigFrom(r.ipv4, r.ipv4Address, r.ipv4Gateway),
            ...(ipv6 && { ipv6 }),
          };
        }),
      }),
    ...((servers.length > 0 || search.length > 0) && {
      dns: {
        ...(servers.length > 0 && { servers }),
        ...(search.length > 0 && { search }),
      },
    }),
    ...(v.userData !== "" && { user_data: v.userData }),
    ...(v.vendorData !== "" && { vendor_data: v.vendorData }),
  };
}

/**
 * `user_data`가 만든 user-data를 대신해 사용자·비밀번호·SSH 키가 쓰이지 않는다(서버의 `orbivisor-warning`과 같은
 * 조건).
 */
export function userDataWarning(v: CloudInitValues): boolean {
  return (
    v.enabled &&
    v.userData !== "" &&
    (v.user.trim() !== "" || v.password !== "" || v.passwordSet || keyLines(v.sshKeys).length > 0)
  );
}

/** SSH 키 글상자의 오류(번역 키와 값). 없으면 null */
export function validateKeys(
  text: string,
): { key: string; values?: Record<string, string> } | null {
  const keys = keyLines(text);
  if (keys.length > MAX_SSH_KEYS) {
    return { key: "vmForm.errors.ciKeyCount", values: { max: String(MAX_SSH_KEYS) } };
  }
  const bad = keys.findIndex((k) => !SSH_KEY_TYPES.some((t) => k.startsWith(t)));
  if (bad !== -1) {
    return { key: "vmForm.errors.ciKey", values: { n: String(bad + 1) } };
  }
  if (keys.reduce((n, k) => n + bytes(k), 0) > MAX_SSH_KEYS_BYTES) {
    return { key: "vmForm.errors.ciKeysSize" };
  }
  return null;
}

/** IPv4 주소(서버의 `Ipv4Addr`처럼 0 앞자리 없는 네 칸) */
export function validIpv4(addr: string): boolean {
  const parts = addr.split(".");
  return parts.length === 4 && parts.every((p) => /^(0|[1-9]\d{0,2})$/.test(p) && Number(p) <= 255);
}

function validIp(addr: string, ipv6: boolean): boolean {
  return ipv6 ? addr.includes(":") && validIpv6(addr) : validIpv4(addr);
}

/** `주소/접두 길이`인지(서버의 `parse_address`) */
function validAddress(text: string, ipv6: boolean): boolean {
  const [addr = "", prefix, ...rest] = text.trim().split("/");
  return (
    rest.length === 0 &&
    prefix !== undefined &&
    /^\d{1,3}$/.test(prefix) &&
    Number(prefix) <= (ipv6 ? 128 : 32) &&
    validIp(addr, ipv6)
  );
}

/**
 * cloud-init이 해시로 보는 crypt 해시(`$6$`·`$5$`·`$1$`·`$2a$`·`$2y$`)의 엄격한 모양인지(서버의 `is_crypt_hash`). 아니면
 * 평문이다.
 */
export function isCryptHash(s: string): boolean {
  return (
    /^\$6\$(rounds=\d{1,9}\$)?[./0-9A-Za-z]{1,16}\$[./0-9A-Za-z]{86}$/.test(s) ||
    /^\$5\$(rounds=\d{1,9}\$)?[./0-9A-Za-z]{1,16}\$[./0-9A-Za-z]{43}$/.test(s) ||
    /^\$1\$[./0-9A-Za-z]{1,8}\$[./0-9A-Za-z]{22}$/.test(s) ||
    /^\$2[ay]\$\d{2}\$[./0-9A-Za-z]{53}$/.test(s)
  );
}

/** cloud-init이 해시로 보지 않아 평문으로 설정해 버리는 해시(yescrypt `$y$`, bcrypt `$2b$`)인지(서버가 400으로 거절) */
export function isUnsupportedHash(s: string): boolean {
  return s.startsWith("$y$") || s.startsWith("$2b$");
}

/** 서버의 `valid_host`(IP가 아닌 RFC 1123 이름, FQDN 253자까지) */
export function validHostname(name: string): boolean {
  return (
    name.length <= 253 &&
    !validIpv4(name) &&
    !(name.includes(":") && validIpv6(name)) &&
    name.split(".").every((l) => /^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$/.test(l))
  );
}

/**
 * 입력 오류: 필드(`ci.<칸>`, NIC는 `ci.networks.<i>.<칸>`) → 번역 키. 서버의 `CloudInit::validate`와 같은 규칙이고, 꺼져
 * 있으면 보지 않는다. 오류 값이 필요한 것(키 줄 번호)은 `keyError`로 따로 준다.
 */
export function cloudInitErrors(
  v: CloudInitValues,
  nics: number,
  sdn: readonly boolean[] = [],
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!v.enabled) {
    return errors;
  }
  if (v.hostname.trim() !== "" && !validHostname(v.hostname.trim())) {
    errors["ci.hostname"] = "vmForm.errors.ciHostname";
  }
  const user = v.user.trim();
  if (user === "root") {
    errors["ci.user"] = "vmForm.errors.ciRoot";
  } else if (user !== "" && !/^[a-z_][a-z0-9_-]{0,31}$/.test(user)) {
    errors["ci.user"] = "vmForm.errors.ciUser";
  }
  if (isUnsupportedHash(v.password)) {
    errors["ci.password"] = "vmForm.errors.ciPasswordHash";
  } else if (
    v.password !== "" &&
    bytes(v.password) > MAX_PASSWORD_BYTES &&
    !isCryptHash(v.password)
  ) {
    errors["ci.password"] = "vmForm.errors.ciPassword";
  }
  if (validateKeys(v.sshKeys)) {
    errors["ci.sshKeys"] = validateKeys(v.sshKeys)!.key;
  }
  for (let i = 0; i < Math.min(nics, v.networks.length); i++) {
    // SDN NIC의 주소 칸은 쓰지 않는다(IPAM이 정함).
    if (sdn[i]) {
      continue;
    }
    const r = v.networks[i]!;
    for (const ipv6 of [false, true]) {
      const [mode, address, gateway, field] = ipv6
        ? [r.ipv6, r.ipv6Address, r.ipv6Gateway, "ipv6"]
        : [r.ipv4, r.ipv4Address, r.ipv4Gateway, "ipv4"];
      if (!ipv6 && mode === "none" && r.ipv6 === "none") {
        errors[`ci.networks.${i}.ipv4`] = "vmForm.errors.ciIpv4None";
      }
      if (mode !== "static") {
        continue;
      }
      if (!validAddress(address, ipv6)) {
        errors[`ci.networks.${i}.${field}Address`] = "vmForm.errors.ciAddress";
      }
      if (gateway.trim() !== "" && !validIp(gateway.trim(), ipv6)) {
        errors[`ci.networks.${i}.${field}Gateway`] = "vmForm.errors.ciGateway";
      }
    }
  }
  const servers = list(v.dns);
  if (servers.length > MAX_DNS_SERVERS || servers.some((s) => !validIp(s, s.includes(":")))) {
    errors["ci.dns"] = "vmForm.errors.ciDns";
  }
  const search = list(v.search);
  if (search.length > MAX_DNS_SEARCH || search.some((s) => !validHostname(s))) {
    errors["ci.search"] = "vmForm.errors.ciSearch";
  }
  if (bytes(v.userData) > MAX_DATA_BYTES) {
    errors["ci.userData"] = "vmForm.errors.ciData";
  }
  if (bytes(v.vendorData) > MAX_DATA_BYTES) {
    errors["ci.vendorData"] = "vmForm.errors.ciData";
  }
  return errors;
}

/** 요약에 쓸 값: 사용자, 키 수, 비밀번호 유무, NIC 주소(IPv4는 정적만, IPv6는 있으면) */
export function cloudInitSummary(ci: CloudInit): {
  user: string | null;
  keys: number;
  password: boolean;
  addresses: string[];
} {
  const addresses = (ci.networks ?? []).flatMap((n, i) => {
    const out: string[] = [];
    if (n.ipv4?.mode === "static") {
      out.push(`nic${i} ${n.ipv4.address}${n.ipv4.gateway ? ` gw ${n.ipv4.gateway}` : ""}`);
    }
    if (n.ipv6?.mode === "static") {
      out.push(`nic${i} ${n.ipv6.address}${n.ipv6.gateway ? ` gw ${n.ipv6.gateway}` : ""}`);
    } else if (n.ipv6) {
      out.push(`nic${i} ipv6 ${n.ipv6.mode}`);
    }
    return out;
  });
  return {
    user: ci.user ?? null,
    keys: (ci.ssh_keys ?? []).length,
    password: ci.password != null,
    addresses,
  };
}
