import type { Node, Task } from "@/api/types";
import { NODE, snapshot, task, vm } from "@/test/fixtures";
import { BULK_LIMIT, bulkReason, runLimited, snapshotName, snapshotVms } from "@/vms/bulk";

const other: Node = { ...NODE, id: "n2", name: "node-b", status: { ...NODE.status, arch: null } };
const ctx = (extra: Partial<Parameters<typeof bulkReason>[2]> = {}) => ({
  tasks: new Map<string, Task[]>(),
  nodes: [NODE, other],
  snapshotVms: new Set<string>(),
  ...extra,
});

it("says why each VM cannot take the action", () => {
  const web = vm("web", "running");
  const db = vm("db", "stopped");
  expect(bulkReason(web, "start", ctx())).toBe("vms.reason.running");
  expect(bulkReason(db, "start", ctx())).toBeNull();
  expect(bulkReason(db, "stop", ctx())).toBe("vms.reason.stopped");
  expect(bulkReason(web, "force-stop", ctx())).toBeNull();
  // 스냅샷은 바뀌는 중인 VM만 막는다.
  expect(bulkReason(db, "snapshot", ctx())).toBeNull();
  expect(bulkReason(vm("up", "starting"), "snapshot", ctx())).toBe("vms.reason.busy");
  const pending = new Map([[db.id, [task("t", "vm-start", db.id)]]]);
  expect(bulkReason(db, "start", ctx({ tasks: pending }))).toBe("vms.reason.powerPending");
});

it("does not delete VMs with snapshots", () => {
  const db = vm("db", "stopped");
  expect(bulkReason(db, "delete", ctx())).toBeNull();
  expect(bulkReason(db, "delete", ctx({ snapshotVms: snapshotVms([snapshot("s1", db, 1)]) }))).toBe(
    "vms.bulk.reason.snapshots",
  );
});

it("checks the migration target for each VM", () => {
  // 픽스처의 VM은 virt(aarch64) 머신이다.
  const web = vm("web", "running");
  const arm: Node = { ...other, status: { ...other.status, arch: "aarch64" } };
  expect(bulkReason(web, "migrate", ctx())).toBe("vms.bulk.reason.noTarget");
  expect(bulkReason(web, "migrate", ctx({ target: "n1" }))).toBe("vms.bulk.reason.sameNode");
  expect(bulkReason(web, "migrate", ctx({ target: "n2" }))).toBe("vms.bulk.reason.target");
  expect(bulkReason(web, "migrate", ctx({ nodes: [NODE, arm], target: "n2" }))).toBeNull();
  expect(bulkReason(vm("db", "stopped"), "migrate", ctx({ target: "n2" }))).toBe(
    "vms.reason.stopped",
  );
});

it("names bulk snapshots by the minute", () => {
  expect(snapshotName(new Date(2026, 0, 5, 7, 3))).toBe("bulk-20260105-0703");
});

it("runs at most the limit at once and keeps the order", async () => {
  let running = 0;
  let most = 0;
  const results = await runLimited([1, 2, 3, 4, 5, 6, 7, 8, 9], BULK_LIMIT, async (n) => {
    running += 1;
    most = Math.max(most, running);
    await new Promise((resolve) => setTimeout(resolve, 10 - n));
    running -= 1;
    if (n === 5) {
      throw new Error("five");
    }
    return n * 2;
  });
  expect(most).toBe(4);
  expect(results.map((r) => (r.status === "fulfilled" ? r.value : "x"))).toEqual([
    2,
    4,
    6,
    8,
    "x",
    12,
    14,
    16,
    18,
  ]);
});
