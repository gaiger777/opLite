import type { Task } from "@/api/types";
import { migrationProgress } from "@/clusterMap/data";
import { mapVm, vmId } from "@/test/clusterMap";

const TASK = "019a0f3c-0000-7000-8d00-000000000001";

function task(id: string, target: string, progress: Task["status"]["progress"]): Task {
  return {
    id,
    name: id,
    revision: 1,
    spec: { op: { kind: "vm-migrate" }, target, node: "n1" },
    status: { state: "running", error: null, progress },
  } as Task;
}

it("keeps only migration progress and the same map while nothing it uses changed", () => {
  const moving = mapVm(1, "web", "n1", "migrating", {
    status: { node: "n1", migration: { node: "n2", task: TASK } },
  });
  const idle = mapVm(2, "db", "n1");
  const backup = task("t-backup", vmId(2), { stage: "copying", done_bytes: 1, total_bytes: 10 });
  const tasks = new Map([
    [vmId(1), [task(TASK, vmId(1), { stage: "copying-disks", done_bytes: 1, total_bytes: 4 })]],
    [vmId(2), [backup]],
  ]);
  const first = migrationProgress([moving, idle], tasks, new Map());
  expect([...first.keys()]).toEqual([TASK]);
  // 백업 Task만 바뀜 → 같은 Map(다시 배치하지 않음)
  const other = new Map(tasks).set(vmId(2), [
    task("t-backup", vmId(2), { stage: "copying", done_bytes: 5, total_bytes: 10 }),
  ]);
  expect(migrationProgress([moving, idle], other, first)).toBe(first);
  // 마이그레이션 진행이 바뀜 → 새 Map
  const moved = new Map(tasks).set(vmId(1), [
    task(TASK, vmId(1), { stage: "copying-disks", done_bytes: 2, total_bytes: 4 }),
  ]);
  const next = migrationProgress([moving, idle], moved, first);
  expect(next).not.toBe(first);
  expect(next.get(TASK)).toEqual({ stage: "copying-disks", done_bytes: 2, total_bytes: 4 });
  // 마이그레이션이 끝남 → 빈 Map
  expect(migrationProgress([idle], moved, next).size).toBe(0);
});
