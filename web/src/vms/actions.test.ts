import { vm } from "@/test/fixtures";
import { ACTIONS, canDelete, canRun, isStopped } from "@/vms/actions";

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
