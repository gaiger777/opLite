import { NODE, pool, replica, replicatedVm, vm, volume } from "@/test/fixtures";
import {
  firstLocalDisk,
  formatAge,
  intervalMinutes,
  replicaRows,
  replicasOn,
  replicationWarning,
  unusableDisk,
} from "@/vms/replication";

const NODE_B = { ...NODE, id: "n2", name: "node-b" };
const NODE_C = { ...NODE, id: "n3", name: "node-c" };
const NOW = 1_800_000_000;

it("reads */N schedules like the server", () => {
  expect(intervalMinutes("*/15")).toBe(15);
  expect(intervalMinutes("*/1")).toBe(1);
  expect(intervalMinutes("*/1440")).toBe(1440);
  for (const bad of ["*/0", "*/1441", "15", "*/", "*/1.5", "*/-5", " */5"]) {
    expect(intervalMinutes(bad)).toBeNull();
  }
});

it("formats ages like the CLI", () => {
  expect(formatAge(0)).toBe("0s");
  expect(formatAge(59)).toBe("59s");
  expect(formatAge(252)).toBe("4m12s");
  expect(formatAge(3600 + 300 + 7)).toBe("1h5m");
  expect(formatAge(2 * 86400 + 3 * 3600 + 59)).toBe("2d3h");
});

it("lists each target with its state, age, size and next run", () => {
  const web = replicatedVm("running", undefined);
  const rows = replicaRows(web, [NODE, NODE_B], NOW + 252);
  expect(rows).toEqual([
    {
      node: "n2",
      name: "node-b",
      state: "ready",
      pool: null,
      minutes: 15,
      syncedAt: NOW,
      age: 252,
      last: { bytes: 12 * 1024 ** 2, full: false, secs: 4 },
      size: 16 * 1024 ** 3,
      nextRun: NOW + 15 * 60,
      error: null,
    },
  ]);
});

it("shows targets without a state as pending, runs them now, and keeps old sources", () => {
  const web = replicatedVm("running", undefined);
  const spec = {
    ...web.spec,
    replication: [
      { node: "n3", schedule: "*/5", pool: "b" },
      { node: "n2", schedule: "*/15" },
    ],
  };
  const status = {
    ...web.status,
    replication: [
      // 마지막 시도가 실패했다: 다음 회차는 그 시도에서 주기 뒤
      replica("n2", { last_try: NOW + 600, error: 'node "node-b" is offline' }),
      // HA 복구 뒤 남은 옛 원본(스펙에 없음)
      replica("n1", { state: "stale", generation: undefined, synced_at: undefined, volumes: [] }),
    ],
  };
  const rows = replicaRows({ ...web, spec, status }, [NODE, NODE_B, NODE_C], NOW + 700);
  expect(rows.map((r) => [r.name, r.state, r.pool, r.nextRun, r.last, r.error])).toEqual([
    ["node-c", "pending", "b", NOW + 700, null, null],
    [
      "node-b",
      "ready",
      null,
      NOW + 600 + 15 * 60,
      { bytes: 12 * 1024 ** 2, full: false, secs: 4 },
      'node "node-b" is offline',
    ],
    ["node-a", "stale", null, null, null, null],
  ]);
  expect(rows[2]!.size).toBe(0);
  expect(rows[2]!.age).toBeNull();
});

it("does not schedule a stopped VM", () => {
  const rows = replicaRows(replicatedVm("stopped"), [NODE_B], NOW);
  expect(rows[0]!.nextRun).toBeNull();
});

it("finds disks on node-local pools", () => {
  const pools = [
    pool("local"),
    pool("nas", { shared: true }),
    pool("thin", { backend: { kind: "lvm-thin", vg: "vg", thinpool: "tp" } }),
  ];
  const volumes = [volume("root", "nas"), volume("data", "local")];
  expect(firstLocalDisk([{ volume: "root" }], volumes, pools)).toBeNull();
  expect(firstLocalDisk([{ volume: "root" }, { volume: "data" }], volumes, pools)).toEqual({
    volume: "data",
    pool: "local",
  });
  // 만들 새 볼륨은 고른 풀을 본다.
  expect(firstLocalDisk([{ volume: "new", pool: "thin" }], volumes, pools)).toEqual({
    volume: "new",
    pool: "thin",
  });
  // 모르는 볼륨·풀은 건너뛴다.
  expect(firstLocalDisk([{ volume: "gone" }], volumes, pools)).toBeNull();
});

it("warns about HA VMs with local disks and no replica", () => {
  const pools = [pool("local")];
  const volumes = [volume("web-root", "local")];
  const plain = replicatedVm("running");
  const ha = { ...plain, spec: { ...plain.spec, ha: true, replication: [] } };
  expect(replicationWarning(ha, volumes, pools)).toEqual({ volume: "web-root", pool: "local" });
  // 복제가 있거나 HA가 아니거나 잠시 끈 HA면 경고하지 않는다.
  expect(
    replicationWarning(
      { ...ha, spec: { ...ha.spec, replication: plain.spec.replication } },
      volumes,
      pools,
    ),
  ).toBeNull();
  expect(replicationWarning({ ...ha, spec: { ...ha.spec, ha: false } }, volumes, pools)).toBeNull();
  expect(
    replicationWarning(
      {
        ...ha,
        spec: {
          ...ha.spec,
          ha: { state: "ignored", max_restart: 1, max_relocate: 1, priority: 50 },
        },
      },
      volumes,
      pools,
    ),
  ).toBeNull();
  // 공유 풀의 디스크는 어느 노드에서나 열린다.
  expect(replicationWarning(ha, volumes, [pool("local", { shared: true })])).toBeNull();
});

it("finds a changed local disk that keeps HA from using a ready replica", () => {
  const web = replicatedVm("running");
  const volumes = [volume("web-root", "local"), volume("web-data", "local"), volume("nfs", "nas")];
  const pools = [pool("local"), pool("nas", { shared: true })];
  expect(unusableDisk(web, "n2", volumes, pools)).toBeNull();
  // 공유 풀의 디스크는 복제하지 않는다.
  const withShared = {
    ...web,
    spec: {
      ...web.spec,
      disks: [...web.spec.disks!, { volume: "nfs", bus: "virtio-blk" as const }],
    },
  };
  expect(unusableDisk(withShared, "n2", volumes, pools)).toBeNull();
  // 로컬 디스크를 더했다.
  const added = {
    ...web,
    spec: {
      ...web.spec,
      disks: [...web.spec.disks!, { volume: "web-data", bus: "virtio-blk" as const }],
    },
  };
  expect(unusableDisk(added, "n2", volumes, pools)).toBe("web-data");
  // 크기를 바꿨다.
  const grown = volumes.map((v) =>
    v.name === "web-root" ? { ...v, spec: { ...v.spec, size_bytes: 32 * 1024 ** 3 } } : v,
  );
  expect(unusableDisk(web, "n2", grown, pools)).toBe("web-root");
  // 준비된 복제본이 아니면 보지 않는다.
  expect(unusableDisk(added, "n3", volumes, pools)).toBeNull();
});

it("finds the VMs a node holds replicas of", () => {
  const web = replicatedVm("running");
  const db = {
    ...vm("db", "running"),
    spec: { ...vm("db", "running").spec, replication: [{ node: "n2", schedule: "*/5" }] },
  };
  const other = vm("other", "running");
  expect(replicasOn("n2", [web, db, other]).map(({ vm, state }) => [vm.name, state])).toEqual([
    ["web", "ready"],
    ["db", "pending"],
  ]);
  expect(replicasOn("n3", [web, db, other])).toEqual([]);
});
