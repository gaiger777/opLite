#!/bin/sh
# opLite APT 저장소의 한 채널에 패키지를 더하고 색인을 다시 만들어 서명한다.
# 쓰는 법: OPL_REPO_GPG_KEY_ID=<키> packaging/repo/build-repo.sh <repo dir> <channel> <deb…>
# 채널은 stable|testing|enterprise. 레이아웃:
#   pool/<channel>/*.deb
#   dists/trixie/<channel>/binary-amd64/Packages(.gz)
#   dists/trixie/Release, InRelease, Release.gpg  (Release는 있는 모든 채널을 묶는다)
# 그 채널의 Packages만 다시 만들고 다른 채널의 Packages는 그대로 둔다. pool의 옛 패키지는 지우지 않는다.
# 이미 올린 이름에 내용이 다른 deb는 거절한다. 색인은 두 서명이 다 끝난 뒤에 바꾼다(InRelease가 마지막).
# 서명 키는 OPL_REPO_GPG_KEY_ID(필수), GPG 홈은 호출하는 쪽(GNUPGHOME). 도구 경로는 APT_FTPARCHIVE·GPG로 바꾼다.
set -eu
umask 022

SUITE=trixie
ARCH=amd64
# Release의 Components 순서
CHANNELS="stable testing enterprise"

usage() {
	echo "usage: $0 <repo dir> <stable|testing|enterprise> <deb…>" >&2
	exit 2
}

[ $# -ge 3 ] || usage
REPO=$1
CHANNEL=$2
shift 2
case $CHANNEL in
stable | testing | enterprise) ;;
*)
	echo "build-repo: 모르는 채널 '$CHANNEL'(stable|testing|enterprise)" >&2
	usage
	;;
esac
for deb; do
	if [ ! -f "$deb" ]; then
		echo "build-repo: $deb 이 없다" >&2
		usage
	fi
done
KEY=${OPL_REPO_GPG_KEY_ID:-}
if [ -z "$KEY" ]; then
	echo "build-repo: OPL_REPO_GPG_KEY_ID(서명 키)가 필요하다" >&2
	exit 1
fi
APT_FTPARCHIVE=${APT_FTPARCHIVE:-apt-ftparchive}
GPG=${GPG:-gpg}

POOL="$REPO/pool/$CHANNEL"
DIST="$REPO/dists/$SUITE"
BIN="$DIST/$CHANNEL/binary-$ARCH"
# 이미 올린 이름에 다른 파일은 받지 않는다(받아 둔 apt의 해시가 맞지 않게 된다). 같은 파일은 넘어간다.
for deb; do
	old="$POOL/$(basename "$deb")"
	if [ -e "$old" ] && ! cmp -s "$deb" "$old"; then
		echo "build-repo: version already published: $old differs from $deb" >&2
		exit 1
	fi
done
mkdir -p "$POOL"
cp "$@" "$POOL/"

# 새 색인과 Release는 dists/.trixie.new에서 만들어 서명까지 끝낸 뒤 옮긴다. 도중에 실패해도 저장소는
# 옛 색인과 서명 그대로다.
STAGE="$REPO/dists/.$SUITE.new"
trap 'rm -rf "$STAGE" "$REPO/dists/.Release.new"' EXIT
rm -rf "$STAGE"
mkdir -p "$STAGE"
for c in $CHANNELS; do
	if [ -f "$DIST/$c/binary-$ARCH/Packages" ]; then
		mkdir -p "$STAGE/$c/binary-$ARCH"
		cp "$DIST/$c/binary-$ARCH/"Packages* "$STAGE/$c/binary-$ARCH/"
	fi
done

# 이 채널의 Packages. Filename이 저장소 루트 기준이 되도록 루트에서 돌린다.
NEW="$STAGE/$CHANNEL/binary-$ARCH"
mkdir -p "$NEW"
(cd "$REPO" && "$APT_FTPARCHIVE" packages "pool/$CHANNEL") >"$NEW/Packages"
gzip -9 -n -c "$NEW/Packages" >"$NEW/Packages.gz"

# Packages가 있는 채널들
COMPONENTS=
for c in $CHANNELS; do
	[ -f "$STAGE/$c/binary-$ARCH/Packages" ] && COMPONENTS="${COMPONENTS:+$COMPONENTS }$c"
done

# Release: 색인에 섞이지 않게 STAGE 밖에 만든 뒤 옮긴다.
(cd "$STAGE" && "$APT_FTPARCHIVE" \
	-o APT::FTPArchive::Release::Origin=opLite \
	-o APT::FTPArchive::Release::Label=opLite \
	-o APT::FTPArchive::Release::Suite="$SUITE" \
	-o APT::FTPArchive::Release::Codename="$SUITE" \
	-o APT::FTPArchive::Release::Components="$COMPONENTS" \
	-o APT::FTPArchive::Release::Architectures="$ARCH" \
	release .) >"$REPO/dists/.Release.new"
mv "$REPO/dists/.Release.new" "$STAGE/Release"

# 서명: InRelease(본문 안 서명)와 Release.gpg(따로 둔 서명)
"$GPG" --batch --yes --local-user "$KEY" --clearsign --output "$STAGE/InRelease" "$STAGE/Release"
"$GPG" --batch --yes --local-user "$KEY" --armor --detach-sign --output "$STAGE/Release.gpg" "$STAGE/Release"

# 옮긴다: 색인 먼저, apt가 먼저 읽는 InRelease는 마지막에.
mkdir -p "$BIN"
mv "$NEW/Packages" "$BIN/Packages"
mv "$NEW/Packages.gz" "$BIN/Packages.gz"
mv "$STAGE/Release" "$DIST/Release"
mv "$STAGE/Release.gpg" "$DIST/Release.gpg"
mv "$STAGE/InRelease" "$DIST/InRelease"

echo "$REPO"
