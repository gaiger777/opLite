import { emptyDisk, emptyForm } from "@/vms/form";
import { applyPreset, errorCounts, stepOf } from "@/vms/form/steps";

it("puts each input error on its step", () => {
  expect(stepOf("name")).toBe("general");
  expect(stepOf("haPriority")).toBe("general");
  expect(stepOf("disks.0.volume")).toBe("disks");
  expect(stepOf("disks.1.bootOrder")).toBe("boot");
  expect(stepOf("cdroms.0.bootOrder")).toBe("boot");
  expect(stepOf("cdroms")).toBe("cdroms");
  expect(stepOf("nics.0.mac")).toBe("nics");
  expect(stepOf("ci.networks.0.ipv4Address")).toBe("cloudInit");
  expect(stepOf("schedule.keep")).toBe("schedule");
  expect(stepOf("replication.0.node")).toBe("replication");
  expect(stepOf("tpm")).toBe("security");
  expect(stepOf("secureBoot")).toBe("security");
  expect(errorCounts({ name: "x", memory: "x", "disks.0.size": "x", "ci.user": "x" })).toEqual({
    general: 2,
    disks: 1,
    cloudInit: 1,
  });
});

it("overlays only the preset's own fields and keeps everything else", () => {
  const blank = emptyForm("q35");
  const current = {
    ...blank,
    name: "web",
    memory: "8192",
    tpm: true,
    ha: true,
    node: "n2",
    cloudInit: { ...blank.cloudInit, user: "ops", sshKeys: "ssh-ed25519 AAAA" },
    snapshotSchedule: { every: "daily", keep: "7", memory: false },
    disks: [{ kind: "existing" as const, volume: "root", bus: "virtio-blk" as const }],
  } as typeof blank;
  const linux = applyPreset("linux", "blank", current);
  expect(linux).toMatchObject({ name: "web", memory: "2048", cores: "2", guestAgent: true });
  expect(linux.cloudInit).toMatchObject({
    enabled: true,
    user: "ops",
    sshKeys: "ssh-ed25519 AAAA",
  });
  // 프리셋이 정하지 않는 값(TPM·HA·노드·스케줄)은 남는다.
  expect(linux).toMatchObject({ tpm: true, ha: true, node: "n2" });
  expect(linux.snapshotSchedule.every).toBe("daily");
  // 템플릿과 빈 VM은 값을 바꾸지 않는다.
  expect(applyPreset("template", "linux", linux)).toBe(linux);
  expect(applyPreset("blank", "linux", linux)).toEqual(linux);
  expect(applyPreset("linux", "linux", linux)).toBe(linux);
});

it("resets the Windows devices when another preset replaces Windows", () => {
  const blank = emptyForm("q35");
  const current = {
    ...blank,
    disks: [emptyDisk("web", 0), emptyDisk("web", 1)],
    nics: [{ network: "user", model: "virtio" as const, mac: "", securityGroups: [], ips: "" }],
  };
  const windows = applyPreset("windows", "blank", current);
  expect(windows).toMatchObject({ tpm: true, secureBoot: true, memory: "4096", machine: "q35" });
  expect(windows.disks.map((d) => d.bus)).toEqual(["sata", "sata"]);
  expect(windows.nics[0]!.model).toBe("e1000e");
  const linux = applyPreset("linux", "windows", windows);
  expect(linux.disks.map((d) => d.bus)).toEqual(["virtio-blk", "virtio-blk"]);
  // Windows가 정한 다른 값은 남는다(프리셋은 제 값만 얹는다).
  expect(linux).toMatchObject({ tpm: true, memory: "2048" });
  expect(linux.nics[0]!.model).toBe("virtio");
  expect(applyPreset("blank", "windows", windows).disks[0]!.bus).toBe("virtio-blk");
});
