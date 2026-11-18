import type { AlertRule } from "@/alerts/alerts";
import { checkRuleForm, emptyRuleForm, ruleFormOf, ruleSpecOf } from "@/alerts/rules";

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
