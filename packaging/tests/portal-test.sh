#!/bin/bash
# packaging/portal/build-web.sh: 인자 검사, dist가 없을 때 실패, 묶음 이름과 안의 경로·내용, 소유자 0을 본다.
# 가짜 dist(index.html, assets/)를 쓴다. bash와 tar만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/portal-test.sh
set -eu

SCRIPT="$(cd "$(dirname "$0")/../portal" && pwd)/build-web.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/dist/assets"
printf '<!doctype html>' >"$WORK/dist/index.html"
printf 'console.log(1)' >"$WORK/dist/assets/index-abc123.js"

FAILED=0
ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

run() {
	code=0
	OPL_WEB_DIST=${DIST-$WORK/dist} sh "$SCRIPT" "$@" >"$WORK/stdout" 2>"$WORK/stderr" || code=$?
}

# 인자 검사: 종료 코드 2
for args in "" "1.0" "1.0 a b" "../x $WORK/out" "1.0/x $WORK/out"; do
	# shellcheck disable=SC2086 # 낱말로 나눠 인자 여럿을 만든다.
	run $args
	if [ "$code" = 2 ]; then ok "usage: '$args'"; else fail "usage: '$args' exited $code"; fi
done

# dist가 없으면 실패하고 아무것도 만들지 않는다.
DIST="$WORK/nope" run 1.0 "$WORK/none"
if [ "$code" = 1 ] && grep -q 'pnpm -C web build' "$WORK/stderr" && [ ! -e "$WORK/none" ]; then
	ok "missing dist"
else
	fail "missing dist: exit $code, $(cat "$WORK/stderr")"
fi

run 1.2.3~rc1 "$WORK/out"
TAR="$WORK/out/orbivisor-web-1.2.3~rc1.tar.gz"
if [ "$code" = 0 ] && [ "$(cat "$WORK/stdout")" = "$TAR" ] && [ -f "$TAR" ]; then
	ok "builds $TAR"
else
	fail "build: exit $code, $(cat "$WORK/stderr")"
fi
LIST=$(tar -tzf "$TAR" | sed 's#/$##' | LC_ALL=C sort | tr '\n' ' ')
want="orbivisor-web-1.2.3~rc1 orbivisor-web-1.2.3~rc1/assets orbivisor-web-1.2.3~rc1/assets/index-abc123.js orbivisor-web-1.2.3~rc1/index.html "
if [ "$LIST" = "$want" ]; then ok "contents"; else fail "contents: $LIST"; fi
if [ "$(tar -xzOf "$TAR" 'orbivisor-web-1.2.3~rc1/index.html')" = '<!doctype html>' ]; then
	ok "index.html"
else
	fail "index.html"
fi
# GNU tar는 "0/0", bsdtar는 링크 수 뒤에 "0 0"으로 소유자·그룹을 찍는다.
if tar -tvzf "$TAR" | awk '{ if ($2 != "0/0" && !($3 == "0" && $4 == "0")) bad = 1 } END { exit bad }'; then
	ok "owner 0"
else
	fail "owner: $(tar -tvzf "$TAR")"
fi

exit "$FAILED"
