#!/bin/bash
# build-repo.sh로 만든 저장소를 진짜 apt가 읽는지, 옛 oplite가 dist-upgrade 한 번으로 orbivisor로 넘어가는지
# Debian 13 컨테이너 안에서 본다(root, systemd는 돌지 않음).
# 임시 GPG 키로 stable에 옛 모양의 oplite V0(이름 바꾸기 전 build-deb.sh가 만들던 것: 실제 파일 /usr/bin/opld·
# oplctl, 옛 control·유지 스크립트 — fixtures/oplite-v0)를, testing에 받은 V1의 orbivisor·oplite(전환)를 넣는다.
# file:// 소스와 Signed-By로 stable에서 oplite를 설치한 뒤 소스를 testing으로 바꾸면 apt-get -s dist-upgrade가
# 두 패키지를 보이고, 실제 dist-upgrade 뒤 orbivisor가 옛 유닛·enable 링크·명령 이름·저장소 키를 이어받는지,
# dpkg가 oplite를 먼저 올리고 orbivisor를 설치했는지, orbivisor가 수동 설치로 남아 autoremove되지 않는지 본다.
# V1 orbivisor는 받은 deb의 바이너리로 build-deb.sh를 OPL_APT_KEY와 함께 다시 돌려 만든다(키를 실은 정식 패키지처럼).
# 쓰는 법(CI): docker run --rm -v "$PWD:/work" debian:13 bash /work/packaging/tests/repo-test.sh /work/<orbivisor deb> /work/<oplite deb>
set -eu

[ $# -eq 2 ] || {
	echo "usage: $0 <orbivisor deb> <oplite deb>" >&2
	exit 2
}
DEB=$1
TRANS_DEB=$2
TESTS_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_SCRIPT="$TESTS_DIR/../repo/build-repo.sh"
DEB_DIR="$TESTS_DIR/../deb"
V0_DIR="$TESTS_DIR/fixtures/oplite-v0"

fail() {
	echo "repo-test: FAIL: $*" >&2
	exit 1
}

export DEBIAN_FRONTEND=noninteractive
apt-get update
# libssl3t64: build-deb.sh가 받은 orbivisord의 --version을 돌린다.
apt-get install -y --no-install-recommends apt-utils gnupg gpgv libssl3t64

# apt의 _apt 사용자도 저장소와 키링을 읽도록 0755 디렉터리에 둔다.
WORK=$(mktemp -d)
chmod 0755 "$WORK"
trap 'rm -rf "$WORK"' EXIT
REPO="$WORK/repo"
KEYRING="$WORK/oplite-archive-keyring.gpg"

# 임시 서명 키(암호 없음)와 apt가 읽을 키링
export GNUPGHOME="$WORK/gnupg"
mkdir -m 0700 "$GNUPGHOME"
gpg --batch --passphrase '' --quick-generate-key 'Orbivisor repo test <repo-test@orbivisor.invalid>' ed25519 sign never
KEY_ID=$(gpg --batch --with-colons --list-secret-keys | awk -F: '$1 == "fpr" { print $10; exit }')
[ -n "$KEY_ID" ] || fail "임시 키 지문을 못 찾음"
gpg --batch --export "$KEY_ID" >"$KEYRING"
chmod 0644 "$KEYRING"

VERSION=$(dpkg-deb -f "$DEB" Version)
[ "$(dpkg-deb -f "$TRANS_DEB" Version)" = "$VERSION" ] || fail "두 deb의 버전이 다름"
# V0는 V1보다 낮은 버전(~는 무엇보다 앞선다).
V0="$VERSION~v0"
# 저장소 공개 키(V0도 V1도 이 경로에 싣는다 — 옛 이름 그대로)
APT_KEYRING=/usr/share/keyrings/oplite-archive-keyring.gpg

# V1 orbivisor를 저장소 키와 함께 다시 만든다: 받은 deb의 바이너리로 build-deb.sh(OPL_APT_KEY, 시험 키).
dpkg-deb -x "$DEB" "$WORK/v1-files"
OPL_DEV_LICENSE=1 OPL_APT_KEY="$KEYRING" OPL_BIN_DIR="$WORK/v1-files/usr/bin" \
	sh "$DEB_DIR/build-deb.sh" "$VERSION" "$WORK/v1" >/dev/null
DEB="$WORK/v1/orbivisor_${VERSION}_amd64.deb"
dpkg-deb -c "$DEB" | grep -q " \.$APT_KEYRING\$" || fail "다시 만든 orbivisor에 키 없음"

# 옛 oplite V0: 이름 바꾸기 전 build-deb.sh의 트리(바이너리는 버전을 찍는 스크립트)
P="$WORK/v0"
mkdir -p "$P/usr/bin" "$P/usr/lib/systemd/system" "$P/usr/share/oplite" "$P/usr/share/doc/oplite"
for bin in opld oplctl; do
	printf '#!/bin/sh\necho "%s %s"\n' "$bin" "$V0" >"$P/usr/bin/$bin"
	chmod 0755 "$P/usr/bin/$bin"
done
install -m 0644 "$V0_DIR/opld.service" "$P/usr/lib/systemd/system/opld.service"
install -m 0644 "$DEB_DIR/opld.toml.example" "$P/usr/share/oplite/opld.toml.example"
install -m 0644 "$DEB_DIR/copyright" "$P/usr/share/doc/oplite/copyright"
mkdir -p "$P/usr/share/keyrings"
install -m 0644 "$KEYRING" "$P$APT_KEYRING"
SIZE=$(du -sk "$P" | cut -f1)
mkdir -p "$P/DEBIAN"
sed -e "s/@VERSION@/$V0/" -e "s/@INSTALLED_SIZE@/$SIZE/" "$V0_DIR/control.in" >"$P/DEBIAN/control"
(cd "$P" && find usr -type f | LC_ALL=C sort | xargs md5sum) >"$P/DEBIAN/md5sums"
for script in postinst prerm postrm; do
	install -m 0755 "$V0_DIR/$script" "$P/DEBIAN/$script"
done
V0_DEB="$WORK/oplite_${V0}_amd64.deb"
dpkg-deb --build --root-owner-group "$P" "$V0_DEB"

# stable: 옛 oplite V0, testing: V1의 orbivisor와 전환 oplite
OPL_REPO_GPG_KEY_ID=$KEY_ID sh "$REPO_SCRIPT" "$REPO" stable "$V0_DEB"
OPL_REPO_GPG_KEY_ID=$KEY_ID sh "$REPO_SCRIPT" "$REPO" testing "$DEB" "$TRANS_DEB"

# 서명과 Release 값(Origin·Label은 이름 바꾸기 전 그대로 — 바뀌면 설치된 노드의 apt-get update가 실패한다)
gpgv --keyring "$KEYRING" "$REPO/dists/trixie/InRelease" || fail "InRelease 서명"
gpgv --keyring "$KEYRING" "$REPO/dists/trixie/Release.gpg" "$REPO/dists/trixie/Release" || fail "Release.gpg 서명"
grep -qx 'Origin: opLite' "$REPO/dists/trixie/Release" || fail "Release Origin"
grep -qx 'Label: opLite' "$REPO/dists/trixie/Release" || fail "Release Label"
grep -qx 'Suite: trixie' "$REPO/dists/trixie/Release" || fail "Release Suite"
grep -qx 'Codename: trixie' "$REPO/dists/trixie/Release" || fail "Release Codename"
grep -qx 'Components: stable testing' "$REPO/dists/trixie/Release" || fail "Release Components"
grep -qx 'Architectures: amd64' "$REPO/dists/trixie/Release" || fail "Release Architectures"

# write_sources <channel>: opld가 쓰는 것과 같은 모양의 deb822 소스 파일(URIs만 file://)
SOURCES=/etc/apt/sources.list.d/oplite.sources
write_sources() {
	cat >"$SOURCES" <<EOF
# managed by opld; edits are overwritten
Types: deb
URIs: file://$REPO
Suites: trixie
Components: $1
Signed-By: $KEYRING
EOF
}

# stable에서 옛 oplite 설치: 옛 postinst가 enable 링크를 만든다.
LINK=/etc/systemd/system/multi-user.target.wants/opld.service
UNIT=/usr/lib/systemd/system/opld.service
write_sources stable
apt-get update
apt-get install -y --no-install-recommends oplite
[ "$(dpkg-query -W -f '${Version}' oplite)" = "$V0" ] || fail "설치된 버전: $(dpkg-query -W -f '${Version}' oplite)"
if [ -L /usr/bin/opld ] || [ ! -f /usr/bin/opld ]; then fail "V0의 /usr/bin/opld가 파일이 아님"; fi
[ -L "$LINK" ] || fail "V0 enable 링크 없음"
dpkg -S "$APT_KEYRING" | grep -qxE "oplite(:amd64)?: $APT_KEYRING" || fail "V0 키: $(dpkg -S "$APT_KEYRING" 2>&1)"

# testing으로 바꾸면 dist-upgrade가 orbivisor를 새로 설치하고 oplite를 전환 패키지로 올린다
# (opld의 parse_simulate가 읽는 줄 모양).
write_sources testing
apt-get update
LANG=C apt-get -s -q dist-upgrade | tee "$WORK/simulate.txt"
grep -qF "Inst oplite [$V0] ($VERSION " "$WORK/simulate.txt" || fail "dist-upgrade에 oplite $VERSION 없음"
grep -qF "Inst orbivisor ($VERSION " "$WORK/simulate.txt" || fail "dist-upgrade에 orbivisor $VERSION 없음"

# 업그레이드 Task처럼 실제 dist-upgrade(시험 시간을 줄이려고 Recommends만 빼고)
DPKG_LOG=/var/log/dpkg.log
[ -f "$DPKG_LOG" ] || fail "$DPKG_LOG 없음"
LOG_START=$(($(wc -l <"$DPKG_LOG") + 1))
apt-get -y -q --no-install-recommends -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade
# 풀 순서(dpkg.log의 "<날짜> <시각> <동작> <패키지[:아키텍처]> <옛 버전> <새 버전>"): orbivisor의
# Breaks: oplite (<< V1) 때문에 dpkg가 oplite를 V1(전환)로 먼저 올리고 그다음 orbivisor를 설치한다.
tail -n "+$LOG_START" "$DPKG_LOG" | tee "$WORK/dpkg.txt"
UP_LINE=$(awk -v a="$V0" -v b="$VERSION" '$3 == "upgrade" && ($4 == "oplite" || $4 == "oplite:amd64") && $5 == a && $6 == b { print NR; exit }' "$WORK/dpkg.txt")
INST_LINE=$(awk -v b="$VERSION" '$3 == "install" && ($4 == "orbivisor" || $4 == "orbivisor:amd64") && $6 == b { print NR; exit }' "$WORK/dpkg.txt")
[ -n "$UP_LINE" ] || fail "dpkg.log에 upgrade oplite $V0 $VERSION 없음"
[ -n "$INST_LINE" ] || fail "dpkg.log에 install orbivisor $VERSION 없음"
[ "$UP_LINE" -lt "$INST_LINE" ] || fail "dpkg.log: oplite 업그레이드($UP_LINE행)가 orbivisor 설치($INST_LINE행)보다 앞이 아님"
[ "$(dpkg-query -W -f '${Version}' orbivisor)" = "$VERSION" ] || fail "orbivisor 버전: $(dpkg-query -W -f '${Version}' orbivisor)"
[ "$(dpkg-query -W -f '${Version}' oplite)" = "$VERSION" ] || fail "oplite 버전: $(dpkg-query -W -f '${Version}' oplite)"
# check_moved <단계>: 옛 이름의 명령·유닛·enable 링크·소스 파일이 orbivisor로 그대로 된다.
check_moved() {
	[ "$(readlink /usr/bin/opld)" = orbivisord ] || fail "$1: /usr/bin/opld: $(ls -l /usr/bin/opld 2>&1)"
	[ "$(readlink /usr/bin/oplctl)" = orbivisorctl ] || fail "$1: /usr/bin/oplctl: $(ls -l /usr/bin/oplctl 2>&1)"
	[ "$(opld --version)" = "orbivisord $VERSION" ] || fail "$1: opld --version: $(opld --version)"
	[ -f "$UNIT" ] || fail "$1: 유닛 파일 없음"
	grep -qx 'ExecStart=/usr/bin/orbivisord --config /etc/opl/opld.toml' "$UNIT" || fail "$1: 옛 유닛 파일"
	[ "$(readlink /usr/lib/systemd/system/orbivisord.service)" = opld.service ] || fail "$1: 별칭 링크 없음"
	[ -L "$LINK" ] || fail "$1: enable 링크 없음"
	[ -f "$SOURCES" ] || fail "$1: 소스 파일 없음"
	[ -f /usr/share/orbivisor/opld.toml.example ] || fail "$1: /usr/share/orbivisor/opld.toml.example 없음"
	dpkg --verify orbivisor || fail "$1: dpkg --verify orbivisor"
	[ -f "$APT_KEYRING" ] || fail "$1: 저장소 키 없음"
	dpkg -S "$APT_KEYRING" | grep -qxE "orbivisor(:amd64)?: $APT_KEYRING" || fail "$1: 키: $(dpkg -S "$APT_KEYRING" 2>&1)"
}
check_moved "dist-upgrade 뒤"
[ ! -e /usr/share/oplite ] || fail "옛 /usr/share/oplite가 남음"
# 수동으로 설치한 oplite가 oldlibs 전환 패키지가 되면 apt가 수동 표시를 새 의존 orbivisor로 옮긴다
# (APT::Move-Autobit-Sections). 자동 설치 표시로 남으면 oplite를 지운 뒤 autoremove가 orbivisor를 지운다.
apt-mark showmanual | grep -qx orbivisor || fail "orbivisor가 수동 설치 표시가 아님: $(apt-mark showmanual | tr '\n' ' ')"

# 전환 패키지를 지워도 아무것도 바뀌지 않는다(유지 스크립트 없음).
apt-get remove -y oplite
[ "$(dpkg-query -W -f '${db:Status-Status}' oplite 2>/dev/null || true)" != installed ] || fail "oplite가 지워지지 않음"
[ "$(dpkg-query -W -f '${db:Status-Status}' orbivisor)" = installed ] || fail "oplite를 지우자 orbivisor가 바뀜"
check_moved "oplite를 지운 뒤"
LANG=C apt-get -s autoremove | tee "$WORK/autoremove.txt"
if grep -qw orbivisor "$WORK/autoremove.txt"; then fail "oplite를 지운 뒤 autoremove가 orbivisor를 지우려 함"; fi
apt-get update

echo "repo-test: 모두 통과"
