import type { Volume } from "@/api/types";
import { pool, snapshot, vm, volume } from "@/test/fixtures";
import { emptyDisk, emptyForm } from "@/vms/form";
import {
  descriptionError,
  formSnapshotSupport,
  snapshotNameError,
  snapshotSupport,
  snapshotTree,
  volumeDisk,
  zfsNotLatest,
} from "@/vms/snapshots";

const web = vm("web", "running");

it("orders snapshots as a tree, siblings by creation", () => {
  const base = snapshot("base", web, 1);
  const b = snapshot("b", web, 3, { spec: { parent: base.id } });
  const a = snapshot("a", web, 2, { spec: { parent: base.id } });
  const child = snapshot("child", web, 4, { spec: { parent: a.id } });
  // 목록에 없는 부모(지워지는 중 등)를 가리키면 뿌리로 보인다.
  const lost = snapshot("lost", web, 5, {
    spec: { parent: "019a0f3c-0000-7000-8003-999999999999" },
  });
  expect(
    snapshotTree([child, b, lost, a, base]).map(({ snapshot, depth }) => [
      snapshot.spec.name,
      depth,
    ]),
  ).toEqual([
    ["base", 0],
    ["a", 1],
    ["child", 2],
    ["b", 1],
    ["lost", 0],
  ]);
});

it("shows snapshots whose parents form a cycle as roots", () => {
  const a = snapshot("a", web, 1);
  const b = snapshot("b", web, 2, { spec: { parent: a.id } });
  const self = snapshot("self", web, 3);
  const cycle = [
    { ...a, spec: { ...a.spec, parent: b.id } },
    b,
    { ...self, spec: { ...self.spec, parent: self.id } },
  ];
  expect(snapshotTree(cycle).map(({ snapshot, depth }) => [snapshot.spec.name, depth])).toEqual([
    ["a", 0],
    ["b", 1],
    ["self", 0],
  ]);
});

it("checks snapshot names and descriptions like the server", () => {
  expect(snapshotNameError("before-upgrade", [])).toBeUndefined();
  expect(snapshotNameError("Before", [])).toBe("forms.name");
  expect(snapshotNameError("base", ["base"])).toBe("snapshots.errors.exists");
  expect(descriptionError("가".repeat(1024))).toBeUndefined();
  expect(descriptionError("가".repeat(1025))).toBe("snapshots.errors.description");
});

const local = pool("local");
const fast = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });
const tank = pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } });
const pools = [local, fast, tank];
const raw = (name: string, poolName: string) => {
  const v = volume(name, poolName);
  return { ...v, spec: { ...v.spec, format: "raw" as const } };
};

it("decides like the server whether disks can be snapshotted", () => {
  const disk = (v: ReturnType<typeof volume>) => volumeDisk(v, pools);
  // 디스크가 없으면 스냅샷 방식을 따질 것이 없다.
  expect(snapshotSupport([])).toEqual({ kind: "ok", block: false });
  // 파일 풀은 qcow2 내부 스냅샷, LVM-thin 풀은 raw LV 스냅샷이다.
  expect(snapshotSupport([disk(volume("a", "local")), disk(volume("b", "local"))])).toEqual({
    kind: "ok",
    block: false,
  });
  expect(snapshotSupport([disk(raw("a", "fast")), disk(raw("b", "fast"))])).toEqual({
    kind: "ok",
    block: true,
  });
  // 파일 풀의 raw 디스크(또는 LVM-thin 풀의 qcow2 디스크)는 담지 못한다.
  const plain = disk(raw("plain", "local"));
  expect(snapshotSupport([disk(volume("a", "local")), plain])).toEqual({
    kind: "format",
    disk: plain,
  });
  const odd = disk(volume("odd", "fast"));
  expect(snapshotSupport([odd])).toEqual({ kind: "format", disk: odd });
  // ZFS 풀도 raw 블록 스냅샷이다.
  expect(snapshotSupport([disk(raw("a", "tank")), disk(raw("b", "tank"))])).toEqual({
    kind: "ok",
    block: true,
  });
  const qcow = disk(volume("q", "tank"));
  expect(snapshotSupport([qcow])).toEqual({ kind: "format", disk: qcow });
  // 방식이 섞이면 안 된다. 블록 백엔드끼리도 종류가 다르면 섞인 것이다.
  expect(snapshotSupport([disk(volume("a", "local")), disk(raw("b", "fast"))])).toEqual({
    kind: "mixed",
  });
  expect(snapshotSupport([disk(raw("a", "fast")), disk(raw("b", "tank"))])).toEqual({
    kind: "mixed",
  });
  // 풀을 모르는 볼륨은 파일 풀처럼 본다.
  expect(snapshotSupport([volumeDisk(volume("a", "gone"), pools)])).toEqual({
    kind: "ok",
    block: false,
  });
});

it("judges the VM form's disks, new ones in their pool's format", () => {
  const values = {
    ...emptyForm("q35"),
    disks: [
      { ...emptyDisk("web", 0), kind: "existing" as const, volume: "data" },
      { ...emptyDisk("web", 1), pool: "fast" },
    ],
  };
  // 새 디스크는 풀의 기본 형식(LVM-thin 풀은 raw)으로 만든다.
  expect(formSnapshotSupport(values, [raw("data", "fast")], pools)).toEqual({
    kind: "ok",
    block: true,
  });
  expect(formSnapshotSupport(values, [volume("data", "local")], pools)).toEqual({
    kind: "mixed",
  });
  // 목록을 다 읽기 전에는 모른다.
  expect(formSnapshotSupport(values, undefined, pools)).toBeUndefined();
  expect(formSnapshotSupport(values, [], undefined)).toBeUndefined();
  // 아직 고르지 않은 볼륨·풀은 따지지 않는다.
  const blank = { ...values, disks: [{ ...emptyDisk("web", 0), pool: "" }] };
  expect(formSnapshotSupport(blank, [], pools)).toEqual({ kind: "ok", block: false });
});

it("finds snapshots a VM with disks in ZFS pools cannot revert to, like the server", () => {
  const base = vm("z", "stopped");
  const zvm = {
    ...base,
    spec: {
      ...base.spec,
      disks: [{ volume: "z-root", bus: "virtio-blk" as const, boot_order: 1 }],
    },
  };
  const zvols = [raw("z-root", "tank")];
  const old = snapshot("old", zvm, 1);
  const mid = snapshot("mid", zvm, 2, { status: { created_at: 1_792_000_100 } });
  // 만든 시각이 같으면 id(만든 차례)로 가린다.
  const tie = snapshot("tie", zvm, 3, { status: { created_at: 1_792_000_100 } });
  const all = [tie, old, mid];
  const notLatest = (s: ReturnType<typeof snapshot>, list = all, vols: Volume[] = zvols) =>
    zfsNotLatest(s, list, vols, pools);
  expect(notLatest(old)).toBe(true);
  expect(notLatest(mid)).toBe(true);
  expect(notLatest(tie)).toBe(false);
  // 만드는 중인 스냅샷은 가장 나중이다.
  const pending = snapshot("pending", zvm, 0, {
    status: { phase: "pending", created_at: null },
  });
  expect(notLatest(tie, [...all, pending])).toBe(true);
  // 스냅샷이 찍은 디스크가 ZFS 풀에 없으면 막지 않는다(지금 VM의 디스크가 아니다).
  expect(notLatest(old, all, [volume("z-root", "local")])).toBe(false);
  expect(notLatest(old, all, [raw("z-root", "fast")])).toBe(false);
  // 목록에 없는 볼륨은 건너뛴다.
  expect(notLatest(old, all, [])).toBe(false);
});
