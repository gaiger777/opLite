#!/bin/bash
# packaging/restore-image/build.sh(파일 복원 도우미 이미지)를 가짜 deb으로 돌려 본다: 인자, debian.lock 형식, 해시가
# 어긋나면 실패, ELF NEEDED를 따라 라이브러리를 모으고 빠지면 이름을 알리며 실패, 정적이 아닌·다른 아키텍처 에이전트
# 거절, 커널 설정 탐침, 모듈 의존 차례(depmod 없이)와 내장 모듈 건너뜀, initramfs 내용(/init 링크, /dev/console,
# 동적 링커 자리, 소유자 0·시각 0), manifest.json, 두 번 빌드하면 같은 바이트.
# 가짜 deb(ar + data.tar.xz)·ELF·커널 모듈은 아래 Python이 만들고 file:// URL로 받는다 — 네트워크·cargo가 필요 없다.
# 필요한 것: bash, curl, bsdtar, zstd, python3(Linux·macOS).
# 쓰는 법: bash packaging/tests/restore-image-test.sh
set -euo pipefail
# bsdtar -tv의 날짜 꼴이 로캘을 따르지 않게
export LC_ALL=C

IMAGE_DIR="$(cd "$(dirname "$0")/../restore-image" && pwd)"
BUILD="$IMAGE_DIR/build.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FAILED=0
ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

REL=9.9.9-fake-amd64
# 가짜 deb·에이전트·debian.lock을 만든다: $WORK/debs/*.deb, $WORK/agent(정적), $WORK/agent-dynamic, $WORK/agent-arm,
# $WORK/debian.lock(DEBS_x86_64 — file:// URL과 실제 해시)
python3 -I - "$IMAGE_DIR" "$WORK" "$REL" <<'EOF'
import hashlib, io, lzma, os, struct, sys, tarfile
image_dir, work, rel = sys.argv[1:4]
sys.path.insert(0, image_dir)
import imagetool  # noqa: E402

def elf(machine=62, interp=None, needed=(), etype=2, modinfo=None):
    """64비트 LE ELF: PT_LOAD 하나(가상 주소 = 파일 자리), PT_INTERP, PT_DYNAMIC(DT_NEEDED·DT_STRTAB), .modinfo 섹션"""
    phnum = 1 + (interp is not None) + bool(needed)
    off = 64 + 56 * phnum
    blobs = b""
    interp_off = None
    if interp is not None:
        interp_off = off
        blobs += interp.encode() + b"\0"
    dyn_off = dynstr_off = None
    if needed:
        dynstr = b"\0"
        offs = []
        for n in needed:
            offs.append(len(dynstr))
            dynstr += n.encode() + b"\0"
        dynstr_off = off + len(blobs)
        blobs += dynstr
        while (off + len(blobs)) % 8:
            blobs += b"\0"
        dyn_off = off + len(blobs)
        dyn = b"".join(struct.pack("<qQ", 1, o) for o in offs) + struct.pack("<qQ", 5, dynstr_off) + struct.pack("<qQ", 0, 0)
        blobs += dyn
    shoff = 0
    shnum = 0
    sections = b""
    if modinfo is not None:
        shstr = b"\0.modinfo\0.shstrtab\0"
        mi_off = off + len(blobs)
        blobs += modinfo
        sh_off = off + len(blobs)
        blobs += shstr
        while (off + len(blobs)) % 8:
            blobs += b"\0"
        shoff = off + len(blobs)
        shnum = 3
        sec = lambda name, o, size: struct.pack("<IIQQQQIIQQ", name, 1, 0, 0, o, size, 0, 0, 1, 0)
        sections = b"\0" * 64 + sec(1, mi_off, len(modinfo)) + sec(10, sh_off, len(shstr))
    total = off + len(blobs) + len(sections)
    ph = struct.pack("<IIQQQQQQ", 1, 5, 0, 0, 0, total, total, 0x1000)
    if interp is not None:
        ph += struct.pack("<IIQQQQQQ", 3, 4, interp_off, interp_off, interp_off, len(interp) + 1, len(interp) + 1, 1)
    if needed:
        ph += struct.pack("<IIQQQQQQ", 2, 6, dyn_off, dyn_off, dyn_off, len(dyn), len(dyn), 8)
    hdr = b"\x7fELF\x02\x01\x01" + b"\0" * 9 + struct.pack(
        "<HHIQQQIHHHHHH", etype, machine, 1, 0, 64, shoff, 0, 64, 56, phnum, 64, shnum, 2 if shnum else 0)
    return hdr + ph + blobs + sections

def deb(name, files, links=()):
    """files: {경로: (바이트, 모드)}, links: [(경로, 대상)] → $WORK/debs/<name>.deb (ar: debian-binary, control, data)"""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:xz", format=tarfile.GNU_FORMAT) as t:
        dirs = set()
        for p in list(files) + [l[0] for l in links]:
            parts = p.split("/")[:-1]
            for i in range(1, len(parts) + 1):
                dirs.add("/".join(parts[:i]))
        for d in sorted(dirs):
            ti = tarfile.TarInfo("./" + d); ti.type = tarfile.DIRTYPE; ti.mode = 0o755; t.addfile(ti)
        for p, (data, mode) in sorted(files.items()):
            ti = tarfile.TarInfo("./" + p); ti.size = len(data); ti.mode = mode; t.addfile(ti, io.BytesIO(data))
        for p, target in links:
            ti = tarfile.TarInfo("./" + p); ti.type = tarfile.SYMTYPE; ti.linkname = target; t.addfile(ti)
    control = io.BytesIO()
    with tarfile.open(fileobj=control, mode="w:xz") as t:
        data = f"Package: {name}\n".encode()
        ti = tarfile.TarInfo("./control"); ti.size = len(data); t.addfile(ti, io.BytesIO(data))
    members = [("debian-binary", b"2.0\n"), ("control.tar.xz", control.getvalue()), ("data.tar.xz", buf.getvalue())]
    out = b"!<arch>\n"
    for n, d in members:
        out += f"{n:<16}{0:<12}{0:<6}{0:<6}{'100644':<8}{len(d):<10}`\n".encode() + d
        if len(d) % 2:
            out += b"\n"
    path = os.path.join(work, "debs", name + ".deb")
    with open(path, "wb") as f:
        f.write(out)
    return path

os.makedirs(os.path.join(work, "debs"))
T = "usr/lib/x86_64-linux-gnu"
INTERP = "/lib64/ld-linux-x86-64.so.2"
# 커널: bzImage 머리("HdrS"), 설정, 모듈(목록 전부 + 의존 jbd2·mbcache·crc16·libcrc32c), 내장 virtio_pci·virtio_console
kernel = bytearray(0x400); kernel[0x202:0x206] = b"HdrS"
config = "".join(f"{k}=y\n" for k in imagetool.CONFIG_BUILTIN) + "".join(f"{k}=m\n" for k in imagetool.CONFIG_MODULE)
deps = {"ext4": "jbd2,mbcache,crc16", "xfs": "libcrc32c", "btrfs": "libcrc32c,xor", "jbd2": "crc16",
        "dm_thin_pool": "dm-persistent-data,dm-bio-prison,dm_mod"}
softdeps = {"ext4": "pre: crc32c", "btrfs": "pre: crc32c xxhash64 blake2b-256 post: nothing"}
builtin = {"virtio_pci", "virtio_console"}
files = {f"boot/vmlinuz-{rel}": (bytes(kernel), 0o644), f"boot/config-{rel}": (config.encode(), 0o644)}
names = set(imagetool.MODULES) | {"jbd2", "mbcache", "crc16", "libcrc32c", "xor", "dm_persistent_data", "dm_bio_prison",
                                   "unused_module"}
for m in sorted(names - builtin):
    info = f"name={m}\0depends={deps.get(m, '')}\0".encode()
    if m in softdeps:
        info += f"softdep={softdeps[m]}\0".encode()
    ko = elf(etype=1, modinfo=info)
    fname = m.replace("_", "-") if m.startswith("dm_") else m
    files[f"usr/lib/modules/{rel}/kernel/fake/{fname}.ko.xz"] = (lzma.compress(ko), 0o644)
files[f"usr/lib/modules/{rel}/modules.builtin"] = (b"kernel/drivers/virtio/virtio_pci.ko\nkernel/drivers/char/virtio_console.ko\n", 0o644)
files["usr/share/doc/linux-image/changelog"] = (b"doc", 0o644)
debs = [("linux-image-fake", deb("linux-image-fake", files))]
debs.append(("util-linux", deb("util-linux", {"usr/sbin/blkid": (elf(interp=INTERP, needed=["libblkid.so.1", "libc.so.6"]), 0o755)})))
debs.append(("lvm2", deb("lvm2", {"usr/sbin/lvm": (elf(interp=INTERP, needed=["libdevmapper.so.1.02.1", "libc.so.6"]), 0o755)})))
# libblkid.so.1은 상대 링크, libc6의 /lib64 동적 링커는 절대 링크(sysroot 안에서 따라가야 한다)
debs.append(("libblkid1", deb("libblkid1", {f"{T}/libblkid.so.1.1.0": (elf(needed=["libc.so.6"]), 0o644)},
                               [(f"{T}/libblkid.so.1", "libblkid.so.1.1.0")])))
debs.append(("libdevmapper1.02.1", deb("libdevmapper1.02.1", {f"{T}/libdevmapper.so.1.02.1": (elf(needed=["libc.so.6", "libm.so.6"]), 0o644)})))
debs.append(("libc6", deb("libc6", {f"{T}/libc.so.6": (elf(needed=["ld-linux-x86-64.so.2"]), 0o644),
                                     f"{T}/libm.so.6": (elf(needed=["libc.so.6"]), 0o644),
                                     f"{T}/ld-linux-x86-64.so.2": (elf(etype=3), 0o644)},
                           [("usr/lib64/ld-linux-x86-64.so.2", f"/{T}/ld-linux-x86-64.so.2")])))
with open(os.path.join(work, "agent"), "wb") as f:
    f.write(elf())
with open(os.path.join(work, "agent-dynamic"), "wb") as f:
    f.write(elf(interp="/lib/ld-musl-x86_64.so.1", needed=["libc.so"]))
with open(os.path.join(work, "agent-arm"), "wb") as f:
    f.write(elf(machine=183))
with open(os.path.join(work, "debian.lock"), "w") as f:
    f.write("# fake\nDEBS_x86_64='\n")
    for name, path in debs:
        sha = hashlib.sha256(open(path, "rb").read()).hexdigest()
        f.write(f"{name} 1.0 file://{path} {sha}\n")
    f.write("'\n")
EOF

LOCK="$WORK/debian.lock"
# run <출력 디렉터리> <인자...>: build.sh를 돌려 종료 코드(code)·출력(log)을 남긴다(캐시는 시험마다 같은 곳).
run() {
	out=$1
	shift
	code=0
	bash "$BUILD" --out "$out" --cache "$WORK/cache" "$@" >"$WORK/log" 2>&1 || code=$?
}
expect_fail() {
	if [ "$code" -eq "$2" ] && grep -qF -- "$3" "$WORK/log"; then
		ok "$1"
	else
		fail "$1: 종료 코드 $code (기대 $2, 문구 '$3')"
		cat "$WORK/log"
	fi
}

# 인자
run "$WORK/o" --arch x86_64 --agent "$WORK/agent" --lock "$LOCK" --version 9.9.9 --oops x
expect_fail "모르는 인자" 2 usage
run "$WORK/o" --arch riscv64 --agent "$WORK/agent" --lock "$LOCK"
expect_fail "모르는 아키텍처" 2 usage
run "$WORK/o" --arch x86_64 --agent "$WORK/none" --lock "$LOCK"
expect_fail "없는 에이전트" 1 "no agent binary"
run "$WORK/o" --arch x86_64 --agent "$WORK/agent" --lock "$WORK/none.lock"
expect_fail "없는 잠금 파일" 1 "no lock file"

# 맞는 빌드
run "$WORK/out1" --arch x86_64 --agent "$WORK/agent" --lock "$LOCK" --version 9.9.9
if [ "$code" -eq 0 ] && [ -f "$WORK/out1/vmlinuz" ] && [ -f "$WORK/out1/initrd.img" ] && [ -f "$WORK/out1/manifest.json" ]; then
	ok "가짜 deb으로 이미지를 만든다"
else
	fail "빌드: 종료 코드 $code"
	cat "$WORK/log"
fi
sha256_of() { python3 -I -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$1"; }
LOCK_SHA=$(sha256_of "$LOCK" | cut -c1-12)
if python3 -I - "$WORK/out1" "$LOCK_SHA" "$REL" <<'EOF'; then
import hashlib, json, os, sys
d, lock, rel = sys.argv[1:4]
m = json.load(open(os.path.join(d, "manifest.json")))
sha = lambda n: hashlib.sha256(open(os.path.join(d, n), "rb").read()).hexdigest()
assert m == {"arch": "x86_64", "files": {"vmlinuz": sha("vmlinuz"), "initrd.img": sha("initrd.img")},
             "image_version": f"orbivisor 9.9.9 lock-{lock}", "initrd": "initrd.img", "kernel": "vmlinuz",
             "kernel_release": rel, "proto": 1}, m
EOF
	ok "manifest.json(이미지 판 = 판 + 잠금 파일 해시, 파일 해시, 통신 규약 판)"
else
	fail "manifest.json: $(cat "$WORK/out1/manifest.json")"
fi
if grep -q "CONFIG_RD_ZSTD=y" "$WORK/log" && grep -q "built in: virtio_console virtio_pci" "$WORK/log"; then
	ok "커널 설정 탐침을 찍고 내장 모듈은 건너뛴다"
else
	fail "설정·내장 모듈: $(cat "$WORK/log")"
fi

# initramfs: newc cpio + zstd
mkdir -p "$WORK/x"
zstd -dcq "$WORK/out1/initrd.img" >"$WORK/initrd.cpio"
bsdtar -tvf "$WORK/initrd.cpio" >"$WORK/listing"
bsdtar -xf "$WORK/initrd.cpio" -C "$WORK/x" --exclude ./dev/console
empty_dirs() {
	for d in proc sys run mnt tmp; do grep -qE "^drwxr-xr-x .* \./$d\$" "$WORK/listing" || return 1; done
}
if head -c 6 "$WORK/initrd.cpio" | grep -q 070701 &&
	grep -qE '^lrwxrwxrwx .* \./init -> bin/orbivisor-restore-agent$' "$WORK/listing" &&
	grep -qE '^crw------- .* 0 +0 +5,1 .* \./dev/console$' "$WORK/listing" &&
	grep -qE '^-rwxr-xr-x .* \./bin/orbivisor-restore-agent$' "$WORK/listing" &&
	grep -qE '^-rwxr-xr-x .* \./bin/blkid$' "$WORK/listing" &&
	grep -qE '^-rwxr-xr-x .* \./bin/lvm$' "$WORK/listing" &&
	grep -qE '^-rwxr-xr-x .* \./lib64/ld-linux-x86-64\.so\.2$' "$WORK/listing" &&
	grep -qE '^-rw-r--r-- .* \./etc/lvm/lvm\.conf$' "$WORK/listing" &&
	[ "$(cat "$WORK/x/etc/orbivisor/image-version")" = "orbivisor 9.9.9 lock-$LOCK_SHA" ] &&
	grep -q 'metadata_read_only = 1' "$WORK/x/etc/lvm/lvm.conf" &&
	empty_dirs &&
	! grep -q usr/share "$WORK/listing"; then
	ok "initramfs: /init 링크, /dev/console(5,1), 프로그램, /lib64 동적 링커, lvm.conf, image-version, 빈 디렉터리"
else
	fail "initramfs: $(cat "$WORK/listing")"
fi
if [ "$(cd "$WORK/x/lib/x86_64-linux-gnu" && printf '%s\n' * | LC_ALL=C sort | tr '\n' ' ')" = "libblkid.so.1 libc.so.6 libdevmapper.so.1.02.1 libm.so.6 " ] &&
	[ ! -L "$WORK/x/lib/x86_64-linux-gnu/libblkid.so.1" ]; then
	ok "NEEDED를 따라 모은 라이브러리(soname 이름의 파일, 링크를 따라감, 동적 링커는 빼고)"
else
	fail "라이브러리: $(ls -l "$WORK/x/lib/x86_64-linux-gnu")"
fi
if awk '{ print $3, $4, $6, $7, $8 }' "$WORK/listing" | grep -qv '^0 0 Jan 1 1970$'; then
	fail "소유자·시각이 0이 아닌 항목: $(cat "$WORK/listing")"
else
	ok "모든 항목의 소유자 0·시각 0"
fi
# modules.order: 상대 경로, 풀린 .ko, 의존이 먼저, 목록 밖 모듈은 없음
ORDER="$WORK/x/lib/modules/$REL/modules.order"
pos() { grep -n "/$1\.ko\$" "$ORDER" | cut -d: -f1; }
if [ -f "$ORDER" ] && ! grep -q '\.xz$' "$ORDER" && ! grep -q '^/' "$ORDER" &&
	[ "$(pos jbd2)" -lt "$(pos ext4)" ] && [ "$(pos crc16)" -lt "$(pos jbd2)" ] && [ "$(pos mbcache)" -lt "$(pos ext4)" ] &&
	[ "$(pos libcrc32c)" -lt "$(pos xfs)" ] && [ "$(pos xor)" -lt "$(pos btrfs)" ] &&
	[ "$(pos dm-persistent-data)" -lt "$(pos dm-thin-pool)" ] && [ "$(pos dm-mod)" -lt "$(pos dm-thin-pool)" ] &&
	[ "$(pos crc32c_generic)" -lt "$(pos ext4)" ] && ! grep -q unused_module "$ORDER" && ! grep -q virtio_pci "$ORDER" &&
	head -c 4 "$WORK/x/lib/modules/$REL/kernel/fake/ext4.ko" | grep -q ELF; then
	ok "modules.order: 의존이 먼저, 풀린 .ko, 목록 밖·내장 모듈 없음"
else
	fail "modules.order: $(cat "$ORDER")"
fi

# 재현: 같은 입력이면 같은 바이트
run "$WORK/out2" --arch x86_64 --agent "$WORK/agent" --lock "$LOCK" --version 9.9.9
if [ "$code" -eq 0 ] && cmp -s "$WORK/out1/initrd.img" "$WORK/out2/initrd.img" &&
	cmp -s "$WORK/out1/manifest.json" "$WORK/out2/manifest.json" && cmp -s "$WORK/out1/vmlinuz" "$WORK/out2/vmlinuz"; then
	ok "두 번 빌드하면 같은 바이트"
else
	fail "재현: 종료 코드 $code"
fi

# 잠금 파일 형식
bad_lock() {
	cp "$LOCK" "$WORK/bad.lock"
	python3 -I -c 'import sys; p=sys.argv[1]; s=open(p).read(); open(p,"w").write(s.replace(sys.argv[2], sys.argv[3], 1))' \
		"$WORK/bad.lock" "$1" "$2"
	run "$WORK/obad" --arch x86_64 --agent "$WORK/agent" --lock "$WORK/bad.lock" --version 9.9.9
}
bad_lock "util-linux 1.0 " "util-linux "
expect_fail "잠금 파일: 칸이 모자란 줄" 1 "bad line in DEBS_x86_64"
bad_lock "util-linux 1.0 file://" "util-linux 1.0 http://"
expect_fail "잠금 파일: https·file이 아닌 URL" 1 "bad line in DEBS_x86_64"
bad_lock "util-linux 1.0 " "Util-Linux 1.0 "
expect_fail "잠금 파일: 꾸러미 이름 형식" 1 "bad line in DEBS_x86_64"
bad_lock "DEBS_x86_64=" "DEBS_other="
expect_fail "잠금 파일: 아키텍처 목록 없음" 1 "has no DEBS_x86_64"
# 해시 어긋남: 캐시·출력에 남기지 않는다
UTIL_SHA=$(sed -n 's/^util-linux .* \([0-9a-f]\{64\}\)$/\1/p' "$LOCK")
bad_lock "$UTIL_SHA" "$(printf '%064d' 0)"
if [ "$code" -eq 1 ] && grep -q "SHA-256 mismatch for file://.*util-linux.deb: got $UTIL_SHA" "$WORK/log" &&
	[ ! -e "$WORK/obad" ] && ! compgen -G "$WORK/cache/0000000000000000-*" >/dev/null; then
	ok "해시가 어긋나면 실패(캐시·출력 없음)"
else
	fail "해시 어긋남: 종료 코드 $code $(cat "$WORK/log")"
fi

# 빠진 라이브러리: libdevmapper deb을 빼면 lvm의 NEEDED가 없다
grep -v '^libdevmapper1.02.1 ' "$LOCK" >"$WORK/nolib.lock"
run "$WORK/obad" --arch x86_64 --agent "$WORK/agent" --lock "$WORK/nolib.lock" --version 9.9.9
expect_fail "빠진 라이브러리는 이름과 필요로 한 프로그램을 알린다" 1 "libdevmapper.so.1.02.1 (needed by /bin/lvm)"
grep -v '^lvm2 ' "$LOCK" >"$WORK/nolvm.lock"
run "$WORK/obad" --arch x86_64 --agent "$WORK/agent" --lock "$WORK/nolvm.lock" --version 9.9.9
expect_fail "lvm이 없으면 실패" 1 "/usr/sbin/lvm is not in the packages in debian.lock"

# 커널 설정 탐침: 필요한 값이 없으면 실패(가짜 커널 deb의 설정에서 CONFIG_RD_ZSTD를 뺀 deb)
python3 -I - "$WORK" <<'PY'
import io, sys, tarfile, hashlib
work = sys.argv[1]
src = open(f"{work}/debs/linux-image-fake.deb", "rb").read()
# ar 꾸러미의 data.tar.xz(마지막 멤버)를 다시 쓴다
head = src.index(b"data.tar.xz")
size = int(src[head + 48 : head + 58])
data = src[head + 60 : head + 60 + size]
out = io.BytesIO()
with tarfile.open(fileobj=io.BytesIO(data), mode="r:xz") as t, tarfile.open(fileobj=out, mode="w:xz") as o:
    for m in t.getmembers():
        f = t.extractfile(m) if m.isfile() else None
        body = f.read() if f else None
        if m.name.startswith("./boot/config-"):
            body = body.replace(b"CONFIG_RD_ZSTD=y\n", b"")
            m.size = len(body)
        o.addfile(m, io.BytesIO(body) if body is not None else None)
new = out.getvalue()
hdr = src[head : head + 48] + f"{len(new):<10}".encode() + b"`\n"
deb = src[:head] + hdr + new + (b"\n" if len(new) % 2 else b"")
open(f"{work}/debs/linux-image-noconf.deb", "wb").write(deb)
print(hashlib.sha256(deb).hexdigest())
PY
NOCONF_SHA=$(sha256_of "$WORK/debs/linux-image-noconf.deb")
sed -e "s|^linux-image-fake .*|linux-image-fake 1.0 file://$WORK/debs/linux-image-noconf.deb $NOCONF_SHA|" "$LOCK" >"$WORK/noconf.lock"
run "$WORK/obad" --arch x86_64 --agent "$WORK/agent" --lock "$WORK/noconf.lock" --version 9.9.9
expect_fail "커널 설정에 필요한 값이 없으면 실패" 1 "CONFIG_RD_ZSTD must be y (is n)"

# 에이전트
run "$WORK/obad" --arch x86_64 --agent "$WORK/agent-dynamic" --lock "$LOCK" --version 9.9.9
expect_fail "정적이 아닌 에이전트 거절" 1 "the agent must be a static binary"
run "$WORK/obad" --arch x86_64 --agent "$WORK/agent-arm" --lock "$LOCK" --version 9.9.9
expect_fail "다른 아키텍처의 에이전트 거절" 1 "ELF machine 183 is not x86_64"

if [ "$FAILED" -ne 0 ]; then
	echo "restore-image-test: 실패"
	exit 1
fi
echo "restore-image-test: 모두 통과"
