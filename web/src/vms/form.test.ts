import { NODE, vm } from "@/test/fixtures";
import { defaultMachine, emptyDisk, emptyForm, fromVm, toSpec, validate } from "@/vms/form";

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
    },
  });
  expect(toSpec(fromVm(web), "running")).toEqual(web.spec);
});

it("picks virt only when every node is Apple Silicon", () => {
  const hvf = { ...NODE, status: { ...NODE.status, hypervisor: "qemu-hvf" } };
  const kvm = NODE;
  expect(defaultMachine([hvf])).toBe("virt");
  expect(defaultMachine([hvf, kvm])).toBe("q35");
  expect(defaultMachine([])).toBe("q35");
});
