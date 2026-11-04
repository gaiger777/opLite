#!/bin/sh
# 이미 빌드한 릴리스 바이너리로 Debian 패키지 oplite_<version>_amd64.deb를 만든다.
# 쓰는 법: packaging/deb/build-deb.sh <version> <out dir>
# 바이너리는 OPL_VERSION=<version>으로 빌드해 둔다(--version이 그 버전이 아니면 실패한다):
#   pnpm -C web build && OPL_VERSION=<version> cargo build --release -p opld --features web-ui -p oplctl
# 바이너리 위치는 기본 target/release이고 OPL_BIN_DIR로 바꾼다.
set -eu
# 만드는 파일·디렉터리 권한이 호출한 셸의 umask에 휘둘리지 않게 한다.
umask 022

usage() {
	echo "usage: $0 <version> <out dir>" >&2
	exit 2
}

[ $# -eq 2 ] || usage
VERSION=$1
OUT_DIR=$2
[ -n "$VERSION" ] || usage

DEB_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$DEB_DIR/../.." && pwd)
BIN_DIR=${OPL_BIN_DIR:-$ROOT/target/release}

for bin in opld oplctl; do
	if [ ! -x "$BIN_DIR/$bin" ]; then
		echo "build-deb: $BIN_DIR/$bin 이 없다(먼저 릴리스로 빌드한다)" >&2
		exit 1
	fi
	# 바이너리가 보이는 버전(--version)이 패키지 버전과 같아야 한다.
	got=$("$BIN_DIR/$bin" --version)
	if [ "$got" != "$bin $VERSION" ]; then
		echo "build-deb: $BIN_DIR/$bin --version이 '$got'이다(기대 '$bin $VERSION')." >&2
		echo "build-deb: OPL_VERSION=$VERSION cargo build --release -p opld --features web-ui -p oplctl 로 다시 빌드한다" >&2
		exit 1
	fi
done

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
# mktemp -d는 0700이다. 패키지 루트(/)의 권한이 되므로 0755로 한다.
chmod 0755 "$STAGE"

# 파일 트리
mkdir -p "$STAGE/usr/bin" "$STAGE/usr/lib/systemd/system" \
	"$STAGE/usr/share/oplite" "$STAGE/usr/share/doc/oplite"
install -m 0755 "$BIN_DIR/opld" "$BIN_DIR/oplctl" "$STAGE/usr/bin/"
install -m 0644 "$DEB_DIR/opld.service" "$STAGE/usr/lib/systemd/system/opld.service"
install -m 0644 "$DEB_DIR/opld.toml.example" "$STAGE/usr/share/oplite/opld.toml.example"
install -m 0644 "$DEB_DIR/copyright" "$STAGE/usr/share/doc/oplite/copyright"

# DEBIAN: control과 maintainer 스크립트. Installed-Size(KiB)는 DEBIAN을 넣기 전에 잰다.
INSTALLED_SIZE=$(du -sk "$STAGE" | cut -f1)
mkdir -p "$STAGE/DEBIAN"
sed -e "s/@VERSION@/$VERSION/" -e "s/@INSTALLED_SIZE@/$INSTALLED_SIZE/" \
	"$DEB_DIR/control.in" >"$STAGE/DEBIAN/control"
chmod 0644 "$STAGE/DEBIAN/control"
# dpkg --verify가 보는 md5sums(usr 아래 파일, 정렬)
(cd "$STAGE" && find usr -type f | LC_ALL=C sort | xargs md5sum) >"$STAGE/DEBIAN/md5sums"
chmod 0644 "$STAGE/DEBIAN/md5sums"
for script in postinst prerm postrm; do
	install -m 0755 "$DEB_DIR/$script" "$STAGE/DEBIAN/$script"
done

mkdir -p "$OUT_DIR"
DEB="$OUT_DIR/oplite_${VERSION}_amd64.deb"
dpkg-deb --build --root-owner-group -Zxz "$STAGE" "$DEB"
echo "$DEB"
