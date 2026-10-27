import { snapshot, vm } from "@/test/fixtures";
import { descriptionError, snapshotNameError, snapshotTree } from "@/vms/snapshots";

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
