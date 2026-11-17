import { bootList, deviceName, effectiveBoot } from "@/vms/boot";

it("puts CD-ROM drives after the disks like the server", () => {
  // 서버의 `effective_boot_puts_cdroms_after_disks`와 같은 값
  expect(effectiveBoot([1, null], [])).toEqual({ disks: [1, null], cdroms: [] });
  expect(effectiveBoot([null, null], [])).toEqual({ disks: [null, null], cdroms: [] });
  expect(effectiveBoot([null, null], [null, null])).toEqual({ disks: [1, 2], cdroms: [3, 4] });
  expect(effectiveBoot([], [null])).toEqual({ disks: [], cdroms: [1] });
  expect(effectiveBoot([1, null], [null, null])).toEqual({ disks: [1, null], cdroms: [2, 3] });
  expect(effectiveBoot([2, null], [1, null])).toEqual({ disks: [2, null], cdroms: [1, 3] });
  expect(effectiveBoot([null], [5, null])).toEqual({ disks: [6], cdroms: [5, 7] });
  expect(effectiveBoot([null, null], [1])).toEqual({ disks: [2, 3], cdroms: [1] });
  expect(effectiveBoot([255], [null])).toEqual({ disks: [255], cdroms: [null] });
  expect(effectiveBoot([null], [255])).toEqual({ disks: [null], cdroms: [255] });
});

it("lists devices in boot order", () => {
  expect(bootList([1, null], [null])).toEqual({
    booting: [
      { kind: "disk", index: 0 },
      { kind: "cdrom", index: 0 },
    ],
    off: [{ kind: "disk", index: 1 }],
  });
  expect(bootList([2], [1]).booting.map(deviceName)).toEqual(["cdrom0", "disk0"]);
});
