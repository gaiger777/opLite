// 서버 `opl-api` `placement.rs`의 `hard_violations` 시험 표를 옮긴 것(같은 입력 → 같은 어김 이름).
import type { Node, PlacementRule, Vm } from "@/api/types";
import { haGroup, NODE, placementRule, vm } from "@/test/fixtures";
import { hardViolations, type RuleSubject, subjectOf } from "@/placement/hard";

function node(name: string, labels: Record<string, string> = {}): Node {
  return { ...NODE, id: `id-${name}`, name, labels };
}
const n1 = node("n1");
const n2 = node("n2");
const n3 = node("n3");

/** 노드 `on`에서 켜진 VM */
function running(name: string, on: Node, extra: Partial<Vm["status"]> = {}): Vm {
  const v = vm(name, "running");
  return {
    ...v,
    spec: { ...v.spec, node: on.id, power: "running" },
    status: { ...v.status, ...extra },
  };
}
function rule(
  name: string,
  kind: PlacementRule["spec"]["kind"],
  vms: string[],
  nodes: string[] = [],
  extra: Partial<PlacementRule["spec"]> = {},
): PlacementRule {
  return placementRule(name, 1, { kind, vms, ...(nodes.length > 0 && { nodes }), ...extra });
}
const me: RuleSubject = { name: "me", haActive: false };

it("checks hard rules of each kind", () => {
  const vms = [running("web", n1)];
  const hard = (rules: PlacementRule[], n: Node) => hardViolations(me, n, rules, vms);
  let rules = [rule("together", "affinity", ["me", "web"])];
  expect(hard(rules, n1)).toEqual([]);
  expect(hard(rules, n2)).toEqual(["together"]);

  rules = [rule("apart", "anti-affinity", ["me", "web"])];
  expect(hard(rules, n1)).toEqual(["apart"]);
  expect(hard(rules, n2)).toEqual([]);

  rules = [rule("only", "node-affinity", ["me"], ["n2", "n3"])];
  expect(hard(rules, n1)).toEqual(["only"]);
  expect(hard(rules, n3)).toEqual([]);

  rules = [rule("never", "node-anti-affinity", ["me"], ["n2"])];
  expect(hard(rules, n1)).toEqual([]);
  expect(hard(rules, n2)).toEqual(["never"]);

  // 규칙에 들지 않은 VM은 상관없다. soft 규칙은 hard 위반이 아니다.
  expect(hardViolations({ name: "other", haActive: false }, n2, rules, vms)).toEqual([]);
  rules = [rule("pref", "soft-node-anti-affinity", ["me"], ["n2"])];
  expect(hard(rules, n2)).toEqual([]);
});

it("picks VMs by label and nodes by zone", () => {
  const a = node("n1", { zone: "rack-a" });
  const b = node("n2", { zone: "rack-b" });
  const peer = { ...running("peer", a), labels: { app: "db" } };
  const rules = [
    placementRule("in-a", 1, { kind: "node-affinity", vm_tag: "app=db", node_tag: "zone=rack-b" }),
    placementRule("apart", 2, { kind: "anti-affinity", vm_tag: "app=db" }),
  ];
  const vms = [peer, running("plain", b)];
  const db: RuleSubject = { name: "me", labels: { app: "db" }, haActive: false };
  expect(hardViolations(db, a, rules, vms)).toEqual(["apart", "in-a"]);
  expect(hardViolations(db, b, rules, vms)).toEqual([]);
  // 레이블이 없는 VM은 태그 규칙에 들지 않는다.
  expect(hardViolations({ name: "plain2", haActive: false }, a, rules, vms)).toEqual([]);
});

it("does not count stopped VMs and holds both nodes of a migrating VM", () => {
  const rules = [rule("apart", "anti-affinity", ["me", "web"])];
  const stopped = { ...vm("web", "stopped"), spec: { ...vm("web", "stopped").spec, node: n1.id } };
  expect(hardViolations(me, n1, rules, [stopped])).toEqual([]);
  const moving = running("web", n1, {
    migration: { node: n2.id, task: "t" } as Vm["status"]["migration"],
  });
  expect(hardViolations(me, n1, rules, [moving])).toEqual(["apart"]);
  expect(hardViolations(me, n2, rules, [moving])).toEqual(["apart"]);
  expect(hardViolations(me, n3, rules, [moving])).toEqual([]);
});

it("ignores disabled rules and the VM itself", () => {
  const rules = [rule("never", "node-anti-affinity", ["me"], ["n1"], { enabled: false })];
  expect(hardViolations(me, n1, rules, [])).toEqual([]);
  // 있는 VM은 자기 자리를 다른 구성원으로 세지 않는다.
  const self = running("me", n1);
  expect(
    hardViolations(subjectOf(self), n2, [rule("apart", "anti-affinity", ["me", "x"])], [self]),
  ).toEqual([]);
});

it("keeps HA VMs of a restricted group on the group's nodes", () => {
  const groups = [haGroup("pair", 1, { restricted: true })];
  const ha: RuleSubject = { name: "me", haActive: true, haGroup: "pair" };
  expect(hardViolations(ha, node("node-a"), [], [], groups)).toEqual([]);
  expect(hardViolations(ha, node("node-c"), [], [], groups)).toEqual(["ha-group:pair"]);
  // 제한 없는 그룹·HA가 꺼진 VM은 막지 않는다.
  expect(hardViolations({ ...ha, haActive: false }, node("node-c"), [], [], groups)).toEqual([]);
  expect(hardViolations(ha, node("node-c"), [], [], [haGroup("pair", 1)])).toEqual([]);
});

it("reads the subject of an existing VM", () => {
  const v = { ...vm("db", "running"), labels: { app: "db" } };
  v.spec = {
    ...v.spec,
    ha: { state: "started", group: "pair", max_restart: 1, max_relocate: 1, priority: 50 },
  };
  expect(subjectOf(v)).toEqual({
    id: v.id,
    name: "db",
    labels: { app: "db" },
    haActive: true,
    haGroup: "pair",
  });
  v.spec = { ...v.spec, ha: { state: "ignored", max_restart: 1, max_relocate: 1, priority: 50 } };
  expect(subjectOf(v).haActive).toBe(false);
});
