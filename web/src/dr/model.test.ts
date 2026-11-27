import { REFRESH_ON, VIEW_KINDS } from "@/api/live";
import type { Backup } from "@/api/types";
import {
  candidates,
  emptyDraft,
  formatDuration,
  moveGroup,
  newVm,
  phaseDots,
  rpoCounts,
  rtoClock,
  toDraft,
  toSpec,
  validate,
} from "@/dr/model";
import { planSpec } from "@/test/dr";

it("round-trips a plan through the editor draft", () => {
  const spec = planSpec({ security_groups: { web: "dr-web" }, folder: "dr/prod" });
  const back = toSpec(toDraft("web-tier", spec));
  expect(back).toEqual({
    ...spec,
    test_networks: undefined,
    groups: spec.groups.map((g) => ({
      ...g,
      vms: g.vms.map((v) => ({
        vm: v.vm,
        pool: undefined,
        node: undefined,
        live: undefined,
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
  expect(phaseDots("failed")).toEqual(["failed", "todo", "todo", "todo"]);
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
