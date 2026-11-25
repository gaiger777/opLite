import { invVm, migrationItem } from "@/test/vmware";
import {
  buildTree,
  canCancel,
  canCutover,
  checkWizardStep,
  copyProgress,
  createRequestOf,
  datastoreOf,
  emptyWizard,
  filterVms,
  formatDuration,
  lastDelta,
  proposeNetworks,
  rollbackFailed,
  snapshotCount,
  usedDatastores,
  usedNetworks,
  vmsUnder,
} from "@/vmware/inventory";

const WEB = invVm("web01", 1, { folder: "DC1/vm/Prod/Web", networks: ["VM Network", "dvpg-web"] });
const DB = invVm("db01", 2, {
  folder: "DC1/vm/Prod",
  power: "powered-off",
  datastores: ["ds1", "ds2"],
  host: "esx2.example.com",
});
const LAB = invVm("lab", 3, { folder: "DC1/vm", cluster: null, host: "esx9.example.com" });
const ALL = [WEB, DB, LAB];

it("builds the VM folder tree under each datacenter with counts", () => {
  const tree = buildTree(ALL, "folders");
  expect(tree.map((n) => [n.label, n.kind, n.count])).toEqual([["DC1", "datacenter", 3]]);
  const prod = tree[0]!.children;
  // 데이터센터의 `vm` 폴더는 뿌리와 같다(LAB은 데이터센터 바로 아래)
  expect(prod.map((n) => [n.label, n.count])).toEqual([["Prod", 2]]);
  expect(prod[0]!.children.map((n) => [n.id, n.count])).toEqual([["folders:DC1/Prod/Web", 1]]);
  expect(vmsUnder(ALL, "folders", "folders:DC1/Prod").map((v) => v.name)).toEqual([
    "web01",
    "db01",
  ]);
  expect(vmsUnder(ALL, "folders", null)).toHaveLength(3);
});

it("builds the host tree with clusters and standalone hosts", () => {
  const tree = buildTree(ALL, "hosts");
  const dc = tree[0]!;
  expect(dc.children.map((n) => [n.label, n.kind])).toEqual([
    ["Cluster1", "cluster"],
    ["esx9.example.com", "host"],
  ]);
  expect(dc.children[0]!.children.map((n) => n.label)).toEqual([
    "esx1.example.com",
    "esx2.example.com",
  ]);
  expect(vmsUnder(ALL, "hosts", "hosts:DC1/Cluster1/esx2.example.com").map((v) => v.name)).toEqual([
    "db01",
  ]);
});

it("searches by name, path, OS, host and guest IP and filters by power", () => {
  expect(filterVms(ALL, "prod", "all").map((v) => v.name)).toEqual(["db01", "web01"]);
  expect(filterVms(ALL, "10.0.0.3", "all").map((v) => v.name)).toEqual(["lab"]);
  expect(filterVms(ALL, "esx2 ubuntu", "all").map((v) => v.name)).toEqual(["db01"]);
  expect(filterVms(ALL, "", "powered-off").map((v) => v.name)).toEqual(["db01"]);
});

it("lists only the networks and datastores the chosen VMs use", () => {
  expect(usedNetworks([WEB])).toEqual(["VM Network", "dvpg-web"].sort());
  expect(usedDatastores([DB])).toEqual(["ds1", "ds2"]);
  expect(datastoreOf({ ...DB.disks[0]!, datastore: null, file: "[nfs-a] db/db.vmdk" })).toBe(
    "nfs-a",
  );
  expect(proposeNetworks(["VM Network", "dvpg-web"], ["VM Network", "lan"], {})).toEqual({
    "VM Network": "VM Network",
  });
  // 사용자가 비운(고른) 값은 바꾸지 않는다
  expect(proposeNetworks(["VM Network"], ["VM Network"], { "VM Network": "" })).toEqual({
    "VM Network": "",
  });
});

it("counts only user snapshots", () => {
  const vm = invVm("x", 9, {
    snapshots: [
      { moref: "snapshot-1", name: "before-upgrade", description: "" },
      { moref: "snapshot-2", name: "orbivisor-migration-0190abcd-1", description: "" },
    ],
  });
  expect(snapshotCount(vm)).toBe(1);
});

it("checks each wizard step", () => {
  const form = { ...emptyWizard(), name: "Wave 1" };
  expect(checkWizardStep("source", form, ALL, [])).toEqual({
    source: "vmware.wizard.errors.source",
    name: "vmware.wizard.errors.name",
  });
  expect(
    checkWizardStep("source", { ...form, name: "wave1", source: "s" }, ALL, ["wave1"]),
  ).toEqual({ name: "vmware.wizard.errors.nameTaken" });
  expect(checkWizardStep("vms", form, ALL, [])).toEqual({ vms: "vmware.wizard.errors.vms" });
  const chosen = { ...form, selected: ["vm-2"] };
  expect(checkWizardStep("mapping", chosen, ALL, [])).toEqual({
    "ds:ds1": "vmware.wizard.errors.pool",
    "ds:ds2": "vmware.wizard.errors.pool",
  });
  expect(checkWizardStep("mapping", { ...chosen, defaultPool: "local" }, ALL, [])).toEqual({});
  expect(
    checkWizardStep(
      "policy",
      { ...form, maxParallel: "0", intervalMin: "2", bandwidth: "x" },
      ALL,
      [],
    ),
  ).toEqual({
    maxParallel: "vmware.wizard.errors.range",
    intervalMin: "vmware.wizard.errors.range",
    bandwidth: "vmware.wizard.errors.range",
  });
});

it("makes the create request with only the mappings of the chosen VMs", () => {
  const form = {
    ...emptyWizard("vc1"),
    name: "wave1",
    selected: ["vm-1"],
    networks: { "VM Network": "lan", "dvpg-web": "web", Unused: "lan" },
    datastores: { ds1: "fast", ds2: "slow" },
    folderKind: "mirror" as const,
    folder: "f1",
    bandwidth: "200",
    cutoverAt: "2026-12-01T02:00",
    maxFinalDelta: "8",
  };
  const body = createRequestOf(form, ALL);
  expect(body).toMatchObject({
    name: "wave1",
    source: "vc1",
    mode: "warm",
    vms: [{ ref: "vm-1" }],
    limits: { max_parallel: 4, max_per_host: 2, max_per_node: 2, bandwidth_mib_s: 200 },
    sync: { interval_min: 60, stop_after_days: 14 },
    cutover: {
      shutdown: "guest",
      shutdown_timeout_s: 600,
      protect_source: true,
      max_final_delta_gib: 8,
    },
  });
  expect(body.cutover?.at).toBe(Math.floor(new Date("2026-12-01T02:00").getTime() / 1000));
  expect(body.rules).toEqual({
    networks: [
      { from: "VM Network", to: "lan" },
      { from: "dvpg-web", to: "web" },
    ],
    datastores: [{ from: "ds1", to: "fast" }],
    folders: { kind: "mirror", under: "f1" },
    keep_macs: true,
    reserve_ips: true,
    start_after: true,
  });
  // 비운 칸은 보내지 않는다(서버 기본값)
  const plain = createRequestOf({ ...emptyWizard("vc1"), name: "w", selected: ["vm-1"] }, ALL);
  expect(plain.limits).not.toHaveProperty("bandwidth_mib_s");
  expect(plain.cutover).not.toHaveProperty("at");
  expect(plain.rules).not.toHaveProperty("default_pool");
  expect(plain.rules?.folders).toEqual({ kind: "flat" });
});

it("reads item progress, deltas and allowed actions", () => {
  const disks = [
    { key: 1, file: "[ds1] a.vmdk", capacity_bytes: 100, copied_bytes: 50, rounds: 0 },
    { key: 2, file: "[ds1] b.vmdk", capacity_bytes: 300, copied_bytes: 50, rounds: 0 },
  ];
  const copying = migrationItem("web01", 1, { state: "copying", disks });
  expect(copyProgress(copying)).toBe(0.25);
  expect(copyProgress(migrationItem("a", 2, { state: "pending" }))).toBeUndefined();
  const synced = migrationItem("web01", 1, {
    state: "syncing",
    rounds: 2,
    disks: disks.map((d) => ({ ...d, last_delta_bytes: 1024 })),
  });
  expect(copyProgress(synced)).toBe(1);
  expect(lastDelta(synced)).toBe(2048);
  expect(lastDelta(copying)).toBeUndefined();
  expect(canCutover(synced)).toBe(true);
  expect(canCutover(copying)).toBe(false);
  expect(canCutover(migrationItem("a", 4, { state: "rolled-back" }))).toBe(true);
  expect(canCutover(migrationItem("a", 5, { state: "done" }))).toBe(false);
  expect(rollbackFailed(migrationItem("a", 6, { state: "failed" }))).toBe(false);
  expect(
    rollbackFailed(migrationItem("a", 7, { state: "failed", cutover_step: "starting-vm" })),
  ).toBe(true);
  expect(canCancel(copying)).toBe(true);
  expect(canCancel(migrationItem("a", 3, { state: "done" }))).toBe(false);
  expect(formatDuration(42)).toBe("42s");
  expect(formatDuration(252)).toBe("4m 12s");
  expect(formatDuration(3780)).toBe("1h 3m");
});
