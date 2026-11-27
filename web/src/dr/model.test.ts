import { REFRESH_ON, VIEW_KINDS } from "@/api/live";
import type { Backup, Network, Task } from "@/api/types";
import type { DrStep } from "@/dr/api";
import {
  candidates,
  emptyDraft,
  failedPhase,
  formatDuration,
  localToSecs,
  moveGroup,
  newVm,
  phaseDots,
  protectionGaps,
  rpoCounts,
  rtoClock,
  sameSpec,
  STALL_SECS,
  stalledFor,
  stepNoteKey,
  testNetworkIssues,
  toDraft,
  toSpec,
  validate,
} from "@/dr/model";
import { planSpec } from "@/test/dr";
import { providerNetwork, sdnNetwork } from "@/test/fixtures";

it("round-trips a plan through the editor draft", () => {
  const spec = planSpec({ security_groups: { web: "dr-web" }, folder: "dr/prod" });
  const back = toSpec(toDraft("web-tier", spec));
  expect(back).toEqual({
    ...spec,
    source_backup_pool: undefined,
    test_networks: undefined,
    groups: spec.groups.map((g) => ({
      ...g,
      vms: g.vms.map((v) => ({
        vm: v.vm,
        pool: undefined,
        node: undefined,
        live: undefined,
        source_backup_pool: undefined,
        ip: v.ip?.map((r) => ({ ...r, dns: undefined })),
      })),
    })),
  });
  // RPO는 시간으로 고치고 초로 보낸다
  const draft = { ...toDraft("web-tier", spec), rpoHours: "1.5" };
  expect(toSpec(draft).rpo_target_secs).toBe(5400);
});

it("drops empty map rows and sends DHCP rules without an address", () => {
  const draft = emptyDraft();
  draft.source = "prod";
  draft.pool = "local";
  draft.networks = [
    { from: "lan", to: "dr-lan" },
    { from: "", to: "" },
  ];
  draft.groups[0]!.vms = [
    { ...newVm("db1"), ip: [{ nic: "0", dhcp: true, address: "x", gateway: "", dns: "" }] },
  ];
  const spec = toSpec(draft);
  expect(spec.networks).toEqual({ lan: "dr-lan" });
  expect(spec.security_groups).toBeUndefined();
  expect(spec.groups[0]!.vms[0]!.ip).toEqual([{ nic: 0, dhcp: true }]);
});

it("validates the draft like the server", () => {
  const draft = emptyDraft();
  expect(validate(draft, true)).toEqual({
    name: "dr.invalid.name",
    source: "dr.invalid.required",
    pool: "dr.invalid.name",
    "g0.vms": "dr.invalid.emptyGroup",
  });
  draft.name = "web-tier";
  draft.source = "prod";
  draft.pool = "local";
  draft.rpoHours = "-1";
  draft.groups = [
    { ...draft.groups[0]!, name: "db", timeout: "5", delay: "4000", vms: [newVm("db1")] },
    {
      ...draft.groups[0]!,
      key: "x",
      name: "db",
      vms: [
        {
          ...newVm("db1"),
          ip: [{ nic: "0", dhcp: false, address: "10.0.0.5", gateway: "", dns: "" }],
        },
      ],
    },
  ];
  expect(validate(draft, true)).toEqual({
    rpo: "dr.invalid.rpo",
    "g0.timeout": "dr.invalid.timeout",
    "g0.delay": "dr.invalid.delay",
    "g1.name": "dr.invalid.duplicateGroup",
    "g1.v0.ip0": "dr.invalid.address",
    vms: "dr.invalid.duplicateVm",
  });
  // 고친 계획은 이름을 다시 보지 않는다
  expect(
    validate(
      {
        ...emptyDraft(),
        name: "",
        source: "prod",
        pool: "local",
        groups: [{ ...draft.groups[0]!, timeout: "600", delay: "0" }],
      },
      false,
    ),
  ).toEqual({});
});

it("moves groups up and down and stops at the ends", () => {
  expect(moveGroup(["a", "b", "c"], 1, -1)).toEqual(["b", "a", "c"]);
  expect(moveGroup(["a", "b", "c"], 1, 1)).toEqual(["a", "c", "b"]);
  expect(moveGroup(["a", "b"], 0, -1)).toEqual(["a", "b"]);
  expect(moveGroup(["a", "b"], 1, 1)).toEqual(["a", "b"]);
});

function copy(vm: string, at: number, ca: string, phase: Backup["status"]["phase"] = "ready") {
  return {
    spec: { vm_name: vm, origin: { cluster: "prod", ca_fingerprint: ca, backup: "b" } },
    status: { phase, created_at: at },
  } as unknown as Backup;
}

it("offers remote VMs and this cluster's copies from the source as candidates", () => {
  const backups = [
    copy("db1", 100, "ca-prod"),
    copy("db1", 300, "ca-prod"),
    copy("old", 200, "ca-prod"),
    copy("other", 200, "ca-elsewhere"),
    copy("broken", 200, "ca-prod", "failed"),
  ];
  expect(candidates(["web1", "db1"], backups, "ca-prod")).toEqual([
    { vm: "db1", remote: true, copyAt: 300 },
    { vm: "old", remote: false, copyAt: 200 },
    { vm: "web1", remote: true },
  ]);
  // 원격에 닿지 않아도 사본으로 고른다. 원격이 없으면 사본을 맞출 수 없다.
  expect(candidates(undefined, backups, "ca-prod").map((c) => c.vm)).toEqual(["db1", "old"]);
  expect(candidates(undefined, backups, undefined)).toEqual([]);
});

it("counts VMs within RPO", () => {
  expect(
    rpoCounts([
      { vm: "a", ok: true },
      { vm: "b", ok: false },
      { vm: "c", ok: true },
    ]),
  ).toEqual({
    ok: 2,
    bad: 1,
    total: 3,
  });
  expect(rpoCounts(undefined)).toEqual({ ok: 0, bad: 0, total: 0 });
});

it("draws phase dots", () => {
  expect(phaseDots("pending")).toEqual(["todo", "todo", "todo", "todo"]);
  expect(phaseDots("starting")).toEqual(["done", "current", "todo", "todo"]);
  expect(phaseDots("ready")).toEqual(["done", "done", "done", "done"]);
  expect(phaseDots("timed_out")).toEqual(["done", "done", "failed", "todo"]);
  // 실패한 단계를 모르면 점은 모두 빈 칸(칩이 실패를 알린다), 알면 그 점이 빨갛다
  expect(phaseDots("failed")).toEqual(["todo", "todo", "todo", "todo"]);
  expect(phaseDots("failed", undefined, "restoring")).toEqual(["failed", "todo", "todo", "todo"]);
  expect(phaseDots("failed", undefined, "starting")).toEqual(["done", "failed", "todo", "todo"]);
  expect(phaseDots("skipped")).toEqual(["todo", "todo", "todo", "todo"]);
  expect(phaseDots("deleted", ["deleting", "deleted"])).toEqual(["done", "done"]);
});

it("runs the RTO clock only while the run goes", () => {
  expect(rtoClock({ state: "running", started_at: 1000 }, 1075)).toBe(75);
  expect(rtoClock({ state: "succeeded", started_at: 1000, rto_secs: 754 }, 9999)).toBe(754);
  expect(rtoClock({ state: "failed", started_at: 1000 }, 9999)).toBeUndefined();
  expect(formatDuration(754)).toBe("12m 34s");
  expect(formatDuration(3 * 3600 + 120)).toBe("3h 2m");
  expect(formatDuration(2 * 86_400 + 3600)).toBe("2d 1h");
});

it("refreshes plans and runs on watch events", () => {
  expect(VIEW_KINDS).toContain("recovery-plan");
  expect(VIEW_KINDS).toContain("dr-run");
  expect(REFRESH_ON["recovery-plan"]).toEqual([["dr-plans"]]);
  expect(REFRESH_ON["dr-run"]).toEqual([["dr-runs"], ["dr-plans"]]);
});

it("checks half-written, misnamed and repeated map rows instead of dropping them", () => {
  const draft = {
    ...emptyDraft(),
    source: "prod",
    pool: "local",
    groups: [{ ...emptyDraft().groups[0]!, vms: [newVm("db1")] }],
    networks: [
      { from: "lan", to: "dr-lan" },
      { from: "lan", to: "dr-other" },
      { from: " ", to: "" },
    ],
    securityGroups: [{ from: "web", to: "" }],
    testNetworks: [{ from: "Lan", to: "iso" }],
  };
  expect(validate(draft, false)).toEqual({
    net1: "dr.invalid.mapDuplicate",
    sg0: "dr.invalid.mapHalf",
    tnet0: "dr.invalid.name",
  });
});

it("compares plan specs regardless of key order", () => {
  const spec = planSpec();
  const shuffled = Object.fromEntries(Object.entries(spec).reverse());
  expect(sameSpec(spec, shuffled)).toBe(true);
  expect(sameSpec(spec, { ...spec, folder: undefined })).toBe(true);
  expect(sameSpec(spec, { ...spec, rpo_target_secs: 1 })).toBe(false);
});

it("warns about test networks that can reach production", () => {
  const draft = {
    ...emptyDraft(),
    networks: [{ from: "lan", to: "dr-lan" }],
    testNetworks: [
      { from: "lan", to: "dr-lan" },
      { from: "db", to: "db" },
      { from: "a", to: "routed" },
      { from: "b", to: "ext" },
      { from: "c", to: "br0" },
      { from: "d", to: "iso" },
      { from: "e", to: "unknown" },
    ],
  };
  const networks = [
    sdnNetwork("routed"),
    providerNetwork("ext", "physnet1"),
    { ...sdnNetwork("br0"), spec: { bridge: "br0" } } as Network,
    sdnNetwork("iso"),
  ];
  expect(testNetworkIssues(draft, networks, new Set(["routed"]))).toEqual({
    0: "failover",
    1: "failover",
    2: "routed",
    3: "provider",
    4: "physical",
  });
});

it("lists protection gaps apart from copies that failed verification", () => {
  expect(
    protectionGaps([
      { vm: "a", ok: true, age_secs: 10 },
      { vm: "b", ok: false },
      { vm: "c", ok: false, issue: "over-rpo", age_secs: 90_000 },
      // 가장 새 사본이 검증에 실패해 앞 사본을 쓴다(나이는 그 사본의 것)
      { vm: "d", ok: false, issue: "verify-failed", age_secs: 100 },
    ]),
  ).toEqual({ missing: ["b"], over: [{ vm: "c", age: 90_000 }], verifyFailed: ["d"] });
});

it("round-trips and checks the source backup pool of a plan and its VMs", () => {
  const spec = planSpec({ source_backup_pool: "prod-bk" });
  spec.groups[0]!.vms[0]!.source_backup_pool = "fast-bk";
  const draft = toDraft("web-tier", spec);
  expect(draft.sourceBackupPool).toBe("prod-bk");
  const back = toSpec(draft);
  expect(back.source_backup_pool).toBe("prod-bk");
  expect(back.groups[0]!.vms[0]!.source_backup_pool).toBe("fast-bk");
  expect(back.groups[1]!.vms[0]!.source_backup_pool).toBeUndefined();
  draft.sourceBackupPool = "Prod BK";
  draft.groups[0]!.vms[0]!.sourceBackupPool = "x_y";
  expect(validate(draft, false)).toEqual({
    sourceBackupPool: "dr.invalid.name",
    "g0.v0.sourceBackupPool": "dr.invalid.name",
  });
});

it("calls a running run stalled after 30 minutes without progress", () => {
  const status = { state: "running" as const, progress_at: 1000 };
  expect(stalledFor(status, 1000 + STALL_SECS)).toBeUndefined();
  expect(stalledFor(status, 1001 + STALL_SECS)).toBe(STALL_SECS + 1);
  expect(stalledFor({ ...status, state: "failed" }, 99_999)).toBeUndefined();
  expect(stalledFor({ state: "running" }, 99_999)).toBeUndefined();
});

it("knows the skip reasons the server writes and reads local times", () => {
  expect(stepNoteKey("already recovered by this plan")).toBe("dr.step.alreadyRecovered");
  expect(stepNoteKey("no recovery point")).toBe("dr.step.noPoint");
  expect(stepNoteKey("restore failed")).toBeUndefined();
  expect(stepNoteKey(undefined)).toBeUndefined();
  expect(localToSecs("")).toBeUndefined();
  expect(localToSecs("nope")).toBeUndefined();
  expect(localToSecs("2026-11-26T00:00")).toBe(new Date(2026, 10, 26).getTime() / 1000);
});

/** 시각 `secs`에 만든 UUIDv7 모양의 Task */
function taskAt(
  secs: number,
  n: number,
  op: Task["spec"]["op"],
  target: string,
  state: Task["status"]["state"],
): Task {
  const ms = (secs * 1000).toString(16).padStart(12, "0");
  return {
    id: `${ms.slice(0, 8)}-${ms.slice(8)}-7000-8000-${String(n).padStart(12, "0")}`,
    name: "t",
    revision: 1,
    labels: {},
    spec: { node: "n", op, target },
    status: { state, error: null },
  } as Task;
}

it("finds the phase a VM failed in from the tasks", () => {
  const step: DrStep = {
    group: "db",
    vm: "db1",
    phase: "failed",
    at: 2000,
    new_vm: "db1-drt",
    backup: "db1-copy",
    backup_id: "b-1",
  };
  const restore = (state: Task["status"]["state"], at = 1500) =>
    taskAt(
      at,
      1,
      { kind: "backup-restore", backup: "b-1", name: "db1-drt", pool: "p" },
      "vm-1",
      state,
    );
  const start = (state: Task["status"]["state"]) =>
    taskAt(1600, 2, { kind: "vm-start" }, "vm-1", state);
  expect(failedPhase(step, [], 1000)).toBeUndefined();
  expect(failedPhase(step, [restore("failed")], 1000)).toBe("restoring");
  // 앞 실행의 Task는 보지 않는다
  expect(failedPhase(step, [restore("failed", 500)], 1000)).toBeUndefined();
  expect(failedPhase(step, [restore("succeeded")], 1000)).toBe("starting");
  expect(failedPhase(step, [restore("succeeded"), start("failed")], 1000)).toBe("starting");
  expect(failedPhase(step, [restore("succeeded"), start("succeeded")], 1000)).toBe("waiting");
  // 운영 쪽 준비에서 실패했으면 DR 쪽 단계는 없다
  expect(
    failedPhase({ ...step, prepare: { stage: "backup" } }, [restore("failed")], 1000),
  ).toBeUndefined();
  expect(failedPhase({ ...step, phase: "timed_out" }, [], 1000)).toBe("waiting");
});
