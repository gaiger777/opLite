#!/bin/bash
# build-deb.sh가 바이너리의 --version이 패키지 버전과 같은지 확인하는지 본다.
# 가짜 opld·oplctl(버전을 찍는 스크립트)과 가짜 dpkg-deb(만들 파일만 만듦)를 쓴다. bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/build-deb-test.sh
set -eu

DEB_DIR="$(cd "$(dirname "$0")/../deb" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/fakebin"
# 가짜 dpkg-deb: 마지막 인자(만들 .deb)를 만든다.
cat >"$WORK/fakebin/dpkg-deb" <<'EOF'
#!/bin/sh
for a; do out=$a; done
echo deb >"$out"
EOF
chmod 0755 "$WORK/fakebin/dpkg-deb"

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

# fake_bins <opld 버전> <oplctl 버전>: <bin> --version이 "<bin> <버전>"을 찍는 가짜 바이너리
fake_bins() {
	rm -rf "${WORK:?}/bin"
	mkdir -p "$WORK/bin"
	printf '#!/bin/sh\necho "opld %s"\n' "$1" >"$WORK/bin/opld"
	printf '#!/bin/sh\necho "oplctl %s"\n' "$2" >"$WORK/bin/oplctl"
	chmod 0755 "$WORK/bin/opld" "$WORK/bin/oplctl"
}

# run <버전>: build-deb.sh를 돌려 종료 코드(code)·stdout·stderr를 남긴다.
run() {
	rm -rf "$WORK/out"
	code=0
	PATH="$WORK/fakebin:$PATH" OPL_BIN_DIR="$WORK/bin" \
		sh "$DEB_DIR/build-deb.sh" "$1" "$WORK/out" >"$WORK/stdout" 2>"$WORK/stderr" || code=$?
}

fake_bins 0.1.0~ci7 0.1.0~ci7
run 0.1.0~ci7
if [ "$code" -eq 0 ] && [ "$(tail -n1 "$WORK/stdout")" = "$WORK/out/oplite_0.1.0~ci7_amd64.deb" ] &&
	[ -f "$WORK/out/oplite_0.1.0~ci7_amd64.deb" ]; then
	ok "버전이 같으면 패키지를 만든다"
else
	fail "버전이 같은데 종료 코드 $code"
	cat "$WORK/stderr"
fi

# check_mismatch <이름>: 종료 코드 1, OPL_VERSION 안내, 패키지 없음
check_mismatch() {
	if [ "$code" -eq 1 ] && grep -q 'OPL_VERSION=0.2.0' "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
		ok "$1"
	else
		fail "$1: 종료 코드 $code"
		cat "$WORK/stderr"
	fi
}

fake_bins 0.1.0 0.2.0
run 0.2.0
check_mismatch "opld 버전이 다르면 실패"
fake_bins 0.2.0 0.1.0
run 0.2.0
check_mismatch "oplctl 버전이 다르면 실패"
# OPL_VERSION 없이 빌드한 바이너리는 크레이트 버전을 보인다. 앞부분만 같아도 안 된다.
fake_bins 0.2.0~ci1 0.2.0~ci1
run 0.2.0
check_mismatch "버전 앞부분만 같으면 실패"

if [ "$FAILED" -ne 0 ]; then
	echo "build-deb-test: 실패"
	exit 1
fi
echo "build-deb-test: 모두 통과"
