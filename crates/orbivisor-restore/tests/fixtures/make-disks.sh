#!/bin/bash
# 파일 복원 시험용 고정물 디스크(계획 69 설계 §14)를 만든다: 같은 파일 나무를 여러 파일시스템·파티션 표·LVM에 담은
# 날(raw) 이미지들. 실제 도우미 VM 시험(restore-it, 스테이지 4·6)이 붙여 나열·내려받은 내용을 expected.sha256과 견준다.
#
# 쓰는 법: crates/orbivisor-restore/tests/fixtures/make-disks.sh <out dir>
#   <out>/src/            원본 나무(hello.txt, dir/nested/deep.txt, 한글-이름.txt, big.bin 3 MiB, empty/,
#                         link → hello.txt 심볼릭 링크, hard.txt = hello.txt 하드 링크)
#   <out>/expected.sha256 원본 나무의 일반 파일 해시(`sha256sum` 꼴, 상대 경로)
#   <out>/disks.txt       만든 이미지와 건너뛴 것(까닭)
#   이미지(도구가 있을 때만 — 없으면 disks.txt에 건너뜀):
#     ext4.img        ext4, 레이블 opl-ext4, 나무 전체(mke2fs -d)
#     ext4-dirty.img  ext4.img와 같되 저널에 커밋된 트랜잭션 하나가 재생되지 않은 채(debugfs jo/jw/jc) — 재생하면
#                     hello.txt의 첫 블록이 "journal replayed\n"으로 시작하고, 재생하지 않으면(noload) 원래 내용
#     xfs.img         XFS v5, 레이블 opl-xfs, 나무 전체(mkfs.xfs -p 프로토 파일 — 300 MiB, 성긴 파일)
#     btrfs.img       Btrfs, 레이블 opl-btrfs, 나무 전체(--rootdir), mkfs.btrfs가 --subvol을 알면 dir을 하위 볼륨으로
#     vfat.img        FAT32, 레이블 OPLVFAT, 링크 빼고(mcopy)
#     exfat.img       exFAT, 레이블 OPLEXFAT, 빈 파일시스템(사용자 공간에서 파일을 넣을 도구가 없다)
#     ntfs.img        NTFS, 레이블 opl-ntfs, 맨 위 일반 파일만(ntfscp는 디렉터리를 만들지 못한다)
#     mbr.img         MBR: 1 주 파티션 ext4(=ext4.img), 2 확장, 5 논리 vfat(=vfat.img)
#     gpt.img         GPT: 1 "Linux filesystem" ext4(=ext4.img), 2 "EFI System" vfat(=vfat.img)
#     luks.img        LUKS2 머리(cryptsetup — "지원 안 함"을 보이는지)
#     lvm.img         root·Linux만(losetup): VG oplfixvg — 선형 LV lin(ext4, 나무 전체), 씬 풀 pool의 씬 LV thin(ext4)
# 루트 없이 돈다(lvm.img만 root). Debian·Ubuntu: apt install e2fsprogs xfsprogs btrfs-progs dosfstools mtools
#   exfatprogs ntfs-3g fdisk cryptsetup-bin lvm2 thin-provisioning-tools. macOS(Homebrew e2fsprogs·dosfstools·mtools)는
#   있는 것만. 이미지는 시각·UUID가 매번 달라 바이트가 같지 않다 — 견줄 것은 expected.sha256의 내용이다.
set -euo pipefail
shopt -s nullglob

[ $# -eq 1 ] || {
	echo "usage: $0 <out dir>" >&2
	exit 2
}
OUT=$1
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
# Homebrew의 e2fsprogs는 keg-only다.
for d in /opt/homebrew/opt/e2fsprogs/sbin /usr/local/opt/e2fsprogs/sbin /usr/sbin /sbin; do
	[ -d "$d" ] && PATH="$PATH:$d"
done
export LC_ALL=C.UTF-8 2>/dev/null || true

REPORT="$OUT/disks.txt"
: >"$REPORT"
made() { echo "made $1" | tee -a "$REPORT"; }
skipped() { echo "skipped $1: $2" | tee -a "$REPORT"; }
have() { command -v "$1" >/dev/null 2>&1; }

sha256() {
	if have sha256sum; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

# 원본 나무
SRC="$OUT/src"
rm -rf "$SRC"
mkdir -p "$SRC/dir/nested" "$SRC/empty"
printf 'hello from orbivisor\n' >"$SRC/hello.txt"
printf 'deep file\n' >"$SRC/dir/nested/deep.txt"
printf 'utf-8 name\n' >"$SRC/한글-이름.txt"
# 3 MiB, 자리마다 다른 내용(어디서나 같은 바이트 — 블록 차례가 어긋나면 해시가 달라진다)
{ seq -w 1 999999 || true; } | head -c 3145728 >"$SRC/big.bin"
ln -s hello.txt "$SRC/link"
ln "$SRC/hello.txt" "$SRC/hard.txt"
(cd "$SRC" && find . -type f | LC_ALL=C sort | sed 's|^\./||' | while read -r f; do sha256 "$f"; done) >"$OUT/expected.sha256"

# 링크 없는 일반 파일·디렉터리(FAT·NTFS용): 맨 위 이름
PLAIN=(hello.txt 한글-이름.txt big.bin dir empty)

# img <이름> <크기>: 성긴 빈 파일
img() {
	rm -f "$OUT/$1"
	if ! truncate -s "$2" "$OUT/$1" 2>/dev/null; then
		dd if=/dev/zero of="$OUT/$1" bs=1 count=0 seek="$2" 2>/dev/null
	fi
}

# ext4
if have mke2fs; then
	rm -f "$OUT/ext4.img"
	mke2fs -q -F -t ext4 -L opl-ext4 -E root_owner=0:0 -d "$SRC" "$OUT/ext4.img" 64M
	made ext4.img
else
	skipped ext4.img "mke2fs not found"
fi

# 더러운 저널의 ext4: hello.txt 첫 블록을 새 내용으로 쓰는 트랜잭션을 저널에 커밋만 하고 재생하지 않는다.
if [ -f "$OUT/ext4.img" ] && have debugfs && debugfs -R help 2>&1 | grep -q journal_write; then
	cp "$OUT/ext4.img" "$OUT/ext4-dirty.img"
	bs=$(debugfs -R stats "$OUT/ext4-dirty.img" 2>/dev/null | sed -n 's/^Block size: *//p')
	blk=$(debugfs -R "bmap /hello.txt 0" "$OUT/ext4-dirty.img" 2>/dev/null)
	block="$OUT/.journal-block"
	{ printf 'journal replayed\n' && head -c $((bs - 17)) /dev/zero; } >"$block"
	debugfs -w -f - "$OUT/ext4-dirty.img" >/dev/null 2>&1 <<-EOF
		jo
		jw -b $blk $block
		jc
	EOF
	rm -f "$block"
	if debugfs -R stats "$OUT/ext4-dirty.img" 2>/dev/null | grep -q needs_recovery; then
		made ext4-dirty.img
	else
		rm -f "$OUT/ext4-dirty.img"
		skipped ext4-dirty.img "debugfs did not leave a journal to replay"
	fi
else
	skipped ext4-dirty.img "debugfs with journal commands not found"
fi

# XFS: 프로토 파일(이름에 공백이 없어야 한다)
proto_dir() {
	local dir=$1 indent=$2 name
	for path in "$dir"/*; do
		name=${path##*/}
		if [ -L "$path" ]; then
			echo "$indent$name l--777 0 0 $(readlink "$path")"
		elif [ -d "$path" ]; then
			echo "$indent$name d--755 0 0"
			proto_dir "$path" "$indent "
			echo "$indent\$"
		else
			echo "$indent$name ---644 0 0 $path"
		fi
	done
}
if have mkfs.xfs; then
	{
		echo /dev/null
		echo 0 0
		echo "d--755 0 0"
		proto_dir "$SRC" " "
		echo "\$"
	} >"$OUT/.xfs-proto"
	img xfs.img 300M
	mkfs.xfs -q -f -L opl-xfs -p "$OUT/.xfs-proto" "$OUT/xfs.img"
	rm -f "$OUT/.xfs-proto"
	made xfs.img
else
	skipped xfs.img "mkfs.xfs not found"
fi

# Btrfs
if have mkfs.btrfs; then
	img btrfs.img 128M
	subvol=()
	if mkfs.btrfs --help 2>&1 | grep -q -- --subvol; then subvol=(--subvol dir); fi
	mkfs.btrfs -q -f -L opl-btrfs --rootdir "$SRC" ${subvol[@]+"${subvol[@]}"} "$OUT/btrfs.img" >/dev/null
	made "btrfs.img${subvol:+ (subvolume dir)}"
else
	skipped btrfs.img "mkfs.btrfs not found"
fi

# FAT32
if have mkfs.vfat && have mcopy; then
	img vfat.img 40M
	mkfs.vfat -F 32 -n OPLVFAT "$OUT/vfat.img" >/dev/null
	(cd "$SRC" && mcopy -s -i "$OUT/vfat.img" "${PLAIN[@]}" ::)
	made vfat.img
else
	skipped vfat.img "mkfs.vfat or mcopy not found"
fi

# exFAT(빈)
if have mkfs.exfat; then
	img exfat.img 16M
	mkfs.exfat -L OPLEXFAT "$OUT/exfat.img" >/dev/null
	made exfat.img
else
	skipped exfat.img "mkfs.exfat not found"
fi

# NTFS
if have mkntfs && have ntfscp; then
	img ntfs.img 16M
	mkntfs -q -F -Q -L opl-ntfs "$OUT/ntfs.img" >/dev/null 2>&1
	for f in hello.txt 한글-이름.txt big.bin; do
		ntfscp -q "$OUT/ntfs.img" "$SRC/$f" "$f" >/dev/null 2>&1
	done
	made ntfs.img
else
	skipped ntfs.img "mkntfs or ntfscp not found"
fi

# part_copy <디스크> <파티션 번호> <이미지>: sfdisk가 정한 시작 섹터에 이미지를 쓴다.
part_copy() {
	start=$(sfdisk -d "$1" | sed -n "s|^.*$2 : start= *\([0-9]*\),.*|\1|p")
	dd if="$3" of="$1" bs=512 seek="$start" conv=notrunc 2>/dev/null
}

if have sfdisk && [ -f "$OUT/ext4.img" ] && [ -f "$OUT/vfat.img" ]; then
	img mbr.img 120M
	sfdisk -q "$OUT/mbr.img" >/dev/null <<-EOF
		label: dos
		size=64MiB, type=83
		type=05
		size=41MiB, type=0c
	EOF
	part_copy "$OUT/mbr.img" 1 "$OUT/ext4.img"
	part_copy "$OUT/mbr.img" 5 "$OUT/vfat.img"
	made mbr.img

	img gpt.img 120M
	sfdisk -q "$OUT/gpt.img" >/dev/null <<-EOF
		label: gpt
		size=64MiB, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name=root
		size=41MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name=esp
	EOF
	part_copy "$OUT/gpt.img" 1 "$OUT/ext4.img"
	part_copy "$OUT/gpt.img" 2 "$OUT/vfat.img"
	made gpt.img
else
	skipped "mbr.img gpt.img" "sfdisk, ext4.img or vfat.img missing"
fi

# LUKS2 머리
if have cryptsetup; then
	img luks.img 32M
	if printf 'orbivisor-fixture' | cryptsetup luksFormat -q --type luks2 --pbkdf pbkdf2 \
		--pbkdf-force-iterations 1000 --key-file - "$OUT/luks.img" >/dev/null 2>&1; then
		made luks.img
	else
		rm -f "$OUT/luks.img"
		skipped luks.img "cryptsetup luksFormat failed"
	fi
else
	skipped luks.img "cryptsetup not found"
fi

# LVM: root·Linux만(루프 장치). 씬 LV는 호스트에 dm-thin-pool·thin_check가 있을 때.
if [ "$(uname)" = Linux ] && [ "$(id -u)" = 0 ] && have losetup && have lvm && have mkfs.ext4; then
	VG=oplfixvg
	if lvm vgs "$VG" >/dev/null 2>&1; then
		echo "make-disks: VG $VG already exists on this host" >&2
		exit 1
	fi
	img lvm.img 256M
	LOOP=$(losetup -f --show "$OUT/lvm.img")
	cleanup_lvm() {
		lvm vgchange -an "$VG" >/dev/null 2>&1 || true
		losetup -d "$LOOP" 2>/dev/null || true
	}
	trap cleanup_lvm EXIT
	lvm pvcreate -q "$LOOP" >/dev/null
	lvm vgcreate -q "$VG" "$LOOP" >/dev/null
	lvm lvcreate -q -y -L 64M -n lin "$VG" >/dev/null
	mkfs.ext4 -q -F -L opl-lvm-lin -d "$SRC" "/dev/$VG/lin"
	note=""
	if lvm lvcreate -q -y -L 96M -T "$VG/pool" >/dev/null 2>&1 &&
		lvm lvcreate -q -y -V 64M -T "$VG/pool" -n thin >/dev/null 2>&1; then
		mkfs.ext4 -q -F -L opl-lvm-thin -d "$SRC" "/dev/$VG/thin"
	else
		note=" (no thin LV: lvcreate -T failed)"
	fi
	cleanup_lvm
	trap - EXIT
	made "lvm.img$note"
else
	skipped lvm.img "needs root on Linux with losetup, lvm and mkfs.ext4"
fi
