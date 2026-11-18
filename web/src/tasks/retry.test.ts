import type { Task } from "@/api/types";
import { latestTasks, type RetryContext, retryOf } from "@/tasks/retry";
import { backup, backupJob, task, vm } from "@/test/fixtures";

const off = vm("db", "stopped");
const on = vm("web", "running");
const job = backupJob("nightly", 1);
const bk = backup("db-1", off, 1, { spec: { job: job.id } });

function ctx(extra: Partial<RetryContext> = {}): RetryContext {
  return {
    role: "operator",
    vm: new Map([
      [off.id, off],
      [on.id, on],
    ]),
    backup: new Map([[bk.id, bk]]),
    job: new Map([[job.id, job]]),
    tasks: new Map(),
    ...extra,
  };
}

function failed(kind: string, target: string, extra: object = {}): Task {
  const t = task("t1", "vm-start", target);
  return {
    ...t,
    spec: { ...t.spec, op: { kind, ...extra } },
    status: { state: "failed", error: "boom" },
  } as Task;
}

it("retries failed power operations of a VM that still exists", () => {
  expect(retryOf(failed("vm-start", off.id), ctx())).toEqual({
    kind: "vm",
    vm: off,
    action: "start",
    reason: undefined,
  });
  expect(retryOf(failed("vm-force-stop", on.id), ctx())).toMatchObject({ action: "force-stop" });
  // 지금 상태로 막히면 이유를 단다.
  expect(retryOf(failed("vm-start", on.id), ctx())).toMatchObject({
    action: "start",
    reason: "vms.reason.running",
  });
  // 끝나지 않은 Task는, 지워진 VM은, viewer는 다시 하지 않는다.
  expect(
    retryOf({ ...failed("vm-start", off.id), status: { state: "running", error: null } }, ctx()),
  ).toBeNull();
  expect(retryOf(failed("vm-start", "gone"), ctx())).toBeNull();
  expect(retryOf(failed("vm-start", off.id), ctx({ role: "viewer" }))).toBeNull();
});

it("does not retry operations that need parameters", () => {
  expect(retryOf(failed("vm-migrate", on.id, { node: "n2" }), ctx())).toBeNull();
  expect(retryOf(failed("snapshot-create", on.id), ctx())).toBeNull();
});

it("runs the backup job of a failed backup again", () => {
  expect(retryOf(failed("vm-backup", off.id, { backup: bk.id }), ctx())).toEqual({
    kind: "job",
    job,
    reason: undefined,
  });
  const running = backupJob("nightly", 1, {
    status: { run: { started_at: 1, queue: [] } as never },
  });
  expect(
    retryOf(
      failed("vm-backup", off.id, { backup: bk.id }),
      ctx({ job: new Map([[job.id, running]]) }),
    ),
  ).toMatchObject({ reason: "center.jobRunning" });
  const disabled = backupJob("nightly", 1, { spec: { enabled: false } });
  expect(
    retryOf(
      failed("vm-backup", off.id, { backup: bk.id }),
      ctx({ job: new Map([[job.id, disabled]]) }),
    ),
  ).toMatchObject({ reason: "center.jobDisabled" });
  // 작업 없이 만든 백업, 작업 권한이 없는 사용자
  const manual = backup("db-2", off, 2);
  expect(
    retryOf(
      failed("vm-backup", off.id, { backup: manual.id }),
      ctx({ backup: new Map([[manual.id, manual]]) }),
    ),
  ).toBeNull();
  expect(
    retryOf(failed("vm-backup", off.id, { backup: bk.id }), ctx({ role: "viewer" })),
  ).toBeNull();
});

it("does not retry a failure that a newer task on the same target replaced", () => {
  const old = { ...failed("vm-start", off.id), id: "019a0000-0000-7000-8000-000000000001" };
  const later = {
    ...task("t2", "vm-stop", off.id),
    id: "019a0000-0000-7000-8000-000000000002",
  } as Task;
  // 다른 VM의 Task와 백업은 전원 동작과 따로 본다.
  const other = { ...task("t3", "vm-start", on.id), id: "019a0000-0000-7000-8000-000000000003" };
  const latest = latestTasks([later, old, other]);
  expect(retryOf(old, ctx({ latest }))).toBeNull();
  const backup = {
    ...failed("vm-backup", off.id, { backup: bk.id }),
    id: "019a0000-0000-7000-8000-000000000000",
  };
  expect(retryOf(backup, ctx({ latest: latestTasks([backup, later]) }))).toMatchObject({
    kind: "job",
  });
  // 가장 새 것이면 그대로 다시 할 수 있다.
  expect(retryOf(old, ctx({ latest: latestTasks([old, other]) }))).toMatchObject({
    action: "start",
  });
});
