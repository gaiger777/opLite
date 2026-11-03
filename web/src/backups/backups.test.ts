import { backupPools, newestFirst, storedBytes } from "@/backups/backups";
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
