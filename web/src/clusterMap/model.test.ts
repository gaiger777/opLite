import { heatTone, labelKeys, permille, toModel } from "@/clusterMap/model";
import { load, mapNode, mapVm, nodeId, placement, vmId } from "@/test/clusterMap";
import type { AlertView } from "@/alerts/alerts";

const A = mapNode(1, "n10");
const B = mapNode(2, "n2", { labels: { zone: "a" } });

it("sorts nodes and VMs by name with numbers as numbers", () => {
  const m = toModel({
    nodes: [A, B],
    vms: [mapVm(1, "vm10", A.id), mapVm(2, "vm9", A.id), mapVm(3, "Vm1", B.id)],
  });
  expect(m.nodes.map((n) => n.name)).toEqual(["n2", "n10"]);
  expect(m.vms.map((v) => v.name)).toEqual(["Vm1", "vm9", "vm10"]);
  expect(m.placement).toBe(false);
  expect(m.name).toBeNull();
});

it("places VMs where they run, else where they are pinned, else nowhere", () => {
  const m = toModel({
    nodes: [A, B],
    vms: [
      // 돌고 있는 곳(status.node)이 고정한 곳보다 먼저
      mapVm(1, "running", A.id, "running", { status: { node: B.id } }),
      mapVm(2, "pinned", A.id, "stopped"),
      // 꺼진 새 VM, 지운 노드의 VM
      mapVm(3, "new", null, "stopped"),
      mapVm(4, "orphan", "gone", "running"),
      // 템플릿은 타일이 아니라 수로만
      mapVm(5, "tpl", A.id, "stopped", { spec: { template: true } }),
    ],
  });
  const at = Object.fromEntries(m.vms.map((v) => [v.name, v.node]));
  expect(at).toEqual({ running: B.id, pinned: A.id, new: null, orphan: null });
  expect(m.nodes.find((n) => n.id === A.id)!.templates).toBe(1);
});

it("turns /placement tenths of a percent into percent and hides stale fallbacks", () => {
  const m = toModel({
    nodes: [A, B],
    vms: [],
    placement: placement([
      load(A, {
        cpu_use_pct: 725,
        mem_use_pct: 641,
        mem_allocated_mib: 18350,
        mem_total_mib: 16384,
      }),
      // 신선한 요약이 없으면 cpu_use_pct는 다른 노드 평균의 대체값 — 부하처럼 보이지 않는다
      load(B, { age_secs: undefined }),
    ]),
  });
  const a = m.nodes.find((n) => n.id === A.id)!;
  const b = m.nodes.find((n) => n.id === B.id)!;
  expect(a.load).toEqual({ cpu: 72.5, mem: 64.1 });
  expect(a.allocatedPct).toBe(112);
  expect(b.load).toBeNull();
  expect(b.allocatedPct).toBe(25);
  expect(m.placement).toBe(true);
  expect(permille(905)).toBe(90.5);
  // /placement을 못 읽으면 모두 모름
  const none = toModel({ nodes: [A], vms: [], placement: null });
  expect(none.nodes[0]!.load).toBeNull();
  expect(none.nodes[0]!.allocatedPct).toBeNull();
  expect(none.placement).toBe(false);
});

it("colours load by the higher of CPU and memory at 70 and 90", () => {
  expect(heatTone(null)).toBe("unknown");
  expect(heatTone({ cpu: 69.9, mem: 10 })).toBe("cool");
  expect(heatTone({ cpu: 10, mem: 70 })).toBe("warm");
  expect(heatTone({ cpu: 89.9, mem: 0 })).toBe("warm");
  expect(heatTone({ cpu: 0, mem: 90 })).toBe("hot");
});

it("counts VMs on a node that the user cannot read", () => {
  const m = toModel({
    nodes: [A, B],
    vms: [mapVm(1, "mine", A.id), mapVm(2, "off", A.id, "stopped")],
    // /placement은 고정된 노드에 켜 둔 VM을 센다: A에 셋(보이는 것 하나), B에 없음
    placement: placement([load(A, { vms: 3 }), load(B, { vms: 0 })]),
  });
  expect(m.nodes.find((n) => n.id === A.id)!.hiddenVms).toBe(2);
  expect(m.nodes.find((n) => n.id === B.id)!.hiddenVms).toBe(0);
  // 같은 기준이라 볼 수 있는 것을 다시 세지 않는다(음수 없음)
  const all = toModel({
    nodes: [A],
    vms: [mapVm(1, "a", A.id), mapVm(2, "b", A.id)],
    placement: placement([load(A, { vms: 1 })]),
  });
  expect(all.nodes[0]!.hiddenVms).toBe(0);
});

it("marks witnesses, maintenance, frozen nodes and the leader", () => {
  const w = mapNode(3, "w", { spec: { witness: true } });
  const frozen = mapNode(4, "f", {
    status: { online: false, shutdown: { action: "freeze", at: 1 } },
  });
  const maint = mapNode(5, "m", { spec: { maintenance: true } });
  const m = toModel({
    nodes: [w, frozen, maint],
    vms: [],
    cluster: { name: "prod", leader: maint.id },
  });
  const by = Object.fromEntries(m.nodes.map((n) => [n.name, n]));
  expect(by.w!.witness).toBe(true);
  expect(by.f!.frozen).toBe(true);
  expect(by.f!.online).toBe(false);
  expect(by.m!.maintenance).toBe(true);
  expect(by.m!.leader).toBe(true);
  expect(by.w!.leader).toBe(false);
  expect(m.name).toBe("prod");
  // /cluster를 못 읽으면 리더 없이
  expect(toModel({ nodes: [maint], vms: [], cluster: null }).nodes[0]!.leader).toBe(false);
});

it("turns migrations into moves with progress and keeps one arrow per VM", () => {
  const task = "019a0f3c-0000-7000-8c00-000000000001";
  const moving = mapVm(1, "web", A.id, "migrating", {
    spec: { power: "running" },
    status: { node: A.id, migration: { node: B.id, task } },
  });
  const m = toModel({
    nodes: [A, B],
    vms: [moving, mapVm(2, "db", A.id), mapVm(3, "api", A.id)],
    progress: new Map([[task, { stage: "memory", done_bytes: 620, total_bytes: 1000 }]]),
    placement: placement([load(A), load(B)], {
      recommendations: [
        // 같은 VM의 추천은 진행 중 화살표에 밀린다
        rec("r1", moving.id, "web", A.id, B.id),
        rec("r2", vmId(2), "db", A.id, B.id),
        // 이미 Task가 있는 추천(적용함·auto)은 마이그레이션 화살표로 보인다
        { ...rec("r3", vmId(3), "api", A.id, B.id), task: "t" },
        // 모르는 노드로 가는 추천은 그리지 않는다
        rec("r4", vmId(3), "api", A.id, nodeId(99)),
      ],
    }),
  });
  expect(m.moves).toEqual([
    {
      key: `move:${moving.id}`,
      kind: "migration",
      vm: moving.id,
      vmName: "web",
      from: A.id,
      to: B.id,
      progress: 62,
      stage: "memory",
      task,
      recommendation: null,
    },
    {
      key: "rec:r2",
      kind: "recommendation",
      vm: vmId(2),
      vmName: "db",
      from: A.id,
      to: B.id,
      progress: null,
      stage: null,
      task: null,
      recommendation: "r2",
    },
  ]);
  const web = m.vms.find((v) => v.name === "web")!;
  expect(web.node).toBe(A.id);
  expect(web.migration).toEqual({ to: B.id, task });
  expect(m.nodes.find((n) => n.id === A.id)!.outgoing).toBe(1);
  // 바이트 수를 모르면 단계 이름만
  const early = toModel({
    nodes: [A, B],
    vms: [moving],
    progress: new Map([[task, { stage: "disks", done_bytes: 0, total_bytes: 0 }]]),
  });
  expect(early.moves[0]).toMatchObject({ progress: null, stage: "disks" });
  // 모르는 대상으로의 마이그레이션은 화살표 없이 원본에
  const lost = toModel({
    nodes: [A],
    vms: [
      mapVm(1, "web", A.id, "migrating", {
        status: { node: A.id, migration: { node: "x", task } },
      }),
    ],
  });
  expect(lost.moves).toEqual([]);
  expect(lost.vms[0]!.migration).toBeNull();
});

function rec(id: string, vm: string, vmName: string, from: string, to: string) {
  return {
    id,
    vm,
    vm_name: vmName,
    from,
    from_name: "",
    to,
    to_name: "",
    reason: "balance" as const,
    gain_pct: 10,
    cost_mib: 1024,
  };
}

it("counts firing alerts on a node and keeps the highest severity", () => {
  const alert = (
    subject: AlertView["subject"],
    severity: AlertView["severity"],
    state: AlertView["state"] = "firing",
  ): AlertView => ({
    rule: "r",
    rule_name: "r",
    condition: "c",
    severity,
    since: 0,
    state,
    subject,
  });
  const m = toModel({
    nodes: [A, B],
    vms: [],
    alerts: [
      alert({ kind: "node", id: A.id, name: "n10" }, "warning"),
      // 주제의 node(이름)가 그 노드인 경보도
      alert({ kind: "vm", id: "v", name: "web", node: "n10" }, "critical"),
      alert({ kind: "pool", id: "p", name: "local", node: "n10" }, "info"),
      // 아직 pending인 것은 세지 않는다
      alert({ kind: "node", id: B.id, name: "n2" }, "critical", "pending"),
      alert({ kind: "vm", id: "v", name: "x", node: "elsewhere" }, "critical"),
    ],
  });
  const a = m.nodes.find((n) => n.id === A.id)!;
  expect(a.alerts).toBe(3);
  expect(a.severity).toBe("critical");
  expect(m.nodes.find((n) => n.id === B.id)!.alerts).toBe(0);
});

it("lists label keys for the group picker", () => {
  const m = toModel({
    nodes: [
      mapNode(1, "a", { labels: { zone: "x", rack: "1" } }),
      mapNode(2, "b", { labels: { room: "2" } }),
    ],
    vms: [],
  });
  expect(labelKeys(m.nodes)).toEqual(["rack", "room", "zone"]);
});

it("treats VMs with HA on and placement rule violations", () => {
  const m = toModel({
    nodes: [A],
    vms: [
      mapVm(1, "a", A.id, "running", { spec: { ha: true } }),
      mapVm(2, "b", A.id, "running", {
        spec: { ha: { state: "ignored", max_relocate: 1, max_restart: 1, priority: 0 } },
        status: { rule_violations: ["only-b", "apart"] },
      }),
      mapVm(3, "c", A.id, "running", {
        spec: { ha: { state: "started", max_relocate: 1, max_restart: 1, priority: 0 } },
      }),
    ],
  });
  expect(m.vms.map((v) => [v.name, v.ha, v.violations])).toEqual([
    ["a", true, 0],
    ["b", false, 2],
    ["c", true, 0],
  ]);
});
