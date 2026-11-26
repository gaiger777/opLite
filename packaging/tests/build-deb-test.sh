#!/bin/bash
# build-deb.sh가 orbivisor 패키지(옛 이름 링크·유닛 별칭)와 빈 전환 패키지 oplite를 만드는지, 바이너리의
# --version이 패키지 버전과 같은지 확인하는지, ORBIVISOR_APT_KEY의 키링을 싣는지, ORBIVISOR_LICENSE_KEYS 없이는
# ORBIVISOR_DEV_LICENSE=1일 때만 만드는지, ORBIVISOR_LICENSE_KEYS의 항목·개발 키·orbivisord에 든 값을 확인하는지 본다.
# 가짜 orbivisord·orbivisorctl(버전을 찍는 스크립트)과 가짜 dpkg-deb(만들 파일과 패키지 트리 사본만 만듦)를 쓴다.
# bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/build-deb-test.sh
set -eu

# 사용자 셸의 값에 결과가 흔들리지 않게 한다(아래 run이 ORBIVISOR_DEV_LICENSE 기본 1을 준다).
unset ORBIVISOR_DEV_LICENSE ORBIVISOR_LICENSE_KEYS OPL_LICENSE_KEYS OPL_APT_KEY OPL_BIN_DIR

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
# "<bin> <버전>"을 찍는 가짜 바이너리. 세 번째 인자는 ORBIVISOR_LICENSE_KEYS로 빌드한 orbivisord처럼 바이너리 안에 둔다.
fake_bins() {
	rm -rf "${WORK:?}/bin"
	mkdir -p "$WORK/bin"
	printf '#!/bin/sh\necho "orbivisord %s"\n# %s\n' "$1" "${3-}" >"$WORK/bin/orbivisord"
	printf '#!/bin/sh\necho "orbivisorctl %s"\n' "$2" >"$WORK/bin/orbivisorctl"
	chmod 0755 "$WORK/bin/orbivisord" "$WORK/bin/orbivisorctl"
}

# run <버전>: build-deb.sh를 돌려 종료 코드(code)·stdout·stderr를 남긴다.
# ORBIVISOR_DEV_LICENSE는 따로 주지 않으면 1(개발 패키지)이다.
run() {
	rm -rf "$WORK/out"
	code=0
	PATH="$WORK/fakebin:$PATH" ORBIVISOR_BIN_DIR="$WORK/bin" ORBIVISOR_DEV_LICENSE="${ORBIVISOR_DEV_LICENSE-1}" \
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
if [ -f "$UNIT_DIR/orbivisord.service" ] && [ ! -L "$UNIT_DIR/orbivisord.service" ] &&
	is_link "$UNIT_DIR/opld.service" orbivisord.service &&
	grep -qx 'ExecStart=/usr/bin/orbivisord' "$UNIT_DIR/orbivisord.service" &&
	grep -qx 'Description=Orbivisor node daemon' "$UNIT_DIR/orbivisord.service" &&
	grep -qx 'ConditionPathExists=|/etc/orbivisor/orbivisord.toml' "$UNIT_DIR/orbivisord.service" &&
	grep -qx 'ConditionPathExists=|/etc/opl/opld.toml' "$UNIT_DIR/orbivisord.service" &&
	[ "$(grep -c '^ConditionPathExists=' "$UNIT_DIR/orbivisord.service")" = 2 ]; then
	ok "유닛 orbivisord.service(설정 두 경로 중 하나)와 별칭 링크 opld.service"
else
	fail "유닛: $(ls -l "$UNIT_DIR" 2>&1)"
fi
# systemd 알림(설계 §3.3): 시작이 끝나면 READY=1, core·api가 답하는 동안 WATCHDOG=1(없으면 SIGKILL). 시작 제한은 기본(90초).
if grep -qx 'Type=notify' "$UNIT_DIR/orbivisord.service" && grep -qx 'NotifyAccess=main' "$UNIT_DIR/orbivisord.service" &&
	grep -qx 'WatchdogSec=30s' "$UNIT_DIR/orbivisord.service" && grep -qx 'WatchdogSignal=SIGKILL' "$UNIT_DIR/orbivisord.service" &&
	grep -qx 'Restart=on-failure' "$UNIT_DIR/orbivisord.service" &&
	grep -qx 'KillMode=process' "$UNIT_DIR/orbivisord.service" && ! grep -q '^TimeoutStartSec=' "$UNIT_DIR/orbivisord.service"; then
	ok "유닛은 Type=notify·WatchdogSec=30s·WatchdogSignal=SIGKILL·NotifyAccess=main"
else
	fail "유닛 systemd 알림: $(cat "$UNIT_DIR/orbivisord.service")"
fi
# 빠른 HA(계획 51): 장치 제한 10초 안에 다시 뜨도록 2초 뒤 다시 띄우고, 그만큼 시작 제한을 늘린다.
if grep -qx 'RestartSec=2' "$UNIT_DIR/orbivisord.service" && grep -qx 'StartLimitIntervalSec=60' "$UNIT_DIR/orbivisord.service" &&
	grep -qx 'StartLimitBurst=10' "$UNIT_DIR/orbivisord.service"; then
	ok "유닛은 RestartSec=2·StartLimitIntervalSec=60·StartLimitBurst=10"
else
	fail "유닛 다시 띄우기: $(grep -E '^(Restart|StartLimit)' "$UNIT_DIR/orbivisord.service")"
fi
# 내장 VIP(계획 46): 죽은 프로세스가 남긴 VIP를 멈춘 뒤 뗀다(SIGKILL 뒤에도 돈다).
if grep -qx 'ExecStopPost=/usr/bin/orbivisord --release-vips' "$UNIT_DIR/orbivisord.service"; then
	ok "유닛은 ExecStopPost로 남은 VIP를 뗀다"
else
	fail "유닛 ExecStopPost: $(grep ExecStop "$UNIT_DIR/orbivisord.service")"
fi
# 노드 정상 종료 정책(계획 50): 비우기(10분)와 HA VM 끄기를 마칠 시간
if grep -qx 'TimeoutStopSec=15min' "$UNIT_DIR/orbivisord.service"; then
	ok "유닛은 TimeoutStopSec=15min"
else
	fail "유닛 TimeoutStopSec: $(grep Timeout "$UNIT_DIR/orbivisord.service")"
fi
if [ -f "$T/usr/share/orbivisor/orbivisord.toml.example" ] && [ ! -e "$T/usr/share/orbivisor/opld.toml.example" ] &&
	[ -f "$T/usr/share/doc/orbivisor/copyright" ] &&
	[ ! -e "$T/usr/share/oplite" ] && [ ! -e "$T/usr/share/doc/oplite" ]; then
	ok "공유 파일은 /usr/share/orbivisor"
else
	fail "공유 파일: $(find "$T/usr/share" 2>&1)"
fi
# swtpm AppArmor 덧붙임(계획 59): conffile로 싣고, 기본 경로(/var/lib/orbivisor, /run/orbivisor)와 옛 이름(/var/lib/opl,
# /run/opl — 이름 바꾸기 앞에 뜬 swtpm이 옛 경로로 열었다)의 TPM 파일을 허용한다.
AA_FILE="$T/etc/apparmor.d/abstractions/orbivisor-swtpm"
# aa_rules <디렉터리 이름>: /var/lib/<이름>·/run/<이름> 아래 TPM 규칙이 다 있다
aa_rules() {
	grep -qxF "  /var/lib/$1/vm/*/tpm/ rw," "$AA_FILE" &&
		grep -qxF "  /var/lib/$1/vm/*/tpm/** rwk," "$AA_FILE" &&
		grep -qxF "  /run/$1/vm/*/tpm.sock rw," "$AA_FILE" &&
		grep -qxF "  /run/$1/vm/*/swtpm.pid rw," "$AA_FILE" &&
		grep -qxF "  /run/$1/vm/*/swtpm.log rw," "$AA_FILE"
}
if [ -f "$AA_FILE" ] && [ "$(cat "$T/DEBIAN/conffiles")" = /etc/apparmor.d/abstractions/orbivisor-swtpm ] &&
	aa_rules orbivisor && aa_rules opl; then
	ok "swtpm AppArmor 덧붙임(conffile, 기본 경로)"
else
	fail "AppArmor: $(cat "$AA_FILE" "$T/DEBIAN/conffiles" 2>&1)"
fi
# md5sums에는 파일만(링크는 dpkg --verify가 보지 않는다)
if [ "$(awk '{ print $2 }' "$T/DEBIAN/md5sums")" = "usr/bin/orbivisorctl
usr/bin/orbivisord
usr/lib/systemd/system/orbivisord.service
usr/share/doc/orbivisor/copyright
usr/share/orbivisor/orbivisord.toml.example" ]; then
	ok "md5sums에 링크가 없다"
else
	fail "md5sums: $(cat "$T/DEBIAN/md5sums")"
fi
if grep -qx 'Package: orbivisor' "$T/DEBIAN/control" && grep -qx 'Version: 0.1.0~ci7' "$T/DEBIAN/control" &&
	grep -qx 'Breaks: oplite (<< 0.1.0~ci7)' "$T/DEBIAN/control" &&
	grep -qx 'Replaces: oplite (<< 0.1.0~ci7)' "$T/DEBIAN/control" &&
	! grep -q '^Provides:' "$T/DEBIAN/control" &&
	grep -q '^Depends: .*iputils-ping' "$T/DEBIAN/control" &&
	grep -q '^Recommends: .*swtpm, swtpm-tools' "$T/DEBIAN/control" &&
	[ -x "$T/DEBIAN/postinst" ] && [ -x "$T/DEBIAN/prerm" ] && [ -x "$T/DEBIAN/postrm" ]; then
	ok "orbivisor control(Breaks·Replaces oplite)과 유지 스크립트, HA 검사의 ping(Depends), vTPM의 swtpm(Recommends)"
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

# check_mismatch <이름>: 종료 코드 1, ORBIVISOR_VERSION 안내, 패키지 없음
check_mismatch() {
	if [ "$code" -eq 1 ] && grep -q 'ORBIVISOR_VERSION=0.2.0' "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
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
# ORBIVISOR_VERSION 없이 빌드한 바이너리는 크레이트 버전을 보인다. 앞부분만 같아도 안 된다.
fake_bins 0.2.0~ci1 0.2.0~ci1
run 0.2.0
check_mismatch "버전 앞부분만 같으면 실패"

# ORBIVISOR_APT_KEY: 저장소 공개 키를 orbivisor의 /usr/share/keyrings/oplite-archive-keyring.gpg(0644, 옛 경로 그대로)로
# 싣고 md5sums에 넣는다. 전환 패키지에는 싣지 않는다.
KEYRING=usr/share/keyrings/oplite-archive-keyring.gpg
fake_bins 0.1.0 0.1.0
run 0.1.0
if [ "$code" -eq 0 ] && [ ! -e "$WORK/tree-orbivisor/usr/share/keyrings" ]; then
	ok "ORBIVISOR_APT_KEY가 없으면 키링을 싣지 않는다"
else
	fail "키링 없음: 종료 코드 $code"
fi
printf 'public key' >"$WORK/archive.gpg"
chmod 0600 "$WORK/archive.gpg"
ORBIVISOR_APT_KEY="$WORK/archive.gpg" run 0.1.0
if [ "$code" -eq 0 ] && cmp -s "$WORK/archive.gpg" "$WORK/tree-orbivisor/$KEYRING" &&
	[ -n "$(find "$WORK/tree-orbivisor/$KEYRING" -perm 0644)" ] &&
	grep -q " $KEYRING\$" "$WORK/tree-orbivisor/DEBIAN/md5sums" &&
	[ ! -e "$WORK/tree-oplite/usr/share/keyrings" ]; then
	ok "ORBIVISOR_APT_KEY의 키링을 싣는다"
else
	fail "키링: 종료 코드 $code"
	cat "$WORK/stderr"
fi
# 옛 이름 OPL_APT_KEY·OPL_BIN_DIR로 빌드하는 파이프라인(계획 74 리뷰 1): 키링을 빠뜨리지 않는다.
rm -rf "$WORK/out"
code=0
PATH="$WORK/fakebin:$PATH" OPL_BIN_DIR="$WORK/bin" OPL_APT_KEY="$WORK/archive.gpg" ORBIVISOR_DEV_LICENSE=1 \
	sh "$DEB_DIR/build-deb.sh" 0.1.0 "$WORK/out" >"$WORK/stdout" 2>"$WORK/stderr" || code=$?
if [ "$code" -eq 0 ] && cmp -s "$WORK/archive.gpg" "$WORK/tree-orbivisor/$KEYRING"; then
	ok "옛 이름 OPL_APT_KEY·OPL_BIN_DIR도 받는다"
else
	fail "옛 이름 OPL_APT_KEY·OPL_BIN_DIR: 종료 코드 $code"
	cat "$WORK/stderr"
fi
ORBIVISOR_APT_KEY="$WORK/missing.gpg" run 0.1.0
if [ "$code" -eq 1 ] && grep -q ORBIVISOR_APT_KEY "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
	ok "ORBIVISOR_APT_KEY 파일이 없으면 실패"
else
	fail "없는 ORBIVISOR_APT_KEY: 종료 코드 $code"
fi
printf -- '-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmQINBG\n-----END PGP PUBLIC KEY BLOCK-----\n' >"$WORK/archive.asc"
ORBIVISOR_APT_KEY="$WORK/archive.asc" run 0.1.0
if [ "$code" -eq 1 ] && grep -q 'ORBIVISOR_APT_KEY must be a binary keyring (gpg --export, not --armor)' "$WORK/stderr" &&
	[ ! -e "$WORK/out" ]; then
	ok "armor 키는 거절"
else
	fail "armor ORBIVISOR_APT_KEY: 종료 코드 $code"
fi

# 구독 키 공개 키: ORBIVISOR_LICENSE_KEYS도 ORBIVISOR_DEV_LICENSE=1도 없으면 만들지 않는다.
fake_bins 0.1.0 0.1.0
ORBIVISOR_LICENSE_KEYS='' ORBIVISOR_DEV_LICENSE='' run 0.1.0
if [ "$code" -eq 1 ] && grep -q 'build-deb: ORBIVISOR_LICENSE_KEYS is not set; set it for a release build or ORBIVISOR_DEV_LICENSE=1 for a development package' "$WORK/stderr" &&
	[ ! -e "$WORK/out" ]; then
	ok "ORBIVISOR_LICENSE_KEYS 없이 개발 패키지가 아니면 실패"
else
	fail "ORBIVISOR_LICENSE_KEYS 없음: 종료 코드 $code"
fi
ORBIVISOR_LICENSE_KEYS='' ORBIVISOR_DEV_LICENSE=0 run 0.1.0
if [ "$code" -eq 1 ] && [ ! -e "$WORK/out" ]; then
	ok "ORBIVISOR_DEV_LICENSE가 1이 아니면 실패"
else
	fail "ORBIVISOR_DEV_LICENSE=0: 종료 코드 $code"
fi
# 정식 키: orbivisor-license keygen과 같은 형식(base64 32바이트)의 개발 키가 아닌 키 둘
RELEASE_KEYS='Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyM=,mR8vT2wX4yZ6aB8cD0eF2gH4iJ6kL8mN0oP2qR4sT6U='
DEV_KEY=$(cat "$DEB_DIR/../../crates/orbivisor-types/keys/dev-license.pub")
fake_bins 0.1.0 0.1.0 "$RELEASE_KEYS"
ORBIVISOR_LICENSE_KEYS=$RELEASE_KEYS ORBIVISOR_DEV_LICENSE='' run 0.1.0
if [ "$code" -eq 0 ] && [ -f "$WORK/out/orbivisor_0.1.0_amd64.deb" ] && [ -f "$WORK/out/oplite_0.1.0_amd64.deb" ]; then
	ok "ORBIVISOR_LICENSE_KEYS가 있으면 만든다"
else
	fail "ORBIVISOR_LICENSE_KEYS 있음: 종료 코드 $code"
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
ORBIVISOR_LICENSE_KEYS=$DEV_KEY ORBIVISOR_DEV_LICENSE='' run 0.1.0
check_key_error "ORBIVISOR_LICENSE_KEYS의 개발 키는 거절" \
	'build-deb: ORBIVISOR_LICENSE_KEYS contains the development key; use ORBIVISOR_DEV_LICENSE=1 for a development package'
fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*},$DEV_KEY"
ORBIVISOR_LICENSE_KEYS="${RELEASE_KEYS%%,*},$DEV_KEY" ORBIVISOR_DEV_LICENSE='' run 0.1.0
check_key_error "목록에 섞인 개발 키도 거절" 'ORBIVISOR_LICENSE_KEYS contains the development key'
# 패딩 없는 키, 끝 비트가 0이 아닌(정규형이 아닌) 키도 거절한다.
for bad in 'not-base64!' 'AAAA' 'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyMA' \
	'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyM' 'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyN='; do
	fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*},$bad"
	ORBIVISOR_LICENSE_KEYS="${RELEASE_KEYS%%,*},$bad" ORBIVISOR_DEV_LICENSE='' run 0.1.0
	check_key_error "32바이트 base64가 아닌 항목 거절: $bad" \
		"build-deb: ORBIVISOR_LICENSE_KEYS entry is not a base64 Ed25519 public key: $bad"
done
# 공백·쉼표뿐이면 키가 하나도 없다.
for none in ' ' ','; do
	fake_bins 0.1.0 0.1.0 "$none"
	ORBIVISOR_LICENSE_KEYS=$none ORBIVISOR_DEV_LICENSE='' run 0.1.0
	check_key_error "키가 없는 ORBIVISOR_LICENSE_KEYS 거절: '$none'" 'build-deb: ORBIVISOR_LICENSE_KEYS has no keys'
done
fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*}"
ORBIVISOR_LICENSE_KEYS=$RELEASE_KEYS ORBIVISOR_DEV_LICENSE='' run 0.1.0
check_key_error "orbivisord에 ORBIVISOR_LICENSE_KEYS가 그대로 없으면 거절" \
	'build-deb: orbivisord was not built with this ORBIVISOR_LICENSE_KEYS'

# 파일 복원 도우미 이미지(계획 69): ORBIVISOR_RESTORE_IMAGE_DIR이 있으면 셋째 패키지 orbivisor-restore-image를 만든다.
# manifest.json의 이미지 판은 패키지 버전과 저장소 debian.lock 해시 앞 12자여야 하고, 아키텍처는 x86_64여야 한다.
if command -v sha256sum >/dev/null; then
	LOCK_SHA=$(sha256sum "$DEB_DIR/../restore-image/debian.lock" | cut -c1-12)
else
	LOCK_SHA=$(shasum -a 256 "$DEB_DIR/../restore-image/debian.lock" | cut -c1-12)
fi
# fake_image <이미지 판> [아키텍처]: 가짜 이미지 디렉터리 $WORK/image
fake_image() {
	rm -rf "${WORK:?}/image"
	mkdir -p "$WORK/image"
	printf 'kernel' >"$WORK/image/vmlinuz"
	printf 'initrd' >"$WORK/image/initrd.img"
	chmod 0600 "$WORK/image/initrd.img"
	printf '{\n  "arch": "%s",\n  "image_version": "%s",\n  "proto": 1\n}\n' "${2:-x86_64}" "$1" >"$WORK/image/manifest.json"
}
fake_bins 0.1.0~ci7 0.1.0~ci7
fake_image "orbivisor 0.1.0~ci7 lock-$LOCK_SHA"
ORBIVISOR_RESTORE_IMAGE_DIR="$WORK/image" run 0.1.0~ci7
I="$WORK/tree-orbivisor-restore-image"
R=usr/share/orbivisor/restore/x86_64
if [ "$code" -eq 0 ] && [ "$(sed -n 3p "$WORK/stdout")" = "$WORK/out/orbivisor-restore-image_0.1.0~ci7_amd64.deb" ] &&
	[ -f "$WORK/out/orbivisor-restore-image_0.1.0~ci7_amd64.deb" ] &&
	[ "$(cd "$I" && find usr ! -type d | LC_ALL=C sort)" = "usr/share/doc/orbivisor-restore-image/copyright
usr/share/doc/orbivisor-restore-image/debian.lock
$R/initrd.img
$R/manifest.json
$R/vmlinuz" ] && cmp -s "$WORK/image/initrd.img" "$I/$R/initrd.img" &&
	[ -n "$(find "$I/$R/initrd.img" -perm 0644)" ] &&
	cmp -s "$DEB_DIR/../restore-image/debian.lock" "$I/usr/share/doc/orbivisor-restore-image/debian.lock" &&
	grep -qx 'Package: orbivisor-restore-image' "$I/DEBIAN/control" &&
	grep -qx 'Version: 0.1.0~ci7' "$I/DEBIAN/control" &&
	grep -qx 'Architecture: amd64' "$I/DEBIAN/control" &&
	grep -qx 'Depends: orbivisor (= 0.1.0~ci7)' "$I/DEBIAN/control" &&
	grep -q " $R/manifest.json\$" "$I/DEBIAN/md5sums" && [ "$(ls "$I/DEBIAN")" = "control
md5sums" ] &&
	grep -q '^Recommends: .*orbivisor-restore-image' "$WORK/tree-orbivisor/DEBIAN/control"; then
	ok "ORBIVISOR_RESTORE_IMAGE_DIR가 있으면 셋째 패키지 orbivisor-restore-image(/$R, Depends orbivisor 같은 판)"
else
	fail "도우미 이미지 패키지: 종료 코드 $code $(cat "$WORK/stdout" "$WORK/stderr") $(find "$I" | sort)"
fi
# check_image_error <이름>: 종료 코드 1, 패키지 없음
check_image_error() {
	if [ "$code" -eq 1 ] && grep -q -e 'ORBIVISOR_RESTORE_IMAGE_DIR has no' -e 'is not an x86_64 image built for' "$WORK/stderr" &&
		[ ! -e "$WORK/out" ]; then
		ok "$1"
	else
		fail "$1: 종료 코드 $code"
		cat "$WORK/stderr"
	fi
}
fake_image "orbivisor 0.1.0~ci6 lock-$LOCK_SHA"
ORBIVISOR_RESTORE_IMAGE_DIR="$WORK/image" run 0.1.0~ci7
check_image_error "이미지 판이 패키지 버전과 다르면 실패"
fake_image "orbivisor 0.1.0~ci7 lock-000000000000"
ORBIVISOR_RESTORE_IMAGE_DIR="$WORK/image" run 0.1.0~ci7
check_image_error "이미지가 다른 debian.lock으로 만든 것이면 실패"
fake_image "orbivisor 0.1.0~ci7 lock-$LOCK_SHA" aarch64
ORBIVISOR_RESTORE_IMAGE_DIR="$WORK/image" run 0.1.0~ci7
check_image_error "aarch64 이미지는 실패"
fake_image "orbivisor 0.1.0~ci7 lock-$LOCK_SHA"
rm "$WORK/image/initrd.img"
ORBIVISOR_RESTORE_IMAGE_DIR="$WORK/image" run 0.1.0~ci7
check_image_error "이미지 파일이 빠지면 실패"

if [ "$FAILED" -ne 0 ]; then
	echo "build-deb-test: 실패"
	exit 1
fi
echo "build-deb-test: 모두 통과"
