#!/bin/bash
# 설치 ISO의 orbivisor-setup.sh 함수(interfaces 바꾸기, 주소 찾기, opld.toml·issue 쓰기, 설치 환경·대상
# 두 곳의 interfaces)를 견본과 비교한다. bash만 있으면 되고 macOS에서도 돈다.
# 함수는 셸 OPL_SH(기본 sh)로 source해 부른다. 설치 프로그램 환경과 같게 보려면(CI):
#   OPL_SH="busybox sh" OPL_AWK="busybox awk" bash packaging/tests/setup-test.sh
# 쓰는 법: bash packaging/tests/setup-test.sh
set -eu

TESTS_DIR="$(cd "$(dirname "$0")" && pwd)"
FIXTURES="$TESTS_DIR/fixtures"
SETUP="$TESTS_DIR/../iso/preseed/orbivisor-setup.sh"
read -r -a SH <<<"${OPL_SH:-sh}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# OPL_AWK가 있으면 함수의 awk가 그것을 쓰게 PATH 앞에 awk를 둔다.
if [ -n "${OPL_AWK:-}" ]; then
	mkdir -p "$WORK/bin"
	printf '#!/bin/sh\nexec %s "$@"\n' "$OPL_AWK" >"$WORK/bin/awk"
	chmod 0755 "$WORK/bin/awk"
	PATH="$WORK/bin:$PATH"
fi

# lib <함수> [인자...]: orbivisor-setup.sh를 함수만 정의하게 source하고 그 함수를 부른다.
lib() {
	# shellcheck disable=SC2016 # $0·$@는 안쪽 셸이 푼다.
	OPL_SETUP_LIB=1 "${SH[@]}" -c '. "$0"; "$@"' "$SETUP" "$@"
}

# 함수 라이브러리를 그 셸로 읽을 수 있어야 한다.
lib true || {
	echo "setup-test: ${SH[*]}로 $SETUP 을 읽지 못했다"
	exit 1
}

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

# same <이름> <기대 파일> <실제 파일>
same() {
	if diff -u "$2" "$3"; then ok "$1"; else fail "$1"; fi
}

# ascii <이름> <파일>: 영어 ASCII(출력 가능한 문자와 공백)만 있어야 한다.
ascii() {
	if LC_ALL=C grep -n '[^ -~[:space:]]' "$2"; then fail "$1: ASCII가 아닌 문자"; else ok "$1 ASCII"; fi
}

# interfaces 견본: <이름> <기대 종료 코드> <기대 주소>
check_interfaces() {
	name=$1 want_code=$2 want_addr=$3
	out="$WORK/interfaces-$name"
	code=0
	lib convert_interfaces "$FIXTURES/interfaces-$name.in" "$out" || code=$?
	if [ "$code" -ne "$want_code" ]; then
		fail "interfaces-$name: 종료 코드 $code (기대 $want_code)"
	fi
	same "interfaces-$name" "$FIXTURES/interfaces-$name.out" "$out"
	addr=$(lib primary_address "$out")
	if [ "$addr" = "$want_addr" ]; then
		ok "interfaces-$name 주소 '$addr'"
	else
		fail "interfaces-$name 주소 '$addr' (기대 '$want_addr')"
	fi
}

check_interfaces dhcp 0 ""
check_interfaces static 0 192.168.10.21
check_interfaces hotplug 0 ""
check_interfaces none 2 ""
check_interfaces ipv6 0 10.0.0.11
# 게이트웨이 없는 고정 주소: 기본 경로 블록이 없으면 첫 inet static 블록
check_interfaces static-nogw 0 10.20.0.5

# 기본 경로 인터페이스 이름
nic=$(lib default_nic "$FIXTURES/interfaces-hotplug.in")
if [ "$nic" = enp1s0 ]; then ok "default_nic hotplug"; else fail "default_nic hotplug: '$nic'"; fi
nic=$(lib default_nic "$FIXTURES/interfaces-static-nogw.in")
if [ "$nic" = enp2s0 ]; then ok "default_nic static-nogw"; else fail "default_nic static-nogw: '$nic'"; fi

# 노드 이름 규칙
for n in node1 opl-ci a 0 "$(printf 'a%.0s' $(seq 63))"; do
	if lib valid_name "$n"; then ok "valid_name '$n'"; else fail "valid_name '$n' 거짓"; fi
done
for n in "" -a a- My-Host my_host my.host "$(printf 'a%.0s' $(seq 64))"; do
	if lib valid_name "$n"; then fail "valid_name '$n' 참"; else ok "valid_name '$n' 거짓"; fi
done

# opld.toml: 정적 주소, DHCP 주소, 규칙에 맞지 않는 이름, 주소 없음(그 줄은 주석)
lib write_opld_toml "$WORK/opld-static.toml" node1 192.168.10.21
same "opld.toml 정적" "$FIXTURES/opld-static.toml" "$WORK/opld-static.toml"
lib write_opld_toml "$WORK/opld-dhcp.toml" opl-ci 10.0.2.15 dhcp
same "opld.toml DHCP" "$FIXTURES/opld-dhcp.toml" "$WORK/opld-dhcp.toml"
lib write_opld_toml "$WORK/opld-badname.toml" my_host 192.168.10.21
same "opld.toml 이름 규칙 밖" "$FIXTURES/opld-badname.toml" "$WORK/opld-badname.toml"
lib write_opld_toml "$WORK/opld-noaddr.toml" node1 "" dhcp
same "opld.toml 주소 없음" "$FIXTURES/opld-noaddr.toml" "$WORK/opld-noaddr.toml"

# 콘솔 로그인 화면: agetty 이스케이프 \4{vmbr0}가 그대로 남아야 한다.
lib write_issue "$WORK/orbivisor.issue"
same "issue" "$FIXTURES/orbivisor.issue" "$WORK/orbivisor.issue"

for f in opld-static.toml opld-dhcp.toml opld-badname.toml opld-noaddr.toml orbivisor.issue; do
	ascii "$f" "$WORK/$f"
done

# setup_network <이름> <설치 환경 interfaces 견본|-> <대상 interfaces 견본|-> <기대 견본> <기대 NIC>:
# 설치 환경 파일(lo가 아닌 iface가 없으면 대상 파일)을 바꿔 두 곳 모두에 쓴다.
check_network() {
	name=$1 inst_in=$2 target_in=$3 want=$4 want_nic=$5
	inst="$WORK/net-$name/installer" target="$WORK/net-$name/target"
	mkdir -p "$inst/etc/network" "$target/etc/network"
	[ "$inst_in" = - ] || cp "$inst_in" "$inst/etc/network/interfaces"
	[ "$target_in" = - ] || cp "$target_in" "$target/etc/network/interfaces"
	# shellcheck disable=SC2016 # $TARGET·$NIC는 안쪽 셸이 푼다.
	got_nic=$(OPL_INSTALLER_ROOT="$inst" TARGET="$target" lib eval 'setup_network "$TARGET" >/dev/null; echo "$NIC"')
	if [ "$got_nic" = "$want_nic" ]; then ok "setup_network $name NIC '$got_nic'"; else
		fail "setup_network $name NIC '$got_nic' (기대 '$want_nic')"
	fi
	same "setup_network $name 대상" "$want" "$target/etc/network/interfaces"
	same "setup_network $name 설치 환경" "$want" "$inst/etc/network/interfaces"
}

printf 'auto lo\niface lo inet loopback\n' >"$WORK/interfaces-lo"
check_network "설치 환경 원본" "$FIXTURES/interfaces-dhcp.in" - "$FIXTURES/interfaces-dhcp.out" ens3
check_network "설치 환경 우선" "$FIXTURES/interfaces-static.in" "$FIXTURES/interfaces-dhcp.in" \
	"$FIXTURES/interfaces-static.out" enp1s0
check_network "설치 환경에 lo만: 대상 원본" "$WORK/interfaces-lo" "$FIXTURES/interfaces-static.in" \
	"$FIXTURES/interfaces-static.out" enp1s0
check_network "설치 환경 없음: 대상 원본" - "$FIXTURES/interfaces-dhcp.in" "$FIXTURES/interfaces-dhcp.out" ens3

# 노드 이름: 설치 환경(없으면 대상)의 호스트 이름을 소문자로
mkdir -p "$WORK/name/installer/etc" "$WORK/name/target/etc"
echo OPL-Node1 >"$WORK/name/target/etc/hostname"
got=$(OPL_INSTALLER_ROOT="$WORK/name/installer" lib node_name "$WORK/name/target")
if [ "$got" = opl-node1 ]; then ok "node_name 대상"; else fail "node_name 대상: '$got'"; fi
echo Opl-CI >"$WORK/name/installer/etc/hostname"
got=$(OPL_INSTALLER_ROOT="$WORK/name/installer" lib node_name "$WORK/name/target")
if [ "$got" = opl-ci ]; then ok "node_name 설치 환경"; else fail "node_name 설치 환경: '$got'"; fi

# 설치 환경의 busybox tr은 문자 집합([:upper:])을 모른다. 스크립트가 그런 tr을 쓰지 않는지 본다.
if grep -n "tr '\[:" "$SETUP" >/dev/null; then fail "orbivisor-setup.sh가 tr 문자 집합을 쓴다"; else ok "tr 문자 집합 없음"; fi

# 단계 기록: 시작·끝은 로그(stdout)와 콘솔에 쓴다. 실패하면 단계 이름과 출력 끝 30줄을 콘솔에 쓰고 그 코드로 끝난다.
mkdir -p "$WORK/steps"
code=0
# shellcheck disable=SC2016 # $?는 안쪽 셸이 푼다.
OPL_CONSOLE="$WORK/steps/console" TMPDIR="$WORK/steps" \
	lib eval 'trap "on_exit \$?" EXIT; step one true; step two sh -c "seq 1 40; exit 3"; step three true' \
	>"$WORK/steps/log" 2>&1 || code=$?
{
	printf 'orbivisor-setup: %s\n' "start one" "done one" "start two" "FAILED two (exit 3)"
	seq 11 40
} >"$WORK/steps/console-want"
if [ "$code" -eq 3 ]; then ok "step 실패 종료 코드"; else fail "step 실패 종료 코드 $code (기대 3)"; fi
same "step 콘솔" "$WORK/steps/console-want" "$WORK/steps/console"
if grep -qx 1 "$WORK/steps/log" && grep -qx 40 "$WORK/steps/log" && grep -qx 'orbivisor-setup: start one' "$WORK/steps/log" &&
	grep -qx 'orbivisor-setup: FAILED two (exit 3)' "$WORK/steps/log"; then
	ok "step 로그"
else
	fail "step 로그"
	cat "$WORK/steps/log"
fi
# 콘솔에 쓰지 못해도(없는 디렉터리) 단계는 이어진다.
code=0
OPL_CONSOLE="$WORK/none/console" TMPDIR="$WORK/steps" lib eval 'step one true' >/dev/null 2>&1 || code=$?
if [ "$code" -eq 0 ]; then ok "step 콘솔 쓰기 실패 무시"; else fail "step 콘솔 쓰기 실패 무시: 종료 코드 $code"; fi

# install_package: CD의 패키지를 대상에 복사하고 in-target으로 apt-get을 무인(질문 없이, stdin 없음)으로 부른다.
# 그동안 대상에 policy-rc.d(exit 101)를 두고 끝나면(실패해도) 지운다.
mkdir -p "$WORK/pkg/cdrom/orbivisor" "$WORK/pkg/target/usr/sbin" "$WORK/pkg/bin"
printf 'deb' >"$WORK/pkg/cdrom/orbivisor/orbivisor_0.1.0_amd64.deb"
cat >"$WORK/pkg/bin/in-target" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$FAKE_LOG"
[ ! -f "$FAKE_TARGET/usr/sbin/policy-rc.d" ] || echo "policy-rc.d: $(tail -n1 "$FAKE_TARGET/usr/sbin/policy-rc.d")" >>"$FAKE_LOG"
exit "${FAKE_CODE:-0}"
EOF
chmod 0755 "$WORK/pkg/bin/in-target"
# install_with <in-target 종료 코드>
install_with() {
	: >"$WORK/pkg/log"
	code=0
	PATH="$WORK/pkg/bin:$PATH" FAKE_LOG="$WORK/pkg/log" FAKE_TARGET="$WORK/pkg/target" FAKE_CODE=$1 \
		OPL_CDROM="$WORK/pkg/cdrom" lib install_package "$WORK/pkg/target" >/dev/null 2>&1 || code=$?
}
cat >"$WORK/pkg/want" <<'EOF'
--pass-stdout sh -c DEBIAN_FRONTEND=noninteractive apt-get -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold install ifupdown2 isc-dhcp-client /var/cache/orbivisor/orbivisor_0.1.0_amd64.deb </dev/null 2>&1
policy-rc.d: exit 101
EOF
install_with 0
if [ "$code" -eq 0 ]; then ok "install_package"; else fail "install_package: 종료 코드 $code"; fi
same "install_package in-target" "$WORK/pkg/want" "$WORK/pkg/log"
if [ -f "$WORK/pkg/target/var/cache/orbivisor/orbivisor_0.1.0_amd64.deb" ]; then ok "install_package 복사"; else fail "install_package 복사"; fi
if [ ! -e "$WORK/pkg/target/usr/sbin/policy-rc.d" ]; then ok "install_package policy-rc.d 지움"; else
	fail "install_package policy-rc.d 남음"
fi
install_with 100
if [ "$code" -eq 100 ] && [ ! -e "$WORK/pkg/target/usr/sbin/policy-rc.d" ]; then ok "install_package 실패"; else
	fail "install_package 실패: 종료 코드 $code"
fi

# main 전체(가짜 in-target·ip): 단계가 차례로 콘솔에 찍히고 설정 파일이 생긴다.
M="$WORK/main"
mkdir -p "$M/installer/etc/network" "$M/target/etc/systemd/system" "$M/cdrom/preseed"
cp -R "$WORK/pkg/cdrom/orbivisor" "$M/cdrom/"
cp "$FIXTURES/interfaces-dhcp.in" "$M/installer/etc/network/interfaces"
echo opl-ci >"$M/installer/etc/hostname"
echo '[Unit]' >"$M/cdrom/preseed/orbivisor-ci-init.service"
cat >"$WORK/pkg/bin/ip" <<'EOF'
#!/bin/sh
echo "2: ens3    inet 10.0.2.15/24 brd 10.0.2.255 scope global dynamic ens3"
EOF
chmod 0755 "$WORK/pkg/bin/ip"
code=0
PATH="$WORK/pkg/bin:$PATH" FAKE_LOG="$M/in-target.log" FAKE_TARGET="$M/target" OPL_CDROM="$M/cdrom" \
	OPL_INSTALLER_ROOT="$M/installer" OPL_CONSOLE="$M/console" TMPDIR="$M" \
	"${SH[@]}" "$SETUP" "$M/target" || code=$?
printf 'orbivisor-setup: %s\n' "start install-package" "done install-package" "start network" "done network" \
	"start config" "done config" "start ci-init" "done ci-init" >"$M/console-want"
if [ "$code" -eq 0 ]; then ok "main"; else fail "main: 종료 코드 $code"; fi
same "main 콘솔" "$M/console-want" "$M/console"
same "main interfaces" "$FIXTURES/interfaces-dhcp.out" "$M/target/etc/network/interfaces"
same "main opld.toml" "$FIXTURES/opld-dhcp.toml" "$M/target/etc/opl/opld.toml"
if [ -f "$M/target/etc/issue.d/orbivisor.issue" ] && [ -f "$M/target/etc/systemd/system/orbivisor-ci-init.service" ] &&
	grep -qx 'systemctl enable orbivisor-ci-init.service' "$M/in-target.log" && [ -s "$M/target/root/orbivisor-setup.log" ]; then
	ok "main issue·ci-init·로그"
else
	fail "main issue·ci-init·로그"
fi

if [ "$FAILED" -ne 0 ]; then
	echo "setup-test: 실패"
	exit 1
fi
echo "setup-test: 모두 통과"
