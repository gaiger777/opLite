import type { Task } from "@/api/types";
import { task, vm } from "@/test/fixtures";
import { ACTIONS, availability, isBusy, isReverting, isStopped } from "@/vms/actions";

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
