import {
  importNameError,
  runForm,
  sourceNetworks,
  toRun,
  validateRun,
  volumeNames,
} from "@/imports/imports";
import { vmImport } from "@/test/fixtures";

const NONE = { vms: [], volumes: [] };

/** 원본 NIC가 둘(네트워크 둘, 한 네트워크에 둘)인 가져오기 */
const two = vmImport("web01", 1, {
  source_vm: {
    ...vmImport("web01", 1).status.source_vm!,
    nics: [
      { network: "VM Network", adapter: "vmxnet3", mac: "00:50:56:01:02:03" },
      { network: "Backup", adapter: "e1000", mac: null },
      { network: "VM Network", adapter: "vmxnet3", mac: null },
    ],
  },
  proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
});

it("checks new import names", () => {
  expect(importNameError("web01", ["db"])).toBeUndefined();
  expect(importNameError("Web 01", [])).toBe("forms.name");
  // `/vm-imports/ova` 경로와 겹친다.
  expect(importNameError("ova", [])).toBe("imports.errors.reserved");
  expect(importNameError("db", ["db"])).toBe("imports.errors.exists");
});

it("lists each source network once", () => {
  expect(sourceNetworks(two)).toEqual(["VM Network", "Backup"]);
  expect(sourceNetworks(vmImport("x", 2, { source_vm: undefined }))).toEqual([]);
});

it("names the volumes like the server", () => {
  expect(volumeNames(vmImport("web01", 1), "web")).toEqual(["web-disk0"]);
  // 63자로 자르고 끝의 `-`를 뗀다.
  expect(volumeNames(vmImport("web01", 1), `${"a".repeat(62)}-b`)).toEqual(["a".repeat(62)]);
});

it("fills the form from the proposal", () => {
  expect(runForm(two)).toEqual({
    name: "web01",
    pool: "",
    sockets: "1",
    cores: "2",
    memory: "1024",
    firmware: "uefi",
    diskBus: "virtio-blk",
    nicModel: "virtio",
    // 이름이 같은 네트워크가 없는 원본 네트워크는 비워 둔다.
    networks: { "VM Network": "lan", Backup: "" },
    keepMacs: true,
  });
});

it("requires a mapping to an existing network for every source network", () => {
  const form = { ...runForm(two), pool: "local" };
  expect(validateRun(form, two, NONE, ["lan", "user"])).toEqual({
    "networks.Backup": "imports.errors.network",
  });
  // 제안한 네트워크가 지워졌다.
  expect(
    validateRun({ ...form, networks: { "VM Network": "lan", Backup: "user" } }, two, NONE, [
      "user",
    ]),
  ).toEqual({ "networks.VM Network": "imports.errors.network" });
  expect(
    validateRun({ ...form, networks: { "VM Network": "lan", Backup: "user" } }, two, NONE, [
      "lan",
      "user",
    ]),
  ).toEqual({});
});

it("checks the name, pool and numbers", () => {
  const imp = vmImport("web01", 1);
  const form = { ...runForm(imp), networks: { "VM Network": "lan" } };
  expect(
    validateRun({ ...form, name: "-x", sockets: "0", cores: "1.5", memory: "32" }, imp, NONE, [
      "lan",
    ]),
  ).toEqual({
    name: "forms.name",
    pool: "imports.errors.pool",
    sockets: "imports.errors.sockets",
    cores: "vmForm.errors.cores",
    memory: "vmForm.errors.memory",
  });
  const ok = { ...form, pool: "local" };
  expect(validateRun({ ...ok, memory: "" }, imp, NONE, ["lan"])).toEqual({
    memory: "vmForm.errors.memory",
  });
  expect(validateRun(ok, imp, { vms: ["web01"], volumes: [] }, ["lan"])).toEqual({
    name: "imports.errors.vmExists",
  });
  expect(validateRun(ok, imp, { vms: [], volumes: ["web01-disk0"] }, ["lan"])).toEqual({
    name: "imports.errors.volumeExists",
  });
});

it("sends every value of the form", () => {
  const form = {
    ...runForm(vmImport("web01", 1)),
    name: "web",
    pool: "local",
    sockets: "2",
    cores: "4",
    memory: "8192",
    firmware: "bios" as const,
    diskBus: "sata" as const,
    nicModel: "e1000e" as const,
    networks: { "VM Network": "lan" },
    keepMacs: false,
  };
  expect(toRun(form)).toEqual({
    name: "web",
    pool: "local",
    networks: { "VM Network": "lan" },
    disk_bus: "sata",
    nic_model: "e1000e",
    firmware: "bios",
    sockets: 2,
    cores: 4,
    memory_mib: 8192,
    keep_macs: false,
  });
});
