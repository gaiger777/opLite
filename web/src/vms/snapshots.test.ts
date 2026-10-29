import { pool, snapshot, vm, volume } from "@/test/fixtures";
import { emptyDisk, emptyForm } from "@/vms/form";
import {
  descriptionError,
  formSnapshotSupport,
  snapshotNameError,
  snapshotSupport,
  snapshotTree,
  volumeDisk,
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
const pools = [local, fast];
const raw = (name: string, poolName: string) => {
  const v = volume(name, poolName);
  return { ...v, spec: { ...v.spec, format: "raw" as const } };
};

it("decides like the server whether disks can be snapshotted", () => {
  const disk = (v: ReturnType<typeof volume>) => volumeDisk(v, pools);
  // 디스크가 없으면 스냅샷 방식을 따질 것이 없다.
  expect(snapshotSupport([])).toEqual({ kind: "ok", lvm: false });
  // 파일 풀은 qcow2 내부 스냅샷, LVM-thin 풀은 raw LV 스냅샷이다.
  expect(snapshotSupport([disk(volume("a", "local")), disk(volume("b", "local"))])).toEqual({
    kind: "ok",
    lvm: false,
  });
  expect(snapshotSupport([disk(raw("a", "fast")), disk(raw("b", "fast"))])).toEqual({
    kind: "ok",
    lvm: true,
  });
  // 파일 풀의 raw 디스크(또는 LVM-thin 풀의 qcow2 디스크)는 담지 못한다.
  const plain = disk(raw("plain", "local"));
  expect(snapshotSupport([disk(volume("a", "local")), plain])).toEqual({
    kind: "format",
    disk: plain,
  });
  const odd = disk(volume("odd", "fast"));
  expect(snapshotSupport([odd])).toEqual({ kind: "format", disk: odd });
  // 방식이 섞이면 안 된다.
  expect(snapshotSupport([disk(volume("a", "local")), disk(raw("b", "fast"))])).toEqual({
    kind: "mixed",
  });
  // 풀을 모르는 볼륨은 파일 풀처럼 본다.
  expect(snapshotSupport([volumeDisk(volume("a", "gone"), pools)])).toEqual({
    kind: "ok",
    lvm: false,
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
    lvm: true,
  });
  expect(formSnapshotSupport(values, [volume("data", "local")], pools)).toEqual({
    kind: "mixed",
  });
  // 목록을 다 읽기 전에는 모른다.
  expect(formSnapshotSupport(values, undefined, pools)).toBeUndefined();
  expect(formSnapshotSupport(values, [], undefined)).toBeUndefined();
  // 아직 고르지 않은 볼륨·풀은 따지지 않는다.
  const blank = { ...values, disks: [{ ...emptyDisk("web", 0), pool: "" }] };
  expect(formSnapshotSupport(blank, [], pools)).toEqual({ kind: "ok", lvm: false });
});
