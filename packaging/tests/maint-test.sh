#!/bin/bash
# maintainer 스크립트(postinst·prerm·postrm)가 인자마다 어떤 systemctl 호출을 하는지 본다.
# postinst는 처음 설치여도 opld.service가 enable돼 있으면(옛 oplite에서 넘어옴) 다시 시작하고, oplite가
# 전에 설정된 적이 있으면 처음 설치로 보지 않는다(꺼 둔 opld를 켜지 않음).
# PATH 앞에 둔 가짜 systemctl이 인자를 한 줄씩 기록한다. bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/maint-test.sh
set -eu

DEB_DIR="$(cd "$(dirname "$0")/../deb" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/bin" "$WORK/systemd-running"
# is-enabled는 OPL_FAKE_ENABLED(기본 yes)가 no면 실패한다.
cat >"$WORK/bin/systemctl" <<'EOF'
#!/bin/sh
echo "$*" >>"$OPL_FAKE_LOG"
[ "$1" != is-enabled ] || [ "${OPL_FAKE_ENABLED:-yes}" = yes ]
EOF
chmod 0755 "$WORK/bin/systemctl"
# dpkg-query -W -f '${Config-Version}' oplite: OPL_FAKE_OPLITE_CONFIG(옛 oplite를 설정한 버전)가 있으면 그것을
# 찍고, 없으면 oplite를 모르는 dpkg-query처럼 실패한다. 인자가 postinst가 쓰는 그대로가 아니면(패키지 이름·필드
# 오타) 종료 코드 2로 끝난다. 기록하지 않는다.
cat >"$WORK/bin/dpkg-query" <<'EOF'
#!/bin/sh
[ "$*" = '-W -f ${Config-Version} oplite' ] || exit 2
[ -n "${OPL_FAKE_OPLITE_CONFIG:-}" ] || exit 1
printf '%s' "$OPL_FAKE_OPLITE_CONFIG"
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
	# OPL_SYSTEMD_ETC·OPL_APT_SOURCES: postrm이 지우는 enable 링크와 소스 파일이 시험 디렉터리 안에 있게 한다.
	if ! PATH="$WORK/bin:$PATH" OPL_FAKE_LOG="$log" OPL_SYSTEMD_DIR="$systemd_dir" \
		OPL_SYSTEMD_ETC="$WORK/etc-systemd" OPL_APT_SOURCES="$WORK/oplite.sources" "$DEB_DIR/$1" "${@:2}"; then
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
OPL_FAKE_ENABLED=no check "처음 설치(systemd 돎)" yes "daemon-reload
is-enabled --quiet opld.service
enable opld.service
start opld.service" postinst configure
check "처음 설치(systemd 없음: 링크만)" no "enable opld.service" postinst configure
OPL_FAKE_ENABLED=no check "처음 설치(빈 이전 버전)" yes "daemon-reload
is-enabled --quiet opld.service
enable opld.service
start opld.service" postinst configure ""
# 옛 oplite에서 넘어옴: orbivisor는 처음 설치지만 opld.service가 이미 enable돼 있으면 restart한다.
check "처음 설치인데 enable돼 있음" yes "daemon-reload
is-enabled --quiet opld.service
restart opld.service" postinst configure
OPL_FAKE_OPLITE_CONFIG=0.1.0 check "oplite에서 넘어옴, enable됨" yes "daemon-reload
is-enabled --quiet opld.service
restart opld.service" postinst configure
# oplite가 전에 설정됐으면 처음 설치가 아니다: 관리자가 꺼 둔 opld는 켜지도 시작하지도 않는다.
OPL_FAKE_ENABLED=no OPL_FAKE_OPLITE_CONFIG=0.1.0 check "처음 설치, enable 안 됨, oplite가 전에 설정됨" yes "daemon-reload
is-enabled --quiet opld.service" postinst configure
OPL_FAKE_OPLITE_CONFIG=0.1.0 check "처음 설치(systemd 없음, oplite가 전에 설정됨, 링크 없음)" no "" postinst configure
# systemd가 돌지 않을 때 enable 링크가 이미 있으면 다시 enable하지 않는다.
mkdir -p "$WORK/etc-systemd/multi-user.target.wants"
ln -sf /usr/lib/systemd/system/opld.service "$WORK/etc-systemd/multi-user.target.wants/opld.service"
check "처음 설치(systemd 없음, 링크 있음)" no "" postinst configure
rm -f "$WORK/etc-systemd/multi-user.target.wants/opld.service"
# 업그레이드·재설정: enable돼 있으면 restart한다(debhelper와 같음). remove 뒤 다시 설치한 경우도 같다.
check "업그레이드(systemd 돎)" yes "daemon-reload
is-enabled --quiet opld.service
restart opld.service" postinst configure 0.1.0
OPL_FAKE_ENABLED=no check "업그레이드(enable 안 됨)" yes "daemon-reload
is-enabled --quiet opld.service" postinst configure 0.1.0
check "업그레이드(systemd 없음)" no "" postinst configure 0.1.0
check "remove 뒤 configure <ver>" yes "daemon-reload
is-enabled --quiet opld.service
restart opld.service" postinst configure 0.1.0
check "postinst abort-remove" yes "daemon-reload
is-enabled --quiet opld.service
restart opld.service" postinst abort-remove
check "postinst abort-deconfigure" yes "daemon-reload
is-enabled --quiet opld.service
restart opld.service" postinst abort-deconfigure in-favour other 1.0
check "postinst abort-remove(systemd 없음)" no "" postinst abort-remove
check "postinst abort-upgrade" yes "" postinst abort-upgrade 0.1.0

check "prerm remove(systemd 돎)" yes "stop opld.service" prerm remove
check "prerm remove(systemd 없음)" no "" prerm remove
check "prerm deconfigure" yes "stop opld.service" prerm deconfigure
check "prerm upgrade(멈추지 않음)" yes "" prerm upgrade 0.2.0

check "postrm remove(systemd 돎)" yes "daemon-reload" postrm remove
check "postrm remove(systemd 없음)" no "" postrm remove
check "postrm purge(systemd 돎)" yes "disable opld.service
daemon-reload" postrm purge
check "postrm purge(systemd 없음)" no "disable opld.service" postrm purge
check "postrm upgrade" yes "" postrm upgrade 0.2.0

# purge는 enable 링크를 지운다(systemctl disable이 못 지운 경우에도). remove는 남긴다.
WANTS="$WORK/etc-systemd/multi-user.target.wants"
mkdir -p "$WANTS"
ln -sf /usr/lib/systemd/system/opld.service "$WANTS/opld.service"
check "postrm remove(링크 둠)" no "" postrm remove
if [ -L "$WANTS/opld.service" ]; then echo "ok   remove 뒤 enable 링크 남음"; else
	echo "FAIL remove가 enable 링크를 지웠다"
	FAILED=1
fi
check "postrm purge(링크 지움)" no "disable opld.service" postrm purge
if [ ! -e "$WANTS/opld.service" ] && [ ! -L "$WANTS/opld.service" ]; then echo "ok   purge 뒤 enable 링크 없음"; else
	echo "FAIL purge 뒤 enable 링크가 남았다"
	FAILED=1
fi

# remove·purge는 opld가 쓴 소스 파일(관리 표시)을 지우고, 관리자가 쓴 파일과 업그레이드 때는 남긴다.
SOURCES="$WORK/oplite.sources"
managed() { printf '# managed by opld; edits are overwritten\nTypes: deb\n' >"$SOURCES"; }
own() { printf 'Types: deb\nURIs: http://mirror.lan/oplite\n' >"$SOURCES"; }
for action in remove purge; do
	managed
	check "postrm $action(관리 소스 파일)" no "$([ "$action" = purge ] && echo "disable opld.service")" postrm "$action"
	if [ ! -e "$SOURCES" ]; then echo "ok   $action 뒤 관리 소스 파일 없음"; else
		echo "FAIL $action 뒤 관리 소스 파일이 남았다"
		FAILED=1
	fi
	own
	check "postrm $action(관리자 소스 파일)" no "$([ "$action" = purge ] && echo "disable opld.service")" postrm "$action"
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

# 가짜 systemctl이 실패해도 maintainer 스크립트는 성공해야 한다(|| true).
cat >"$WORK/bin/systemctl" <<'EOF'
#!/bin/sh
echo "$*" >>"$OPL_FAKE_LOG"
exit 1
EOF
check "systemctl 실패를 무시(postinst)" yes "daemon-reload
is-enabled --quiet opld.service
enable opld.service
start opld.service" postinst configure
check "systemctl 실패를 무시(postrm purge)" yes "disable opld.service
daemon-reload" postrm purge

if [ "$FAILED" -ne 0 ]; then
	echo "maint-test: 실패"
	exit 1
fi
echo "maint-test: 모두 통과"
