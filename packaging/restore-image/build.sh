#!/bin/bash
# 파일 단위 복원의 도우미 VM 이미지(계획 69 설계 §8)를 만든다: Debian 13 커널 + 우리 initramfs.
#
# 쓰는 법: packaging/restore-image/build.sh --arch x86_64|aarch64 --out <dir>
#            [--agent <정적 바이너리>] [--version <판>] [--lock <debian.lock>] [--cache <dir>]
#            [--test-guest | --test-lock <test-guest.lock>]
#   결과(<dir>): 커널(x86_64 `vmlinuz`, aarch64 푼 `Image`), `initrd.img`(cpio newc + zstd), `manifest.json`
#   (이미지 판, 아키텍처, 커널 판, 통신 규약 판, 파일마다 SHA-256 — 맨 나중에 쓴다).
#   설치 자리: deb `orbivisor-restore-image`는 /usr/share/orbivisor/restore/x86_64, Rocky·소스 빌드 노드는
#   `--out /usr/share/orbivisor/restore/<arch>`(opld `[file_restore] image_dir`의 기본), macOS 개발은
#   `{data_dir}/restore-image/<arch>`.
#
# 무엇이 들어가나:
#   - 커널과 모듈: debian.lock의 `linux-image-*` deb. 모듈은 imagetool.py의 목록(virtio, ext4·xfs·btrfs·ntfs3·vfat·
#     exfat, nls, device mapper·thin·raid)과 그 `.modinfo` 의존만, 풀어서 `/lib/modules/<커널 판>/`에 넣고
#     의존이 먼저 오는 `modules.order`를 만든다(depmod 없음 — 에이전트가 차례로 `finit_module`).
#   - 사용자 공간: `/init` → `/bin/orbivisor-restore-agent`(정적 musl, PID 1), Debian의 `/bin/blkid`·`/bin/lvm`과
#     그 공유 라이브러리(`/lib/<triplet>/`, 동적 링커는 프로그램이 적은 자리). 라이브러리는 ELF `NEEDED`를 따라
#     debian.lock의 deb에서만 찾는다 — 없으면 이름을 알리고 실패하므로 그 deb을 debian.lock에 더한다.
#   - `/dev/console`(문자 장치 5,1 — 커널이 init 전에 연다), 빈 `/proc /sys /run /mnt /tmp`,
#     `/etc/lvm/lvm.conf`(메타데이터 읽기 전용), `/etc/orbivisor/image-version`.
#   - 셸·busybox·udev는 없다. 에이전트가 `/proc /sys /dev(devtmpfs) /run /mnt`를 붙인다.
#   커널 명령줄은 opld가 준다: `console=ttyS0|ttyAMA0 panic=-1 quiet opl.session=<id> opl.proto=<manifest proto>`.
#
# 재현: deb은 debian.lock의 SHA-256과 같아야 하고(어긋나면 실패), cpio는 imagetool.py가 직접 쓴다(소유자 0, 시각 0,
#   정렬, 차례 inode — 장치 노드도 루트 없이). 같은 입력이면 같은 바이트다(zstd 판이 같을 때).
# 에이전트: --agent가 없으면 빌드한다 — `cargo build --release -p opl-restore --bin orbivisor-restore-agent
#   --target <arch>-unknown-linux-musl`, 링커 rust-lld(C 툴체인 없이 교차 빌드, `rustup target add <타깃>`).
# 판: 기본은 워크스페이스 판(Cargo.toml). 이미지 판 = `orbivisor <판> lock-<debian.lock SHA-256 앞 12자>`.
# 받은 deb은 --cache(기본 ${XDG_CACHE_HOME:-~/.cache}/orbivisor-restore-image)에 두고 다시 쓴다(해시는 늘 다시 봄).
#   snapshot.debian.org에서 받지 못하면 같은 pool 경로를 $DEBIAN_MIRROR(기본 deb.debian.org)에서 한 번 더 받아 본다.
# --test-guest: 도우미 이미지 대신 파일 복원 E2E (5)의 **시험 게스트**를 만든다(설계 §14) — 같은 커널·모듈에 Debian
#   `qemu-guest-agent`(test-guest.lock)와 정적 busybox를 넣고, `/init`(셸 스크립트)이 첫 virtio 디스크(/dev/vda, ext4)를
#   /target에 붙인 뒤 qemu-ga를 PID 1로 띄운다(1초마다 sync — 시험이 VM을 강제로 꺼도 쓴 것이 디스크에 남게). 결과는 커널과
#   `initrd.img`뿐(manifest 없음 — opld가 도우미로 쓰지 않는다). 시험은 이 커널을 QEMU 감싸개로 `-kernel`·`-initrd`에 준다.
#   `--test-lock`은 test-guest.lock 대신 다른 잠금 파일로(빌더 시험).
# 필요한 것: bash, curl, bsdtar(Debian·Ubuntu libarchive-tools, Rocky bsdtar, macOS 기본), zstd, python3
#   (imagetool.py — ELF·모듈 정보 읽기, cpio 쓰기), sha256sum 또는 shasum.
set -euo pipefail

IMAGE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$IMAGE_DIR/../.." && pwd)
TOOL="$IMAGE_DIR/imagetool.py"

usage() {
	echo "usage: $0 --arch x86_64|aarch64 --out <dir> [--agent <file>] [--version <version>] [--lock <file>] [--cache <dir>] [--test-guest | --test-lock <file>]" >&2
	exit 2
}

die() {
	echo "build-restore-image: $*" >&2
	exit 1
}

ARCH="" OUT="" AGENT="" VERSION="" LOCK="$IMAGE_DIR/debian.lock" TEST_GUEST=""
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/orbivisor-restore-image"
while [ $# -gt 0 ]; do
	if [ "$1" = --test-guest ]; then
		TEST_GUEST="$IMAGE_DIR/test-guest.lock"
		shift
		continue
	fi
	[ $# -ge 2 ] || usage
	case $1 in
	--arch) ARCH=$2 ;;
	--out) OUT=$2 ;;
	--agent) AGENT=$2 ;;
	--version) VERSION=$2 ;;
	--lock) LOCK=$2 ;;
	--cache) CACHE=$2 ;;
	--test-lock) TEST_GUEST=$2 ;;
	*) usage ;;
	esac
	shift 2
done
case $ARCH in
x86_64 | aarch64) ;;
*) usage ;;
esac
[ -n "$OUT" ] || usage
[ -f "$LOCK" ] || die "no lock file $LOCK"
[ -z "$AGENT" ] || [ -f "$AGENT" ] || die "no agent binary $AGENT"
for tool in curl bsdtar zstd python3; do
	command -v "$tool" >/dev/null || die "$tool is not installed"
done

sha256() {
	if command -v sha256sum >/dev/null; then
		sha256sum "$1" | cut -d' ' -f1
	else
		shasum -a 256 "$1" | cut -d' ' -f1
	fi
}

if [ -z "$VERSION" ]; then
	VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)
fi
PROTO=$(sed -n 's/^pub const PROTO_VERSION: u32 = \([0-9]*\);/\1/p' "$ROOT/crates/opl-restore/src/proto.rs")
[ -n "$PROTO" ] || die "PROTO_VERSION not found in crates/opl-restore/src/proto.rs"
LOCK_SHA=$(sha256 "$LOCK")
IMAGE_VERSION="orbivisor $VERSION lock-${LOCK_SHA:0:12}"

# debian.lock: 셸 KEY=value. DEBS_<arch>는 한 줄에 deb 하나 `NAME VERSION URL SHA256`.
# shellcheck source=/dev/null
. "$LOCK"
DEBS_VAR="DEBS_$ARCH"
DEBS=${!DEBS_VAR:-}
[ -n "$DEBS" ] || die "$LOCK has no $DEBS_VAR"
if [ -n "$TEST_GUEST" ]; then
	[ -f "$TEST_GUEST" ] || die "no lock file $TEST_GUEST"
	unset "$DEBS_VAR"
	# shellcheck source=/dev/null
	. "$TEST_GUEST"
	EXTRA=${!DEBS_VAR:-}
	[ -n "$EXTRA" ] || die "$TEST_GUEST has no $DEBS_VAR"
	DEBS="$DEBS$EXTRA"
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
SYSROOT="$WORK/sysroot"
mkdir -p "$SYSROOT" "$CACHE" "$WORK/out"

# fetch <url> <sha256> <파일>: 캐시에 없거나 해시가 다르면 받는다. 받은 뒤에도 다르면 실패.
fetch() {
	url=$1 want=$2 file=$3
	if [ -f "$file" ] && [ "$(sha256 "$file")" = "$want" ]; then
		return
	fi
	rm -f "$file.part"
	if ! curl -fsSL --retry 3 --connect-timeout 30 -o "$file.part" "$url"; then
		case $url in
		*/pool/*)
			mirror="${DEBIAN_MIRROR:-https://deb.debian.org/debian}/pool/${url#*/pool/}"
			echo "build-restore-image: $url failed, trying $mirror" >&2
			curl -fsSL --retry 3 --connect-timeout 30 -o "$file.part" "$mirror" || die "cannot download $url"
			;;
		*) die "cannot download $url" ;;
		esac
	fi
	got=$(sha256 "$file.part")
	if [ "$got" != "$want" ]; then
		rm -f "$file.part"
		die "SHA-256 mismatch for $url: got $got, debian.lock has $want"
	fi
	mv "$file.part" "$file"
}

echo "build-restore-image: $IMAGE_VERSION ($ARCH)"
n=0
while read -r name version url sum extra; do
	[ -n "$name" ] || continue
	case $name in \#*) continue ;; esac
	if [ -z "$sum" ] || [ -n "$extra" ] || ! [[ $name =~ ^[a-z0-9][a-z0-9.+-]*$ ]] ||
		! [[ $sum =~ ^[0-9a-f]{64}$ ]] || ! [[ $url =~ ^(https|file):// ]]; then
		die "bad line in $DEBS_VAR of $LOCK: $name $version $url $sum $extra"
	fi
	deb="$CACHE/${sum:0:16}-$name.deb"
	fetch "$url" "$sum" "$deb"
	# data.tar.*만 sysroot에 푼다(문서·man은 뺌).
	data=$(bsdtar -tf "$deb" | grep '^data\.tar' | head -n1)
	[ -n "$data" ] || die "$deb has no data.tar"
	bsdtar -xOf "$deb" "$data" | bsdtar -xf - -C "$SYSROOT" --exclude ./usr/share
	n=$((n + 1))
	echo "  $name $version"
done <<<"$DEBS"
echo "build-restore-image: $n packages"

# 커널 판: linux-image deb의 /boot/vmlinuz-<판> 하나
kernels=("$SYSROOT"/boot/vmlinuz-*)
if [ ${#kernels[@]} -ne 1 ] || [ ! -f "${kernels[0]}" ]; then
	die "debian.lock must have exactly one linux-image package for $ARCH"
fi
RELEASE=${kernels[0]##*/vmlinuz-}
echo "build-restore-image: kernel $RELEASE"
python3 "$TOOL" config "$SYSROOT/boot/config-$RELEASE"

if [ -z "$AGENT" ] && [ -z "$TEST_GUEST" ]; then
	TARGET="$ARCH-unknown-linux-musl"
	LINKER_VAR="CARGO_TARGET_$(echo "$TARGET" | tr 'a-z-' 'A-Z_')_LINKER"
	echo "build-restore-image: building the agent for $TARGET"
	(cd "$ROOT" && env "$LINKER_VAR=${!LINKER_VAR:-rust-lld}" cargo build --release --locked \
		-p opl-restore --bin orbivisor-restore-agent --target "$TARGET") ||
		die "building the agent failed (rustup target add $TARGET)"
	AGENT="${CARGO_TARGET_DIR:-$ROOT/target}/$TARGET/release/orbivisor-restore-agent"
fi

case $ARCH in
x86_64) KERNEL_FILE=vmlinuz ;;
aarch64) KERNEL_FILE=Image ;;
esac
python3 "$TOOL" kernel --arch "$ARCH" "${kernels[0]}" "$WORK/out/$KERNEL_FILE"
if [ -n "$TEST_GUEST" ]; then
	python3 "$TOOL" assemble-test-guest --arch "$ARCH" --sysroot "$SYSROOT" --release "$RELEASE" \
		--tree "$WORK/tree" --cpio "$WORK/initrd.cpio"
	zstd -q -19 -T1 -c "$WORK/initrd.cpio" >"$WORK/out/initrd.img"
	mkdir -p "$OUT"
	for f in "$KERNEL_FILE" initrd.img; do
		cp "$WORK/out/$f" "$OUT/$f.tmp"
		chmod 0644 "$OUT/$f.tmp"
		mv -f "$OUT/$f.tmp" "$OUT/$f"
	done
	echo "build-restore-image: test guest $OUT: $KERNEL_FILE, initrd.img $(wc -c <"$OUT/initrd.img" | tr -d ' ') bytes"
	exit 0
fi
python3 "$TOOL" assemble --arch "$ARCH" --sysroot "$SYSROOT" --release "$RELEASE" --agent "$AGENT" \
	--image-version "$IMAGE_VERSION" --tree "$WORK/tree" --cpio "$WORK/initrd.cpio"
zstd -q -19 -T1 -c "$WORK/initrd.cpio" >"$WORK/out/initrd.img"
python3 "$TOOL" manifest --arch "$ARCH" --release "$RELEASE" --image-version "$IMAGE_VERSION" \
	--proto "$PROTO" --dir "$WORK/out"

# 결과: 커널·initrd를 먼저, manifest.json을 맨 나중에(opld는 manifest가 바뀌면 다시 검사한다)
mkdir -p "$OUT"
for f in "$KERNEL_FILE" initrd.img manifest.json; do
	cp "$WORK/out/$f" "$OUT/$f.tmp"
	chmod 0644 "$OUT/$f.tmp"
	mv -f "$OUT/$f.tmp" "$OUT/$f"
done
echo "build-restore-image: $OUT: $KERNEL_FILE $(wc -c <"$OUT/$KERNEL_FILE" | tr -d ' ') bytes, initrd.img $(wc -c <"$OUT/initrd.img" | tr -d ' ') bytes"
