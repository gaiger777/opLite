import type { AlertCondition, AlertRule, AlertRuleSpec } from "@/alerts/alerts";
import type { Severity } from "@/alerts/severity";
import type { components } from "@/api/schema";

type Schemas = components["schemas"];
export type ConditionKind = AlertCondition["kind"];
export type NodeMetric = Schemas["NodeAlertMetric"];
export type VmMetric = Schemas["VmAlertMetric"];
type Op = Schemas["AlertOp"];

export const CONDITION_KINDS: readonly ConditionKind[] = [
  "node_metric",
  "vm_metric",
  "pool_usage",
  "node_offline",
  "vm_failed",
  "task_failed",
  "ha_recovery",
  "backup_job_failed",
];
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
  return kind === "task_failed" || kind === "ha_recovery" || kind === "backup_job_failed";
}

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
  forSecs: string;
  severity: Severity;
  targets: string[];
  repeatSecs: string;
  sendResolved: boolean;
  /** 노드·VM 이름(쉼표로 나눔, 비면 모두) */
  nodes: string;
  vms: string;
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
    forSecs: "300",
    severity: "warning",
    targets: [],
    repeatSecs: "14400",
    sendResolved: true,
    nodes: "",
    vms: "",
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
    default:
      return { kind: form.kind };
  }
}

/**
 * 보낼 규칙. 이벤트 조건은 지속 시간이 0이고 반복이 없다(서버 기본값). `vms`는 VM 조건에만,
 * `nodes`는 백업 작업 조건이 아닐 때만 보낸다(서버가 다른 조합을 거절한다).
 */
export function ruleSpecOf(form: RuleForm): AlertRuleSpec {
  const event = isEvent(form.kind);
  const nodes = form.kind === "backup_job_failed" ? [] : splitNames(form.nodes);
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
