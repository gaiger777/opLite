#!/bin/bash
# build-iso.sh의 인자 검사, netinst 해시 확인, debian.lock 모양, 부트 메뉴 패치(13.7.0 ISO의 실제 파일 견본),
# md5sum.txt·sha256sum.txt 갱신, 가짜 xorriso로 돌린 main(꺼내기·다시 묶기 인자, 실패 때 정리)을 본다.
# xorriso가 없어도 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/iso-test.sh
set -eu

ISO_DIR="$(cd "$(dirname "$0")/../iso" && pwd)"
ISO_FIXTURES="$(cd "$(dirname "$0")/fixtures/iso" && pwd)"

# 함수만 정의한다(ISO는 만들지 않는다). trap보다 먼저 해서 source 실패가 종료 코드로 남게 한다.
# shellcheck source=packaging/iso/build-iso.sh
OPLITE_ISO_LIB=1 . "$ISO_DIR/build-iso.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

printf 'netinst' >"$WORK/netinst.iso"
printf 'deb' >"$WORK/oplite_0.1.0_amd64.deb"
N="$WORK/netinst.iso"
D="$WORK/oplite_0.1.0_amd64.deb"
O="$WORK/out.iso"

# args_exit <이름> <기대 종료 코드> <인자...>: parse_args를 서브셸에서 돌려 종료 코드를 본다.
args_exit() {
	name=$1 want=$2
	shift 2
	code=0
	(parse_args "$@") 2>/dev/null || code=$?
	if [ "$code" -eq "$want" ]; then ok "인자 $name"; else fail "인자 $name: 종료 코드 $code (기대 $want)"; fi
}

args_exit "없음" 2
args_exit "--deb 빠짐" 2 --netinst "$N" --out "$O"
args_exit "--out 빠짐" 2 --netinst "$N" --deb "$D"
args_exit "값 없는 --netinst" 2 --deb "$D" --out "$O" --netinst
args_exit "모르는 인자" 2 --netinst "$N" --deb "$D" --out "$O" --skip-hash
args_exit "없는 netinst 파일" 1 --netinst "$WORK/none.iso" --deb "$D" --out "$O"
args_exit "없는 deb 파일" 1 --netinst "$N" --deb "$WORK/oplite_9.9.9_amd64.deb" --out "$O"
# --deb는 build-deb.sh가 만든 이름(oplite_<version>_amd64.deb)이어야 한다.
printf 'deb' >"$WORK/other.deb"
printf 'deb' >"$WORK/oplite_0.1.0_arm64.deb"
args_exit "deb 이름이 oplite_가 아님" 2 --netinst "$N" --deb "$WORK/other.deb" --out "$O"
args_exit "deb 이름이 amd64가 아님" 2 --netinst "$N" --deb "$WORK/oplite_0.1.0_arm64.deb" --out "$O"
args_exit "맞음" 0 --netinst "$N" --deb "$D" --out "$O"
args_exit "맞음(--unattended)" 0 --unattended --netinst "$N" --deb "$D" --out "$O"

parse_args --netinst "$N" --deb "$D" --out "$O" --unattended
if [ "$NETINST" = "$N" ] && [ "$DEB" = "$D" ] && [ "$OUT" = "$O" ] && [ "$UNATTENDED" = 1 ]; then
	ok "인자 값"
else
	fail "인자 값: $NETINST $DEB $OUT $UNATTENDED"
fi
parse_args --netinst "$N" --deb "$D" --out "$O"
if [ "$UNATTENDED" = 0 ]; then ok "인자 기본 unattended=0"; else fail "인자 기본 unattended=$UNATTENDED"; fi

# 해시: lock과 같으면 통과, 다르거나 lock에 해시가 없으면 실패
hash=$(sha256_file "$N")
printf 'DEBIAN_NETINST_URL=https://example.invalid/x.iso\nDEBIAN_NETINST_SHA256=%s\n' "$hash" >"$WORK/good.lock"
echo 'DEBIAN_NETINST_SHA256=0000000000000000000000000000000000000000000000000000000000000000' >"$WORK/bad.lock"
printf 'DEBIAN_NETINST_URL=https://example.invalid/x.iso\n' >"$WORK/empty.lock"
hash_exit() {
	name=$1 want=$2 lock=$3
	code=0
	(verify_netinst "$N" "$lock") >/dev/null 2>&1 || code=$?
	if [ "$code" -eq "$want" ]; then ok "해시 $name"; else fail "해시 $name: 종료 코드 $code (기대 $want)"; fi
}
hash_exit "같음" 0 "$WORK/good.lock"
hash_exit "다름" 1 "$WORK/bad.lock"
hash_exit "lock에 해시 없음" 1 "$WORK/empty.lock"

# 저장소의 debian.lock: 값이 모두 있고, URL이 그 판의 amd64 netinst를 가리킨다.
LOCK="$ISO_DIR/debian.lock"
release=$(lock_value "$LOCK" DEBIAN_RELEASE)
url=$(lock_value "$LOCK" DEBIAN_NETINST_URL)
sha=$(lock_value "$LOCK" DEBIAN_NETINST_SHA256)
codename=$(lock_value "$LOCK" DEBIAN_CODENAME)
if [ "$codename" = trixie ] &&
	[[ $release =~ ^13\.[0-9]+\.[0-9]+$ ]] &&
	[[ $url == https://cdimage.debian.org/cdimage/*/$release/amd64/iso-cd/debian-$release-amd64-netinst.iso ]] &&
	[[ $sha =~ ^[0-9a-f]{64}$ ]]; then
	ok "debian.lock"
else
	fail "debian.lock: release=$release codename=$codename url=$url sha=$sha"
fi

# 부트 메뉴 패치: 13.7.0 netinst의 isolinux·grub 설정에 패치한 결과가 기대 파일과 같다.
for mode in normal unattended; do
	cp -R "$ISO_FIXTURES/netinst" "$WORK/tree-$mode"
	if [ "$mode" = unattended ]; then a=$UNATTENDED_ARGS u=1; else a=$PRESEED_ARGS u=0; fi
	if (
		patch_grub "$WORK/tree-$mode" "$a" "$u"
		patch_isolinux "$WORK/tree-$mode" "$a" "$u"
	) && diff -ru "$ISO_FIXTURES/$mode" "$WORK/tree-$mode"; then
		ok "메뉴 패치 $mode"
	else
		fail "메뉴 패치 $mode"
	fi
	code=0
	(check_patched "$WORK/tree-$mode" "$u") 2>/dev/null || code=$?
	if [ "$code" -eq 0 ]; then ok "패치 확인 $mode"; else fail "패치 확인 $mode: 종료 코드 $code"; fi
done
# 패치하지 않은 메뉴, timeout을 바꾸지 않은 메뉴는 확인에서 걸린다.
code=0
(check_patched "$ISO_FIXTURES/netinst" 0) 2>/dev/null || code=$?
if [ "$code" -eq 1 ]; then ok "패치 확인: 패치 안 됨"; else fail "패치 확인: 패치 안 됨, 종료 코드 $code"; fi
code=0
(check_patched "$ISO_FIXTURES/normal" 1) 2>/dev/null || code=$?
if [ "$code" -eq 1 ]; then ok "패치 확인: unattended isolinux.cfg·timeout 없음"; else
	fail "패치 확인: unattended isolinux.cfg·timeout 없음, 종료 코드 $code"
fi

# md5sum.txt: 있던 경로는 합을 바꾸고 없던 경로는 끝에 더한다. 다른 줄은 그대로다.
mkdir -p "$WORK/md5/boot/grub" "$WORK/md5/preseed"
echo new-grub >"$WORK/md5/boot/grub/grub.cfg"
echo seed >"$WORK/md5/preseed/oplite.cfg"
printf '%s  ./README.txt\n%s  ./boot/grub/grub.cfg\n%s  ./pool/a.deb\n' \
	11111111111111111111111111111111 22222222222222222222222222222222 33333333333333333333333333333333 \
	>"$WORK/md5/md5sum.txt"
md5_update "$WORK/md5" boot/grub/grub.cfg preseed/oplite.cfg
printf '%s  ./README.txt\n%s  ./boot/grub/grub.cfg\n%s  ./pool/a.deb\n%s  ./preseed/oplite.cfg\n' \
	11111111111111111111111111111111 "$(md5_file "$WORK/md5/boot/grub/grub.cfg")" \
	33333333333333333333333333333333 "$(md5_file "$WORK/md5/preseed/oplite.cfg")" >"$WORK/md5-want.txt"
if diff -u "$WORK/md5-want.txt" "$WORK/md5/md5sum.txt"; then ok "md5sum.txt 갱신"; else fail "md5sum.txt 갱신"; fi

# sha256sum.txt도 같은 방식이다.
printf '%s  ./README.txt\n%s  ./boot/grub/grub.cfg\n' "$(printf '1%.0s' $(seq 64))" "$(printf '2%.0s' $(seq 64))" \
	>"$WORK/md5/sha256sum.txt"
sha256_update "$WORK/md5" boot/grub/grub.cfg preseed/oplite.cfg
printf '%s  ./README.txt\n%s  ./boot/grub/grub.cfg\n%s  ./preseed/oplite.cfg\n' "$(printf '1%.0s' $(seq 64))" \
	"$(sha256_file "$WORK/md5/boot/grub/grub.cfg")" "$(sha256_file "$WORK/md5/preseed/oplite.cfg")" >"$WORK/sha-want.txt"
if diff -u "$WORK/sha-want.txt" "$WORK/md5/sha256sum.txt"; then ok "sha256sum.txt 갱신"; else fail "sha256sum.txt 갱신"; fi

# main을 가짜 xorriso·sha256sum으로 돌린다. 가짜 sha256sum은 netinst에는 저장소 debian.lock의 해시를 낸다.
# 가짜 xorriso는 인자를 기록하고, -extract는 견본 ISO 루트에서 복사하고(없는 파일이면 xorriso처럼 실패),
# -map은 얹는 파일·디렉터리를 $FAKE_ISO_OUT에 복사하고, -outdev는 파일을 만든다.
mkdir -p "$WORK/fakebin" "$WORK/isoroot"
cp -R "$ISO_FIXTURES/netinst/." "$WORK/isoroot/"
printf '%s  ./boot/grub/grub.cfg\n' 22222222222222222222222222222222 >"$WORK/isoroot/md5sum.txt"
cat >"$WORK/fakebin/sha256sum" <<EOF
#!/bin/sh
if [ "\$1" = "$N" ]; then
	echo "$(lock_value "$ISO_DIR/debian.lock" DEBIAN_NETINST_SHA256)  \$1"
else
	exec shasum -a 256 "\$1"
fi
EOF
cat >"$WORK/fakebin/xorriso" <<'EOF'
#!/bin/bash
echo "$*" >>"$FAKE_XORRISO_LOG"
if [ -n "${FAKE_XORRISO_FAIL:-}" ]; then
	echo "xorriso : FAILURE : fake failure" >&2
	exit 5
fi
while [ $# -gt 0 ]; do
	case $1 in
	-extract)
		if [ ! -e "$FAKE_ISO_ROOT$2" ]; then
			echo "xorriso : FAILURE : Cannot determine attributes of (ISO) source file '$2'" >&2
			exit 5
		fi
		cp "$FAKE_ISO_ROOT$2" "$3"
		chmod a-w "$3"
		shift 3
		;;
	-map)
		mkdir -p "$(dirname "$FAKE_ISO_OUT$3")"
		cp -R "$2" "$FAKE_ISO_OUT$3"
		shift 3
		;;
	-outdev)
		echo iso >"$2"
		shift 2
		;;
	*) shift ;;
	esac
done
EOF
chmod 0755 "$WORK/fakebin/sha256sum" "$WORK/fakebin/xorriso"

# run_main <이름> [xorriso 실패 1]: main을 따로 bash로 돌려 종료 코드·stderr·남은 임시 파일을 본다.
run_main() {
	rm -rf "$WORK/tmp" "$WORK/xorriso.log" "$O" "$WORK/isoout"
	mkdir -p "$WORK/tmp"
	code=0
	PATH="$WORK/fakebin:$PATH" TMPDIR="$WORK/tmp" FAKE_XORRISO_LOG="$WORK/xorriso.log" \
		FAKE_ISO_ROOT="$WORK/isoroot" FAKE_ISO_OUT="$WORK/isoout" FAKE_XORRISO_FAIL="${2:-}" \
		bash "$ISO_DIR/build-iso.sh" --netinst "$N" --deb "$D" --out "$O" --unattended \
		>"$WORK/main.out" 2>"$WORK/main.err" || code=$?
	if [ -n "$(ls -A "$WORK/tmp")" ]; then fail "$1: 작업 디렉터리가 남음"; else ok "$1: 작업 디렉터리 지움"; fi
}

run_main "main"
if [ "$code" -eq 0 ] && [ -f "$O" ] && [ "$(tail -n1 "$WORK/main.out")" = "$O" ]; then ok "main 성공"; else
	fail "main: 종료 코드 $code"
	cat "$WORK/main.err"
fi
extract_line=$(sed -n 1p "$WORK/xorriso.log")
repack_line=$(grep -e '-outdev' "$WORK/xorriso.log")
for f in isolinux/txt.cfg isolinux/gtk.cfg isolinux/isolinux.cfg boot/grub/grub.cfg md5sum.txt; do
	if [[ $extract_line == *"-extract /$f "* ]] && [[ $repack_line == *"/iso/$f /$f"* ]]; then ok "main 꺼내기·얹기 $f"; else
		fail "main 꺼내기·얹기 $f"
	fi
done
if [[ $extract_line == "-osirrox on -indev $N "* ]] &&
	[[ $repack_line == "-indev $N -outdev $O "* ]] &&
	[[ $repack_line == *"/iso/oplite /oplite -map "*"/iso/preseed /preseed "* ]] &&
	[[ $repack_line == *" -boot_image any replay "* ]] &&
	[[ $repack_line != *"/iso / "* ]]; then
	ok "main 다시 묶기 인자"
else
	fail "main 다시 묶기 인자: $repack_line"
fi
# netinst에 sha256sum.txt가 없으면 꺼내 보기만 하고 얹지 않는다.
if grep -q -e '-extract /sha256sum.txt ' "$WORK/xorriso.log" && [[ $repack_line != *sha256sum.txt* ]]; then
	ok "main sha256sum.txt 없음"
else
	fail "main sha256sum.txt 없음: $(cat "$WORK/xorriso.log")"
fi
# 얹은 md5sum.txt: grub.cfg와 새 파일의 합
# sums_want <목록> <합 함수>: 견본 목록에 얹은 grub.cfg·새 파일의 합을 반영한 기대 내용
sums_want() {
	local f
	sed '/boot\/grub\/grub.cfg$/d' "$WORK/isoroot/$1"
	echo "$("$2" "$WORK/isoout/boot/grub/grub.cfg")  ./boot/grub/grub.cfg"
	for f in oplite/oplite_0.1.0_amd64.deb preseed/oplite.cfg preseed/oplite-setup.sh preseed/unattended.cfg \
		preseed/oplite-ci-init.service; do
		echo "$("$2" "$WORK/isoout/$f")  ./$f"
	done
}
if diff -u <(sums_want md5sum.txt md5_file | sort) <(sort "$WORK/isoout/md5sum.txt"); then ok "main md5sum.txt"; else
	fail "main md5sum.txt"
fi

# netinst에 sha256sum.txt가 있으면 md5sum.txt처럼 고쳐 얹는다.
printf '%s  ./README.txt\n%s  ./boot/grub/grub.cfg\n' "$(printf '1%.0s' $(seq 64))" "$(printf '2%.0s' $(seq 64))" \
	>"$WORK/isoroot/sha256sum.txt"
run_main "main(sha256sum.txt)"
repack_line=$(grep -e '-outdev' "$WORK/xorriso.log" || true)
if [ "$code" -eq 0 ] && [[ $repack_line == *"/iso/sha256sum.txt /sha256sum.txt"* ]] &&
	diff -u <(sums_want sha256sum.txt sha256_file | sort) <(sort "$WORK/isoout/sha256sum.txt"); then
	ok "main sha256sum.txt"
else
	fail "main sha256sum.txt: 종료 코드 $code, $repack_line"
	cat "$WORK/main.err"
fi
rm -f "$WORK/isoroot/sha256sum.txt"

# xorriso가 실패하면 그 종료 코드로 끝나고, EXIT trap이 set -u에 걸리지 않는다.
run_main "main(xorriso 실패)" 1
if [ "$code" -eq 5 ] && ! grep -q 'unbound variable' "$WORK/main.err"; then ok "main xorriso 실패"; else
	fail "main xorriso 실패: 종료 코드 $code"
	cat "$WORK/main.err"
fi

if [ "$FAILED" -ne 0 ]; then
	echo "iso-test: 실패"
	exit 1
fi
echo "iso-test: 모두 통과"
