#!/bin/bash
# build-repo.sh로 만든 저장소를 진짜 apt가 읽는지, 옛 oplite가 dist-upgrade 한 번으로 orbivisor로 넘어가는지
# Debian 13 컨테이너 안에서 본다(root, systemd는 돌지 않음).
# 임시 GPG 키로 stable에 옛 모양의 oplite V0(이름 바꾸기 전 build-deb.sh가 만들던 것: 실제 파일 /usr/bin/opld·
# oplctl, 옛 control·유지 스크립트 — fixtures/oplite-v0)를, testing에 받은 V1의 orbivisor·oplite(전환)를 넣는다.
# file:// 소스와 Signed-By로 stable에서 oplite를 설치한 뒤 소스를 testing으로 바꾸면 apt-get -s dist-upgrade가
# 두 패키지를 보이고, 실제 dist-upgrade 뒤 orbivisor가 옛 유닛·enable 링크·명령 이름·저장소 키를 이어받는지(유닛과
# enable 링크는 새 이름 orbivisord.service, 옛 이름 opld.service는 별칭 링크 — 계획 74), 새 데몬이 옛 배치(/etc/opl,
# /var/lib/opl)를 새 이름으로 옮기고 옛 이름을 링크로 남기는지,
# dpkg가 oplite를 먼저 올리고 orbivisor를 설치했는지, orbivisor가 수동 설치로 남아 autoremove되지 않는지 본다.
# V1 orbivisor는 받은 deb의 바이너리로 build-deb.sh를 ORBIVISOR_APT_KEY와 함께 다시 돌려 만든다(키를 실은 정식 패키지처럼).
# 셋째 인자(파일 복원 도우미 이미지 꾸러미 orbivisor-restore-image, 계획 69)가 있으면 testing에 함께 올려 저장소에서
# 설치하고, 다음 판(받은 둘을 판만 바꿔 다시 묶음)을 둘 다 올리면 dist-upgrade가 둘을 함께 올리는지 본다 —
# 이미지는 Depends: orbivisor (= 판)이라 판마다 함께 내야 한다.
# 끝으로 지금(계획 43~73) 노드에 깔린 orbivisor의 모양(fixtures/orbivisor-v1 — 진짜 opld.service와 별칭 링크
# orbivisord.service, 옛 유지 스크립트)을 설치·enable하고 덮어쓰기를 둔 뒤 받은 판으로 올려, enable 링크·덮어쓰기·유닛
# 파일·디렉터리가 새 이름으로 옮겨지는지, 다시 V1로 되돌리면 옛 판이 enable된 opld.service를 보는지 본다(계획 74 리뷰 2 —
# 가장 흔한 업그레이드 길과 되돌리기).
# 쓰는 법(CI): docker run --rm -v "$PWD:/work" debian:13 bash /work/packaging/tests/repo-test.sh /work/<orbivisor deb> /work/<oplite deb> [/work/<orbivisor-restore-image deb>]
set -eu

[ $# -eq 2 ] || [ $# -eq 3 ] || {
	echo "usage: $0 <orbivisor deb> <oplite deb> [<orbivisor-restore-image deb>]" >&2
	exit 2
}
DEB=$1
TRANS_DEB=$2
IMAGE_DEB=${3:-}
TESTS_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_SCRIPT="$TESTS_DIR/../repo/build-repo.sh"
DEB_DIR="$TESTS_DIR/../deb"
V0_DIR="$TESTS_DIR/fixtures/oplite-v0"
V1_DIR="$TESTS_DIR/fixtures/orbivisor-v1"

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

# V1 orbivisor를 저장소 키와 함께 다시 만든다: 받은 deb의 바이너리로 build-deb.sh(ORBIVISOR_APT_KEY, 시험 키).
dpkg-deb -x "$DEB" "$WORK/v1-files"
ORBIVISOR_DEV_LICENSE=1 ORBIVISOR_APT_KEY="$KEYRING" ORBIVISOR_BIN_DIR="$WORK/v1-files/usr/bin" \
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
install -m 0644 "$DEB_DIR/orbivisord.toml.example" "$P/usr/share/oplite/opld.toml.example"
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
ORBIVISOR_REPO_GPG_KEY_ID=$KEY_ID sh "$REPO_SCRIPT" "$REPO" stable "$V0_DEB"
ORBIVISOR_REPO_GPG_KEY_ID=$KEY_ID sh "$REPO_SCRIPT" "$REPO" testing "$DEB" "$TRANS_DEB" ${IMAGE_DEB:+"$IMAGE_DEB"}

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
OLD_LINK=/etc/systemd/system/multi-user.target.wants/opld.service
LINK=/etc/systemd/system/multi-user.target.wants/orbivisord.service
UNIT=/usr/lib/systemd/system/orbivisord.service
write_sources stable
apt-get update
apt-get install -y --no-install-recommends oplite
[ "$(dpkg-query -W -f '${Version}' oplite)" = "$V0" ] || fail "설치된 버전: $(dpkg-query -W -f '${Version}' oplite)"
if [ -L /usr/bin/opld ] || [ ! -f /usr/bin/opld ]; then fail "V0의 /usr/bin/opld가 파일이 아님"; fi
[ -L "$OLD_LINK" ] || fail "V0 enable 링크 없음"
# V0 노드의 설정·데이터(옛 배치). 새 데몬이 새 이름으로 옮긴다.
mkdir -p /etc/opl /var/lib/opl
printf 'name = "repo-test"\naddress = "127.0.0.1"\n' >/etc/opl/opld.toml
echo keep >/var/lib/opl/repo-test.marker
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
	[ -f "$UNIT" ] && [ ! -L "$UNIT" ] || fail "$1: 유닛 파일 없음"
	grep -qx 'ExecStart=/usr/bin/orbivisord' "$UNIT" || fail "$1: 옛 유닛 파일"
	[ "$(readlink /usr/lib/systemd/system/opld.service)" = orbivisord.service ] || fail "$1: 별칭 링크 없음"
	[ -L "$LINK" ] || fail "$1: enable 링크 없음"
	if [ -e "$OLD_LINK" ] || [ -L "$OLD_LINK" ]; then fail "$1: 옛 이름 enable 링크가 남음"; fi
	[ "$(systemctl is-enabled orbivisord.service)" = enabled ] || fail "$1: is-enabled orbivisord: $(systemctl is-enabled orbivisord.service 2>&1)"
	[ -f "$SOURCES" ] || fail "$1: 소스 파일 없음"
	[ -f /usr/share/orbivisor/orbivisord.toml.example ] || fail "$1: /usr/share/orbivisor/orbivisord.toml.example 없음"
	dpkg --verify orbivisor || fail "$1: dpkg --verify orbivisor"
	[ -f "$APT_KEYRING" ] || fail "$1: 저장소 키 없음"
	dpkg -S "$APT_KEYRING" | grep -qxE "orbivisor(:amd64)?: $APT_KEYRING" || fail "$1: 키: $(dpkg -S "$APT_KEYRING" 2>&1)"
}
check_moved "dist-upgrade 뒤"
# check_layout <단계>: 새 데몬의 배치 맞추기(계획 74 설계 §3.4). 옛 설정 경로로 뜨고, 시작하면서 /etc/opl·/var/lib/opl을
# 새 이름으로 옮기고 옛 이름을 링크로 남긴다. systemd가 돌지 않으므로 직접 띄워 제어 소켓이 열리면 멈춘다(클러스터 밖
# 노드는 소켓만 연다).
check_layout() {
	orbivisord >"$WORK/daemon.log" 2>&1 &
	DAEMON=$!
	for _ in $(seq 1 60); do
		[ -S /run/orbivisor/orbivisord.sock ] && break
		sleep 0.5
	done
	[ -S /run/orbivisor/orbivisord.sock ] || fail "$1: 새 데몬의 제어 소켓 없음: $(cat "$WORK/daemon.log")"
	for d in /etc /var/lib /run; do
		[ -d "$d/orbivisor" ] && [ ! -L "$d/orbivisor" ] || fail "$1: $d/orbivisor가 진짜 디렉터리가 아님: $(ls -la "$d")"
		[ "$(readlink "$d/opl")" = orbivisor ] || fail "$1: $d/opl이 링크가 아님: $(ls -la "$d")"
	done
	[ -f /etc/orbivisor/orbivisord.toml ] && [ ! -L /etc/orbivisor/orbivisord.toml ] || fail "$1: orbivisord.toml이 진짜 파일이 아님"
	[ "$(readlink /etc/orbivisor/opld.toml)" = orbivisord.toml ] || fail "$1: opld.toml이 링크가 아님"
	[ "$(cat /var/lib/opl/repo-test.marker)" = keep ] || fail "$1: 옛 경로로 데이터를 읽지 못함"
	[ "$(readlink /run/orbivisor/opld.sock)" = orbivisord.sock ] || fail "$1: 옛 이름 소켓 링크 없음"
	grep -q "moved /var/lib/opl to /var/lib/orbivisor" "$WORK/daemon.log" || fail "$1: 시작 로그에 옮긴 것이 없음: $(cat "$WORK/daemon.log")"
	kill "$DAEMON"
	wait "$DAEMON" || true
}
check_layout "dist-upgrade 뒤"
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

# 도우미 이미지 꾸러미: 저장소에서 설치하고, 다음 판(V2)을 둘 다 올리면 dist-upgrade가 함께 올린다.
if [ -n "$IMAGE_DEB" ]; then
	[ "$(dpkg-deb -f "$IMAGE_DEB" Version)" = "$VERSION" ] || fail "이미지 꾸러미 판: $(dpkg-deb -f "$IMAGE_DEB" Version)"
	apt-get install -y --no-install-recommends orbivisor-restore-image
	[ "$(dpkg-query -W -f '${Version}' orbivisor-restore-image)" = "$VERSION" ] || fail "저장소에서 설치한 이미지 판"
	V2="$VERSION+v2"
	# rebuild_v2 <deb> <out>: 판(과 이미지의 Depends)만 V2로 바꿔 다시 묶는다.
	rebuild_v2() {
		rm -rf "$WORK/re"
		dpkg-deb -R "$1" "$WORK/re"
		sed -i -e "s/^Version: .*/Version: $V2/" -e "s/^Depends: orbivisor (= .*)\$/Depends: orbivisor (= $V2)/" \
			"$WORK/re/DEBIAN/control"
		dpkg-deb --build --root-owner-group "$WORK/re" "$2" >/dev/null
	}
	rebuild_v2 "$DEB" "$WORK/orbivisor_${V2}_amd64.deb"
	rebuild_v2 "$IMAGE_DEB" "$WORK/orbivisor-restore-image_${V2}_amd64.deb"
	[ "$(dpkg-deb -f "$WORK/orbivisor-restore-image_${V2}_amd64.deb" Depends)" = "orbivisor (= $V2)" ] || fail "V2 이미지 Depends"
	ORBIVISOR_REPO_GPG_KEY_ID=$KEY_ID sh "$REPO_SCRIPT" "$REPO" testing \
		"$WORK/orbivisor_${V2}_amd64.deb" "$WORK/orbivisor-restore-image_${V2}_amd64.deb"
	apt-get update
	LANG=C apt-get -s -q dist-upgrade | tee "$WORK/simulate2.txt"
	grep -qF "Inst orbivisor [$VERSION] ($V2 " "$WORK/simulate2.txt" || fail "V2 dist-upgrade에 orbivisor 없음"
	grep -qF "Inst orbivisor-restore-image [$VERSION] ($V2 " "$WORK/simulate2.txt" || fail "V2 dist-upgrade에 이미지 없음"
	if grep -q "^Remv " "$WORK/simulate2.txt"; then fail "V2 dist-upgrade가 꾸러미를 지우려 함"; fi
fi

# 지금 노드에 깔린 판(계획 43~73의 orbivisor) → 받은 판(리뷰 2): 받은 판을 지우고 옛 배치로 되돌린 뒤 V1 고정물을 설치한다.
apt-get purge -y orbivisor ${IMAGE_DEB:+"orbivisor-restore-image"}
rm -rf /etc/orbivisor /etc/opl /var/lib/orbivisor /var/lib/opl /run/orbivisor /run/opl /var/log/orbivisor /var/log/opl
V1="$VERSION~p43"
P="$WORK/orbivisor-v1"
mkdir -p "$P/usr/bin" "$P/usr/lib/systemd/system" "$P/usr/share/orbivisor" "$P/usr/share/doc/orbivisor" \
	"$P/etc/apparmor.d/abstractions"
for bin in orbivisord orbivisorctl; do
	printf '#!/bin/sh\necho "%s %s"\n' "$bin" "$V1" >"$P/usr/bin/$bin"
	chmod 0755 "$P/usr/bin/$bin"
done
ln -s orbivisord "$P/usr/bin/opld"
ln -s orbivisorctl "$P/usr/bin/oplctl"
install -m 0644 "$V1_DIR/opld.service" "$P/usr/lib/systemd/system/opld.service"
ln -s opld.service "$P/usr/lib/systemd/system/orbivisord.service"
install -m 0644 "$DEB_DIR/orbivisord.toml.example" "$P/usr/share/orbivisor/opld.toml.example"
install -m 0644 "$DEB_DIR/copyright" "$P/usr/share/doc/orbivisor/copyright"
install -m 0644 "$V1_DIR/orbivisor-swtpm" "$P/etc/apparmor.d/abstractions/orbivisor-swtpm"
SIZE=$(du -sk "$P" | cut -f1)
mkdir -p "$P/DEBIAN"
sed -e "s/@VERSION@/$V1/g" -e "s/@INSTALLED_SIZE@/$SIZE/" "$V1_DIR/control.in" >"$P/DEBIAN/control"
(cd "$P" && find usr -type f | LC_ALL=C sort | xargs md5sum) >"$P/DEBIAN/md5sums"
echo /etc/apparmor.d/abstractions/orbivisor-swtpm >"$P/DEBIAN/conffiles"
for script in postinst prerm postrm; do
	install -m 0755 "$V1_DIR/$script" "$P/DEBIAN/$script"
done
V1_DEB="$WORK/orbivisor_${V1}_amd64.deb"
dpkg-deb --build --root-owner-group "$P" "$V1_DEB"
apt-get install -y --no-install-recommends "$V1_DEB"
[ "$(dpkg-query -W -f '${Version}' orbivisor)" = "$V1" ] || fail "V1 버전: $(dpkg-query -W -f '${Version}' orbivisor)"
# V1의 postinst가 옛 이름 enable 링크를 만든다(진짜 유닛이 opld.service).
[ "$(readlink "$OLD_LINK")" = /usr/lib/systemd/system/opld.service ] || fail "V1 enable 링크: $(ls -l "$OLD_LINK" 2>&1)"
[ "$(systemctl is-enabled opld.service)" = enabled ] || fail "V1 is-enabled opld: $(systemctl is-enabled opld.service 2>&1)"
# 운영자의 덮어쓰기(`systemctl edit opld`)와 옛 배치의 설정·데이터
DROPIN=/etc/systemd/system/opld.service.d
mkdir -p "$DROPIN"
printf '[Service]\nEnvironment=REPO_TEST=1\n' >"$DROPIN/override.conf"
mkdir -p /etc/opl /var/lib/opl
printf 'name = "repo-test"\naddress = "127.0.0.1"\n' >/etc/opl/opld.toml
echo keep >/var/lib/opl/repo-test.marker
apt-get install -y --no-install-recommends -o Dpkg::Options::=--force-confold "$DEB"
[ "$(dpkg-query -W -f '${Version}' orbivisor)" = "$VERSION" ] || fail "V1에서 올린 버전: $(dpkg-query -W -f '${Version}' orbivisor)"
[ -f "$UNIT" ] && [ ! -L "$UNIT" ] || fail "V1에서 올린 뒤: 유닛 파일이 진짜 파일이 아님: $(ls -l "$UNIT")"
[ "$(readlink /usr/lib/systemd/system/opld.service)" = orbivisord.service ] || fail "V1에서 올린 뒤: 옛 이름 opld.service는 별칭 링크가 아님"
[ "$(readlink "$LINK")" = "$UNIT" ] || fail "V1에서 올린 뒤: enable 링크: $(ls -l "$LINK" 2>&1)"
if [ -e "$OLD_LINK" ] || [ -L "$OLD_LINK" ]; then fail "V1에서 올린 뒤: 옛 이름 enable 링크가 남음"; fi
[ "$(systemctl is-enabled orbivisord.service)" = enabled ] || fail "V1에서 올린 뒤: is-enabled orbivisord: $(systemctl is-enabled orbivisord.service 2>&1)"
[ -f /etc/systemd/system/orbivisord.service.d/override.conf ] && [ ! -L /etc/systemd/system/orbivisord.service.d ] ||
	fail "V1에서 올린 뒤: 덮어쓰기가 새 이름 디렉터리에 없음: $(ls -la /etc/systemd/system)"
[ "$(readlink "$DROPIN")" = orbivisord.service.d ] || fail "V1에서 올린 뒤: 옛 이름 덮어쓰기가 링크가 아님"
[ ! -e /usr/share/orbivisor/opld.toml.example ] || fail "V1에서 올린 뒤: 옛 예시 설정이 남음"
dpkg --verify orbivisor || fail "V1에서 올린 뒤: dpkg --verify orbivisor"
check_layout "V1에서 올린 뒤"
# 옛 판(V1)으로 되돌림: 새 판의 postrm이 enable 링크를 opld.service로 되돌려 옛 postinst의 is-enabled opld가 참이다(리뷰 2).
apt-get install -y --no-install-recommends --allow-downgrades "$V1_DEB"
[ "$(dpkg-query -W -f '${Version}' orbivisor)" = "$V1" ] || fail "되돌린 버전: $(dpkg-query -W -f '${Version}' orbivisor)"
[ "$(readlink "$OLD_LINK")" = /usr/lib/systemd/system/opld.service ] || fail "되돌린 뒤 enable 링크: $(ls -l "$OLD_LINK" 2>&1)"
[ "$(systemctl is-enabled opld.service)" = enabled ] || fail "되돌린 뒤 is-enabled opld: $(systemctl is-enabled opld.service 2>&1)"
# 다시 올리면 다시 옮긴다.
apt-get install -y --no-install-recommends -o Dpkg::Options::=--force-confold "$DEB"
[ "$(readlink "$LINK")" = "$UNIT" ] || fail "다시 올린 뒤 enable 링크: $(ls -l "$LINK" 2>&1)"
[ "$(systemctl is-enabled orbivisord.service)" = enabled ] || fail "다시 올린 뒤 is-enabled orbivisord: $(systemctl is-enabled orbivisord.service 2>&1)"

echo "repo-test: 모두 통과"
