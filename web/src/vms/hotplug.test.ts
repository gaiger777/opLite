import { hardware, vm } from "@/test/fixtures";
import {
  deviceMode,
  failedChanges,
  hotplugPorts,
  isHotAdded,
  liveRemovable,
  memoryMatchesBoot,
  needsRestart,
  pendingReasonKey,
  runningMemory,
  runningVcpus,
  savedCount,
} from "@/vms/hotplug";

const disks = [
  { volume: "root", bus: "virtio-blk" as const, boot_order: 1 },
  { volume: "data", bus: "virtio-blk" as const, boot_order: null },
  { volume: "logs", bus: "virtio-blk" as const, boot_order: null },
];

/** 디스크 하나로 켠 뒤 둘을 붙인 VM */
const web = vm("web", "running", {
  spec: { ...vm("web", "running").spec, disks },
  status: {
    ...vm("web", "running").status,
    hardware: hardware({
      disks,
      layout: { vcpus: 4, boot_disks: 1, disk_ports: [0, 1], plugged_mib: 2048 },
    }),
  },
});

it("tells devices added while running from boot devices", () => {
  expect(isHotAdded(web, "disk", 0)).toBe(false);
  expect(isHotAdded(web, "disk", 1)).toBe(true);
  expect(isHotAdded(web, "disk", 2)).toBe(true);
  // 켤 때 단 NIC 하나
  expect(isHotAdded(web, "nic", 0)).toBe(false);
  expect(isHotAdded(web, "nic", 1)).toBe(true);
  // 배치가 없으면 모두 켤 때 단 것이다.
  const legacy = vm("old", "running", {
    status: { ...vm("old", "running").status, hardware: hardware({ layout: null }) },
  });
  expect(isHotAdded(legacy, "disk", 3)).toBe(false);
  expect(isHotAdded(vm("off", "stopped"), "disk", 3)).toBe(false);
});

it("shows the running vCPUs and memory with their maximums", () => {
  const hw = web.status.hardware!;
  expect(runningVcpus(hw)).toEqual({ now: 4, max: 8 });
  expect(runningMemory(hw)).toEqual({ now: 4096, max: 16384 });
  expect(hotplugPorts(hw)).toEqual({ used: 2, ports: 8 });
  // 배치가 없으면 켤 때 값이고 상한은 없다.
  const legacy = hardware({ layout: null, cpu: { sockets: 2, cores: 2, model: "host" } });
  expect(runningVcpus(legacy)).toEqual({ now: 4, max: null });
  expect(runningMemory(legacy)).toEqual({ now: 2048, max: null });
  expect(hotplugPorts(legacy)).toBeNull();
});

it("picks how the form may change disks and NICs", () => {
  expect(deviceMode(undefined)).toBe("free");
  expect(deviceMode(vm("off", "stopped"))).toBe("free");
  expect(deviceMode(web)).toBe("live");
  const legacy = vm("old", "running", {
    status: { ...vm("old", "running").status, hardware: hardware({ layout: null }) },
  });
  expect(deviceMode(legacy)).toBe("restart");
  // 하드웨어 기록이 없으면(옛 서버, 켜는 중) 지금처럼 잠근다.
  expect(deviceMode(vm("new", "running"))).toBe("locked");
});

it("removes only rows added in the form or the last device added while running", () => {
  const rows = disks.map((d) => d.volume);
  const same = (a: string, b: string) => a === b;
  expect(savedCount(rows, rows, same)).toBe(3);
  // 마지막 것을 뗀 뒤 새 행을 더했다.
  expect(savedCount(["root", "data", "new"], rows, same)).toBe(2);
  // 저장된 행: 실행 중 붙인 마지막 것만
  expect(liveRemovable(0, 3, 1)).toBe(false);
  expect(liveRemovable(1, 3, 1)).toBe(false);
  expect(liveRemovable(2, 3, 1)).toBe(true);
  expect(liveRemovable(1, 2, 1)).toBe(true);
  // 폼에서 더한 행은 언제나
  expect(liveRemovable(3, 3, 1)).toBe(true);
  // 켤 때 단 것은 마지막이어도 안 된다.
  expect(liveRemovable(0, 1, 1)).toBe(false);
});

it("lists the pending changes and the failed ones", () => {
  expect(needsRestart(web)).toBe(false);
  const pending = vm("p", "running", {
    status: {
      ...vm("p", "running").status,
      hardware: hardware({
        pending: [
          { field: "firmware", running: "uefi", wanted: "bios", reason: "restart" },
          {
            field: "memory_mib",
            running: "4096",
            wanted: "2048",
            reason: "failed",
            detail: "the guest released only 0 of 2048 MiB within 60s",
          },
        ],
      }),
    },
  });
  expect(needsRestart(pending)).toBe(true);
  expect(failedChanges(pending).map((c) => c.field)).toEqual(["memory_mib"]);
  expect(pendingReasonKey("unsupported")).toBe("vms.pendingReason.unsupported");
});

it("tells whether a memory snapshot can be taken like the server", () => {
  const at = (hw: ReturnType<typeof hardware> | undefined) =>
    memoryMatchesBoot(
      vm("web", "running", { status: { ...vm("web", "running").status, hardware: hw } }),
    );
  expect(at(undefined)).toBe(true);
  expect(at(hardware())).toBe(true);
  expect(at(hardware({ layout: null }))).toBe(true);
  expect(at(hardware({ layout: { vcpus: 3 } }))).toBe(false);
  expect(at(hardware({ layout: { plugged_mib: 128 } }))).toBe(false);
  expect(at(hardware({ layout: { disk_ports: [0] } }))).toBe(false);
  expect(at(hardware({ layout: { nic_ports: [1] } }))).toBe(false);
  expect(
    at(
      hardware({
        layout: null,
        pending: [{ field: "firmware", running: "uefi", wanted: "bios", reason: "restart" }],
      }),
    ),
  ).toBe(false);
});
