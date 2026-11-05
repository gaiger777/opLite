#!/bin/bash
# oplite 패키지를 Debian 13 컨테이너 안에서 설치해 본다(root, systemd는 돌지 않음).
# 쓰는 법(CI): docker run --rm -v "$PWD:/work" debian:13 bash /work/packaging/tests/deb-test.sh /work/<deb>
set -eu

[ $# -eq 1 ] || {
	echo "usage: $0 <oplite deb>" >&2
	exit 2
}
DEB=$1

fail() {
	echo "deb-test: FAIL: $*" >&2
	exit 1
}

# 설치: 의존성은 미러에서 받는다.
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends "$DEB"

VERSION=$(dpkg-query -W -f '${Version}' oplite)
echo "deb-test: oplite $VERSION"

# 바이너리가 패키지 버전을 보인다(OPL_VERSION으로 빌드).
[ "$(opld --version)" = "opld $VERSION" ] || fail "opld --version: $(opld --version)"
[ "$(oplctl --version)" = "oplctl $VERSION" ] || fail "oplctl --version: $(oplctl --version)"

# 경로와 권한
UNIT=/usr/lib/systemd/system/opld.service
for path in /usr/bin/opld /usr/bin/oplctl; do
	[ "$(stat -c '%a %U' "$path")" = "755 root" ] || fail "$path: $(stat -c '%a %U' "$path")"
done
for path in "$UNIT" /usr/share/oplite/opld.toml.example /usr/share/doc/oplite/copyright; do
	[ "$(stat -c '%a %U' "$path")" = "644 root" ] || fail "$path: $(stat -c '%a %U' "$path")"
done

# md5sums(dpkg --verify)
[ -s /var/lib/dpkg/info/oplite.md5sums ] || fail "md5sums 없음"
dpkg --verify oplite || fail "dpkg --verify oplite"

# 유닛 파일 검사
systemd-analyze verify "$UNIT" || fail "systemd-analyze verify $UNIT"

# 설정이 없으면 유닛 조건이 거짓이다(설치가 /etc/opl/opld.toml을 만들지 않는다).
grep -qx 'ConditionPathExists=/etc/opl/opld.toml' "$UNIT" || fail "ConditionPathExists 없음"
[ ! -e /etc/opl/opld.toml ] || fail "/etc/opl/opld.toml이 있다"

# systemd가 돌지 않아도 postinst가 enable 링크를 만든다.
[ -L /etc/systemd/system/multi-user.target.wants/opld.service ] || fail "enable 링크 없음"

# purge: enable 링크와 opld가 쓴 APT 소스 파일은 지우고, 데이터(/var/lib/opl)와 설정(/etc/opl)은 남긴다.
mkdir -p /var/lib/opl /etc/opl
echo keep >/var/lib/opl/deb-test.marker
echo keep >/etc/opl/deb-test.marker
SOURCES=/etc/apt/sources.list.d/oplite.sources
printf '# managed by opld; edits are overwritten\nTypes: deb\n' >"$SOURCES"
dpkg --purge oplite
[ ! -e "$SOURCES" ] || fail "purge 뒤 opld가 쓴 소스 파일이 남음"
LINK=/etc/systemd/system/multi-user.target.wants/opld.service
if [ -e "$LINK" ] || [ -L "$LINK" ]; then fail "purge 뒤 enable 링크가 남음"; fi
[ -f /var/lib/opl/deb-test.marker ] || fail "purge가 /var/lib/opl을 지웠다"
[ -f /etc/opl/deb-test.marker ] || fail "purge가 /etc/opl을 지웠다"

echo "deb-test: 모두 통과"
