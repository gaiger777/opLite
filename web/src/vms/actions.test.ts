import { vm } from "@/test/fixtures";
import { ACTIONS, canDelete, canRun, isBusy, isStopped } from "@/vms/actions";

function allowed(v: ReturnType<typeof vm>) {
  return ACTIONS.filter((action) => canRun(v, action));
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
