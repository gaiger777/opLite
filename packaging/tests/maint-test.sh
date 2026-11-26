#!/bin/bash
# maintainer 스크립트(postinst·prerm·postrm)가 인자마다 어떤 systemctl 호출을 하는지 본다.
# postinst는 처음 설치여도 orbivisord.service가 enable돼 있으면(옛 oplite에서 넘어옴) 다시 시작하고, oplite가
# 전에 설정된 적이 있으면 처음 설치로 보지 않는다(꺼 둔 데몬을 켜지 않음). 옛 이름(opld.service)의 enable 링크와
# 덮어쓰기 디렉터리는 새 이름으로 옮긴다(계획 74).
# PATH 앞에 둔 가짜 systemctl이 인자를 한 줄씩 기록한다. bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/maint-test.sh
set -eu

DEB_DIR="$(cd "$(dirname "$0")/../deb" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/bin" "$WORK/systemd-running"
# is-enabled는 ORBIVISOR_FAKE_ENABLED(기본 yes)가 no면 실패한다. daemon-reload 때 옛 이름의 enable 링크나 /etc의 진짜
# opld.service가 아직 있으면 "daemon-reload (old names)"로 기록한다 — postinst는 파일을 옮긴 뒤에 reload한다(리뷰 2 N3).
cat >"$WORK/bin/systemctl" <<'EOF'
#!/bin/sh
etc="$ORBIVISOR_SYSTEMD_ETC"
if [ "$1" = daemon-reload ] && { [ -L "$etc/multi-user.target.wants/opld.service" ] ||
	{ [ -f "$etc/opld.service" ] && [ ! -L "$etc/opld.service" ]; }; }; then
	echo "daemon-reload (old names)" >>"$ORBIVISOR_FAKE_LOG"
else
	echo "$*" >>"$ORBIVISOR_FAKE_LOG"
fi
[ "$1" != is-enabled ] || [ "${ORBIVISOR_FAKE_ENABLED:-yes}" = yes ]
EOF
chmod 0755 "$WORK/bin/systemctl"
# dpkg-query -W -f '${Config-Version}' oplite: ORBIVISOR_FAKE_OPLITE_CONFIG(옛 oplite를 설정한 버전)가 있으면 그것을
# 찍고, 없으면 oplite를 모르는 dpkg-query처럼 실패한다. 인자가 postinst가 쓰는 그대로가 아니면(패키지 이름·필드
# 오타) 종료 코드 2로 끝난다. 기록하지 않는다.
cat >"$WORK/bin/dpkg-query" <<'EOF'
#!/bin/sh
[ "$*" = '-W -f ${Config-Version} oplite' ] || exit 2
[ -n "${ORBIVISOR_FAKE_OPLITE_CONFIG:-}" ] || exit 1
printf '%s' "$ORBIVISOR_FAKE_OPLITE_CONFIG"
EOF
chmod 0755 "$WORK/bin/dpkg-query"

FAILED=0

# check <이름> <systemd 돎: yes|no> <기대 호출(줄마다 하나)> <스크립트> [인자...]
check() {
	name=$1 running=$2 expected=$3
	shift 3
	log="$WORK/log"
	: >"$log"
	if [ "$running" = yes ]; then
		systemd_dir="$WORK/systemd-running"
	else
		systemd_dir="$WORK/no-systemd"
	fi
	# ORBIVISOR_SYSTEMD_ETC·ORBIVISOR_APT_SOURCES: postrm이 지우는 enable 링크와 소스 파일이 시험 디렉터리 안에 있게 한다.
	if ! PATH="$WORK/bin:$PATH" ORBIVISOR_FAKE_LOG="$log" ORBIVISOR_SYSTEMD_DIR="$systemd_dir" \
		ORBIVISOR_SYSTEMD_ETC="$WORK/etc-systemd" ORBIVISOR_APT_SOURCES="$WORK/oplite.sources" \
		ORBIVISOR_APPARMOR_DIR="$WORK/apparmor" ORBIVISOR_SYSTEMD_LIB="$WORK/lib-systemd" "$DEB_DIR/$1" "${@:2}"; then
		echo "FAIL $name: 스크립트가 0이 아닌 코드로 끝났다"
		FAILED=1
		return
	fi
	if [ "$(cat "$log")" = "$expected" ]; then
		echo "ok   $name"
	else
		echo "FAIL $name"
		echo "  기대:"
		printf '%s\n' "$expected" | sed 's/^/    /'
		echo "  실제:"
		sed 's/^/    /' "$log"
		FAILED=1
	fi
}

# 처음 설치: enable돼 있지 않으면 켜고 시작한다.
ORBIVISOR_FAKE_ENABLED=no check "처음 설치(systemd 돎)" yes "daemon-reload
is-enabled --quiet orbivisord.service
enable orbivisord.service
start orbivisord.service" postinst configure
check "처음 설치(systemd 없음: 링크만)" no "enable orbivisord.service" postinst configure
ORBIVISOR_FAKE_ENABLED=no check "처음 설치(빈 이전 버전)" yes "daemon-reload
is-enabled --quiet orbivisord.service
enable orbivisord.service
start orbivisord.service" postinst configure ""
# 옛 oplite에서 넘어옴: orbivisor는 처음 설치지만 orbivisord.service가 이미 enable돼 있으면 restart한다.
check "처음 설치인데 enable돼 있음" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst configure
ORBIVISOR_FAKE_OPLITE_CONFIG=0.1.0 check "oplite에서 넘어옴, enable됨" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst configure
# oplite가 전에 설정됐으면 처음 설치가 아니다: 관리자가 꺼 둔 opld는 켜지도 시작하지도 않는다.
ORBIVISOR_FAKE_ENABLED=no ORBIVISOR_FAKE_OPLITE_CONFIG=0.1.0 check "처음 설치, enable 안 됨, oplite가 전에 설정됨" yes "daemon-reload
is-enabled --quiet orbivisord.service" postinst configure
ORBIVISOR_FAKE_OPLITE_CONFIG=0.1.0 check "처음 설치(systemd 없음, oplite가 전에 설정됨, 링크 없음)" no "" postinst configure
# systemd가 돌지 않을 때 enable 링크가 이미 있으면 다시 enable하지 않는다.
mkdir -p "$WORK/etc-systemd/multi-user.target.wants"
ln -sf /usr/lib/systemd/system/orbivisord.service "$WORK/etc-systemd/multi-user.target.wants/orbivisord.service"
check "처음 설치(systemd 없음, 링크 있음)" no "" postinst configure
rm -f "$WORK/etc-systemd/multi-user.target.wants/orbivisord.service"
# 이름 바꾸기(계획 74): 옛 이름의 enable 링크·덮어쓰기를 새 이름으로 옮긴 뒤 daemon-reload를 한 번 한다.
ETC="$WORK/etc-systemd"
WANTS="$ETC/multi-user.target.wants"
mkdir -p "$WANTS" "$ETC/opld.service.d"
ln -sf /usr/lib/systemd/system/opld.service "$WANTS/opld.service"
echo '[Service]' >"$ETC/opld.service.d/override.conf"
check "업그레이드: enable 링크·덮어쓰기 옮기기(옮긴 뒤 reload 한 번)" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst configure 0.1.0
if [ ! -e "$WANTS/opld.service" ] && [ ! -L "$WANTS/opld.service" ] &&
	[ "$(readlink "$WANTS/orbivisord.service")" = /usr/lib/systemd/system/orbivisord.service ] &&
	[ "$(readlink "$ETC/opld.service.d")" = orbivisord.service.d ] &&
	[ -f "$ETC/orbivisord.service.d/override.conf" ] && [ -f "$ETC/opld.service.d/override.conf" ]; then
	echo "ok   enable 링크는 새 이름, 덮어쓰기는 새 이름의 진짜 디렉터리와 옛 이름 링크"
else
	echo "FAIL 옮긴 뒤: $(ls -la "$WANTS" "$ETC")"
	FAILED=1
fi
check "업그레이드: 이미 옮김" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst configure 0.1.0
# 덮어쓰기가 두 이름 다 진짜면 손대지 않는다.
rm -f "$ETC/opld.service.d"
mkdir -p "$ETC/opld.service.d"
check "업그레이드: 덮어쓰기 둘 다 진짜(그대로)" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst configure 0.1.0
if [ -d "$ETC/opld.service.d" ] && [ ! -L "$ETC/opld.service.d" ]; then echo "ok   옛 덮어쓰기 디렉터리 그대로"; else
	echo "FAIL 옛 덮어쓰기 디렉터리를 바꿨다"
	FAILED=1
fi
rm -rf "$ETC"
# chroot(systemd 없음): 링크만 옮긴다.
mkdir -p "$WANTS"
ln -sf /usr/lib/systemd/system/opld.service "$WANTS/opld.service"
check "업그레이드(systemd 없음): 링크만 옮김" no "" postinst configure 0.1.0
if [ -L "$WANTS/orbivisord.service" ] && [ ! -L "$WANTS/opld.service" ]; then echo "ok   chroot에서 enable 링크 옮김"; else
	echo "FAIL chroot에서 enable 링크를 옮기지 않았다"
	FAILED=1
fi
rm -rf "$ETC"
# /etc의 진짜 opld.service(`systemctl edit --full opld`, 리뷰 1 I2): orbivisord.service로 옮기고(내용 그대로) 옛 이름은 링크.
mkdir -p "$WANTS"
printf '[Service]\nExecStart=/usr/bin/orbivisord --config /srv/node.toml\n' >"$ETC/opld.service"
ln -sf "$ETC/opld.service" "$WANTS/opld.service"
check "업그레이드: /etc의 진짜 opld.service를 새 이름으로(옮긴 뒤 reload 한 번)" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst configure 0.1.0
if [ -f "$ETC/orbivisord.service" ] && [ ! -L "$ETC/orbivisord.service" ] &&
	grep -qF -- '--config /srv/node.toml' "$ETC/orbivisord.service" &&
	[ "$(readlink "$ETC/opld.service")" = orbivisord.service ] &&
	[ -L "$WANTS/orbivisord.service" ] && [ ! -L "$WANTS/opld.service" ]; then
	echo "ok   운영자의 유닛 파일은 orbivisord.service, opld.service는 그 링크"
else
	echo "FAIL /etc 유닛 옮긴 뒤: $(ls -la "$ETC")"
	FAILED=1
fi
check "업그레이드: /etc 유닛 이미 옮김" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst configure 0.1.0
rm -rf "$ETC"
# mask한 opld(/dev/null 링크): orbivisord.service도 mask하고 다시 시작·enable하지 않는다.
mkdir -p "$ETC"
ln -s /dev/null "$ETC/opld.service"
check "업그레이드: mask한 opld는 orbivisord도 mask" yes "daemon-reload" postinst configure 0.1.0
if [ "$(readlink "$ETC/orbivisord.service")" = /dev/null ]; then echo "ok   orbivisord.service도 mask"; else
	echo "FAIL orbivisord.service를 mask하지 않았다"
	FAILED=1
fi
ORBIVISOR_FAKE_ENABLED=no check "처음 설치: mask한 opld는 enable하지 않음" yes "daemon-reload" postinst configure
check "처음 설치(systemd 없음): mask한 opld는 enable하지 않음" no "" postinst configure
rm -rf "$ETC"
# 옮기기가 실패하면(읽기 전용 등) 성공으로 끝내지 않는다(리뷰 1 M3) — 다시 시작하지 않고 0이 아닌 코드.
if [ "$(id -u)" != 0 ]; then
	mkdir -p "$WANTS" "$ETC/opld.service.d"
	ln -sf /usr/lib/systemd/system/opld.service "$WANTS/opld.service"
	chmod 0555 "$ETC"
	: >"$WORK/log"
	if PATH="$WORK/bin:$PATH" ORBIVISOR_FAKE_LOG="$WORK/log" ORBIVISOR_SYSTEMD_DIR="$WORK/systemd-running" \
		ORBIVISOR_SYSTEMD_ETC="$ETC" ORBIVISOR_APPARMOR_DIR="$WORK/apparmor" \
		"$DEB_DIR/postinst" configure 0.1.0 2>"$WORK/err"; then
		echo "FAIL 옮기기 실패가 성공으로 끝났다"
		FAILED=1
	elif grep -q restart "$WORK/log" || ! grep -q 'cannot move' "$WORK/err"; then
		echo "FAIL 옮기기 실패 뒤: $(cat "$WORK/log" "$WORK/err")"
		FAILED=1
	else
		echo "ok   옮기기 실패는 알리고 0이 아닌 코드(다시 시작 없음)"
	fi
	chmod 0755 "$ETC"
	rm -rf "$ETC"
fi
# 옛 oplite V0에서 바로 넘어옴: V0의 enable 링크(opld.service)를 옮기고 다시 시작한다.
mkdir -p "$WANTS"
ln -sf /lib/systemd/system/opld.service "$WANTS/opld.service"
ORBIVISOR_FAKE_OPLITE_CONFIG=0.1.0 check "oplite V0에서 바로: 링크 옮기고 restart" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst configure
if [ -L "$WANTS/orbivisord.service" ] && [ ! -L "$WANTS/opld.service" ]; then echo "ok   V0의 enable 링크 옮김"; else
	echo "FAIL V0의 enable 링크를 옮기지 않았다"
	FAILED=1
fi
rm -rf "$ETC"
mkdir -p "$WANTS"
ln -sf /lib/systemd/system/opld.service "$WANTS/opld.service"
check "oplite V0에서 바로(systemd 없음): 링크만, 다시 enable하지 않음" no "" postinst configure
rm -rf "$ETC"

# 업그레이드·재설정: enable돼 있으면 restart한다(debhelper와 같음). remove 뒤 다시 설치한 경우도 같다.
check "업그레이드(systemd 돎)" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst configure 0.1.0
ORBIVISOR_FAKE_ENABLED=no check "업그레이드(enable 안 됨)" yes "daemon-reload
is-enabled --quiet orbivisord.service" postinst configure 0.1.0
check "업그레이드(systemd 없음)" no "" postinst configure 0.1.0
check "remove 뒤 configure <ver>" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst configure 0.1.0
check "postinst abort-remove" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst abort-remove
check "postinst abort-deconfigure" yes "daemon-reload
is-enabled --quiet orbivisord.service
restart orbivisord.service" postinst abort-deconfigure in-favour other 1.0
check "postinst abort-remove(systemd 없음)" no "" postinst abort-remove
check "postinst abort-upgrade" yes "" postinst abort-upgrade 0.1.0

check "prerm remove(systemd 돎)" yes "stop orbivisord.service" prerm remove
check "prerm remove(systemd 없음)" no "" prerm remove
check "prerm deconfigure" yes "stop orbivisord.service" prerm deconfigure
check "prerm upgrade(멈추지 않음)" yes "" prerm upgrade 0.2.0

check "postrm remove(systemd 돎)" yes "daemon-reload" postrm remove
check "postrm remove(systemd 없음)" no "" postrm remove
check "postrm purge(systemd 돎)" yes "disable orbivisord.service
daemon-reload" postrm purge
check "postrm purge(systemd 없음)" no "disable orbivisord.service" postrm purge
check "postrm upgrade" yes "" postrm upgrade 0.2.0

# purge는 enable 링크를 지운다(systemctl disable이 못 지운 경우에도). remove는 남긴다.
WANTS="$WORK/etc-systemd/multi-user.target.wants"
mkdir -p "$WANTS"
ln -sf /usr/lib/systemd/system/orbivisord.service "$WANTS/orbivisord.service"
check "postrm remove(링크 둠)" no "" postrm remove
if [ -L "$WANTS/orbivisord.service" ]; then echo "ok   remove 뒤 enable 링크 남음"; else
	echo "FAIL remove가 enable 링크를 지웠다"
	FAILED=1
fi
check "postrm purge(링크 지움)" no "disable orbivisord.service" postrm purge
if [ ! -e "$WANTS/orbivisord.service" ] && [ ! -L "$WANTS/orbivisord.service" ]; then echo "ok   purge 뒤 enable 링크 없음"; else
	echo "FAIL purge 뒤 enable 링크가 남았다"
	FAILED=1
fi

# 옮기지 못한 옛 이름의 enable 링크도 purge가 지운다.
ln -sf /usr/lib/systemd/system/opld.service "$WANTS/opld.service"
check "postrm purge(옛 이름 링크 지움)" no "disable orbivisord.service" postrm purge
if [ ! -L "$WANTS/opld.service" ]; then echo "ok   purge 뒤 옛 이름 enable 링크 없음"; else
	echo "FAIL purge 뒤 옛 이름 enable 링크가 남았다"
	FAILED=1
fi

# 옛 판(계획 74 앞)으로 되돌림(리뷰 2 N1): 새 postrm upgrade가 OVN 중앙 단위 이름을 되돌린다 — 새 이름 파일은 옛 이름으로,
# 새 이름은 옛 이름으로의 링크로, enable 링크도 옛 이름으로, daemon-reload 한 번(멈추지 않음).
ETC="$WORK/etc-systemd"
WANTS="$ETC/multi-user.target.wants"
LIB="$WORK/lib-systemd"
MARK='# Managed by Orbivisor (orbivisord); changes are overwritten.'
ovn_after_upgrade() {
	rm -rf "$ETC" "$LIB"
	mkdir -p "$WANTS" "$LIB"
	for db in nb sb northd; do
		printf '%s\n[Service]\nExecStart=/x %s\n' "$MARK" "$db" >"$ETC/orbivisor-ovn-$db.service"
		ln -s "orbivisor-ovn-$db.service" "$ETC/opl-ovn-$db.service"
		ln -s "$ETC/orbivisor-ovn-$db.service" "$WANTS/orbivisor-ovn-$db.service"
	done
}
# 옛 판의 유닛: opld.service가 진짜 파일, orbivisord.service는 그 별칭
old_package_unpacked() {
	echo '[Unit]' >"$LIB/opld.service"
	ln -s opld.service "$LIB/orbivisord.service"
}
ovn_after_upgrade
old_package_unpacked
check "postrm upgrade <옛 판>: OVN 단위 되돌림" yes "daemon-reload" postrm upgrade 0.0.9
reverted=yes
for db in nb sb northd; do
	if ! { [ -f "$ETC/opl-ovn-$db.service" ] && [ ! -L "$ETC/opl-ovn-$db.service" ] &&
		grep -qx "ExecStart=/x $db" "$ETC/opl-ovn-$db.service" &&
		[ "$(readlink "$ETC/orbivisor-ovn-$db.service")" = "opl-ovn-$db.service" ] &&
		[ "$(readlink "$WANTS/opl-ovn-$db.service")" = "$ETC/opl-ovn-$db.service" ] &&
		[ ! -L "$WANTS/orbivisor-ovn-$db.service" ]; }; then
		reverted=no
	fi
done
if [ "$reverted" = yes ]; then echo "ok   옛 이름이 진짜 파일, 새 이름은 그 링크, enable 링크는 옛 이름"; else
	echo "FAIL 되돌린 뒤: $(ls -la "$ETC" "$WANTS")"
	FAILED=1
fi
check "postrm upgrade <옛 판>: 이미 되돌림(reload 없음)" yes "" postrm upgrade 0.0.9
ovn_after_upgrade
old_package_unpacked
check "postrm failed-upgrade <옛 판>(systemd 없음): 파일만" no "" postrm failed-upgrade 0.0.9
if [ "$(readlink "$ETC/orbivisor-ovn-nb.service")" = opl-ovn-nb.service ]; then echo "ok   systemd 없이도 되돌림"; else
	echo "FAIL systemd 없이 되돌리지 않았다"
	FAILED=1
fi
# 새 판으로 올림(풀린 판의 opld.service가 링크): 되돌리지 않는다.
ovn_after_upgrade
echo '[Unit]' >"$LIB/orbivisord.service"
ln -s orbivisord.service "$LIB/opld.service"
check "postrm upgrade <새 판>: 그대로" yes "" postrm upgrade 0.2.0
if [ -f "$ETC/orbivisor-ovn-nb.service" ] && [ ! -L "$ETC/orbivisor-ovn-nb.service" ] &&
	[ -L "$ETC/opl-ovn-nb.service" ]; then echo "ok   새 판으로 올릴 때 OVN 단위 그대로"; else
	echo "FAIL 새 판으로 올릴 때 OVN 단위를 바꿨다"
	FAILED=1
fi
# 표시 없는 새 이름 파일, 옛 이름이 진짜 파일(이미 갈라짐)은 손대지 않는다.
ovn_after_upgrade
old_package_unpacked
echo '[Unit]' >"$ETC/orbivisor-ovn-nb.service"
rm -f "$ETC/opl-ovn-sb.service"
printf '%s\n' "$MARK" >"$ETC/opl-ovn-sb.service"
check "postrm upgrade <옛 판>: 남의 파일·갈라진 단위는 그대로" yes "daemon-reload" postrm upgrade 0.0.9
if [ -L "$ETC/opl-ovn-nb.service" ] && [ ! -L "$ETC/orbivisor-ovn-nb.service" ] &&
	[ ! -L "$ETC/orbivisor-ovn-sb.service" ] && [ ! -L "$ETC/opl-ovn-sb.service" ] &&
	[ "$(readlink "$ETC/orbivisor-ovn-northd.service")" = opl-ovn-northd.service ]; then
	echo "ok   northd만 되돌림"
else
	echo "FAIL 남의 파일·갈라진 단위를 바꿨다: $(ls -la "$ETC")"
	FAILED=1
fi
rm -rf "$ETC" "$LIB"

# remove·purge는 opld가 쓴 소스 파일(관리 표시)을 지우고, 관리자가 쓴 파일과 업그레이드 때는 남긴다.
SOURCES="$WORK/oplite.sources"
managed() { printf '# managed by opld; edits are overwritten\nTypes: deb\n' >"$SOURCES"; }
own() { printf 'Types: deb\nURIs: http://mirror.lan/oplite\n' >"$SOURCES"; }
for action in remove purge; do
	managed
	check "postrm $action(관리 소스 파일)" no "$([ "$action" = purge ] && echo "disable orbivisord.service")" postrm "$action"
	if [ ! -e "$SOURCES" ]; then echo "ok   $action 뒤 관리 소스 파일 없음"; else
		echo "FAIL $action 뒤 관리 소스 파일이 남았다"
		FAILED=1
	fi
	own
	check "postrm $action(관리자 소스 파일)" no "$([ "$action" = purge ] && echo "disable orbivisord.service")" postrm "$action"
	if grep -q mirror.lan "$SOURCES"; then echo "ok   $action 뒤 관리자 소스 파일 남음"; else
		echo "FAIL $action가 관리자 소스 파일을 지웠다"
		FAILED=1
	fi
done
managed
check "postrm upgrade(소스 파일 둠)" no "" postrm upgrade 0.2.0
if [ -e "$SOURCES" ]; then echo "ok   upgrade 뒤 소스 파일 남음"; else
	echo "FAIL upgrade가 소스 파일을 지웠다"
	FAILED=1
fi
rm -f "$SOURCES"

# swtpm AppArmor(계획 59): postinst는 swtpm 프로필의 local 파일에 include 줄을 한 번만 더하고, 프로필이 있고
# AppArmor가 켜져 있으면 다시 읽힌다. purge는 그 줄만 뺀다.
AA="$WORK/apparmor"
LOCAL="$AA/local/usr.bin.swtpm"
INCLUDE='include <abstractions/orbivisor-swtpm>'
rm -rf "$AA"
check "AppArmor 프로필 없음(local 파일만)" no "" postinst configure 0.1.0
check "AppArmor 두 번째 configure" no "" postinst configure 0.1.0
if [ "$(grep -cxF "$INCLUDE" "$LOCAL")" = 1 ]; then echo "ok   local 파일에 include 한 줄"; else
	echo "FAIL local 파일: $(cat "$LOCAL")"
	FAILED=1
fi
# 새 경로 규칙도 local 파일에 한 번씩(리뷰 2 N2 — 옛 판 conffile·고친 conffile에도 남게)
RULES='/var/lib/orbivisor/vm/*/tpm/ rw,
/var/lib/orbivisor/vm/*/tpm/** rwk,
/run/orbivisor/vm/*/ r,
/run/orbivisor/vm/*/tpm.sock rw,
/run/orbivisor/vm/*/swtpm.pid rw,
/run/orbivisor/vm/*/swtpm.log rw,'
rules_once=yes
while IFS= read -r rule; do
	[ "$(grep -cxF "$rule" "$LOCAL")" = 1 ] || rules_once=no
done <<<"$RULES"
if [ "$rules_once" = yes ] && [ "$(wc -l <"$LOCAL" | tr -d ' ')" = 7 ]; then echo "ok   local 파일에 새 경로 규칙 한 번씩"; else
	echo "FAIL local 파일의 규칙: $(cat "$LOCAL")"
	FAILED=1
fi
# 규칙 목록이 conffile의 새 경로 규칙과 같다.
if [ "$(grep -E '^ +/(var/lib|run)/orbivisor/' "$DEB_DIR/apparmor/orbivisor-swtpm" | sed 's/^ *//')" = "$RULES" ]; then
	echo "ok   local 규칙이 conffile의 새 경로 규칙과 같음"
else
	echo "FAIL local 규칙과 conffile이 다르다"
	FAILED=1
fi
printf '#include <tunables/global>\n' >"$AA/usr.bin.swtpm"
# 가짜 명령의 본문이라 $는 가짜 명령이 돌 때 풀린다(작은따옴표 그대로).
# shellcheck disable=SC2016
printf '#!/bin/sh\n[ "${ORBIVISOR_FAKE_AA:-yes}" = yes ]\n' >"$WORK/bin/aa-enabled"
# shellcheck disable=SC2016
printf '#!/bin/sh\necho "apparmor_parser $*" >>"$ORBIVISOR_FAKE_LOG"\n' >"$WORK/bin/apparmor_parser"
chmod 0755 "$WORK/bin/aa-enabled" "$WORK/bin/apparmor_parser"
check "AppArmor 켜짐: 프로필을 다시 읽힘" no "apparmor_parser -r -T -W $AA/usr.bin.swtpm" postinst configure 0.1.0
ORBIVISOR_FAKE_AA=no check "AppArmor 꺼짐: 읽히지 않음" no "" postinst configure 0.1.0
echo '/srv/tpm/** rwk,' >>"$LOCAL"
check "postrm remove(include 둠)" no "" postrm remove
check "postrm purge(include 뺌)" no "disable orbivisord.service
apparmor_parser -r -T -W $AA/usr.bin.swtpm" postrm purge
if ! grep -qxF "$INCLUDE" "$LOCAL" && grep -qxF '/srv/tpm/** rwk,' "$LOCAL" &&
	! grep -qF /orbivisor/ "$LOCAL" && [ "$(wc -l <"$LOCAL" | tr -d ' ')" = 1 ]; then
	echo "ok   purge가 include 줄과 새 경로 규칙만 뺐다"
else
	echo "FAIL purge 뒤 local 파일: $(cat "$LOCAL")"
	FAILED=1
fi
rm -f "$WORK/bin/aa-enabled" "$WORK/bin/apparmor_parser"
rm -rf "$AA"

# 가짜 systemctl이 실패해도 maintainer 스크립트는 성공해야 한다(|| true).
cat >"$WORK/bin/systemctl" <<'EOF'
#!/bin/sh
echo "$*" >>"$ORBIVISOR_FAKE_LOG"
exit 1
EOF
check "systemctl 실패를 무시(postinst)" yes "daemon-reload
is-enabled --quiet orbivisord.service
enable orbivisord.service
start orbivisord.service" postinst configure
check "systemctl 실패를 무시(postrm purge)" yes "disable orbivisord.service
daemon-reload" postrm purge

if [ "$FAILED" -ne 0 ]; then
	echo "maint-test: 실패"
	exit 1
fi
echo "maint-test: 모두 통과"
