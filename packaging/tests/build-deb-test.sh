#!/bin/bash
# build-deb.sh가 orbivisor 패키지(옛 이름 링크·유닛 별칭)와 빈 전환 패키지 oplite를 만드는지, 바이너리의
# --version이 패키지 버전과 같은지 확인하는지, OPL_APT_KEY의 키링을 싣는지, OPL_LICENSE_KEYS 없이는
# OPL_DEV_LICENSE=1일 때만 만드는지, OPL_LICENSE_KEYS의 항목·개발 키·orbivisord에 든 값을 확인하는지 본다.
# 가짜 orbivisord·orbivisorctl(버전을 찍는 스크립트)과 가짜 dpkg-deb(만들 파일과 패키지 트리 사본만 만듦)를 쓴다.
# bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/build-deb-test.sh
set -eu

# 사용자 셸의 값에 결과가 흔들리지 않게 한다(아래 run이 OPL_DEV_LICENSE 기본 1을 준다).
unset OPL_DEV_LICENSE OPL_LICENSE_KEYS

DEB_DIR="$(cd "$(dirname "$0")/../deb" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/fakebin"
# 가짜 dpkg-deb: 마지막 인자(만들 .deb)를 만들고, 그 앞 인자(패키지 트리)를 $WORK/tree-<패키지 이름>으로
# 복사한다(링크는 링크로).
cat >"$WORK/fakebin/dpkg-deb" <<'EOF'
#!/bin/sh
for a; do
	root=${out-}
	out=$a
done
echo deb >"$out"
tree="$(dirname "$out")/../tree-$(basename "$out" | cut -d_ -f1)"
rm -rf "$tree"
cp -Rp "$root" "$tree"
EOF
chmod 0755 "$WORK/fakebin/dpkg-deb"

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

# fake_bins <orbivisord 버전> <orbivisorctl 버전> [orbivisord에 넣을 문자열]: <bin> --version이
# "<bin> <버전>"을 찍는 가짜 바이너리. 세 번째 인자는 OPL_LICENSE_KEYS로 빌드한 orbivisord처럼 바이너리 안에 둔다.
fake_bins() {
	rm -rf "${WORK:?}/bin"
	mkdir -p "$WORK/bin"
	printf '#!/bin/sh\necho "orbivisord %s"\n# %s\n' "$1" "${3-}" >"$WORK/bin/orbivisord"
	printf '#!/bin/sh\necho "orbivisorctl %s"\n' "$2" >"$WORK/bin/orbivisorctl"
	chmod 0755 "$WORK/bin/orbivisord" "$WORK/bin/orbivisorctl"
}

# run <버전>: build-deb.sh를 돌려 종료 코드(code)·stdout·stderr를 남긴다.
# OPL_DEV_LICENSE는 따로 주지 않으면 1(개발 패키지)이다.
run() {
	rm -rf "$WORK/out"
	code=0
	PATH="$WORK/fakebin:$PATH" OPL_BIN_DIR="$WORK/bin" OPL_DEV_LICENSE="${OPL_DEV_LICENSE-1}" \
		sh "$DEB_DIR/build-deb.sh" "$1" "$WORK/out" >"$WORK/stdout" 2>"$WORK/stderr" || code=$?
}

fake_bins 0.1.0~ci7 0.1.0~ci7
run 0.1.0~ci7
if [ "$code" -eq 0 ] && [ "$(cat "$WORK/stdout")" = "$WORK/out/orbivisor_0.1.0~ci7_amd64.deb
$WORK/out/oplite_0.1.0~ci7_amd64.deb" ] &&
	[ -f "$WORK/out/orbivisor_0.1.0~ci7_amd64.deb" ] && [ -f "$WORK/out/oplite_0.1.0~ci7_amd64.deb" ]; then
	ok "버전이 같으면 두 패키지(orbivisor, 전환 oplite)를 만든다"
else
	fail "버전이 같은데 종료 코드 $code"
	cat "$WORK/stdout" "$WORK/stderr"
fi

# orbivisor: 실제 바이너리와 옛 이름의 상대 링크, 유닛과 별칭 링크, /usr/share/orbivisor
T="$WORK/tree-orbivisor"
# is_link <경로> <대상>: 대상이 그 값인 심볼릭 링크
is_link() { [ -L "$1" ] && [ "$(readlink "$1")" = "$2" ]; }
if [ -f "$T/usr/bin/orbivisord" ] && [ ! -L "$T/usr/bin/orbivisord" ] &&
	[ -f "$T/usr/bin/orbivisorctl" ] && [ ! -L "$T/usr/bin/orbivisorctl" ] &&
	is_link "$T/usr/bin/opld" orbivisord && is_link "$T/usr/bin/oplctl" orbivisorctl; then
	ok "orbivisord·orbivisorctl과 링크 opld·oplctl"
else
	fail "바이너리·링크: $(ls -l "$T/usr/bin" 2>&1)"
fi
UNIT_DIR="$T/usr/lib/systemd/system"
if [ -f "$UNIT_DIR/opld.service" ] && [ ! -L "$UNIT_DIR/opld.service" ] &&
	is_link "$UNIT_DIR/orbivisord.service" opld.service &&
	grep -qx 'ExecStart=/usr/bin/orbivisord --config /etc/opl/opld.toml' "$UNIT_DIR/opld.service"; then
	ok "유닛 opld.service와 별칭 링크 orbivisord.service"
else
	fail "유닛: $(ls -l "$UNIT_DIR" 2>&1)"
fi
# systemd 알림(설계 §3.3): 시작이 끝나면 READY=1, core·api가 답하는 동안 WATCHDOG=1(없으면 SIGKILL). 시작 제한은 기본(90초).
if grep -qx 'Type=notify' "$UNIT_DIR/opld.service" && grep -qx 'NotifyAccess=main' "$UNIT_DIR/opld.service" &&
	grep -qx 'WatchdogSec=30s' "$UNIT_DIR/opld.service" && grep -qx 'WatchdogSignal=SIGKILL' "$UNIT_DIR/opld.service" &&
	grep -qx 'Restart=on-failure' "$UNIT_DIR/opld.service" &&
	grep -qx 'KillMode=process' "$UNIT_DIR/opld.service" && ! grep -q '^TimeoutStartSec=' "$UNIT_DIR/opld.service"; then
	ok "유닛은 Type=notify·WatchdogSec=30s·WatchdogSignal=SIGKILL·NotifyAccess=main"
else
	fail "유닛 systemd 알림: $(cat "$UNIT_DIR/opld.service")"
fi
# 내장 VIP(계획 46): 죽은 프로세스가 남긴 VIP를 멈춘 뒤 뗀다(SIGKILL 뒤에도 돈다).
if grep -qx 'ExecStopPost=/usr/bin/orbivisord --config /etc/opl/opld.toml --release-vips' "$UNIT_DIR/opld.service"; then
	ok "유닛은 ExecStopPost로 남은 VIP를 뗀다"
else
	fail "유닛 ExecStopPost: $(grep ExecStop "$UNIT_DIR/opld.service")"
fi
if [ -f "$T/usr/share/orbivisor/opld.toml.example" ] && [ -f "$T/usr/share/doc/orbivisor/copyright" ] &&
	[ ! -e "$T/usr/share/oplite" ] && [ ! -e "$T/usr/share/doc/oplite" ]; then
	ok "공유 파일은 /usr/share/orbivisor"
else
	fail "공유 파일: $(find "$T/usr/share" 2>&1)"
fi
# md5sums에는 파일만(링크는 dpkg --verify가 보지 않는다)
if [ "$(awk '{ print $2 }' "$T/DEBIAN/md5sums")" = "usr/bin/orbivisorctl
usr/bin/orbivisord
usr/lib/systemd/system/opld.service
usr/share/doc/orbivisor/copyright
usr/share/orbivisor/opld.toml.example" ]; then
	ok "md5sums에 링크가 없다"
else
	fail "md5sums: $(cat "$T/DEBIAN/md5sums")"
fi
if grep -qx 'Package: orbivisor' "$T/DEBIAN/control" && grep -qx 'Version: 0.1.0~ci7' "$T/DEBIAN/control" &&
	grep -qx 'Breaks: oplite (<< 0.1.0~ci7)' "$T/DEBIAN/control" &&
	grep -qx 'Replaces: oplite (<< 0.1.0~ci7)' "$T/DEBIAN/control" &&
	! grep -q '^Provides:' "$T/DEBIAN/control" &&
	[ -x "$T/DEBIAN/postinst" ] && [ -x "$T/DEBIAN/prerm" ] && [ -x "$T/DEBIAN/postrm" ]; then
	ok "orbivisor control(Breaks·Replaces oplite)과 유지 스크립트"
else
	fail "orbivisor control: $(cat "$T/DEBIAN/control")"
fi
# 전환 패키지 oplite: copyright만, 유지 스크립트 없음, orbivisor 같은 버전에 의존
O="$WORK/tree-oplite"
if [ "$(cd "$O" && find usr ! -type d)" = usr/share/doc/oplite/copyright ] &&
	[ "$(ls "$O/DEBIAN")" = "control
md5sums" ] && grep -qx 'Package: oplite' "$O/DEBIAN/control" &&
	grep -qx 'Version: 0.1.0~ci7' "$O/DEBIAN/control" &&
	grep -qx 'Depends: orbivisor (= 0.1.0~ci7)' "$O/DEBIAN/control" &&
	grep -qx 'Section: oldlibs' "$O/DEBIAN/control" &&
	grep -qx 'Architecture: amd64' "$O/DEBIAN/control" &&
	grep -q ' usr/share/doc/oplite/copyright$' "$O/DEBIAN/md5sums"; then
	ok "전환 패키지 oplite"
else
	fail "전환 패키지: $(find "$O" | sort) $(cat "$O/DEBIAN/control")"
fi

# check_mismatch <이름>: 종료 코드 1, OPL_VERSION 안내, 패키지 없음
check_mismatch() {
	if [ "$code" -eq 1 ] && grep -q 'OPL_VERSION=0.2.0' "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
		ok "$1"
	else
		fail "$1: 종료 코드 $code"
		cat "$WORK/stderr"
	fi
}

fake_bins 0.1.0 0.2.0
run 0.2.0
check_mismatch "orbivisord 버전이 다르면 실패"
fake_bins 0.2.0 0.1.0
run 0.2.0
check_mismatch "orbivisorctl 버전이 다르면 실패"
# OPL_VERSION 없이 빌드한 바이너리는 크레이트 버전을 보인다. 앞부분만 같아도 안 된다.
fake_bins 0.2.0~ci1 0.2.0~ci1
run 0.2.0
check_mismatch "버전 앞부분만 같으면 실패"

# OPL_APT_KEY: 저장소 공개 키를 orbivisor의 /usr/share/keyrings/oplite-archive-keyring.gpg(0644, 옛 경로 그대로)로
# 싣고 md5sums에 넣는다. 전환 패키지에는 싣지 않는다.
KEYRING=usr/share/keyrings/oplite-archive-keyring.gpg
fake_bins 0.1.0 0.1.0
run 0.1.0
if [ "$code" -eq 0 ] && [ ! -e "$WORK/tree-orbivisor/usr/share/keyrings" ]; then
	ok "OPL_APT_KEY가 없으면 키링을 싣지 않는다"
else
	fail "키링 없음: 종료 코드 $code"
fi
printf 'public key' >"$WORK/archive.gpg"
chmod 0600 "$WORK/archive.gpg"
OPL_APT_KEY="$WORK/archive.gpg" run 0.1.0
if [ "$code" -eq 0 ] && cmp -s "$WORK/archive.gpg" "$WORK/tree-orbivisor/$KEYRING" &&
	[ -n "$(find "$WORK/tree-orbivisor/$KEYRING" -perm 0644)" ] &&
	grep -q " $KEYRING\$" "$WORK/tree-orbivisor/DEBIAN/md5sums" &&
	[ ! -e "$WORK/tree-oplite/usr/share/keyrings" ]; then
	ok "OPL_APT_KEY의 키링을 싣는다"
else
	fail "키링: 종료 코드 $code"
	cat "$WORK/stderr"
fi
OPL_APT_KEY="$WORK/missing.gpg" run 0.1.0
if [ "$code" -eq 1 ] && grep -q OPL_APT_KEY "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
	ok "OPL_APT_KEY 파일이 없으면 실패"
else
	fail "없는 OPL_APT_KEY: 종료 코드 $code"
fi
printf -- '-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmQINBG\n-----END PGP PUBLIC KEY BLOCK-----\n' >"$WORK/archive.asc"
OPL_APT_KEY="$WORK/archive.asc" run 0.1.0
if [ "$code" -eq 1 ] && grep -q 'OPL_APT_KEY must be a binary keyring (gpg --export, not --armor)' "$WORK/stderr" &&
	[ ! -e "$WORK/out" ]; then
	ok "armor 키는 거절"
else
	fail "armor OPL_APT_KEY: 종료 코드 $code"
fi

# 구독 키 공개 키: OPL_LICENSE_KEYS도 OPL_DEV_LICENSE=1도 없으면 만들지 않는다.
fake_bins 0.1.0 0.1.0
OPL_LICENSE_KEYS='' OPL_DEV_LICENSE='' run 0.1.0
if [ "$code" -eq 1 ] && grep -q 'build-deb: OPL_LICENSE_KEYS is not set; set it for a release build or OPL_DEV_LICENSE=1 for a development package' "$WORK/stderr" &&
	[ ! -e "$WORK/out" ]; then
	ok "OPL_LICENSE_KEYS 없이 개발 패키지가 아니면 실패"
else
	fail "OPL_LICENSE_KEYS 없음: 종료 코드 $code"
fi
OPL_LICENSE_KEYS='' OPL_DEV_LICENSE=0 run 0.1.0
if [ "$code" -eq 1 ] && [ ! -e "$WORK/out" ]; then
	ok "OPL_DEV_LICENSE가 1이 아니면 실패"
else
	fail "OPL_DEV_LICENSE=0: 종료 코드 $code"
fi
# 정식 키: opl-license keygen과 같은 형식(base64 32바이트)의 개발 키가 아닌 키 둘
RELEASE_KEYS='Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyM=,mR8vT2wX4yZ6aB8cD0eF2gH4iJ6kL8mN0oP2qR4sT6U='
DEV_KEY=$(cat "$DEB_DIR/../../crates/opl-types/keys/dev-license.pub")
fake_bins 0.1.0 0.1.0 "$RELEASE_KEYS"
OPL_LICENSE_KEYS=$RELEASE_KEYS OPL_DEV_LICENSE='' run 0.1.0
if [ "$code" -eq 0 ] && [ -f "$WORK/out/orbivisor_0.1.0_amd64.deb" ] && [ -f "$WORK/out/oplite_0.1.0_amd64.deb" ]; then
	ok "OPL_LICENSE_KEYS가 있으면 만든다"
else
	fail "OPL_LICENSE_KEYS 있음: 종료 코드 $code"
	cat "$WORK/stderr"
fi
# check_key_error <이름> <문구>: 종료 코드 1, 그 문구, 패키지 없음
check_key_error() {
	if [ "$code" -eq 1 ] && grep -qF -- "$2" "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
		ok "$1"
	else
		fail "$1: 종료 코드 $code"
		cat "$WORK/stderr"
	fi
}
fake_bins 0.1.0 0.1.0 "$DEV_KEY"
OPL_LICENSE_KEYS=$DEV_KEY OPL_DEV_LICENSE='' run 0.1.0
check_key_error "OPL_LICENSE_KEYS의 개발 키는 거절" \
	'build-deb: OPL_LICENSE_KEYS contains the development key; use OPL_DEV_LICENSE=1 for a development package'
fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*},$DEV_KEY"
OPL_LICENSE_KEYS="${RELEASE_KEYS%%,*},$DEV_KEY" OPL_DEV_LICENSE='' run 0.1.0
check_key_error "목록에 섞인 개발 키도 거절" 'OPL_LICENSE_KEYS contains the development key'
# 패딩 없는 키, 끝 비트가 0이 아닌(정규형이 아닌) 키도 거절한다.
for bad in 'not-base64!' 'AAAA' 'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyMA' \
	'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyM' 'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyN='; do
	fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*},$bad"
	OPL_LICENSE_KEYS="${RELEASE_KEYS%%,*},$bad" OPL_DEV_LICENSE='' run 0.1.0
	check_key_error "32바이트 base64가 아닌 항목 거절: $bad" \
		"build-deb: OPL_LICENSE_KEYS entry is not a base64 Ed25519 public key: $bad"
done
# 공백·쉼표뿐이면 키가 하나도 없다.
for none in ' ' ','; do
	fake_bins 0.1.0 0.1.0 "$none"
	OPL_LICENSE_KEYS=$none OPL_DEV_LICENSE='' run 0.1.0
	check_key_error "키가 없는 OPL_LICENSE_KEYS 거절: '$none'" 'build-deb: OPL_LICENSE_KEYS has no keys'
done
fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*}"
OPL_LICENSE_KEYS=$RELEASE_KEYS OPL_DEV_LICENSE='' run 0.1.0
check_key_error "orbivisord에 OPL_LICENSE_KEYS가 그대로 없으면 거절" \
	'build-deb: orbivisord was not built with this OPL_LICENSE_KEYS'

if [ "$FAILED" -ne 0 ]; then
	echo "build-deb-test: 실패"
	exit 1
fi
echo "build-deb-test: 모두 통과"
