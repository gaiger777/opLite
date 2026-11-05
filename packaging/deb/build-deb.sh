#!/bin/sh
# 이미 빌드한 릴리스 바이너리로 Debian 패키지 oplite_<version>_amd64.deb를 만든다.
# 쓰는 법: packaging/deb/build-deb.sh <version> <out dir>
# 바이너리는 OPL_VERSION=<version>으로 빌드해 둔다(--version이 그 버전이 아니면 실패한다):
#   pnpm -C web build && OPL_VERSION=<version> cargo build --release -p opld --features web-ui -p oplctl
# 바이너리 위치는 기본 target/release이고 OPL_BIN_DIR로 바꾼다.
# OPL_APT_KEY(APT 저장소 공개 키 파일, armor가 아닌 gpg --export)가 있으면
# /usr/share/keyrings/oplite-archive-keyring.gpg로 싣는다.
# 구독 키 공개 키: 바이너리를 OPL_LICENSE_KEYS로 빌드한 정식 패키지는 그 값을 여기에도 준다. 없으면
# 저장소의 개발 키를 믿는 바이너리이므로 OPL_DEV_LICENSE=1(개발 패키지)일 때만 만든다.
# OPL_LICENSE_KEYS에는 정규 base64(44자, 32바이트) 항목이 하나 이상 있어야 하고, 개발 키가 들어 있으면
# 안 되며, opld에 그 값이 그대로 들어 있어야 한다(그 값으로 빌드했는지).
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

if [ -z "${OPL_LICENSE_KEYS:-}" ] && [ "${OPL_DEV_LICENSE:-}" != 1 ]; then
	echo "build-deb: OPL_LICENSE_KEYS is not set; set it for a release build or OPL_DEV_LICENSE=1 for a development package" >&2
	exit 1
fi

DEB_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$DEB_DIR/../.." && pwd)
BIN_DIR=${OPL_BIN_DIR:-$ROOT/target/release}

if [ -n "${OPL_LICENSE_KEYS:-}" ]; then
	DEV_KEY=$(cat "$ROOT/crates/opl-types/keys/dev-license.pub")
	set -f
	old_ifs=$IFS
	IFS=,
	n=0
	for k in $OPL_LICENSE_KEYS; do
		k=$(printf %s "$k" | tr -d ' \t\n')
		[ -n "$k" ] || continue
		n=$((n + 1))
		# 표준 base64 정규형(44자, = 끝, 풀었다 다시 만들면 같음)이고 32바이트여야 한다.
		# GNU base64는 패딩 없는 값·끝 비트가 0이 아닌 값도 풀어 주므로 길이와 왕복을 함께 본다.
		if [ "${#k}" != 44 ] || [ "${k%=}" = "$k" ] ||
			[ "$(printf %s "$k" | base64 -d 2>/dev/null | base64)" != "$k" ] ||
			[ "$(printf %s "$k" | base64 -d 2>/dev/null | wc -c | tr -d ' ')" != 32 ]; then
			echo "build-deb: OPL_LICENSE_KEYS entry is not a base64 Ed25519 public key: $k" >&2
			exit 1
		fi
		if [ "$k" = "$DEV_KEY" ]; then
			echo "build-deb: OPL_LICENSE_KEYS contains the development key; use OPL_DEV_LICENSE=1 for a development package" >&2
			exit 1
		fi
	done
	IFS=$old_ifs
	set +f
	if [ "$n" -eq 0 ]; then
		echo "build-deb: OPL_LICENSE_KEYS has no keys" >&2
		exit 1
	fi
fi

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
# 정식 패키지: opld가 이 OPL_LICENSE_KEYS로 빌드됐는지(빌드 값은 바이너리에 그대로 들어간다)
if [ -n "${OPL_LICENSE_KEYS:-}" ] && ! grep -qaF -- "$OPL_LICENSE_KEYS" "$BIN_DIR/opld"; then
	echo "build-deb: opld was not built with this OPL_LICENSE_KEYS" >&2
	exit 1
fi

APT_KEY=${OPL_APT_KEY:-}
if [ -n "$APT_KEY" ] && [ ! -f "$APT_KEY" ]; then
	echo "build-deb: OPL_APT_KEY의 $APT_KEY 이 없다" >&2
	exit 1
fi
# apt의 Signed-By는 .gpg 확장자의 armor 키를 읽지 못한다.
if [ -n "$APT_KEY" ] && head -n 1 "$APT_KEY" | grep -q '^-----BEGIN PGP'; then
	echo "build-deb: OPL_APT_KEY must be a binary keyring (gpg --export, not --armor)" >&2
	exit 1
fi

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
# 저장소 공개 키: opld가 쓰는 소스 파일의 Signed-By
if [ -n "$APT_KEY" ]; then
	mkdir -p "$STAGE/usr/share/keyrings"
	install -m 0644 "$APT_KEY" "$STAGE/usr/share/keyrings/oplite-archive-keyring.gpg"
fi

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
