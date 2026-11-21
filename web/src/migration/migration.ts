// 단순 → SDN 네트워크 옮기기(계획 67 §9)의 질의와 순수 도움 함수. 목록은 watch(`network-migration`)가 오면 다시 읽는다.
import { useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { StatusTone } from "@/components/StatusChip";

type Schemas = components["schemas"];
export type Migration = Schemas["Resource_NetworkMigrationSpec_NetworkMigrationStatus"];
export type MigrationSpec = Schemas["NetworkMigrationSpec"];
export type MigrationPlan = Schemas["MigrationPlan"];
export type PlannedNic = Schemas["PlannedNic"];
export type MigrationMode = Schemas["MigrationMode"];
export type MigrationPhase = Schemas["MigrationPhase"];
export type VmMove = Schemas["VmMove"];
export type MoveState = Schemas["MoveState"];
export type PlannedMode = Schemas["PlannedMode"];

export const MIGRATIONS_KEY = ["network-migrations"] as const;
export const MODES: readonly MigrationMode[] = ["auto", "live", "restart", "offline"];
/** 서버의 `parallel` 한도 */
export const MAX_PARALLEL = 8;

/** 옮기기 목록(끝난 기록 포함) */
export function useMigrations(enabled = true) {
  return useQuery({
    queryKey: MIGRATIONS_KEY,
    queryFn: () => call(api.GET("/network-migrations")),
    select: (list) => list.items,
    enabled,
  });
}

/** 끝나지 않은 옮기기(서버 `NetworkMigrationStatus::is_done`의 반대) */
export function unfinished(m: Migration): boolean {
  return m.status.phase !== "finished" && m.status.phase !== "rolled-back";
}

/** 그 원본 네트워크의 끝나지 않은 옮기기(한 원본에 하나 — Raft `migration-source`) */
export function migrationOf(list: readonly Migration[] | undefined, source: string) {
  return list?.find((m) => m.spec.source === source && unfinished(m));
}

/** 옮긴 NIC 수와 옮길 NIC 수(막힌 것 빼고 — 토폴로지의 진행 수와 같다) */
export function progress(m: Migration): { moved: number; total: number } {
  const vms = m.status.vms ?? [];
  return {
    moved: vms.filter((v) => v.state === "moved").length,
    total: vms.filter((v) => v.mode !== "blocked").length,
  };
}

export const PHASE_TONE: Record<MigrationPhase, StatusTone> = {
  planned: "neutral",
  running: "info",
  paused: "warning",
  moved: "success",
  finished: "success",
  "rolling-back": "info",
  "rolled-back": "neutral",
  failed: "danger",
};

export const STATE_TONE: Record<MoveState, StatusTone> = {
  pending: "neutral",
  moving: "info",
  moved: "success",
  failed: "danger",
  "rolling-back": "info",
  "rolled-back": "neutral",
};

export const MODE_TONE: Record<PlannedMode, StatusTone> = {
  offline: "neutral",
  live: "success",
  restart: "warning",
  blocked: "danger",
};

/**
 * 계획을 정하는 칸만 남긴 스펙의 열쇠(VM 빼기는 화면에서만 하므로 `vms`는 뺀다 — 리뷰 1 I6). 화면의 계획을 만든 스펙과
 * 지금 고른 것이 다르면 시작하기 전에 다시 계획해야 한다(리뷰 1 C1).
 */
export function planKey(spec: MigrationSpec): string {
  const addresses = [...(spec.addresses ?? [])]
    .filter((a) => a.ip.length > 0)
    .map((a) => `${a.vm}/${a.mac.toLowerCase()}=${[...a.ip].sort().join(",")}`)
    .sort();
  return JSON.stringify([
    spec.source,
    spec.target,
    spec.mode ?? "auto",
    spec.bridge ?? null,
    spec.bridge_node ?? null,
    spec.allow_mtu_shrink ?? false,
    spec.parallel ?? 1,
    addresses,
  ]);
}

/** 계획의 경고 수(NIC마다 + 다리) */
export function planWarnings(plan: MigrationPlan): number {
  return plan.nics.reduce((sum, n) => sum + n.warnings.length, 0) + plan.bridge.warnings.length;
}

/** 화면의 계획과 만들 때 서버가 다시 한 계획이 다른 것 하나 */
export interface PlanChange {
  vm: string;
  name: string;
  nic: number;
  /** 새로 들어옴·방식·주소·막힘이 바뀜 */
  what: "new" | "mode" | "ips" | "blockers";
  before?: string;
  after: string;
  /** 켜진 VM이 이제 꺼졌다 켜지거나 라이브로 옮겨진다(눈에 띄게) */
  disrupts: boolean;
}

/**
 * 화면에 보인 계획 `shown`(빼지 않은 VM만)과 만들기가 돌려준 계획 `created`의 다른 점(리뷰 1 C1 — 그새 켜진 VM은 `auto`에서
 * `offline` → `live`·`restart`가 된다). 다리 노드·오류는 Task가 다시 보므로 NIC만 견준다.
 */
export function planChanges(
  shown: MigrationPlan,
  created: MigrationPlan,
  excluded: ReadonlySet<string> = new Set(),
): PlanChange[] {
  const out: PlanChange[] = [];
  const before = new Map(shown.nics.map((n) => [`${n.vm}/${n.nic}`, n]));
  const sameList = (a: readonly string[], b: readonly string[]) =>
    [...a].sort().join("\n") === [...b].sort().join("\n");
  for (const n of created.nics) {
    if (excluded.has(n.vm)) {
      continue;
    }
    const was = before.get(`${n.vm}/${n.nic}`);
    const base = { vm: n.vm, name: n.name, nic: n.nic };
    const disrupts = n.mode === "live" || n.mode === "restart";
    if (!was) {
      out.push({ ...base, what: "new", after: n.mode, disrupts });
      continue;
    }
    if (was.mode !== n.mode) {
      out.push({ ...base, what: "mode", before: was.mode, after: n.mode, disrupts });
    }
    if (!sameList(was.ips, n.ips)) {
      out.push({
        ...base,
        what: "ips",
        before: was.ips.join(", "),
        after: n.ips.join(", "),
        disrupts: false,
      });
    }
    if (!sameList(was.blockers, n.blockers)) {
      out.push({
        ...base,
        what: "blockers",
        before: was.blockers.join("; "),
        after: n.blockers.join("; "),
        disrupts: false,
      });
    }
  }
  return out;
}

/** cloud-init instance-id가 바뀐다는 경고(스테이지 4 — 다음 켜기에 인스턴스마다 하는 일을 다시 함) */
export function isInstanceIdWarning(text: string): boolean {
  return text.startsWith("the cloud-init instance-id changes");
}

type Translate = (key: string, values?: Record<string, unknown>) => string;

/** 서버 계획의 영어 막힘·경고 가운데 웹이 아는 꼴(정규식 → 번역 키, 잡은 묶음 이름) */
const PLAN_TEXTS: readonly [RegExp, string, readonly string[]][] = [
  [/^a template is not migrated/, "migration.text.template", []],
  [/^no permission: vm\.config/, "migration.text.denied", []],
  [/^the VM is in another network migration$/, "migration.text.busy", []],
  [/^another NIC of the VM is already on "(.+)"$/, "migration.text.onTarget", ["network"]],
  [/^the NIC has no MAC address yet$/, "migration.text.noMac", []],
  [/^address (\S+) is outside the subnets of "(.+)"$/, "migration.text.outside", ["ip", "network"]],
  [/^address (\S+) is reserved in "(.+)"/, "migration.text.reserved", ["ip", "network"]],
  [/^address (\S+) is already in use on "(.+)"$/, "migration.text.inUse", ["ip", "network"]],
  [/^address (\S+) is also planned for another NIC$/, "migration.text.duplicate", ["ip"]],
  [/^the VM runs on a node that is not an SDN \(OVN\) chassis/, "migration.text.notChassis", []],
  [/^the VM is running \(mode offline\)$/, "migration.text.runningOffline", []],
  [
    /^cannot move it live: the VM's node is not an SDN \(OVN\) chassis$/,
    "migration.text.liveChassis",
    [],
  ],
  [/^cannot move it live: the MTU the guest sees would shrink$/, "migration.text.liveMtu", []],
  [/^cannot move it live: the address is not known$/, "migration.text.liveAddress", []],
  [/^the address changes: a new address comes from the target's pool$/, "migration.text.ipam", []],
  [/^no guest agent: /, "migration.text.noAgent", []],
  [/^the MTU shrinks from (\d+) to (\d+)/, "migration.text.mtu", ["from", "to"]],
  [/^the cloud-init instance-id changes/, "migration.text.instanceId", []],
  [/^the NIC has no security groups/, "migration.text.noGroups", []],
  [/^about a second without network$/, "migration.text.liveGap", []],
  [/^the VM is shut down and started again$/, "migration.text.restartGap", []],
  [/^the target is a provider network: no bridge/, "migration.text.providerNoBridge", []],
  [/^no bridge: moved VMs cannot reach/, "migration.text.noBridge", []],
  [/^a DHCP server on the source network also answers/, "migration.text.dhcp", []],
  [
    /^IPAM does not know the addresses other hosts use on the source network/,
    "migration.text.ipamOthers",
    [],
  ],
  [/^a bridge is only laid to an overlay target$/, "migration.text.bridgeOverlay", []],
  [/^node (\S+) cannot be the bridge node/, "migration.text.badBridgeNode", ["node"]],
  [/^no SDN chassis has the source bridge "(.+)"/, "migration.text.noBridgeNode", ["bridge"]],
  [
    /^the bridge node "(.+)" was removed from the cluster/,
    "migration.text.bridgeRemoved",
    ["node"],
  ],
];

/** 서버 계획의 문장을 아는 꼴이면 번역한다(모르는 꼴은 그대로). */
export function planText(t: Translate, text: string): string {
  for (const [pattern, key, names] of PLAN_TEXTS) {
    const m = pattern.exec(text);
    if (m) {
      const values: Record<string, string> = {};
      names.forEach((name, i) => {
        values[name] = m[i + 1] ?? "";
      });
      return t(key, values);
    }
  }
  return text;
}
