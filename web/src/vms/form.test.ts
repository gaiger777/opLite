import { NODE, vm } from "@/test/fixtures";
import {
  defaultMachine,
  emptyDisk,
  emptyForm,
  fromVm,
  gibToBytes,
  toSpec,
  validate,
} from "@/vms/form";

it("checks values the server would reject", () => {
  const v = {
    ...emptyForm("q35"),
    name: "Web_01",
    cores: "0",
    memory: "32",
    disks: [
      { ...emptyDisk("web", 0), pool: "" },
      { ...emptyDisk("web", 1), pool: "local", bootOrder: "1" },
    ],
    nics: [{ network: "user", model: "virtio" as const, mac: "52:54:00:12:34" }],
  };
  expect(validate(v, true)).toEqual({
    name: "vmForm.errors.name",
    cores: "vmForm.errors.cores",
    memory: "vmForm.errors.memory",
    "disks.0.pool": "vmForm.errors.pool",
    "disks.1.bootOrder": "vmForm.errors.bootOrder",
    "nics.0.mac": "vmForm.errors.mac",
  });
  expect(validate({ ...emptyForm("q35"), name: "web-01" }, true)).toEqual({});
});

it("round-trips a VM through the form", () => {
  const web = vm("web", "running", {
    spec: {
      ...vm("web", "running").spec,
      disks: [{ volume: "web-root", bus: "sata", boot_order: 1 }],
      nics: [{ network: "lan", model: "e1000e", mac: "52:54:00:12:34:56" }],
      snapshot_schedule: { every: "daily", keep: 7, memory: true },
    },
  });
  // 폼에 없는 예약 스냅샷도 고칠 때 잃지 않는다.
  expect(toSpec(fromVm(web), "running")).toEqual(web.spec);
});

it("picks virt only when every node is Apple Silicon", () => {
  const hvf = { ...NODE, status: { ...NODE.status, hypervisor: "qemu-hvf" } };
  const kvm = NODE;
  expect(defaultMachine([hvf])).toBe("virt");
  expect(defaultMachine([hvf, kvm])).toBe("q35");
  expect(defaultMachine([])).toBe("q35");
});

it("matches the server's limits on cores, boot order and MACs", () => {
  const v = {
    ...emptyForm("q35"),
    name: "web",
    cores: "65536",
    disks: [
      { ...emptyDisk("web", 0), pool: "local", bootOrder: "256" },
      { ...emptyDisk("web", 1), pool: "local", bootOrder: "2" },
      { ...emptyDisk("web", 2), pool: "local", bootOrder: "02" },
    ],
    nics: [
      { network: "user", model: "virtio" as const, mac: "01:00:5e:00:00:01" },
      { network: "user", model: "virtio" as const, mac: "52:54:00:12:34:56" },
      { network: "user", model: "virtio" as const, mac: "52:54:00:12:34:56" },
    ],
  };
  expect(validate(v, true)).toEqual({
    cores: "vmForm.errors.cores",
    "disks.0.bootOrder": "vmForm.errors.bootOrder",
    // "02"와 "2"는 같은 순서다.
    "disks.2.bootOrder": "vmForm.errors.bootOrder",
    "nics.0.mac": "vmForm.errors.macMulticast",
    "nics.2.mac": "vmForm.errors.macDuplicate",
  });
  expect(validate({ ...v, cores: "65535", disks: [], nics: [] }, true)).toEqual({});
});

it("checks volume names across disks and against existing volumes", () => {
  const v = {
    ...emptyForm("q35"),
    name: "web",
    disks: [
      { ...emptyDisk("", 0), kind: "existing" as const },
      { ...emptyDisk("web", 0), pool: "local", bootOrder: "" },
      { ...emptyDisk("web", 0), pool: "local", bootOrder: "" },
      { ...emptyDisk("web", 3), pool: "local" },
    ],
  };
  expect(validate(v, true, ["web-disk3"])).toEqual({
    "disks.0.volume": "vmForm.errors.volumeChoose",
    "disks.2.volume": "vmForm.errors.volumeDuplicate",
    "disks.3.volume": "vmForm.errors.volumeExists",
  });
});

it("rejects the reserved volume name for new disks", () => {
  const v = {
    ...emptyForm("q35"),
    name: "web",
    disks: [{ ...emptyDisk("web", 0), volume: "import", pool: "local" }],
  };
  expect(validate(v, true)).toEqual({ "disks.0.volume": "volumes.errors.reserved" });
});

it("turns GiB into a whole number of bytes", () => {
  expect(gibToBytes("16")).toBe(16 * 1024 ** 3);
  expect(gibToBytes("1.5")).toBe(1.5 * 1024 ** 3);
  for (const bad of [
    "",
    "0",
    "-1",
    "1e-10",
    "0.0000000001",
    "1e3",
    "0x10",
    "Infinity",
    " 1",
    "99999999",
  ]) {
    expect(gibToBytes(bad)).toBeUndefined();
  }
  const v = {
    ...emptyForm("q35"),
    name: "web",
    disks: [{ ...emptyDisk("web", 0), pool: "local" }],
  };
  expect(validate({ ...v, disks: [{ ...v.disks[0]!, sizeGib: "1e3" }] }, true)).toEqual({
    "disks.0.size": "vmForm.errors.size",
  });
});
