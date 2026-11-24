import type { Node } from "@/api/types";
import { fileRestoreAction } from "@/fileRestore/availability";
import {
  clock,
  defaultTargetDir,
  displayPath,
  downloadFormat,
  entryKey,
  entryPath,
  type EntryView,
  firstBrowsable,
  groupVolumes,
  modeString,
  nameSegments,
  pathQuery,
  serverSkew,
  targetDirOk,
  visibleName,
  visiblePath,
  volumeBlocked,
  type VolumeView,
  volumeWarning,
} from "@/fileRestore/fileRestore";
import { NODE } from "@/test/fixtures";

function vol(id: string, extra: Partial<VolumeView> = {}): VolumeView {
  return {
    id,
    disk: 0,
    kind: "disk",
    mountable: true,
    size: 1 << 20,
    state: "unmounted",
    ...extra,
  };
}

function entry(name: string, extra: Partial<EntryView> = {}): EntryView {
  return { name, kind: "file", mode: 0o644, size: 1, mtime: 0, uid: 0, gid: 0, ...extra };
}

it("encodes paths as strings or raw bytes", () => {
  const raw = btoa("\xff\xfe"); // UTF-8가 아닌 이름
  expect(pathQuery([])).toEqual({ path: "/" });
  expect(pathQuery([{ name: "etc" }, { name: "nginx" }])).toEqual({ path: "/etc/nginx" });
  // `/etc/` + 0xff 0xfe → URL-safe base64, 채움 없음
  const b64 = pathQuery([{ name: "etc" }, { name: "\ufffd\ufffd", raw }]);
  expect(b64).toEqual({ path_b64: "L2V0Yy___g" });
  expect(entryPath([{ name: "etc" }, { name: "\ufffd\ufffd", raw }])).toEqual([
    "etc",
    { b64: raw },
  ]);
  expect(displayPath([{ name: "etc" }, { name: "x" }])).toBe("/etc/x");
  expect(entryKey({ name: "a" })).not.toBe(entryKey({ name: "a", raw: "YQ==" }));
});

it("formats modes like ls -l", () => {
  expect(modeString("dir", 0o755)).toBe("drwxr-xr-x");
  expect(modeString("file", 0o644)).toBe("-rw-r--r--");
  expect(modeString("symlink", 0o777)).toBe("lrwxrwxrwx");
  expect(modeString("file", 0o4755)).toBe("-rwsr-xr-x");
  expect(modeString("dir", 0o1777)).toBe("drwxrwxrwt");
  expect(modeString("file", 0o2640)).toBe("-rw-r-S---");
  expect(modeString("other", 0o600)).toBe("?rw-------");
});

it("groups volumes by disk with partitions in order and LVs after them", () => {
  const groups = groupVolumes([
    vol("lvm/vg/root", { kind: "logical_volume", disk: 1 }),
    vol("disk1p2", {
      kind: "partition",
      disk: 1,
      partition: { number: 2, table: "gpt", type_name: "Linux LVM" },
    }),
    vol("disk1", { disk: 1, mountable: false }),
    vol("disk1p1", {
      kind: "partition",
      disk: 1,
      partition: { number: 1, table: "gpt", type_name: "EFI System" },
    }),
    vol("disk0"),
  ]);
  expect(groups.map((g) => [g.disk, g.self?.id, g.children.map((c) => c.id)])).toEqual([
    [0, "disk0", []],
    [1, "disk1", ["disk1p1", "disk1p2", "lvm/vg/root"]],
  ]);
});

it("explains volumes that cannot be browsed", () => {
  expect(volumeBlocked(vol("d"))).toBeNull();
  expect(volumeBlocked(vol("d", { mountable: false }))?.key).toBe("fileRestore.blocked.none");
  expect(volumeBlocked(vol("d", { mountable: false, fs: "LVM2_member" }))?.key).toBe(
    "fileRestore.blocked.lvm",
  );
  expect(
    volumeBlocked(
      vol("d", { mountable: false, fs: "crypto_LUKS", note: "encrypted (LUKS) — not supported" }),
    ),
  ).toEqual({
    key: "fileRestore.blocked.note",
    values: { note: "encrypted (LUKS) — not supported" },
  });
  expect(volumeBlocked(vol("d", { mountable: false, fs: "zfs_member" }))?.values).toEqual({
    fs: "zfs_member",
  });
  expect(volumeBlocked(vol("d", { state: "failed", note: "bad superblock" }))?.key).toBe(
    "fileRestore.blocked.failed",
  );
  expect(volumeWarning(vol("d", { note: "journal not replayed" }))).toBe("journal not replayed");
  expect(firstBrowsable([vol("disk0", { mountable: false }), vol("disk1", { disk: 1 })])?.id).toBe(
    "disk1",
  );
});

it("picks download formats and target folders", () => {
  expect(downloadFormat([entry("a")])).toBe("auto");
  expect(downloadFormat([entry("a", { kind: "dir" })])).toBe("zip");
  expect(downloadFormat([entry("a"), entry("b")])).toBe("zip");
  expect(downloadFormat([entry("a")], "tar.gz")).toBe("tar.gz");
  expect(defaultTargetDir([{ name: "etc" }, { name: "nginx" }], "linux")).toBe("/etc/nginx");
  expect(defaultTargetDir([], undefined)).toBe("/");
  expect(defaultTargetDir([{ name: "Users" }, { name: "bob" }], "windows")).toBe("C:\\Users\\bob");
  expect(defaultTargetDir([{ name: "x", raw: "/w==" }], "linux")).toBe("/");
  expect(targetDirOk("/srv", "linux")).toBe(true);
  expect(targetDirOk("srv", "linux")).toBe(false);
  expect(targetDirOk("C:\\restored", "windows")).toBe(true);
  expect(targetDirOk("/restored", "windows")).toBe(false);
  expect(clock(83)).toBe("1:23");
});

it("shows the browse action only when the cluster can do it", () => {
  const withFeature = (n: Partial<Node["status"]>): Node => ({
    ...NODE,
    status: { ...NODE.status, features: ["file-restore"], ...n },
  });
  const image = {
    accel: "kvm",
    arch: "x86_64" as const,
    image_version: "orbivisor 0.1.0 lock-1",
    max_sessions: 4,
    sessions: 0,
  };
  // 기능을 모르는 노드(옛 노드)가 있으면 숨긴다.
  expect(fileRestoreAction([NODE], true)).toBe("hidden");
  expect(fileRestoreAction([withFeature({ file_restore: image }), NODE], true)).toBe("hidden");
  expect(fileRestoreAction(undefined, true)).toBe("hidden");
  // 이미지가 있는 노드가 없으면 막는다.
  expect(fileRestoreAction([withFeature({})], true)).toBe("unavailable");
  expect(fileRestoreAction([withFeature({}), withFeature({ file_restore: image })], true)).toBe(
    "ready",
  );
  // 노드를 읽지 못하는 사용자는 서버가 판단한다.
  expect(fileRestoreAction(undefined, false)).toBe("ready");
});

it("makes hidden and bidi characters in names visible", () => {
  expect(visibleName("plain.txt")).toBe("plain.txt");
  expect(visibleName("a\nb\tc\rd")).toBe("a\\nb\\tc\\rd");
  expect(visibleName("invoice‮txt.exe")).toBe("invoice\\u202Etxt.exe");
  expect(visibleName("\u0000\u007f\u0085​⁦﻿")).toBe("\\u0000\\u007F\\u0085\\u200B\\u2066\\uFEFF");
  // 한글·U+FFFD(UTF-8이 아닌 이름)·이모지는 그대로
  expect(visibleName("문서 � 😀")).toBe("문서 � 😀");
  expect(nameSegments("a‮b")).toEqual([
    { text: "a", escaped: false },
    { text: "\\u202E", escaped: true },
    { text: "b", escaped: false },
  ]);
  expect(nameSegments("\n")).toEqual([{ text: "\\n", escaped: true }]);
  expect(nameSegments("")).toEqual([{ text: "", escaped: false }]);
  expect(visiblePath([{ name: "etc" }, { name: "x‏y" }])).toBe("/etc/x\\u200Fy");
});

it("measures the server clock from the Date header", () => {
  const now = Date.parse("2026-11-24T10:00:00Z");
  expect(serverSkew("Tue, 24 Nov 2026 10:03:00 GMT", now)).toBe(180);
  expect(serverSkew("Tue, 24 Nov 2026 09:58:00 GMT", now)).toBe(-120);
  // 초 단위로 잘린 머리와 지연은 무시한다.
  expect(serverSkew("Tue, 24 Nov 2026 10:00:01 GMT", now + 300)).toBe(0);
  expect(serverSkew(null, now)).toBe(0);
  expect(serverSkew("garbage", now)).toBe(0);
});

// 리뷰 2 웹 S1: 빈칸으로 보이거나 보이지 않는 다른 문자(BMP 밖의 태그 문자도)를 드러낸다.
it("escapes blank-looking and tag characters too", () => {
  expect(visibleName("a\u00adb")).toBe("a\\u00ADb");
  expect(visibleName("\u3164")).toBe("\\u3164");
  expect(visibleName("x\u2028y\u2029")).toBe("x\\u2028y\\u2029");
  expect(visibleName("a\u034fb\u180e\ufffa")).toBe("a\\u034Fb\\u180E\\uFFFA");
  expect(visibleName("\u115f\u1160")).toBe("\\u115F\\u1160");
  // 태그 문자(U+E0041)는 한 글자로 드러낸다(대리 쌍을 둘로 쪼개지 않는다).
  expect(visibleName("flag\u{e0041}")).toBe("flag\\u{E0041}");
  expect(nameSegments("a\u{e007f}b")).toEqual([
    { text: "a", escaped: false },
    { text: "\\u{E007F}", escaped: true },
    { text: "b", escaped: false },
  ]);
  // 보이는 BMP 밖 글자(이모지)는 그대로다.
  expect(visibleName("😀.txt")).toBe("😀.txt");
});

// 리뷰 2 웹 #7: 숨은 문자가 든 부모 폴더는 고칠 수 있는 대상 칸에 넣지 않고 루트를 기본으로 한다.
it("does not default the target folder to a parent with hidden characters", () => {
  expect(defaultTargetDir([{ name: "srv" }, { name: "‮gnp.exe" }], "linux")).toBe("/");
  expect(defaultTargetDir([{ name: "two\nlines" }], "windows")).toBe("C:\\");
  expect(defaultTargetDir([{ name: "srv" }, { name: "www" }], "linux")).toBe("/srv/www");
});

// 리뷰 2 웹 #6: 볼륨의 덧붙임(마운트 오류 글)과 파일시스템 이름은 게스트에서 온다 — 숨은 문자를 드러낸다.
it("escapes hidden characters in volume notes", () => {
  expect(volumeBlocked(vol("d", { state: "failed", note: "bad\u202esb" }))).toEqual({
    key: "fileRestore.blocked.failed",
    values: { note: "bad\\u202Esb" },
  });
  expect(volumeBlocked(vol("d", { mountable: false, note: "x\ny" }))?.values).toEqual({
    note: "x\\ny",
  });
});
