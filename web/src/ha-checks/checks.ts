import type {
  CheckReport,
  HaCheck,
  HaCheckAction,
  HaCheckRole,
  HaCheckTarget,
  Node,
  StoragePool,
} from "@/api/types";
import { isShared, validIpv6 } from "@/storage/pools";
import { validName } from "@/vms/form";

type Spec = HaCheck["spec"];
export type TargetKind = HaCheckTarget["kind"];

/** 화면에 보이는 순서 */
export const ROLES: readonly HaCheckRole[] = ["storage", "vm", "management"];
export const ACTIONS: readonly HaCheckAction[] = ["warn", "migrate", "restart_elsewhere", "fence"];

/** 서버의 `DEFAULT_GRACE_SECS`·`MIN_GRACE_SECS`·`MAX_GRACE_SECS`·`MAX_PING_ADDRESSES`·`AUTO_PREFIX` */
export const DEFAULT_GRACE_SECS = 10;
export const MIN_GRACE_SECS = 10;
export const MAX_GRACE_SECS = 600;
export const MAX_PING_ADDRESSES = 8;
export const AUTO_PREFIX = "cluster-link-";

/** 서버의 `HaCheckRole::default_action` */
export function defaultAction(role: HaCheckRole): HaCheckAction {
  return role === "storage" ? "restart_elsewhere" : role === "vm" ? "migrate" : "warn";
}

/**
 * 서버의 `HaCheckRole::allows`. `management`는 경고·전원 끄기만, `storage`는 라이브 마이그레이션을 빼고(디스크
 * I/O가 멈춘 VM은 옮길 수 없다), `vm`은 넷 다.
 */
export function allowedActions(role: HaCheckRole): HaCheckAction[] {
  return ACTIONS.filter((a) =>
    role === "management"
      ? a === "warn" || a === "fence"
      : role === "storage"
        ? a !== "migrate"
        : true,
  );
}

/** 역할에 맞는 대상 종류. 풀 검사는 `storage`만 쓴다. */
export function targetKinds(role: HaCheckRole): TargetKind[] {
  return role === "storage" ? ["link", "ping", "pool"] : ["link", "ping"];
}

/** 대상 입력. 종류마다 쓰는 칸만 본다. */
export interface TargetForm {
  iface: string;
  /** 쉼표·공백으로 나눈 주소 */
  addresses: string;
  via: string;
  /** 비면 1 */
  need: string;
  pool: string;
}

/** 노드별 덮기 한 줄. 종류는 폼의 종류와 같다. */
export interface OverrideRow {
  node: string;
  target: TargetForm;
}

/** 만들기·바꾸기 폼의 값. 숫자도 입력 그대로 문자열로 둔다. */
export interface CheckForm {
  name: string;
  role: HaCheckRole;
  kind: TargetKind;
  target: TargetForm;
  /** 비면 역할의 기본 */
  onFail: HaCheckAction | "";
  /** 비면 기본(10초) */
  grace: string;
  overrides: OverrideRow[];
  enabled: boolean;
}

export function emptyTarget(): TargetForm {
  return { iface: "", addresses: "", via: "", need: "", pool: "" };
}

export function emptyCheckForm(): CheckForm {
  return {
    name: "",
    role: "storage",
    kind: "link",
    target: emptyTarget(),
    onFail: "",
    grace: "",
    overrides: [],
    enabled: true,
  };
}

function targetForm(target: HaCheckTarget): TargetForm {
  const form = emptyTarget();
  if (target.kind === "link") {
    form.iface = target.iface;
  } else if (target.kind === "ping") {
    form.addresses = target.addresses.join(", ");
    form.via = target.via ?? "";
    // 서버는 기본(1)을 쓰지 않는다.
    form.need = String(target.need ?? 1);
  } else {
    form.pool = target.name;
  }
  return form;
}

/** 저장된 검사의 폼. 서버는 기본값(`grace_secs` 10, `enabled` true)을 쓰지 않는다. */
export function fromSpec(check: HaCheck): CheckForm {
  const s = check.spec;
  return {
    name: check.name,
    role: s.role,
    kind: s.target.kind,
    target: targetForm(s.target),
    onFail: s.on_fail ?? "",
    grace: String(s.grace_secs ?? DEFAULT_GRACE_SECS),
    overrides: Object.entries(s.nodes ?? {})
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([node, target]) => ({ node, target: targetForm(target) })),
    enabled: s.enabled ?? true,
  };
}

function parseAddresses(text: string): string[] {
  return text.split(/[\s,]+/).filter((a) => a !== "");
}

/** 서버의 `IpAddr` 모양(IPv4 점 넷, 앞자리 0 없음, 또는 IPv6) */
export function validIp(addr: string): boolean {
  const octet = "(25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)";
  return (
    new RegExp(`^${octet}(\\.${octet}){3}$`).test(addr) || (addr.includes(":") && validIpv6(addr))
  );
}

/** 서버의 `valid_ifname`(관리 이름 `oplv*`·`oplx*`는 검사 대상이 될 수 없지만 서버는 막지 않는다) */
function validIface(name: string): boolean {
  return /^[\x21-\x7e]{1,15}$/.test(name) && !/[/:]/.test(name) && name !== "." && name !== "..";
}

function toTarget(kind: TargetKind, form: TargetForm): HaCheckTarget {
  if (kind === "link") {
    return { kind, iface: form.iface.trim() };
  }
  if (kind === "pool") {
    return { kind, name: form.pool };
  }
  const via = form.via.trim();
  return {
    kind,
    addresses: parseAddresses(form.addresses),
    need: Number(form.need.trim() || "1"),
    ...(via ? { via } : {}),
  };
}

/** 검사한 폼의 스펙. 빈 대응은 역할 기본, 빈 grace는 10초다. */
export function toSpec(form: CheckForm): Spec {
  const spec: Spec = {
    role: form.role,
    target: toTarget(form.kind, form.target),
    grace_secs: Number(form.grace.trim() || String(DEFAULT_GRACE_SECS)),
    enabled: form.enabled,
  };
  if (form.onFail !== "") {
    spec.on_fail = form.onFail;
  }
  if (form.overrides.length > 0) {
    spec.nodes = Object.fromEntries(
      form.overrides.map((o) => [o.node, toTarget(form.kind, o.target)]),
    );
  }
  return spec;
}

/** 대상 하나의 오류(번역 키). 없으면 undefined */
function targetError(kind: TargetKind, form: TargetForm): string | undefined {
  if (kind === "link") {
    return validIface(form.iface.trim()) ? undefined : "haChecks.errors.iface";
  }
  if (kind === "pool") {
    return form.pool ? undefined : "haChecks.errors.pool";
  }
  const addresses = parseAddresses(form.addresses);
  if (addresses.length === 0 || addresses.length > MAX_PING_ADDRESSES) {
    return "haChecks.errors.addressCount";
  }
  if (addresses.some((a) => !validIp(a))) {
    return "haChecks.errors.address";
  }
  if (new Set(addresses.map((a) => a.toLowerCase())).size !== addresses.length) {
    return "haChecks.errors.duplicate";
  }
  const need = form.need.trim();
  if (
    need !== "" &&
    !(/^\d+$/.test(need) && Number(need) >= 1 && Number(need) <= addresses.length)
  ) {
    return "haChecks.errors.need";
  }
  const via = form.via.trim();
  if (via !== "" && !validIface(via)) {
    return "haChecks.errors.iface";
  }
  return undefined;
}

/**
 * 서버와 같은 검사. 오류는 필드별 번역 키(`target`, `override.<i>` …)다. `taken`은 다른 검사의 이름이고
 * 바꾸기면 null(이름 없음).
 */
export function checkErrors(form: CheckForm, taken: string[] | null): Record<string, string> {
  const found: Record<string, string> = {};
  if (taken !== null) {
    if (!validName(form.name)) {
      found.name = "forms.name";
    } else if (form.name.startsWith(AUTO_PREFIX)) {
      found.name = "haChecks.errors.reserved";
    } else if (taken.includes(form.name)) {
      found.name = "haChecks.errors.exists";
    }
  }
  if (!targetKinds(form.role).includes(form.kind)) {
    found.target = "haChecks.errors.poolRole";
  } else {
    const error = targetError(form.kind, form.target);
    if (error) {
      found.target = error;
    }
  }
  if (form.onFail !== "" && !allowedActions(form.role).includes(form.onFail)) {
    found.onFail = "haChecks.errors.action";
  }
  const grace = form.grace.trim();
  if (
    grace !== "" &&
    !(/^\d+$/.test(grace) && Number(grace) >= MIN_GRACE_SECS && Number(grace) <= MAX_GRACE_SECS)
  ) {
    found.grace = "haChecks.errors.grace";
  }
  form.overrides.forEach((o, i) => {
    const error = o.node === "" ? "haChecks.errors.overrideNode" : targetError(form.kind, o.target);
    if (error) {
      found[`override.${i}`] = error;
    }
  });
  return found;
}

/** 같은 스펙인지. 바꾼 것이 없으면 저장 버튼을 막는다. */
export function sameSpec(a: Spec, b: Spec): boolean {
  const norm = (s: Spec) => {
    // 키 순서와 서버가 쓰지 않는 기본값에 상관없이 비교한다.
    const target = (t: HaCheckTarget) =>
      t.kind === "ping"
        ? [t.kind, t.addresses, t.via ?? null, t.need ?? 1]
        : [t.kind, t.kind === "link" ? t.iface : t.name];
    return JSON.stringify([
      s.role,
      target(s.target),
      s.on_fail ?? null,
      s.grace_secs ?? DEFAULT_GRACE_SECS,
      Object.entries(s.nodes ?? {})
        .sort(([x], [y]) => x.localeCompare(y))
        .map(([n, t]) => [n, target(t)]),
      s.enabled ?? true,
    ]);
  };
  return norm(a) === norm(b);
}

/** 표에 보이는 대상: `link:eth0`, `ping:10.0.0.1,10.0.0.2@bond1 need=2`, `pool:nas`(CLI와 같은 모양) */
export function targetText(target: HaCheckTarget): string {
  if (target.kind === "link") {
    return `link:${target.iface}`;
  }
  if (target.kind === "pool") {
    return `pool:${target.name}`;
  }
  const via = target.via ? `@${target.via}` : "";
  const need = (target.need ?? 1) > 1 ? ` need=${target.need}` : "";
  return `ping:${target.addresses.join(",")}${via}${need}`;
}

/** 인터페이스 이름 후보: 모든 노드가 보고한 인터페이스 이름의 합집합(이름 순) */
export function interfaceNames(nodes: readonly Node[]): string[] {
  const names = new Set<string>();
  for (const node of nodes) {
    for (const iface of node.status.interfaces ?? []) {
      names.add(iface.name);
    }
  }
  return [...names].sort();
}

/** 풀 검사가 볼 수 있는 풀: 공유 풀(서버도 `is_shared`가 아니면 거절한다) */
export function sharedPools(pools: readonly StoragePool[]): string[] {
  return pools
    .filter(isShared)
    .map((p) => p.name)
    .sort();
}

/** 노드가 보고한 검사 가운데 실패한 것 */
export function failingChecks(node: Node): CheckReport[] {
  return (node.status.checks ?? []).filter((c) => c.state === "failing");
}
