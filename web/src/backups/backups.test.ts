import { backupPools, chainBytes, chainOf, newestFirst, storedBytes } from "@/backups/backups";
import { backup, pool, vm } from "@/test/fixtures";

const web = vm("web", "stopped");

it("adds up the stored size of a ready backup only", () => {
  const ready = backup("b1", web, 1, {
    status: {
      disks: [
        { name: "disk0", file: "disk0.qcow2", virtual_bytes: 10, stored_bytes: 3 },
        { name: "disk1", file: "disk1.qcow2", virtual_bytes: 10, stored_bytes: 4 },
      ],
    },
  });
  expect(storedBytes(ready)).toBe(7);
  expect(storedBytes(backup("b2", web, 2, { status: { phase: "pending" } }))).toBeUndefined();
  expect(storedBytes(backup("b3", web, 3, { status: { phase: "failed" } }))).toBeUndefined();
});

it("offers only pools that store backups", () => {
  const pools = [pool("local"), pool("bk", { backups: true }), pool("off", { backups: false })];
  expect(backupPools(pools).map((p) => p.name)).toEqual(["bk"]);
});

it("puts the newest backup first", () => {
  const list = [backup("a", web, 1), backup("c", web, 3), backup("b", web, 2)];
  expect(newestFirst(list).map((b) => b.name)).toEqual(["c", "b", "a"]);
});

it("follows the chain of an incremental backup from its full backup", () => {
  const full = backup("full", web, 1);
  const inc1 = backup("inc1", web, 2, {
    spec: { incremental: true },
    status: {
      parent: full.id,
      disks: [{ name: "disk0", file: "disk0.qcow2", virtual_bytes: 10, stored_bytes: 5 }],
    },
  });
  const inc2 = backup("inc2", web, 3, {
    spec: { incremental: true },
    status: {
      parent: inc1.id,
      disks: [{ name: "disk0", file: "disk0.qcow2", virtual_bytes: 10, stored_bytes: 7 }],
    },
  });
  const all = [inc2, full, inc1];
  expect(chainOf(inc2, all)?.map((b) => b.name)).toEqual(["full", "inc1", "inc2"]);
  expect(chainOf(full, all)?.map((b) => b.name)).toEqual(["full"]);
  // 체인 크기는 전체 백업과 증분 파일 크기의 합이다.
  expect(chainBytes(inc2, all)).toBe(1536 * 1024 ** 2 + 5 + 7);
  // 목록에 없는 조상이 있으면 체인을 모른다.
  expect(chainOf(inc2, [inc2, inc1])).toBeUndefined();
  expect(chainBytes(inc2, [inc2, inc1])).toBeUndefined();
  // 다 만들지 않은 백업이 체인에 있으면 크기를 모른다.
  const pending = backup("inc3", web, 4, { status: { phase: "pending", parent: inc2.id } });
  expect(chainBytes(pending, [...all, pending])).toBeUndefined();
});
