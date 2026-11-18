import { emptyForm } from "@/vms/form";
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

it("fills a preset and keeps the name and chosen devices", () => {
  const blank = emptyForm("q35");
  const current = {
    ...blank,
    name: "web",
    memory: "8192",
    tpm: true,
    disks: [{ kind: "existing" as const, volume: "root", bus: "virtio-blk" as const }],
  } as typeof blank;
  const linux = applyPreset("linux", current, blank);
  expect(linux).toMatchObject({ name: "web", memory: "2048", cores: "2", guestAgent: true });
  expect(linux.cloudInit.enabled).toBe(true);
  // 앞서 바꾼 값(TPM)은 처음 값으로 돌아가고, 고른 디스크는 남는다.
  expect(linux.tpm).toBe(false);
  expect(linux.disks).toHaveLength(1);
  const windows = applyPreset("windows", current, blank);
  expect(windows).toMatchObject({ tpm: true, secureBoot: true, memory: "4096", machine: "q35" });
  expect(windows.disks[0]!.bus).toBe("sata");
  expect(applyPreset("blank", windows, blank)).toMatchObject({
    name: "web",
    tpm: false,
    memory: blank.memory,
  });
});
