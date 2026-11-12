#!/bin/sh
# 웹 UI 정적 파일 묶음 orbivisor-web-<version>.tar.gz를 만든다(설계 §4 "UI 따로 내보내기"). 노드의 내장 UI를
# 끄고(`[web] enabled = false`) 포털(nginx, packaging/portal/Dockerfile)이 UI를 줄 때 쓴다.
# 쓰는 법: packaging/portal/build-web.sh <version> <out dir>
# 먼저 `pnpm -C web build`로 web/dist를 만든다. 위치는 OPL_WEB_DIST로 바꾼다.
# 묶음 안은 orbivisor-web-<version>/ 아래에 dist의 파일(index.html, assets/…)이다.
set -eu
umask 022

usage() {
	echo "usage: $0 <version> <out dir>" >&2
	exit 2
}

[ $# -eq 2 ] || usage
VERSION=$1
OUT_DIR=$2
# 파일 이름에 들어가므로 Debian 버전에 쓰는 글자만 받는다.
case $VERSION in
'' | *[!0-9A-Za-z.+~-]*) usage ;;
esac

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
DIST=${OPL_WEB_DIST:-$ROOT/web/dist}
if [ ! -f "$DIST/index.html" ]; then
	echo "build-web: $DIST/index.html is missing; run pnpm -C web build first" >&2
	exit 1
fi

NAME=orbivisor-web-$VERSION
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
mkdir "$STAGE/$NAME"
cp -R "$DIST/." "$STAGE/$NAME/"
mkdir -p "$OUT_DIR"
# 다른 사용자 이름이 새지 않게 소유자를 숫자 0으로 둔다(GNU tar와 bsdtar 둘 다 받는 옵션).
tar -C "$STAGE" --numeric-owner --owner=0 --group=0 -czf "$OUT_DIR/$NAME.tar.gz" "$NAME"
echo "$OUT_DIR/$NAME.tar.gz"
