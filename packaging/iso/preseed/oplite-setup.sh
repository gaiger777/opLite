#!/bin/sh
# 설치 ISO의 preseed/late_command가 설치 프로그램 환경(busybox sh)에서 돌린다.
#   sh /cdrom/preseed/oplite-setup.sh /target
# oplite 패키지(와 ifupdown2, isc-dhcp-client)를 설치하고, 기본 경로 인터페이스를 브리지 vmbr0으로 옮기고,
# /etc/opl/opld.toml과 콘솔 로그인 안내(/etc/issue.d/oplite.issue)를 쓴다. 로그: <target>/root/oplite-setup.log
# 단계마다 시작·끝(실패면 출력 끝 30줄)을 설치 환경의 콘솔(무인 설치에서는 직렬)에도 "oplite-setup: "로 쓴다.
# OPLITE_SETUP_LIB=1로 source하면 함수만 정의한다(packaging/tests/setup-test.sh).
set -eu

CDROM=${OPLITE_CDROM:-/cdrom}
# 단계 시작·끝을 쓰는 콘솔과 단계 출력을 잠시 두는 파일. 시험이 바꾼다.
CONSOLE=${OPLITE_CONSOLE:-/dev/console}
STEP_OUT="${TMPDIR:-/tmp}/oplite-setup.step"
STEP_NAME=""
# 설치 프로그램 환경의 루트. late_command(finish-install 07preseed)는 netcfg가 interfaces·hostname을
# /target으로 복사하는 단계(55netcfg-copy-config)보다 먼저 돌므로 원본은 이쪽에 있다. 시험이 바꾼다.
INSTALLER_ROOT=${OPLITE_INSTALLER_ROOT:-}

# default_nic <interfaces>: 기본 경로 인터페이스(inet dhcp 또는 gateway가 있는 inet 블록, lo 빼고 첫 번째).
# 그런 블록이 없으면 lo가 아닌 첫 inet static 블록(게이트웨이 없는 고정 주소).
default_nic() {
	awk '
	function end_stanza() {
		if (cand != "" && found == "" && ok) found = cand
		cand = ""
		ok = 0
	}
	/^[ \t]+[^ \t]/ && cand != "" { if ($1 == "gateway") ok = 1; next }
	{ end_stanza() }
	$1 == "iface" && $3 == "inet" && $2 != "lo" {
		cand = $2
		ok = ($4 == "dhcp")
		if ($4 == "static" && first_static == "") first_static = $2
	}
	END { end_stanza(); print (found != "" ? found : first_static) }
	' "$1"
}

# has_iface <interfaces>: lo가 아닌 iface 블록이 있으면 참
has_iface() {
	[ -f "$1" ] && awk '$1 == "iface" && $2 != "lo" { f = 1 } END { exit !f }' "$1"
}

# convert_interfaces <in> <out>: 기본 경로 인터페이스의 inet(과 inet6) 블록을 vmbr0으로 옮기고
# NIC는 auto <nic> / iface <nic> inet manual로 남긴다. 찾지 못하면 out에 in을 그대로 쓰고 2를 낸다.
convert_interfaces() {
	nic=$(default_nic "$1")
	if [ -z "$nic" ]; then
		cat "$1" >"$2"
		return 2
	fi
	awk -v nic="$nic" '
	# 빈 줄은 다음 줄을 낼 때 함께 낸다(옮긴 블록 뒤에 빈 줄이 겹치지 않게).
	function emit(line) {
		while (blank > 0) { print ""; blank-- }
		print line
	}
	function rest(from,   s, i) {
		s = ""
		for (i = from; i <= NF; i++) s = s " " $i
		return s
	}
	/^[ \t]+[^ \t]/ {
		if (mode == "inet") v4 = v4 "\n" $0
		else if (mode == "inet6") v6 = v6 "\n" $0
		else emit($0)
		next
	}
	{ mode = "" }
	/^[ \t]*$/ { blank++; next }
	($1 == "auto" || $1 == "allow-hotplug") && NF == 2 && $2 == nic { emit("auto " nic); next }
	$1 == "iface" && $2 == nic && $3 == "inet" {
		emit("iface " nic " inet manual")
		mode = "inet"
		v4 = "iface vmbr0 inet" rest(4)
		next
	}
	$1 == "iface" && $2 == nic && $3 == "inet6" { mode = "inet6"; v6 = "iface vmbr0 inet6" rest(4); next }
	{ emit($0) }
	END {
		print ""
		print "auto vmbr0"
		print v4
		print "\tbridge-ports " nic
		print "\tbridge-stp off"
		print "\tbridge-fd 0"
		print "\tbridge-vlan-aware yes"
		print "\tbridge-vids 2-4094"
		if (v6 != "") { print ""; print v6 }
	}
	' "$1" >"$2"
}

# primary_address <interfaces>: vmbr0 inet 블록의 정적 address(/prefix 뗌). DHCP면 빈 값.
primary_address() {
	awk '
	$1 == "iface" { in_br = ($2 == "vmbr0" && $3 == "inet"); next }
	/^[^ \t]/ { in_br = 0 }
	in_br && $1 == "address" { a = $2; sub(/\/.*/, "", a); print a; exit }
	' "$1"
}

# current_address <nic>: 설치 프로그램 환경에서 그 NIC가 지금 가진 IPv4 주소(DHCP로 받은 것)
current_address() {
	ip -4 -o addr show dev "$1" </dev/null | awk '{ a = $4; sub(/\/.*/, "", a); print a; exit }'
}

# valid_name <name>: opld 노드 이름 규칙(소문자·숫자·-, 1~63자, 앞뒤 - 아님)에 맞으면 참
valid_name() {
	case $1 in
	# 범위(a-z)는 로캘에 따라 대문자도 받으므로 글자를 다 적는다.
	"" | -* | *- | *[!abcdefghijklmnopqrstuvwxyz0123456789-]*) return 1 ;;
	esac
	[ "${#1}" -le 63 ]
}

# write_opld_toml <out> <name> <address> [dhcp]: 영어 ASCII만 쓴다(콘솔·편집기 어디서나 보이게).
# name이 규칙에 맞지 않거나 address가 비면 그 줄을 주석으로 둔다(opld는 설정 오류로 멈춘다).
write_opld_toml() {
	{
		echo '# Written by the opLite installer. Example with all settings: /usr/share/oplite/opld.toml.example'
		echo '# The address cannot change once this node is in a cluster (it is in the node certificate). Reserve the DHCP address or use a static one; before cluster init/join you may still edit it.'
		if valid_name "$2"; then
			echo "name = \"$2\""
		else
			echo "# Set name: 1-63 lowercase letters, digits and '-', not starting or ending with '-'."
			echo "# name = \"$2\""
		fi
		if [ -z "$3" ]; then
			echo "# Set address to this node's IP address (none was found during install)."
			echo '# address = ""'
		else
			if [ "${4:-}" = dhcp ]; then
				echo '# This address came from DHCP.'
			fi
			echo "address = \"$3\""
		fi
	} >"$1"
}

# write_issue <out>: 콘솔 로그인 화면 안내(영어 ASCII). \4{vmbr0}는 agetty가 vmbr0의 IPv4 주소로 바꾼다.
write_issue() {
	cat >"$1" <<'EOF'
opLite: https://\4{vmbr0}:8006/ (web UI and API, after the node is in a cluster)
  First node:  oplctl cluster init --name <cluster-name>
  Other nodes: on a logged-in client run `oplctl cluster join-token`,
               then here run `oplctl cluster join '<opl-join://... URL>'`

EOF
}

log() {
	echo "oplite-setup: $*"
}

# say <문구>: 로그와 설치 환경 콘솔에 쓴다(영어 ASCII). 콘솔에 쓰지 못해도 무시한다.
say() {
	echo "oplite-setup: $*"
	{ echo "oplite-setup: $*" >>"$CONSOLE"; } 2>/dev/null || true
}

# step <이름> <명령...>: 단계 시작·끝을 로그와 콘솔에 쓰고 명령 출력은 로그에 남긴다. 명령은 이 셸에서 돌아
# 변수(NIC 등)가 남는다. 실패하면 set -e로 끝나고 EXIT trap(on_exit)이 이름과 출력 끝을 콘솔에 쓴다.
step() {
	STEP_NAME=$1
	shift
	say "start $STEP_NAME"
	"$@" >"$STEP_OUT" 2>&1
	cat "$STEP_OUT"
	rm -f "$STEP_OUT"
	say "done $STEP_NAME"
	STEP_NAME=""
}

# on_exit <종료 코드>: 실패로 끝나면 실패한 단계와 그 출력(콘솔에는 끝 30줄)을 남긴다.
on_exit() {
	[ "$1" -ne 0 ] || return 0
	if [ -z "$STEP_NAME" ]; then
		say "FAILED (exit $1)"
		return 0
	fi
	cat "$STEP_OUT" 2>/dev/null || true
	say "FAILED $STEP_NAME (exit $1)"
	{ tail -n 30 "$STEP_OUT" >>"$CONSOLE"; } 2>/dev/null || true
}

# install_package <target>: CD의 oplite 패키지를 대상 시스템에 설치한다(의존성은 미러에서).
# apt-get은 질문 없이(noninteractive, conffile은 기본값) stdin 없이 돈다. in-target의 stdin은 설치 프로그램의
# debconf 통로라 dpkg·apt가 거기서 답을 기다리면 무인 설치가 멈춘다. 출력은 --pass-stdout으로 이 로그에 남긴다.
# 그동안 대상에 policy-rc.d(exit 101)를 두어 패키지(ifupdown2 등)가 서비스를 시작하지 못하게 한다.
install_package() {
	mkdir -p "$1/var/cache/oplite"
	if [ -d "$1/run/systemd/system" ]; then
		log "$1/run/systemd/system이 있다: oplite postinst가 opld를 시작하려 할 수 있다"
	fi
	for deb in "$CDROM"/oplite/oplite_*.deb; do
		[ -f "$deb" ] || {
			log "패키지가 없다: $CDROM/oplite/oplite_*.deb"
			return 1
		}
		cp "$deb" "$1/var/cache/oplite/"
		log "설치: $(basename "$deb")"
		mkdir -p "$1/usr/sbin"
		printf '#!/bin/sh\nexit 101\n' >"$1/usr/sbin/policy-rc.d"
		chmod 0755 "$1/usr/sbin/policy-rc.d"
		code=0
		# isc-dhcp-client: ifupdown2가 vmbr0의 DHCP에 dhclient를 쓴다.
		in-target --pass-stdout sh -c "DEBIAN_FRONTEND=noninteractive apt-get -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold install ifupdown2 isc-dhcp-client /var/cache/oplite/$(basename "$deb") </dev/null 2>&1" || code=$?
		rm -f "$1/usr/sbin/policy-rc.d"
		[ "$code" -eq 0 ] || return "$code"
	done
}

# setup_network <target>: 기본 경로 인터페이스를 vmbr0으로 옮긴다. 원본은 설치 환경의 interfaces이고,
# 거기에 lo가 아닌 iface가 없으면 대상의 것이다. 결과는 대상과 설치 환경 둘 다에 써서 뒤에
# netcfg가 설치 환경의 파일을 대상으로 복사해도 바뀐 파일이 남게 한다. NIC 이름은 NIC에 둔다(못 찾으면 빈 값).
setup_network() {
	target_ifaces="$1/etc/network/interfaces"
	installer_ifaces="$INSTALLER_ROOT/etc/network/interfaces"
	src=$installer_ifaces
	has_iface "$src" || src=$target_ifaces
	NIC=""
	if [ ! -f "$src" ]; then
		log "interfaces가 없다($installer_ifaces, $target_ifaces). 네트워크를 바꾸지 않았다"
		return 0
	fi
	log "원래 $src:"
	cat "$src"
	code=0
	convert_interfaces "$src" "$src.oplite" || code=$?
	if [ "$code" -ne 0 ]; then
		rm -f "$src.oplite"
		log "기본 경로 인터페이스를 찾지 못해 interfaces를 바꾸지 않았다"
		return 0
	fi
	NIC=$(default_nic "$src")
	mkdir -p "$1/etc/network" "$INSTALLER_ROOT/etc/network"
	cp "$src.oplite" "$target_ifaces"
	cp "$src.oplite" "$installer_ifaces"
	rm -f "$src.oplite"
	log "$NIC 을 vmbr0으로 옮겼다($target_ifaces, $installer_ifaces):"
	cat "$target_ifaces"
}

# node_name <target>: 호스트 이름(설치 환경, 없으면 대상의 /etc/hostname)을 소문자로
node_name() {
	f="$INSTALLER_ROOT/etc/hostname"
	[ -s "$f" ] || f="$1/etc/hostname"
	[ -s "$f" ] || return 0
	# 설치 환경의 busybox tr은 [:upper:] 같은 문자 집합을 몰라 글자 그대로 바꾼다("opl" → "owl").
	head -n1 "$f" | tr ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz
}

# write_config <target>: /etc/opl/opld.toml. 정적이면 그 주소, DHCP면 지금 받은 주소
write_config() {
	name=$(node_name "$1")
	valid_name "$name" || log "호스트 이름 '$name'이 노드 이름 규칙에 맞지 않아 /etc/opl/opld.toml의 name을 주석으로 둔다"
	address=""
	if [ -n "$NIC" ]; then
		address=$(primary_address "$1/etc/network/interfaces")
	fi
	dhcp=""
	if [ -z "$address" ] && [ -n "$NIC" ]; then
		address=$(current_address "$NIC" || true)
		dhcp=dhcp
	fi
	[ -n "$address" ] || log "주소를 찾지 못해 /etc/opl/opld.toml의 address를 주석으로 둔다. 고친 뒤 opld를 다시 시작한다"
	mkdir -p "$1/etc/opl"
	write_opld_toml "$1/etc/opl/opld.toml" "$name" "$address" "$dhcp"
	log "/etc/opl/opld.toml:"
	cat "$1/etc/opl/opld.toml"

	mkdir -p "$1/etc/issue.d"
	write_issue "$1/etc/issue.d/oplite.issue"
}

# enable_ci_init <target>: 시험 ISO(--unattended)면 첫 부팅에 cluster init을 하는 유닛을 켠다.
enable_ci_init() {
	if [ -f "$CDROM/preseed/oplite-ci-init.service" ]; then
		cp "$CDROM/preseed/oplite-ci-init.service" "$1/etc/systemd/system/"
		in-target systemctl enable oplite-ci-init.service
		log "oplite-ci-init.service를 켰다"
	fi
}

main() {
	target=$1
	mkdir -p "$target/root"
	exec >>"$target/root/oplite-setup.log" 2>&1
	trap 'on_exit $?' EXIT
	log "시작 $(date)"

	step install-package install_package "$target"
	# 네트워크: 기본 경로 인터페이스를 vmbr0으로
	step network setup_network "$target"
	step config write_config "$target"
	step ci-init enable_ci_init "$target"

	log "끝"
}

if [ "${OPLITE_SETUP_LIB:-}" = 1 ]; then
	return 0
fi
main "$@"
