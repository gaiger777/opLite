#!/bin/bash
# Debian netinst ISO에 orbivisor 패키지와 preseed를 넣고 "Install Orbivisor"를 기본 부트 항목으로 해 다시 묶는다.
# 쓰는 법: packaging/iso/build-iso.sh --netinst <debian netinst.iso> --deb <orbivisor.deb> --out <orbivisor.iso> [--unattended]
#   --unattended: 시험 전용. 묻는 값을 모두 정한 preseed(unattended.cfg)와 첫 부팅 cluster init 유닛을 넣고,
#                 BIOS는 메뉴 없이(vesamenu 없이 직렬 콘솔로) 바로, UEFI는 메뉴를 1초 뒤에 고르고 직렬 콘솔(ttyS0)을 쓴다.
# netinst는 debian.lock의 SHA-256과 같아야 한다. Linux에서 xorriso가 필요하다.
# OPL_ISO_LIB=1로 source하면 함수만 정의한다(packaging/tests/iso-test.sh).
set -euo pipefail

ISO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PRESEED_ARGS="auto=true priority=high preseed/file=/cdrom/preseed/orbivisor.cfg"
UNATTENDED_ARGS="auto=true priority=critical preseed/file=/cdrom/preseed/unattended.cfg console=ttyS0,115200n8"
# ISO에서 꺼내 고치는 파일. 다시 묶을 때는 원래 ISO 위에 이 파일들과 새 디렉터리(/orbivisor, /preseed)만
# 하나씩 -map으로 얹는다(Debian 위키 RepackBootableISO). 부트 이미지와 boot.cat은 원래 것을 그대로 둔다.
EDIT_FILES=(isolinux/txt.cfg isolinux/gtk.cfg isolinux/isolinux.cfg boot/grub/grub.cfg md5sum.txt)
# 작업 디렉터리. EXIT trap이 main 밖에서도 볼 수 있게 전역이다(set -u).
ISO_WORK=""

usage() {
	echo "usage: $0 --netinst <debian netinst.iso> --deb <orbivisor.deb> --out <orbivisor.iso> [--unattended]" >&2
	exit 2
}

# parse_args <인자...>: NETINST·DEB·OUT·UNATTENDED(0|1)를 정한다. 잘못된 인자는 2, 없는 파일은 1로 끝낸다.
parse_args() {
	NETINST="" DEB="" OUT="" UNATTENDED=0
	while [ $# -gt 0 ]; do
		case $1 in
		--netinst | --deb | --out)
			[ $# -ge 2 ] || usage
			case $1 in
			--netinst) NETINST=$2 ;;
			--deb) DEB=$2 ;;
			--out) OUT=$2 ;;
			esac
			shift 2
			;;
		--unattended)
			UNATTENDED=1
			shift
			;;
		*)
			echo "build-iso: 모르는 인자: $1" >&2
			usage
			;;
		esac
	done
	if [ -z "$NETINST" ] || [ -z "$DEB" ] || [ -z "$OUT" ]; then
		usage
	fi
	# 설치는 /cdrom/orbivisor/orbivisor_*.deb를 찾는다. build-deb.sh가 만든 이름이어야 한다(전환 패키지는 새 설치에 필요 없어 받지 않는다).
	case $(basename "$DEB") in
	orbivisor_*_amd64.deb) ;;
	*)
		echo "build-iso: --deb는 orbivisor_<version>_amd64.deb여야 한다: $DEB" >&2
		exit 2
		;;
	esac
	for f in "$NETINST" "$DEB"; do
		if [ ! -f "$f" ]; then
			echo "build-iso: 파일이 없다: $f" >&2
			exit 1
		fi
	done
}

# lock_value <lock> <KEY>: debian.lock(KEY=value)의 값
lock_value() {
	sed -n "s/^$2=//p" "$1" | head -n1
}

# sha256_file <file>: sha256sum이 없으면(macOS) shasum -a 256
sha256_file() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	else
		shasum -a 256 "$1" | cut -d' ' -f1
	fi
}

# md5_file <file>: md5sum이 없으면(macOS) md5 -q
md5_file() {
	if command -v md5sum >/dev/null 2>&1; then
		md5sum "$1" | cut -d' ' -f1
	else
		md5 -q "$1"
	fi
}

# verify_netinst <iso> <lock>: ISO의 SHA-256이 lock의 DEBIAN_NETINST_SHA256과 다르면 1로 끝낸다.
verify_netinst() {
	local want got
	want=$(lock_value "$2" DEBIAN_NETINST_SHA256)
	if [ -z "$want" ]; then
		echo "build-iso: $2 에 DEBIAN_NETINST_SHA256이 없다" >&2
		exit 1
	fi
	got=$(sha256_file "$1")
	if [ "$got" != "$want" ]; then
		echo "build-iso: $1 의 SHA-256이 debian.lock과 다르다 (기대 $want, 실제 $got)" >&2
		exit 1
	fi
	echo "build-iso: netinst SHA-256 확인 $got"
}

# patch_isolinux <tree> <커널 인자> <unattended 0|1>: BIOS 메뉴(vesamenu) 맨 위에 Orbivisor 항목을 기본으로 넣는다.
# 메뉴의 기본 항목은 `menu default`가 정한다(gtk.cfg의 Graphical install). isolinux.cfg의
# `default vesamenu.c32`를 바꾸면 메뉴가 사라지므로, 그 표시를 Orbivisor 항목으로 옮긴다.
# unattended면 isolinux.cfg를 메뉴 없이 새로 쓴다: vesamenu는 직렬 콘솔에 보이지 않고 화면 없이(-display none)
# 기다릴 수 있다. 직렬 콘솔에 isolinux부터 찍히고 Orbivisor 항목으로 바로 부팅한다.
patch_isolinux() {
	local tree=$1 args=$2 unattended=$3 txt kernel append
	txt="$tree/isolinux/txt.cfg"
	# 첫 label(install)의 kernel과 append를 복사하고 append의 `---` 앞에 preseed 인자를 넣는다.
	kernel=$(awk '$1 == "kernel" { print $2; exit }' "$txt")
	append=$(awk '$1 == "append" { sub(/^[ \t]*append[ \t]+/, ""); sub(/[ \t]+$/, ""); print; exit }' "$txt")
	if [ -z "$kernel" ] || [[ $append != *" ---"* ]]; then
		echo "build-iso: $txt 에서 install 항목의 kernel·append를 찾지 못했다" >&2
		exit 1
	fi
	append="${append%% ---*} $args ---${append#* ---}"
	awk -v kernel="$kernel" -v append="$append" '
	!done && $1 == "label" {
		print "label orbivisor"
		print "\tmenu label ^Install Orbivisor"
		print "\tmenu default"
		print "\tkernel " kernel
		print "\tappend " append
		done = 1
	}
	{ print }
	' "$txt" >"$txt.new"
	mv "$txt.new" "$txt"
	sed '/^[[:space:]]*menu default[[:space:]]*$/d' "$tree/isolinux/gtk.cfg" >"$tree/isolinux/gtk.cfg.new"
	mv "$tree/isolinux/gtk.cfg.new" "$tree/isolinux/gtk.cfg"
	if [ "$unattended" = 1 ]; then
		# serial은 맨 앞에 있어야 한다. timeout 단위는 1/10초다.
		printf 'serial 0 115200\ndefault orbivisor\nprompt 0\ntimeout 1\nlabel orbivisor\n\tkernel %s\n\tappend %s\n' \
			"$kernel" "$append" >"$tree/isolinux/isolinux.cfg"
	fi
}

# patch_grub <tree> <커널 인자> <unattended 0|1>: UEFI 메뉴의 첫 menuentry 앞에 Orbivisor 항목을 넣고 기본으로 한다.
# kernel·initrd·인자는 isolinux txt.cfg의 install 항목(텍스트 설치)에서 가져오므로 patch_isolinux보다 먼저 부른다.
patch_grub() {
	local tree=$1 args=$2 unattended=$3 grub kernel append initrd timeout=""
	grub="$tree/boot/grub/grub.cfg"
	kernel=$(awk '$1 == "kernel" { print $2; exit }' "$tree/isolinux/txt.cfg")
	append=$(awk '$1 == "append" { sub(/^[ \t]*append[ \t]+/, ""); sub(/[ \t]+$/, ""); print; exit }' "$tree/isolinux/txt.cfg")
	initrd=$(sed -n 's/.*initrd=\([^ ]*\).*/\1/p' <<<"$append")
	append=${append/initrd=$initrd /}
	append="${append%% ---*} $args ---${append#* ---}"
	[ "$unattended" = 1 ] && timeout=1
	awk -v kernel="$kernel" -v append="$append" -v initrd="$initrd" -v timeout="$timeout" '
	!done && /^menuentry/ {
		print "set default=0"
		if (timeout != "") print "set timeout=" timeout
		print "menuentry --hotkey=o '\''Install Orbivisor'\'' {"
		print "    set background_color=black"
		print "    linux    " kernel " " append
		print "    initrd   " initrd
		print "}"
		done = 1
	}
	{ print }
	' "$grub" >"$grub.new"
	mv "$grub.new" "$grub"
}

# check_patched <tree> <unattended 0|1>: 메뉴 패치가 들어갔는지 본다. 아니면 1로 끝낸다.
check_patched() {
	local tree=$1 unattended=$2 txt grub isolinux
	txt="$tree/isolinux/txt.cfg" grub="$tree/boot/grub/grub.cfg" isolinux="$tree/isolinux/isolinux.cfg"
	if ! grep -q '^label orbivisor' "$txt" || ! grep -q 'Install Orbivisor' "$grub"; then
		echo "build-iso: 부트 메뉴에 Orbivisor 항목을 넣지 못했다($txt, $grub)" >&2
		exit 1
	fi
	if [ "$unattended" = 1 ] && { [ "$(head -n1 "$isolinux")" != 'serial 0 115200' ] ||
		! grep -qx 'default orbivisor' "$isolinux" || ! grep -qx 'set timeout=1' "$grub"; }; then
		echo "build-iso: unattended 부트 설정(직렬 isolinux.cfg, grub timeout)을 쓰지 못했다($isolinux, $grub)" >&2
		exit 1
	fi
}

# sums_update <tree> <목록 파일> <합 함수> <ISO 안 경로...>: <tree>/<목록 파일>에서 그 경로의 합을 새로 하고,
# 없던 경로는 끝에 더한다. 원래 목록에 없는 isolinux/는 부르지 않는다(원래처럼 빠진다).
sums_update() {
	local tree=$1 list="$1/$2" hash=$3 rel sum
	shift 3
	for rel in "$@"; do
		sum=$("$hash" "$tree/$rel")
		awk -v p="./$rel" -v s="$sum" '
		$2 == p { print s "  " p; found = 1; next }
		{ print }
		END { if (!found) print s "  " p }
		' "$list" >"$list.new"
		mv "$list.new" "$list"
	done
}

# md5_update·sha256_update <tree> <ISO 안 경로...>: 설치 프로그램의 CD 검사가 보는 md5sum.txt·sha256sum.txt
md5_update() {
	sums_update "$1" md5sum.txt md5_file "${@:2}"
}
sha256_update() {
	sums_update "$1" sha256sum.txt sha256_file "${@:2}"
}

cleanup() {
	if [ -n "$ISO_WORK" ]; then
		# 꺼낸 파일은 읽기 전용이라 지우기 전에 쓰기 권한을 준다.
		chmod -R u+w "$ISO_WORK" 2>/dev/null || true
		rm -rf "$ISO_WORK"
	fi
}

main() {
	parse_args "$@"
	if ! command -v xorriso >/dev/null 2>&1; then
		echo "build-iso: xorriso가 없다 (apt-get install xorriso)" >&2
		exit 1
	fi
	verify_netinst "$NETINST" "$ISO_DIR/debian.lock"

	local tree args f new=() extract=() map=()
	trap cleanup EXIT
	ISO_WORK=$(mktemp -d)
	tree="$ISO_WORK/iso"

	# 고칠 파일만 꺼낸다.
	for f in "${EDIT_FILES[@]}"; do
		mkdir -p "$(dirname "$tree/$f")"
		extract+=(-extract "/$f" "$tree/$f")
		map+=(-map "$tree/$f" "/$f")
	done
	xorriso -osirrox on -indev "$NETINST" "${extract[@]}"
	# sha256sum.txt는 판에 따라 없을 수 있다. 꺼내 보고 있으면 md5sum.txt처럼 고친다.
	if xorriso -osirrox on -indev "$NETINST" -extract /sha256sum.txt "$tree/sha256sum.txt" >/dev/null 2>&1 &&
		[ -f "$tree/sha256sum.txt" ]; then
		map+=(-map "$tree/sha256sum.txt" /sha256sum.txt)
	else
		rm -f "$tree/sha256sum.txt"
		echo "build-iso: netinst에 sha256sum.txt가 없다(건너뜀)"
	fi
	chmod -R u+w "$tree"

	mkdir -p "$tree/orbivisor" "$tree/preseed"
	cp "$DEB" "$tree/orbivisor/"
	new=("orbivisor/$(basename "$DEB")" preseed/orbivisor.cfg preseed/orbivisor-setup.sh)
	cp "$ISO_DIR/preseed/orbivisor.cfg" "$ISO_DIR/preseed/orbivisor-setup.sh" "$tree/preseed/"
	args=$PRESEED_ARGS
	if [ "$UNATTENDED" = 1 ]; then
		cp "$ISO_DIR/preseed/unattended.cfg" "$ISO_DIR/preseed/orbivisor-ci-init.service" "$tree/preseed/"
		new+=(preseed/unattended.cfg preseed/orbivisor-ci-init.service)
		args=$UNATTENDED_ARGS
	fi

	# patch_grub이 원래 install 항목을 읽으므로 먼저 한다.
	patch_grub "$tree" "$args" "$UNATTENDED"
	patch_isolinux "$tree" "$args" "$UNATTENDED"
	check_patched "$tree" "$UNATTENDED"

	# 설치 프로그램의 CD 검사가 보는 md5sum.txt(와 sha256sum.txt): 고친 grub.cfg와 새 파일의 합(isolinux/는 원래처럼 없다)
	md5_update "$tree" boot/grub/grub.cfg "${new[@]}"
	if [ -f "$tree/sha256sum.txt" ]; then
		sha256_update "$tree" boot/grub/grub.cfg "${new[@]}"
	fi

	rm -f "$OUT"
	xorriso -indev "$NETINST" -outdev "$OUT" -overwrite on \
		-map "$tree/orbivisor" /orbivisor -map "$tree/preseed" /preseed "${map[@]}" \
		-boot_image any replay -volid ORBIVISOR
	echo "$OUT"
}

if [ "${OPL_ISO_LIB:-}" = 1 ]; then
	return 0
fi
main "$@"
