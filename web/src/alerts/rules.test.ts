import type { AlertRule } from "@/alerts/alerts";
import {
  checkRuleForm,
  CONDITION_KINDS,
  emptyRuleForm,
  isEvent,
  isSdn,
  ruleFormOf,
  ruleSpecOf,
  SDN_DEFAULTS,
  takesNodes,
} from "@/alerts/rules";

const rule = (spec: Partial<AlertRule["spec"]>): AlertRule => ({
  id: "r1",
  name: "cpu-hot",
  revision: 3,
  labels: {},
  spec: {
    enabled: true,
    condition: { kind: "node_metric", metric: "cpu", op: "gt", value: 90 },
    for_secs: 300,
    severity: "critical",
    targets: ["ops-mail"],
    repeat_secs: 14_400,
    send_resolved: true,
    ...spec,
  },
  status: {},
});

it("round-trips a rule through the form", () => {
  const r = rule({ nodes: ["node-a"] });
  expect(ruleSpecOf(ruleFormOf(r))).toEqual(r.spec);
  const vm = rule({
    condition: { kind: "vm_metric", metric: "mem_rss", op: "lt", value: 512.5 },
    vms: ["web", "db"],
    repeat_secs: 0,
    send_resolved: false,
    enabled: false,
  });
  expect(ruleSpecOf(ruleFormOf(vm))).toEqual(vm.spec);
  const tasks = rule({
    condition: { kind: "task_failed", ops: ["vm-start", "vm-migrate"] },
    for_secs: 0,
  });
  expect(ruleSpecOf(ruleFormOf(tasks))).toEqual(tasks.spec);
});

it("sends events without a duration and filters only where the server takes them", () => {
  const form = {
    ...emptyRuleForm(),
    name: "backups",
    kind: "backup_job_failed" as const,
    forSecs: "600",
    nodes: "node-a",
    vms: "web",
    targets: ["ops"],
  };
  expect(ruleSpecOf(form)).toEqual({
    enabled: true,
    condition: { kind: "backup_job_failed" },
    for_secs: 0,
    severity: "warning",
    targets: ["ops"],
    repeat_secs: 14_400,
    send_resolved: true,
  });
  // 노드 조건에는 VM 거르기를 보내지 않는다.
  const node = ruleSpecOf({ ...form, kind: "node_offline", forSecs: "60" });
  expect(node.nodes).toEqual(["node-a"]);
  expect(node.vms).toBeUndefined();
  expect(node.for_secs).toBe(60);
  // 모든 Task는 ops 없이
  expect(ruleSpecOf({ ...form, kind: "task_failed", ops: " , " }).condition).toEqual({
    kind: "task_failed",
  });
});

it("checks the form like the server", () => {
  const ok = { ...emptyRuleForm(), name: "cpu-hot", targets: ["ops"] };
  expect(checkRuleForm(ok, [])).toEqual({});
  expect(checkRuleForm(ok, ["cpu-hot"])).toEqual({ name: "alerts.rules.errors.taken" });
  // 고칠 때는 이름을 보지 않는다.
  expect(checkRuleForm({ ...ok, name: "Bad Name" }, null)).toEqual({});
  expect(
    checkRuleForm(
      { ...ok, name: "-x", value: "high", forSecs: "1.5", repeatSecs: "30", targets: [] },
      [],
    ),
  ).toEqual({
    name: "alerts.rules.errors.name",
    value: "alerts.rules.errors.value",
    forSecs: "alerts.rules.errors.forSecs",
    repeatSecs: "alerts.rules.errors.repeatSecs",
    targets: "alerts.rules.errors.targets",
  });
  expect(checkRuleForm({ ...ok, kind: "pool_usage", percent: "101" }, [])).toEqual({
    percent: "alerts.rules.errors.percent",
  });
  // 이벤트는 지속 시간·반복을 보지 않는다.
  expect(checkRuleForm({ ...ok, kind: "ha_recovery", forSecs: "x", repeatSecs: "x" }, [])).toEqual(
    {},
  );
});

it("offers SDN conditions and filters nodes only on node subjects (plan 67)", () => {
  const form = { ...emptyRuleForm(), name: "q", targets: ["ops"], nodes: "node-a" };
  // 정족수(대상 SDN)는 노드로 거르지 않고, chassis 꺼짐(대상 노드)은 거른다.
  expect(ruleSpecOf({ ...form, kind: "sdn_quorum_lost" })).not.toHaveProperty("nodes");
  expect(ruleSpecOf({ ...form, kind: "sdn_chassis_down" }).nodes).toEqual(["node-a"]);
  expect(takesNodes("sdn_lb_member_down")).toBe(false);
  expect(takesNodes("node_metric")).toBe(true);
  // 게이트웨이 넘김은 이벤트(지속 시간 0)
  expect(isEvent("sdn_gateway_failover")).toBe(true);
  expect(ruleSpecOf({ ...form, kind: "sdn_gateway_failover", forSecs: "60" }).for_secs).toBe(0);
  expect(CONDITION_KINDS.filter(isSdn)).toHaveLength(8);
  expect(SDN_DEFAULTS.map((r) => r.kind).sort()).toEqual(CONDITION_KINDS.filter(isSdn).sort());
});

it("does not filter the backup store conditions by node or VM", () => {
  expect(CONDITION_KINDS).toContain("backup_verify_failed");
  expect(CONDITION_KINDS).toContain("backup_store_unavailable");
  expect(takesNodes("backup_verify_failed")).toBe(false);
  expect(takesNodes("backup_store_unavailable")).toBe(false);
  expect(isEvent("backup_verify_failed")).toBe(false);
  const spec = ruleSpecOf({
    ...emptyRuleForm(),
    name: "verify",
    kind: "backup_verify_failed",
    nodes: "node-a",
    targets: ["mail"],
  });
  expect(spec.condition).toEqual({ kind: "backup_verify_failed" });
  expect(spec).not.toHaveProperty("nodes");
});

it("keeps a certificate rule's threshold when another field is edited", () => {
  const expiring = rule({ condition: { kind: "certificate_expiring", days: 30 } });
  const form = { ...ruleFormOf(expiring), severity: "warning" as const };
  expect(ruleSpecOf(form).condition).toEqual({ kind: "certificate_expiring", days: 30 });
  const failing = rule({ condition: { kind: "certificate_renew_failed", failures: 5 } });
  expect(ruleSpecOf(ruleFormOf(failing)).condition).toEqual({
    kind: "certificate_renew_failed",
    failures: 5,
  });
  // 다른 조건으로 바꾸면 받은 조건을 쓰지 않는다
  expect(ruleSpecOf({ ...ruleFormOf(expiring), kind: "node_offline" }).condition).toEqual({
    kind: "node_offline",
  });
});
