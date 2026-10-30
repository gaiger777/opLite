import type { Task, Vm } from "@/api/types";
import { task, vm } from "@/test/fixtures";
import { ACTIONS, availability, isBusy, isMigrating, isReverting, isStopped } from "@/vms/actions";

function allowed(v: ReturnType<typeof vm>, vmTasks: Task[] = []) {
  return ACTIONS.filter((action) => availability(v, action, vmTasks).ok);
}

function canDelete(v: ReturnType<typeof vm>) {
  return availability(v, "delete", []).ok;
}

it("follows the server's is_stopped: desired power and phase together", () => {
  const stopped = vm("a", "stopped");
  expect(isStopped(stopped)).toBe(true);
  expect(canDelete(stopped)).toBe(true);
  expect(allowed(stopped)).toEqual(["start"]);

  const running = vm("b", "running");
  expect(isStopped(running)).toBe(false);
  expect(canDelete(running)).toBe(false);
  expect(allowed(running)).toEqual(["stop", "force-stop", "reboot"]);
});

it("treats a VM that should run but has not started as running", () => {
  // 막 시작했거나 노드가 내려가 아직 멈춰 있는 VM
  for (const phase of ["stopped", "failed"] as const) {
    const pending = vm("c", phase, { spec: { ...vm("c", phase).spec, power: "running" } });
    expect(isStopped(pending)).toBe(false);
    expect(canDelete(pending)).toBe(false);
    expect(allowed(pending)).toEqual(["stop", "force-stop"]);
  }
});

it("lets a VM that is shutting down be forced off", () => {
  const stopping = vm("d", "stopping");
  expect(allowed(stopping)).toEqual(["force-stop"]);
  expect(canDelete(stopping)).toBe(false);
});

it("does not reboot a VM whose ACPI stop is pending", () => {
  // 정지를 요청했지만 게스트가 아직 꺼지지 않았다.
  const pending = vm("e", "running", { spec: { ...vm("e", "running").spec, power: "stopped" } });
  expect(allowed(pending)).toEqual(["stop", "force-stop"]);
});

it("follows the server's check_not_busy: starting, stopping or power and phase apart", () => {
  const power = (v: ReturnType<typeof vm>, p: "running" | "stopped") =>
    vm(v.name, v.status.phase, { spec: { ...v.spec, power: p } });
  expect(isBusy(vm("a", "stopped"))).toBe(false);
  expect(isBusy(vm("b", "running"))).toBe(false);
  expect(isBusy(vm("c", "failed"))).toBe(false);
  expect(isBusy(vm("d", "starting"))).toBe(true);
  expect(isBusy(vm("e", "stopping"))).toBe(true);
  // 켜려는데 아직 실행 중이 아니다(막 시작했거나 노드가 내려갔다).
  expect(isBusy(power(vm("f", "stopped"), "running"))).toBe(true);
  expect(isBusy(power(vm("g", "failed"), "running"))).toBe(true);
  // 끄려는데 아직 실행 중이다(ACPI 정지를 기다린다).
  expect(isBusy(power(vm("h", "running"), "stopped"))).toBe(true);
});

it("follows the server's check_no_revert_task: only an unfinished revert of this VM", () => {
  const web = vm("web", "running");
  const of = (kind: Task["spec"]["op"]["kind"], state: Task["status"]["state"]) => ({
    ...task(`t-${kind}-${state}`, kind, web.id),
    status: { state, error: null },
  });
  expect(isReverting([])).toBe(false);
  expect(isReverting([of("snapshot-revert", "pending")])).toBe(true);
  expect(isReverting([of("snapshot-revert", "running")])).toBe(true);
  // 끝난 되돌리기
  expect(isReverting([of("snapshot-revert", "succeeded")])).toBe(false);
  expect(isReverting([of("snapshot-revert", "failed")])).toBe(false);
  // 만들기·삭제·복제 중에는 강제 정지를 받는다.
  for (const kind of ["snapshot-create", "snapshot-delete", "vm-clone"] as const) {
    expect(isReverting([of(kind, "running")])).toBe(false);
  }
});

/** 이 VM을 대상으로 한, 아직 끝나지 않은 Task */
function pendingOf(v: ReturnType<typeof vm>, kind: Task["spec"]["op"]["kind"]): Task {
  return { ...task(`t-${kind}`, kind, v.id), status: { state: "running", error: null } };
}

it("says why a power action is not available in the VM's state", () => {
  const stopped = vm("a", "stopped");
  const running = vm("b", "running");
  const stopping = vm("c", "stopping");
  const starting = vm("d", "starting");
  expect(availability(stopped, "start", [])).toEqual({ ok: true });
  expect(availability(stopped, "stop", [])).toEqual({ ok: false, reason: "vms.reason.stopped" });
  expect(availability(stopped, "force-stop", [])).toEqual({
    ok: false,
    reason: "vms.reason.stopped",
  });
  expect(availability(stopped, "reboot", [])).toEqual({ ok: false, reason: "vms.reason.stopped" });
  expect(availability(running, "start", [])).toEqual({ ok: false, reason: "vms.reason.running" });
  expect(availability(stopping, "start", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
  expect(availability(stopping, "stop", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
  expect(availability(stopping, "reboot", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
  expect(availability(starting, "start", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
  expect(availability(starting, "reboot", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
});

it("blames a failed VM's state as failed", () => {
  const failed = vm("e", "failed");
  const reason = { ok: false, reason: "vms.reason.failed" };
  expect(availability(failed, "start", [])).toEqual({ ok: true });
  expect(availability(failed, "delete", [])).toEqual({ ok: true });
  for (const action of ["stop", "force-stop", "reboot", "console"] as const) {
    expect(availability(failed, action, [])).toEqual(reason);
  }
  // 켜려는 채 실패했으면 시작도 실패 때문에 막힌다.
  const wantsRunning = vm("f", "failed", {
    spec: { ...failed.spec, power: "running" },
  });
  expect(availability(wantsRunning, "start", [])).toEqual(reason);
});

it("blocks start, stop and reboot while a power task of the VM is unfinished", () => {
  const stopped = vm("a", "stopped");
  const running = vm("b", "running");
  const pending = { ok: false, reason: "vms.reason.powerPending" };
  for (const kind of ["vm-start", "vm-stop", "vm-force-stop", "vm-reboot"] as const) {
    expect(availability(stopped, "start", [pendingOf(stopped, kind)])).toEqual(pending);
    expect(availability(running, "stop", [pendingOf(running, kind)])).toEqual(pending);
    expect(availability(running, "reboot", [pendingOf(running, kind)])).toEqual(pending);
    // 강제 정지는 ACPI 정지가 멈췄을 때 빠져나갈 길이다.
    expect(availability(running, "force-stop", [pendingOf(running, kind)])).toEqual({ ok: true });
  }
  // 끝난 전원 Task나 다른 Task는 막지 않는다.
  const done = { ...pendingOf(stopped, "vm-start"), status: { state: "succeeded", error: null } };
  expect(availability(stopped, "start", [done as Task])).toEqual({ ok: true });
  expect(availability(stopped, "start", [pendingOf(stopped, "snapshot-create")])).toEqual({
    ok: true,
  });
});

it("blocks force stop while reverting, but blames the state first", () => {
  const running = vm("b", "running");
  const stopped = vm("a", "stopped");
  expect(availability(running, "force-stop", [pendingOf(running, "snapshot-revert")])).toEqual({
    ok: false,
    reason: "vms.reason.reverting",
  });
  expect(availability(stopped, "force-stop", [pendingOf(stopped, "snapshot-revert")])).toEqual({
    ok: false,
    reason: "vms.reason.stopped",
  });
});

it("says why the console or delete is not available", () => {
  const stopped = vm("a", "stopped");
  const running = vm("b", "running");
  const starting = vm("d", "starting");
  expect(availability(running, "console", [])).toEqual({ ok: true });
  expect(availability(stopped, "console", [])).toEqual({ ok: false, reason: "vms.reason.stopped" });
  expect(availability(starting, "console", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
  expect(availability(stopped, "delete", [])).toEqual({ ok: true });
  expect(availability(running, "delete", [])).toEqual({ ok: false, reason: "vms.reason.running" });
  expect(availability(starting, "delete", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
});

/** 서버처럼 `migration`을 빼고 보낸 VM(없으면 undefined) */
function withoutMigration(v: ReturnType<typeof vm>): ReturnType<typeof vm> {
  const status: Partial<Vm["status"]> = { ...v.status };
  delete status.migration;
  return { ...v, status } as Vm;
}

const MIGRATION = { node: "n2", task: "t-migrate" };

it("migrates only a running VM that should run", () => {
  const running = withoutMigration(vm("b", "running"));
  expect(isMigrating(running, [])).toBe(false);
  expect(availability(running, "migrate", [])).toEqual({ ok: true });
  expect(availability(vm("a", "stopped"), "migrate", [])).toEqual({
    ok: false,
    reason: "vms.reason.stopped",
  });
  expect(availability(vm("d", "starting"), "migrate", [])).toEqual({
    ok: false,
    reason: "vms.reason.busy",
  });
  // 정지를 기다리는 VM
  const stopping = vm("e", "running", { spec: { ...running.spec, power: "stopped" } });
  expect(availability(stopping, "migrate", []).ok).toBe(false);
  // 전원 Task가 끝나지 않았다.
  expect(availability(running, "migrate", [pendingOf(running, "vm-reboot")])).toEqual({
    ok: false,
    reason: "vms.reason.powerPending",
  });
  // 서버의 check_no_snapshot_task: 스냅샷 만들기·되돌리기·복제 Task가 끝나지 않았다.
  for (const kind of ["snapshot-create", "snapshot-revert", "vm-clone"] as const) {
    expect(availability(running, "migrate", [pendingOf(running, kind)])).toEqual({
      ok: false,
      reason: "vms.reason.snapshotPending",
    });
  }
  // 스냅샷 지우기는 막지 않는다.
  expect(availability(running, "migrate", [pendingOf(running, "snapshot-delete")])).toEqual({
    ok: true,
  });
});

it("follows the server's check_no_migration: every action waits for a migration", () => {
  const running = vm("b", "running");
  const migrating = vm("m", "migrating", {
    spec: { ...running.spec, power: "running" },
    status: { ...running.status, phase: "migrating", migration: MIGRATION },
  });
  // 상태를 아직 쓰지 않았지만 마이그레이션 Task가 끝나지 않았다.
  const queued = [pendingOf(running, "vm-migrate")];
  const reason = { ok: false, reason: "vms.reason.migrating" };
  expect(isMigrating(migrating, [])).toBe(true);
  expect(isMigrating(running, queued)).toBe(true);
  expect(isBusy(migrating)).toBe(true);
  for (const action of [...ACTIONS, "delete", "migrate"] as const) {
    expect(availability(migrating, action, [])).toEqual(reason);
    expect(availability(running, action, queued)).toEqual(reason);
  }
  // 콘솔은 서버가 마이그레이션 중에도 받는다(확정하면 끊기고 다시 연결한다).
  expect(availability(migrating, "console", [])).toEqual({ ok: true });
  expect(availability(running, "console", queued)).toEqual({ ok: true });
  // 끝난 마이그레이션 Task는 막지 않는다.
  const done = { ...queued[0]!, status: { state: "failed", error: "boom" } } as Task;
  expect(availability(running, "migrate", [done])).toEqual({ ok: true });
});
