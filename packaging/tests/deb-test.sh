#!/bin/bash
# orbivisor 패키지와 전환 패키지 oplite를 Debian 13 컨테이너 안에서 설치해 본다(root, systemd는 돌지 않음).
# 쓰는 법(CI): docker run --rm -v "$PWD:/work" debian:13 bash /work/packaging/tests/deb-test.sh /work/<orbivisor deb> /work/<oplite deb>
set -eu

[ $# -eq 2 ] || {
	echo "usage: $0 <orbivisor deb> <oplite deb>" >&2
	exit 2
}
DEB=$1
TRANS_DEB=$2

fail() {
	echo "deb-test: FAIL: $*" >&2
	exit 1
}

# 설치: 의존성은 미러에서 받는다.
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends "$DEB"

VERSION=$(dpkg-query -W -f '${Version}' orbivisor)
echo "deb-test: orbivisor $VERSION"

# 바이너리가 패키지 버전을 보인다(OPL_VERSION으로 빌드). 옛 이름(링크)으로 불러도 --version의 이름은
# clap의 name(새 이름)이다 — 사용법 줄만 argv[0]을 따른다.
[ "$(orbivisord --version)" = "orbivisord $VERSION" ] || fail "orbivisord --version: $(orbivisord --version)"
[ "$(orbivisorctl --version)" = "orbivisorctl $VERSION" ] || fail "orbivisorctl --version: $(orbivisorctl --version)"
[ "$(opld --version)" = "orbivisord $VERSION" ] || fail "opld --version: $(opld --version)"
[ "$(oplctl --version)" = "orbivisorctl $VERSION" ] || fail "oplctl --version: $(oplctl --version)"

# 경로와 권한
UNIT=/usr/lib/systemd/system/opld.service
for path in /usr/bin/orbivisord /usr/bin/orbivisorctl; do
	[ "$(stat -c '%a %U' "$path")" = "755 root" ] || fail "$path: $(stat -c '%a %U' "$path")"
done
for path in "$UNIT" /usr/share/orbivisor/opld.toml.example /usr/share/doc/orbivisor/copyright; do
	[ "$(stat -c '%a %U' "$path")" = "644 root" ] || fail "$path: $(stat -c '%a %U' "$path")"
done
# 옛 이름은 상대 심볼릭 링크
[ "$(readlink /usr/bin/opld)" = orbivisord ] || fail "/usr/bin/opld: $(readlink /usr/bin/opld)"
[ "$(readlink /usr/bin/oplctl)" = orbivisorctl ] || fail "/usr/bin/oplctl: $(readlink /usr/bin/oplctl)"
ALIAS=/usr/lib/systemd/system/orbivisord.service
[ "$(readlink "$ALIAS")" = opld.service ] || fail "$ALIAS: $(readlink "$ALIAS")"

# md5sums(dpkg --verify)
[ -s /var/lib/dpkg/info/orbivisor.md5sums ] || fail "md5sums 없음"
dpkg --verify orbivisor || fail "dpkg --verify orbivisor"

# 유닛 파일 검사
systemd-analyze verify "$UNIT" || fail "systemd-analyze verify $UNIT"
grep -qx 'ExecStart=/usr/bin/orbivisord --config /etc/opl/opld.toml' "$UNIT" || fail "ExecStart: $(grep ExecStart "$UNIT")"

# 설정이 없으면 유닛 조건이 거짓이다(설치가 /etc/opl/opld.toml을 만들지 않는다).
grep -qx 'ConditionPathExists=/etc/opl/opld.toml' "$UNIT" || fail "ConditionPathExists 없음"
[ ! -e /etc/opl/opld.toml ] || fail "/etc/opl/opld.toml이 있다"

# systemd가 돌지 않아도 postinst가 enable 링크를 만든다.
LINK=/etc/systemd/system/multi-user.target.wants/opld.service
[ -L "$LINK" ] || fail "enable 링크 없음"

# 전환 패키지: orbivisor 같은 버전에 의존하고 유지 스크립트가 없다. 지워도 orbivisor는 그대로다.
apt-get install -y --no-install-recommends "$TRANS_DEB"
[ "$(dpkg-query -W -f '${Version}' oplite)" = "$VERSION" ] || fail "oplite 버전: $(dpkg-query -W -f '${Version}' oplite)"
[ "$(dpkg-deb -f "$TRANS_DEB" Depends)" = "orbivisor (= $VERSION)" ] || fail "oplite Depends: $(dpkg-deb -f "$TRANS_DEB" Depends)"
for script in preinst postinst prerm postrm; do
	[ ! -e "/var/lib/dpkg/info/oplite.$script" ] || fail "전환 패키지에 $script가 있다"
done
dpkg --verify oplite || fail "dpkg --verify oplite"
apt-get remove -y oplite
[ "$(dpkg-query -W -f '${db:Status-Status}' oplite 2>/dev/null || true)" != installed ] || fail "oplite가 지워지지 않음"
[ "$(dpkg-query -W -f '${Status}' orbivisor)" = "install ok installed" ] || fail "oplite를 지우자 orbivisor가 바뀜"
[ -L "$LINK" ] || fail "oplite를 지운 뒤 enable 링크 없음"
[ -x /usr/bin/opld ] || fail "oplite를 지운 뒤 /usr/bin/opld 없음"

# purge: enable 링크와 opld가 쓴 APT 소스 파일은 지우고, 데이터(/var/lib/opl)와 설정(/etc/opl)은 남긴다.
mkdir -p /var/lib/opl /etc/opl
echo keep >/var/lib/opl/deb-test.marker
echo keep >/etc/opl/deb-test.marker
SOURCES=/etc/apt/sources.list.d/oplite.sources
printf '# managed by opld; edits are overwritten\nTypes: deb\n' >"$SOURCES"
dpkg --purge orbivisor
[ ! -e "$SOURCES" ] || fail "purge 뒤 opld가 쓴 소스 파일이 남음"
if [ -e "$LINK" ] || [ -L "$LINK" ]; then fail "purge 뒤 enable 링크가 남음"; fi
[ -f /var/lib/opl/deb-test.marker ] || fail "purge가 /var/lib/opl을 지웠다"
[ -f /etc/opl/deb-test.marker ] || fail "purge가 /etc/opl을 지웠다"

echo "deb-test: 모두 통과"
