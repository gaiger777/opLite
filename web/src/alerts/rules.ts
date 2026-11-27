import type { AlertCondition, AlertRule, AlertRuleSpec } from "@/alerts/alerts";
import type { Severity } from "@/alerts/severity";
import type { components } from "@/api/schema";

type Schemas = components["schemas"];
export type ConditionKind = AlertCondition["kind"];
export type NodeMetric = Schemas["NodeAlertMetric"];
export type VmMetric = Schemas["VmAlertMetric"];
type Op = Schemas["AlertOp"];
export type LinkRole = Schemas["LinkRole"];

export const CONDITION_KINDS: readonly ConditionKind[] = [
  "node_metric",
  "vm_metric",
  "pool_usage",
  "node_offline",
  "vm_failed",
  "task_failed",
  "ha_recovery",
  "backup_job_failed",
  // SDN(OVN, 계획 67 §11) — 리더가 평가한다.
  "sdn_quorum_lost",
  "sdn_central_down",
  "sdn_chassis_down",
  "sdn_gateway_unavailable",
  "sdn_gateway_failover",
  "sdn_sync_failed",
  "sdn_physnet_not_ready",
  "sdn_lb_member_down",
  // 백업 저장소(계획 68 §10.5·§6) — 리더가 평가한다.
  "backup_verify_failed",
  "backup_store_unavailable",
  // 링크(계획 75 §3.4) — 리더가 노드 상태의 포트·본딩으로 평가한다.
  "node_link_down",
  "bond_degraded",
  // DR(계획 76 §3.4) — 리더가 복구 계획의 보호 상태와 실행으로 평가한다.
  "dr_rpo_exceeded",
  "dr_run_failed",
];
export const LINK_ROLES: readonly LinkRole[] = ["uplink", "any"];
export const NODE_METRICS: readonly NodeMetric[] = [
  "cpu",
  "mem_percent",
  "fs_percent",
  "disk_read",
  "disk_write",
  "net_rx",
  "net_tx",
];
export const VM_METRICS: readonly VmMetric[] = [
  "cpu",
  "mem_rss",
  "disk_read",
  "disk_write",
  "disk_read_iops",
  "disk_write_iops",
  "net_rx",
  "net_tx",
];

/** 한 번 일어나는 일(지속 시간이 없고 해제도 없다) */
export function isEvent(kind: ConditionKind): boolean {
  return (
    kind === "task_failed" ||
    kind === "ha_recovery" ||
    kind === "backup_job_failed" ||
    kind === "sdn_gateway_failover" ||
    kind === "dr_run_failed"
  );
}

/** DR 조건(서버 `AlertCondition::is_dr`) — 노드·VM이 아니라 복구 계획(`plans`, 비면 모두)으로 거른다. */
export function isDr(kind: ConditionKind): kind is "dr_rpo_exceeded" | "dr_run_failed" {
  return kind === "dr_rpo_exceeded" || kind === "dr_run_failed";
}

/** 백업 저장소 조건(서버 `AlertCondition::is_backup_store`) — 노드·VM으로 거르지 않는다. */
export function isBackupStore(kind: ConditionKind): boolean {
  return kind === "backup_verify_failed" || kind === "backup_store_unavailable";
}

/** 링크 조건(서버 `AlertCondition::is_link`) */
export function isLink(kind: ConditionKind): boolean {
  return kind === "node_link_down" || kind === "bond_degraded";
}

/** `NodeLinkDown.ports` 글롭(서버 `valid_port_glob`): 1~64자, 공백·`/`·`:` 없음, `*`·`?` */
export function validPortGlob(text: string): boolean {
  return /^[!-~]{1,64}$/.test(text) && !/[/:]/.test(text);
}

/** SDN 조건(서버 `AlertCondition::is_sdn`) */
export function isSdn(kind: ConditionKind): boolean {
  return kind.startsWith("sdn_");
}

/**
 * `nodes`로 거를 수 있는 조건: 백업 작업 조건이 아니고, SDN 조건이면 대상이 노드인 것만(중앙 꺼짐·chassis 꺼짐·physnet
 * 준비 안 됨 — 나머지는 서버가 400 `NodesFilter`)
 */
export function takesNodes(kind: ConditionKind): boolean {
  if (kind === "backup_job_failed" || isBackupStore(kind) || isDr(kind)) {
    return false;
  }
  return (
    !isSdn(kind) ||
    kind === "sdn_central_down" ||
    kind === "sdn_chassis_down" ||
    kind === "sdn_physnet_not_ready"
  );
}

/** "SDN 경보 켜기"가 만드는 권하는 규칙(§11 — 이름·조건·심각도·지속 시간 초) */
export const SDN_DEFAULTS: readonly {
  name: string;
  kind: ConditionKind;
  severity: Severity;
  forSecs: number;
}[] = [
  { name: "sdn-quorum-lost", kind: "sdn_quorum_lost", severity: "critical", forSecs: 0 },
  { name: "sdn-central-down", kind: "sdn_central_down", severity: "warning", forSecs: 60 },
  { name: "sdn-chassis-down", kind: "sdn_chassis_down", severity: "warning", forSecs: 60 },
  {
    name: "sdn-gateway-unavailable",
    kind: "sdn_gateway_unavailable",
    severity: "critical",
    forSecs: 30,
  },
  { name: "sdn-gateway-failover", kind: "sdn_gateway_failover", severity: "info", forSecs: 0 },
  { name: "sdn-sync-failed", kind: "sdn_sync_failed", severity: "warning", forSecs: 120 },
  {
    name: "sdn-physnet-not-ready",
    kind: "sdn_physnet_not_ready",
    severity: "warning",
    forSecs: 60,
  },
  { name: "sdn-lb-member-down", kind: "sdn_lb_member_down", severity: "warning", forSecs: 30 },
];

/** "링크 경보 켜기"(계획 75 §3.4, 서버 `link_default_rules`)가 만드는 권하는 규칙 */
export const LINK_DEFAULTS: typeof SDN_DEFAULTS = [
  { name: "node-link-down", kind: "node_link_down", severity: "critical", forSecs: 30 },
  { name: "bond-degraded", kind: "bond_degraded", severity: "warning", forSecs: 60 },
];

/** `vms`로 거를 수 있는 조건(서버의 `about_vms`) */
export function aboutVms(kind: ConditionKind): boolean {
  return kind === "vm_metric" || kind === "vm_failed" || kind === "ha_recovery";
}

/** 규칙 폼. 숫자는 입력 그대로(글)이고 보낼 때 바꾼다. */
export interface RuleForm {
  name: string;
  enabled: boolean;
  kind: ConditionKind;
  nodeMetric: NodeMetric;
  vmMetric: VmMetric;
  op: Op;
  value: string;
  percent: string;
  /** `task_failed`의 Task 종류(쉼표로 나눔, 비면 모두) */
  ops: string;
  /** `node_link_down`의 포트 글롭(비면 모두)과 역할 */
  ports: string;
  role: LinkRole;
  forSecs: string;
  severity: Severity;
  targets: string[];
  repeatSecs: string;
  sendResolved: boolean;
  /** 노드·VM 이름(쉼표로 나눔, 비면 모두) */
  nodes: string;
  vms: string;
  /** DR 조건의 복구 계획 이름(쉼표로 나눔, 비면 모두) */
  plans: string;
  /** 폼이 고치지 않는 조건(인증서 조건 — `CONDITION_KINDS` 밖)의 받은 값. 고치면 그대로 돌려보낸다(문턱값을 지키려고). */
  rawCondition?: AlertCondition;
}

export function emptyRuleForm(): RuleForm {
  return {
    name: "",
    enabled: true,
    kind: "node_metric",
    nodeMetric: "cpu",
    vmMetric: "cpu",
    op: "gt",
    value: "90",
    percent: "85",
    ops: "",
    ports: "",
    role: "uplink",
    forSecs: "300",
    severity: "warning",
    targets: [],
    repeatSecs: "14400",
    sendResolved: true,
    nodes: "",
    vms: "",
    plans: "",
  };
}

export function ruleFormOf(rule: AlertRule): RuleForm {
  const spec = rule.spec;
  const c = spec.condition;
  return {
    ...emptyRuleForm(),
    name: rule.name,
    enabled: spec.enabled,
    kind: c.kind,
    ...(c.kind === "node_metric" && { nodeMetric: c.metric, op: c.op, value: String(c.value) }),
    ...(c.kind === "vm_metric" && { vmMetric: c.metric, op: c.op, value: String(c.value) }),
    ...(c.kind === "pool_usage" && { percent: String(c.percent) }),
    ...(c.kind === "task_failed" && { ops: (c.ops ?? []).join(", ") }),
    ...(c.kind === "node_link_down" && { ports: c.ports ?? "", role: c.role ?? "uplink" }),
    ...((c.kind === "dr_rpo_exceeded" || c.kind === "dr_run_failed") && {
      plans: (c.plans ?? []).join(", "),
    }),
    ...(!CONDITION_KINDS.includes(c.kind) && { rawCondition: c }),
    forSecs: String(spec.for_secs),
    severity: spec.severity,
    targets: [...spec.targets],
    repeatSecs: String(spec.repeat_secs),
    sendResolved: spec.send_resolved,
    nodes: (spec.nodes ?? []).join(", "),
    vms: (spec.vms ?? []).join(", "),
  };
}

/** 쉼표·공백으로 나눈 이름 목록 */
export function splitNames(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter((s) => s !== "");
}

function integer(text: string): number | null {
  return /^\d+$/.test(text.trim()) ? Number(text.trim()) : null;
}

function decimal(text: string): number | null {
  const trimmed = text.trim();
  const n = Number(trimmed);
  return trimmed !== "" && Number.isFinite(n) ? n : null;
}

/** 폼의 오류(칸 → 번역 키). `taken`은 만들 때 이미 있는 규칙 이름이다. */
export function checkRuleForm(form: RuleForm, taken: string[] | null): Record<string, string> {
  const errors: Record<string, string> = {};
  if (taken) {
    if (!/^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(form.name)) {
      errors.name = "alerts.rules.errors.name";
    } else if (taken.includes(form.name)) {
      errors.name = "alerts.rules.errors.taken";
    }
  }
  if ((form.kind === "node_metric" || form.kind === "vm_metric") && decimal(form.value) === null) {
    errors.value = "alerts.rules.errors.value";
  }
  if (form.kind === "pool_usage") {
    const p = decimal(form.percent);
    if (p === null || p < 0 || p > 100) {
      errors.percent = "alerts.rules.errors.percent";
    }
  }
  if (
    form.kind === "node_link_down" &&
    form.ports.trim() !== "" &&
    !validPortGlob(form.ports.trim())
  ) {
    errors.ports = "alerts.rules.errors.ports";
  }
  if (
    isDr(form.kind) &&
    splitNames(form.plans).some((p) => !/^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(p))
  ) {
    errors.plans = "alerts.rules.errors.plans";
  }
  if (!isEvent(form.kind)) {
    const f = integer(form.forSecs);
    if (f === null || f > 86_400) {
      errors.forSecs = "alerts.rules.errors.forSecs";
    }
    const r = integer(form.repeatSecs);
    if (r === null || (r !== 0 && (r < 60 || r > 604_800))) {
      errors.repeatSecs = "alerts.rules.errors.repeatSecs";
    }
  }
  if (form.targets.length === 0 || form.targets.length > 16) {
    errors.targets = "alerts.rules.errors.targets";
  }
  return errors;
}

function conditionOf(form: RuleForm): AlertCondition {
  switch (form.kind) {
    case "node_metric":
      return {
        kind: "node_metric",
        metric: form.nodeMetric,
        op: form.op,
        value: decimal(form.value)!,
      };
    case "vm_metric":
      return { kind: "vm_metric", metric: form.vmMetric, op: form.op, value: decimal(form.value)! };
    case "pool_usage":
      return { kind: "pool_usage", percent: decimal(form.percent)! };
    case "task_failed": {
      const ops = splitNames(form.ops);
      return ops.length > 0 ? { kind: "task_failed", ops } : { kind: "task_failed" };
    }
    case "node_link_down": {
      const ports = form.ports.trim();
      return { kind: "node_link_down", role: form.role, ...(ports !== "" && { ports }) };
    }
    case "dr_rpo_exceeded":
    case "dr_run_failed": {
      const plans = splitNames(form.plans);
      return plans.length > 0 ? { kind: form.kind, plans } : { kind: form.kind };
    }
    // 인증서 조건(계획 73)은 아직 폼에 없다(CONDITION_KINDS 밖). 받은 조건을 그대로, 없으면 서버 기본값으로 보낸다.
    case "certificate_expiring":
      return form.rawCondition?.kind === form.kind
        ? form.rawCondition
        : { kind: "certificate_expiring", days: 14 };
    case "certificate_renew_failed":
      return form.rawCondition?.kind === form.kind
        ? form.rawCondition
        : { kind: "certificate_renew_failed", failures: 3 };
    default:
      return { kind: form.kind };
  }
}

/**
 * 보낼 규칙. 이벤트 조건은 지속 시간이 0이고 반복이 없다(서버 기본값). `vms`는 VM 조건에만,
 * `nodes`는 노드로 거를 수 있는 조건에만(`takesNodes`) 보낸다(서버가 다른 조합을 거절한다).
 */
export function ruleSpecOf(form: RuleForm): AlertRuleSpec {
  const event = isEvent(form.kind);
  const nodes = takesNodes(form.kind) ? splitNames(form.nodes) : [];
  const vms = aboutVms(form.kind) ? splitNames(form.vms) : [];
  return {
    enabled: form.enabled,
    condition: conditionOf(form),
    for_secs: event ? 0 : integer(form.forSecs)!,
    severity: form.severity,
    targets: form.targets,
    repeat_secs: event ? 14_400 : integer(form.repeatSecs)!,
    send_resolved: form.sendResolved,
    ...(nodes.length > 0 && { nodes }),
    ...(vms.length > 0 && { vms }),
  };
}
