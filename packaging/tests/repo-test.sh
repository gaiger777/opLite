#!/bin/bash
# build-repo.sh로 만든 저장소를 진짜 apt가 읽는지 Debian 13 컨테이너 안에서 본다(root).
# 임시 GPG 키로 받은 deb를 stable에, 버전을 <v>+1로 고친 사본을 testing에 넣고, file:// 소스와 Signed-By로
# stable에서 oplite를 설치한 뒤 소스를 testing으로 바꾸면 apt-get -s dist-upgrade가 새 버전을 보이는지 본다.
# 쓰는 법(CI): docker run --rm -v "$PWD:/work" debian:13 bash /work/packaging/tests/repo-test.sh /work/<deb>
set -eu

[ $# -eq 1 ] || {
	echo "usage: $0 <oplite deb>" >&2
	exit 2
}
DEB=$1
REPO_SCRIPT="$(cd "$(dirname "$0")/../repo" && pwd)/build-repo.sh"

fail() {
	echo "repo-test: FAIL: $*" >&2
	exit 1
}

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends apt-utils gnupg gpgv

# apt의 _apt 사용자도 저장소와 키링을 읽도록 0755 디렉터리에 둔다.
WORK=$(mktemp -d)
chmod 0755 "$WORK"
trap 'rm -rf "$WORK"' EXIT
REPO="$WORK/repo"
KEYRING="$WORK/oplite-archive-keyring.gpg"

# 임시 서명 키(암호 없음)와 apt가 읽을 키링
export GNUPGHOME="$WORK/gnupg"
mkdir -m 0700 "$GNUPGHOME"
gpg --batch --passphrase '' --quick-generate-key 'opLite repo test <repo-test@oplite.invalid>' ed25519 sign never
KEY_ID=$(gpg --batch --with-colons --list-secret-keys | awk -F: '$1 == "fpr" { print $10; exit }')
[ -n "$KEY_ID" ] || fail "임시 키 지문을 못 찾음"
gpg --batch --export "$KEY_ID" >"$KEYRING"
chmod 0644 "$KEYRING"

# stable: 받은 deb
VERSION=$(dpkg-deb -f "$DEB" Version)
OPL_REPO_GPG_KEY_ID=$KEY_ID sh "$REPO_SCRIPT" "$REPO" stable "$DEB"

# testing: 버전만 <v>+1로 고친 사본
NEW_VERSION="$VERSION+1"
dpkg-deb -R "$DEB" "$WORK/pkg"
sed -i "s/^Version: .*/Version: $NEW_VERSION/" "$WORK/pkg/DEBIAN/control"
NEW_DEB="$WORK/oplite_${NEW_VERSION}_amd64.deb"
dpkg-deb --build --root-owner-group "$WORK/pkg" "$NEW_DEB"
OPL_REPO_GPG_KEY_ID=$KEY_ID sh "$REPO_SCRIPT" "$REPO" testing "$NEW_DEB"

# 서명과 Release 값
gpgv --keyring "$KEYRING" "$REPO/dists/trixie/InRelease" || fail "InRelease 서명"
gpgv --keyring "$KEYRING" "$REPO/dists/trixie/Release.gpg" "$REPO/dists/trixie/Release" || fail "Release.gpg 서명"
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

# stable에서 설치
write_sources stable
apt-get update
apt-get install -y --no-install-recommends oplite
[ "$(dpkg-query -W -f '${Version}' oplite)" = "$VERSION" ] || fail "설치된 버전: $(dpkg-query -W -f '${Version}' oplite)"

# testing으로 바꾸면 새 버전이 보인다(opld의 parse_simulate가 읽는 줄 모양).
write_sources testing
apt-get update
LANG=C apt-get -s -q dist-upgrade | tee "$WORK/simulate.txt"
grep -qF "Inst oplite [$VERSION] ($NEW_VERSION " "$WORK/simulate.txt" || fail "dist-upgrade에 oplite $NEW_VERSION 없음"

echo "repo-test: 모두 통과"
