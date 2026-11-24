#!/usr/bin/env python3
"""build.sh의 도우미(파일 복원 도우미 이미지, 설계 §8). 셸로 하기 어려운 일만 한다 — 표준 라이브러리만 쓴다.

  config <config-파일>                       커널 설정 탐침(필요한 값이 없으면 실패)
  kernel --arch A <vmlinuz> <출력>            x86_64는 그대로, aarch64는 푼 Image(gzip이면 푼다)
  assemble --arch A --sysroot S --release R --agent F --image-version V --tree T --cpio C
                                              initramfs 트리(T)와 그 cpio newc(C, 소유자 0·시각 0·차례 inode)
  manifest --arch A --release R --image-version V --proto N --dir D
  assemble-test-guest --arch A --sysroot S --release R --tree T --cpio C
                                              파일 복원 E2E (5)의 시험 게스트 initramfs(qemu-ga + busybox + /init 스크립트)

assemble은 ELF `NEEDED`를 따라 sysroot에서 공유 라이브러리를 모으고(없으면 이름을 알리고 실패), 커널 모듈은
`.modinfo`의 `depends=`·`softdep=pre:`를 따라 모아(depmod 없이) 의존이 먼저 오는 `modules.order`를 만든다. 모듈은 풀어
넣는다(initramfs 전체를 zstd로 묶으므로 더 작고, 커널의 압축 모듈 지원에 기대지 않는다).
"""

import argparse
import gzip
import hashlib
import json
import lzma
import os
import shutil
import stat
import struct
import subprocess
import sys

ARCHES = {
    # 아키텍처: (ELF e_machine, Debian 다중 아키텍처 이름, 커널 파일 이름)
    "x86_64": (62, "x86_64-linux-gnu", "vmlinuz"),
    "aarch64": (183, "aarch64-linux-gnu", "Image"),
}

# 커널 설정 탐침(설계 §4): y만 되는 것, y나 m이면 되는 것
CONFIG_BUILTIN = [
    "CONFIG_BLK_DEV_INITRD",
    "CONFIG_RD_ZSTD",
    "CONFIG_DEVTMPFS",
    "CONFIG_MSDOS_PARTITION",
    "CONFIG_EFI_PARTITION",
    "CONFIG_XFS_SUPPORT_V4",
]
CONFIG_MODULE = [
    "CONFIG_VIRTIO_PCI",
    "CONFIG_VIRTIO_BLK",
    "CONFIG_VIRTIO_CONSOLE",
    "CONFIG_HW_RANDOM_VIRTIO",
    "CONFIG_EXT4_FS",
    "CONFIG_XFS_FS",
    "CONFIG_BTRFS_FS",
    "CONFIG_NTFS3_FS",
    "CONFIG_VFAT_FS",
    "CONFIG_EXFAT_FS",
    "CONFIG_BLK_DEV_DM",
    "CONFIG_DM_THIN_PROVISIONING",
    "CONFIG_DM_SNAPSHOT",
    "CONFIG_DM_MIRROR",
    "CONFIG_DM_RAID",
]

# 올릴 모듈(이름의 `-`는 `_`로). 내장이면 건너뛴다. 커널이 이름으로 부르는 암호 알고리즘(ext4·xfs·btrfs의 crc32c,
# btrfs의 xxhash64·blake2b)은 modprobe가 없어 스스로 올라오지 못하므로 먼저 올린다.
MODULES = [
    "crc32c_generic",
    "xxhash_generic",
    "blake2b_generic",
    "virtio_pci",
    "virtio_blk",
    "virtio_console",
    "virtio_rng",
    "ext4",
    "xfs",
    "btrfs",
    "ntfs3",
    "nls_ascii",
    "nls_cp437",
    "nls_iso8859_1",
    "nls_utf8",
    "vfat",
    "exfat",
    "dm_mod",
    "dm_snapshot",
    "dm_mirror",
    "dm_thin_pool",
    "raid1",
    "raid10",
    "raid456",
    "dm_raid",
]

# sysroot(Debian deb을 푼 곳)에서 initramfs로 가져올 프로그램: (sysroot 경로, initramfs 경로)
PROGRAMS = [("usr/sbin/blkid", "bin/blkid"), ("usr/sbin/lvm", "bin/lvm")]
AGENT = "bin/orbivisor-restore-agent"

LVM_CONF = """\
# orbivisor-restore-image: 도우미 VM은 백업 디스크의 LVM 메타데이터를 쓰지 않는다. udev가 없어 장치 노드는 LVM이
# 스스로 만든다. 에이전트도 같은 값을 --config로 준다.
global {
\tmetadata_read_only = 1
}
activation {
\tudev_sync = 0
\tudev_rules = 0
\tverify_udev_operations = 1
\tmonitoring = 0
\tthin_check_executable = ""
}
devices {
\tobtain_device_list_from_udev = 0
}
backup {
\tbackup = 0
\tarchive = 0
}
"""


def die(msg):
    print(f"imagetool: {msg}", file=sys.stderr)
    sys.exit(1)


# --- ELF -----------------------------------------------------------------------------------------


class Elf:
    """64비트 리틀 엔디언 ELF의 머리·프로그램 머리·섹션 머리만 읽는다."""

    def __init__(self, data, name):
        if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
            die(f"{name}: not a 64-bit little-endian ELF file")
        self.data = data
        self.name = name
        (self.machine,) = struct.unpack_from("<H", data, 18)
        self.phoff, self.shoff = struct.unpack_from("<QQ", data, 32)
        self.phentsize, self.phnum, self.shentsize, self.shnum, self.shstrndx = (
            struct.unpack_from("<HHHHH", data, 54)
        )

    def segments(self):
        for i in range(self.phnum):
            p_type, _flags, offset, vaddr, _paddr, filesz, _memsz, _align = struct.unpack_from(
                "<IIQQQQQQ", self.data, self.phoff + i * self.phentsize
            )
            yield p_type, offset, vaddr, filesz

    def _cstr(self, off):
        end = self.data.index(b"\0", off)
        return self.data[off:end].decode()

    def interp(self):
        for p_type, offset, _vaddr, filesz in self.segments():
            if p_type == 3:  # PT_INTERP
                return self.data[offset : offset + filesz].rstrip(b"\0").decode()
        return None

    def _vaddr_to_off(self, vaddr):
        for p_type, offset, seg_vaddr, filesz in self.segments():
            if p_type == 1 and seg_vaddr <= vaddr < seg_vaddr + filesz:  # PT_LOAD
                return offset + vaddr - seg_vaddr
        die(f"{self.name}: address {vaddr:#x} is not in a loaded segment")

    def needed(self):
        dyn = None
        for p_type, offset, _vaddr, filesz in self.segments():
            if p_type == 2:  # PT_DYNAMIC
                dyn = (offset, filesz)
        if dyn is None:
            return []
        entries = []
        strtab = None
        for i in range(dyn[1] // 16):
            tag, val = struct.unpack_from("<qQ", self.data, dyn[0] + i * 16)
            if tag == 0:
                break
            if tag == 5:  # DT_STRTAB
                strtab = self._vaddr_to_off(val)
            entries.append((tag, val))
        if strtab is None:
            return []
        return [self._cstr(strtab + val) for tag, val in entries if tag == 1]  # DT_NEEDED

    def section(self, wanted):
        if self.shoff == 0:
            return None
        sections = []
        for i in range(self.shnum):
            name, _type, _flags, _addr, offset, size = struct.unpack_from(
                "<IIQQQQ", self.data, self.shoff + i * self.shentsize
            )
            sections.append((name, offset, size))
        names_off = sections[self.shstrndx][1]
        for name, offset, size in sections:
            if self._cstr(names_off + name) == wanted:
                return self.data[offset : offset + size]
        return None


def read_elf(path, arch, label):
    with open(path, "rb") as f:
        elf = Elf(f.read(), label)
    if elf.machine != ARCHES[arch][0]:
        die(f"{label}: ELF machine {elf.machine} is not {arch}")
    return elf


# --- 커널 설정 -----------------------------------------------------------------------------------


def cmd_config(args):
    values = {}
    with open(args.config) as f:
        for line in f:
            line = line.strip()
            if line.startswith("CONFIG_") and "=" in line:
                key, val = line.split("=", 1)
                values[key] = val
    missing = []
    for key in CONFIG_BUILTIN + CONFIG_MODULE:
        val = values.get(key, "n")
        good = ("y",) if key in CONFIG_BUILTIN else ("y", "m")
        print(f"  {key}={val}")
        if val not in good:
            missing.append(f"{key} must be {' or '.join(good)} (is {val})")
    if missing:
        die("kernel config does not fit the helper image: " + "; ".join(missing))


# --- 커널 이미지 ---------------------------------------------------------------------------------


def cmd_kernel(args):
    with open(args.input, "rb") as f:
        data = f.read()
    if args.arch == "aarch64":
        if data[:2] == b"\x1f\x8b":
            data = gzip.decompress(data)
        # arm64 Image 머리: 56바이트 자리에 "ARM\x64"(EFI zboot·다른 압축은 QEMU -kernel이 받지 못할 수 있다)
        if data[56:60] != b"ARM\x64":
            die(f"{args.input}: not an arm64 Image (EFI zboot kernels are not supported)")
    elif data[0x202:0x206] != b"HdrS":
        die(f"{args.input}: not an x86 bzImage")
    with open(args.output, "wb") as f:
        f.write(data)


# --- initramfs 트리 ------------------------------------------------------------------------------


def norm(name):
    return name.replace("-", "_")


def module_base(rel):
    base = os.path.basename(rel)
    for ext in (".ko.xz", ".ko.zst", ".ko.gz", ".ko"):
        if base.endswith(ext):
            return norm(base[: -len(ext)])
    return None


def decompress_module(path):
    with open(path, "rb") as f:
        data = f.read()
    if path.endswith(".xz"):
        return lzma.decompress(data)
    if path.endswith(".gz"):
        return gzip.decompress(data)
    if path.endswith(".zst"):
        return subprocess.run(
            ["zstd", "-dcq", path], check=True, stdout=subprocess.PIPE
        ).stdout
    return data


def modinfo(data, label):
    info = Elf(data, label).section(".modinfo") or b""
    out = {}
    for item in info.split(b"\0"):
        if b"=" in item:
            key, val = item.decode(errors="replace").split("=", 1)
            out.setdefault(key, []).append(val)
    return out


def collect_modules(sysroot, release, tree):
    """MODULES와 그 의존을 tree/lib/modules/<release>/에 풀어 넣고 modules.order를 쓴다."""
    src = os.path.join(sysroot, "usr/lib/modules", release)
    if not os.path.isdir(src):
        die(f"no kernel modules for {release} in the kernel package (usr/lib/modules/{release})")
    files = {}
    for root, _dirs, names in os.walk(os.path.join(src, "kernel")):
        for n in names:
            rel = os.path.relpath(os.path.join(root, n), src)
            base = module_base(rel)
            if base:
                files[base] = rel
    builtin = set()
    builtin_path = os.path.join(src, "modules.builtin")
    if os.path.exists(builtin_path):
        with open(builtin_path) as f:
            builtin = {module_base(line.strip()) for line in f if line.strip()}

    order = []
    seen = set()
    data_of = {}

    def visit(name, needed_by, required):
        name = norm(name)
        if name in seen or name in builtin:
            return
        if name not in files:
            if required:
                why = f" (needed by {needed_by})" if needed_by else ""
                die(f"kernel module {name}{why} is not in the kernel package")
            return
        seen.add(name)
        data = decompress_module(os.path.join(src, files[name]))
        info = modinfo(data, files[name])
        for deps in info.get("depends", []):
            for dep in filter(None, deps.split(",")):
                visit(dep, name, True)
        # softdep=pre: a b post: c — pre만, 별칭(crypto-…)은 파일이 없으니 건너뛴다
        for soft in info.get("softdep", []):
            words = soft.split()
            pre = False
            for w in words:
                if w.endswith(":"):
                    pre = w == "pre:"
                elif pre:
                    visit(w, name, False)
        data_of[name] = data
        order.append(name)

    for m in MODULES:
        visit(m, None, True)

    dst = os.path.join(tree, "lib/modules", release)
    lines = []
    for name in order:
        rel = files[name]
        rel = rel[: rel.index(".ko") + 3]
        path = os.path.join(dst, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as f:
            f.write(data_of[name])
        lines.append(rel)
    with open(os.path.join(dst, "modules.order"), "w") as f:
        f.write("".join(line + "\n" for line in lines))
    skipped = sorted(norm(m) for m in MODULES if norm(m) in builtin)
    print(f"  modules: {len(order)} to load, built in: {' '.join(skipped) or '-'}")


def install(src, dst, mode):
    """cpio의 모드는 실행 비트로 정하므로(0755·0644) 복사한 파일의 모드를 umask와 관계없이 정한다."""
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    shutil.copyfile(src, dst)
    os.chmod(dst, mode)


def write_text(dst, text):
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    with open(dst, "w") as f:
        f.write(text)
    os.chmod(dst, 0o644)


def resolve_in_sysroot(sysroot, path):
    """sysroot 안 경로의 심볼릭 링크를 sysroot 안에서 따라간다(절대 링크는 sysroot 기준)."""
    for _ in range(40):
        full = os.path.join(sysroot, path.lstrip("/"))
        if not os.path.islink(full):
            return full if os.path.isfile(full) else None
        target = os.readlink(full)
        if target.startswith("/"):
            path = target
        else:
            path = os.path.normpath(os.path.join(os.path.dirname("/" + path.lstrip("/")), target))
    return None


def collect_libraries(arch, sysroot, tree, programs):
    """프로그램들의 NEEDED를 따라 sysroot의 라이브러리를 tree/lib/<triplet>/<soname>으로 모은다."""
    triplet = ARCHES[arch][1]
    search = [f"usr/lib/{triplet}", f"lib/{triplet}", "usr/lib", "lib"]
    lib_dir = os.path.join(tree, "lib", triplet)
    os.makedirs(lib_dir, exist_ok=True)
    interps = set()
    done = set()
    missing = []
    queue = []
    for path, label in programs:
        elf = read_elf(path, arch, label)
        if elf.interp():
            interps.add(elf.interp())
        queue += [(n, label) for n in elf.needed()]
    interp_names = {os.path.basename(i) for i in interps}
    while queue:
        name, needed_by = queue.pop(0)
        if name in done or name in interp_names:
            continue
        done.add(name)
        found = None
        for d in search:
            found = resolve_in_sysroot(sysroot, f"{d}/{name}")
            if found:
                break
        if not found:
            missing.append(f"{name} (needed by {needed_by})")
            continue
        install(found, os.path.join(lib_dir, name), 0o755)
        elf = read_elf(found, arch, name)
        queue += [(n, name) for n in elf.needed()]
    # 동적 링커는 프로그램이 적은 경로 그대로(x86_64 /lib64/…, aarch64 /lib/…)
    for interp in sorted(interps):
        found = None
        for d in search:
            found = resolve_in_sysroot(sysroot, f"{d}/{os.path.basename(interp)}")
            if found:
                break
        if not found:
            missing.append(f"{os.path.basename(interp)} (the dynamic linker {interp})")
            continue
        install(found, os.path.join(tree, interp.lstrip("/")), 0o755)
    if missing:
        die(
            "shared libraries missing from the packages in debian.lock: "
            + ", ".join(missing)
            + " — add the Debian packages that ship them"
        )
    print(f"  libraries: {' '.join(sorted(done))}")


def newc_entry(ino, path, mode, data=b"", rdev=(0, 0)):
    """cpio newc 항목 하나(소유자 0, 시각 0, 링크 수 1, 머리+이름·내용은 4바이트 맞춤)"""
    name = path.encode() + b"\0"
    fields = (ino, mode, 0, 0, 1, 0, len(data), 0, 0, rdev[0], rdev[1], len(name), 0)
    head = b"070701" + b"".join(b"%08X" % v for v in fields) + name
    head += b"\0" * (-len(head) % 4)
    return head + data + b"\0" * (-len(data) % 4)


def write_cpio(tree, out):
    """tree의 파일·디렉터리·링크 + 빈 디렉터리·/dev/console(5,1)·/init 링크를 newc cpio로. 실행 파일만 0755,
    inode는 차례 번호 — 같은 입력이면 같은 바이트(bsdtar의 mtree 입력은 contents 파일의 inode를 그대로 써서 쓰지 않는다)."""
    entries = {}
    for d in ("dev", "proc", "sys", "run", "mnt", "tmp"):
        entries[d] = (stat.S_IFDIR | 0o755, None, (0, 0))
    entries["dev/console"] = (stat.S_IFCHR | 0o600, None, (5, 1))
    entries["init"] = (stat.S_IFLNK | 0o777, AGENT.encode(), (0, 0))
    for root, dirs, names in os.walk(tree):
        for n in dirs + names:
            full = os.path.join(root, n)
            rel = os.path.relpath(full, tree)
            st = os.lstat(full)
            if stat.S_ISLNK(st.st_mode):
                entries[rel] = (stat.S_IFLNK | 0o777, os.readlink(full).encode(), (0, 0))
            elif stat.S_ISDIR(st.st_mode):
                entries[rel] = (stat.S_IFDIR | 0o755, None, (0, 0))
            else:
                perm = 0o755 if st.st_mode & 0o111 else 0o644
                entries[rel] = (stat.S_IFREG | perm, full, (0, 0))
    with open(out, "wb") as f:
        # 조각 차례로 정렬 — 부모 디렉터리가 늘 먼저(커널 cpio 풀기는 차례대로 만든다)
        for ino, rel in enumerate(sorted(entries, key=lambda p: p.split("/")), start=1):
            mode, body, rdev = entries[rel]
            if stat.S_ISREG(mode):
                with open(body, "rb") as src:
                    body = src.read()
            f.write(newc_entry(ino, "./" + rel, mode, body or b"", rdev))
        f.write(newc_entry(0, "TRAILER!!!", 0))


def cmd_assemble(args):
    tree = args.tree
    if os.path.exists(tree):
        shutil.rmtree(tree)
    os.makedirs(os.path.join(tree, "bin"))
    agent = read_elf(args.agent, args.arch, "agent")
    if agent.interp() or agent.needed():
        die(f"{args.agent}: the agent must be a static binary (build it for {args.arch}-unknown-linux-musl)")
    install(args.agent, os.path.join(tree, AGENT), 0o755)
    programs = []
    for src, dst in PROGRAMS:
        found = resolve_in_sysroot(args.sysroot, src)
        if not found:
            die(f"/{src} is not in the packages in debian.lock")
        install(found, os.path.join(tree, dst), 0o755)
        programs.append((found, "/" + dst))
    collect_libraries(args.arch, args.sysroot, tree, programs)
    collect_modules(args.sysroot, args.release, tree)
    write_text(os.path.join(tree, "etc/lvm/lvm.conf"), LVM_CONF)
    write_text(os.path.join(tree, "etc/orbivisor/image-version"), args.image_version + "\n")
    write_cpio(tree, args.cpio)


# --- 시험 게스트(build.sh --test-guest) ---------------------------------------------------------------

# busybox 애플릿: /init과 되돌려 쓰기가 부르는 것(`guest-exec`의 /bin/mkdir·/bin/mv·/bin/rm)
TEST_GUEST_APPLETS = [
    "sh", "mount", "umount", "mkdir", "mv", "rm", "cat", "ls", "sleep", "sync", "insmod", "uname", "poweroff",
]

TEST_GUEST_INIT = """\
#!/bin/sh
# orbivisor 시험 게스트(계획 69 파일 복원 E2E (5)): 모듈을 올리고 첫 virtio 디스크(/dev/vda, ext4)를 /target에 붙인 뒤
# qemu-ga를 PID 1로 띄운다. 1초마다 sync — 시험이 VM을 강제로 꺼도 되돌려 쓴 파일이 디스크에 남는다.
export PATH=/bin
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
rel=$(uname -r)
while read -r m; do
\tinsmod "/lib/modules/$rel/$m" 2>/dev/null
done <"/lib/modules/$rel/modules.order"
port=""
i=0
while [ -z "$port" ] && [ "$i" -lt 60 ]; do
\tfor p in /sys/class/virtio-ports/*; do
\t\tif [ -f "$p/name" ] && [ "$(cat "$p/name")" = org.qemu.guest_agent.0 ]; then
\t\t\tport="/dev/${p##*/}"
\t\tfi
\tdone
\t[ -n "$port" ] || { sleep 1; i=$((i + 1)); }
done
i=0
while [ ! -b /dev/vda ] && [ "$i" -lt 60 ]; do
\tsleep 1
\ti=$((i + 1))
done
mkdir -p /target
mount -t ext4 /dev/vda /target || echo "test-guest: cannot mount /dev/vda" >/dev/console
(while :; do sleep 1; sync; done) &
echo "test-guest: qemu-ga on ${port:-?}" >/dev/console
exec /bin/qemu-ga -m virtio-serial -p "$port" --statedir=/run --pidfile=/run/qemu-ga.pid
"""


def cmd_assemble_test_guest(args):
    tree = args.tree
    if os.path.exists(tree):
        shutil.rmtree(tree)
    os.makedirs(os.path.join(tree, "bin"))
    qga = resolve_in_sysroot(args.sysroot, "usr/sbin/qemu-ga")
    if not qga:
        die("/usr/sbin/qemu-ga is not in the packages (test-guest.lock: qemu-guest-agent)")
    install(qga, os.path.join(tree, "bin/qemu-ga"), 0o755)
    busybox = resolve_in_sysroot(args.sysroot, "usr/bin/busybox") or resolve_in_sysroot(
        args.sysroot, "bin/busybox"
    )
    if not busybox:
        die("busybox is not in the packages (test-guest.lock: busybox-static)")
    elf = read_elf(busybox, args.arch, "busybox")
    if elf.interp() or elf.needed():
        die("busybox must be static (busybox-static)")
    install(busybox, os.path.join(tree, "bin/busybox"), 0o755)
    for applet in TEST_GUEST_APPLETS:
        os.symlink("busybox", os.path.join(tree, "bin", applet))
    collect_libraries(args.arch, args.sysroot, tree, [(qga, "/bin/qemu-ga")])
    collect_modules(args.sysroot, args.release, tree)
    init = os.path.join(tree, "init")
    write_text(init, TEST_GUEST_INIT)
    os.chmod(init, 0o755)
    os.makedirs(os.path.join(tree, "target"))
    write_cpio(tree, args.cpio)


# --- manifest.json -------------------------------------------------------------------------------


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def cmd_manifest(args):
    kernel = ARCHES[args.arch][2]
    manifest = {
        "arch": args.arch,
        "files": {name: sha256(os.path.join(args.dir, name)) for name in (kernel, "initrd.img")},
        "image_version": args.image_version,
        "initrd": "initrd.img",
        "kernel": kernel,
        "kernel_release": args.release,
        "proto": args.proto,
    }
    with open(os.path.join(args.dir, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2, sort_keys=True)
        f.write("\n")


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("config")
    c.add_argument("config")
    k = sub.add_parser("kernel")
    k.add_argument("--arch", choices=ARCHES, required=True)
    k.add_argument("input")
    k.add_argument("output")
    a = sub.add_parser("assemble")
    a.add_argument("--arch", choices=ARCHES, required=True)
    for opt in ("--sysroot", "--release", "--agent", "--image-version", "--tree", "--cpio"):
        a.add_argument(opt, required=True)
    t = sub.add_parser("assemble-test-guest")
    t.add_argument("--arch", choices=ARCHES, required=True)
    for opt in ("--sysroot", "--release", "--tree", "--cpio"):
        t.add_argument(opt, required=True)
    m = sub.add_parser("manifest")
    m.add_argument("--arch", choices=ARCHES, required=True)
    m.add_argument("--release", required=True)
    m.add_argument("--image-version", required=True)
    m.add_argument("--proto", type=int, required=True)
    m.add_argument("--dir", required=True)
    args = p.parse_args()
    {
        "config": cmd_config,
        "kernel": cmd_kernel,
        "assemble": cmd_assemble,
        "assemble-test-guest": cmd_assemble_test_guest,
        "manifest": cmd_manifest,
    }[args.cmd](args)


if __name__ == "__main__":
    main()
