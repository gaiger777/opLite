#!/bin/bash
# build-repo.sh의 인자 검사, 저장소 레이아웃, apt-ftparchive·gpg 인자, 다른 채널 보존, 잘못된 채널 거절을 본다.
# 가짜 apt-ftparchive(인자를 남기고 Packages·Release 모양을 찍음)와 가짜 gpg(인자를 남기고 --output 파일을 만듦)를
# 쓴다. bash만 있으면 되고 macOS에서도 돈다. 진짜 도구로 apt가 읽는지는 repo-test.sh(CI 컨테이너)가 본다.
# 쓰는 법: bash packaging/tests/build-repo-test.sh
set -eu

REPO_SCRIPT="$(cd "$(dirname "$0")/../repo" && pwd)/build-repo.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/fakebin"
# 가짜 apt-ftparchive: 인자 한 줄을 calls에 남긴다.
# packages <dir>: <dir> 안의 .deb마다 Filename 줄. release <dir>: -o 값과 <dir> 아래 Packages 파일 목록.
cat >"$WORK/fakebin/apt-ftparchive" <<'EOF'
#!/bin/sh
echo "apt-ftparchive $*" >>"$FAKE_LOG"
cmd=
dir=
for a; do
	case $a in
	packages | release) cmd=$a ;;
	*) [ -n "$cmd" ] && dir=$a ;;
	esac
done
case $cmd in
packages)
	for f in "$dir"/*.deb; do
		printf 'Package: orbivisor\nFilename: %s\n\n' "$f"
	done
	;;
release)
	for a; do
		case $a in APT::FTPArchive::Release::*) echo "${a#APT::FTPArchive::Release::}" ;; esac
	done
	(cd "$dir" && find . -name 'Packages*' | LC_ALL=C sort)
	;;
*) exit 1 ;;
esac
EOF
# 가짜 gpg: 인자 한 줄을 calls에 남기고 --output 파일에 서명 대신 표시를 쓴다.
cat >"$WORK/fakebin/gpg" <<'EOF'
#!/bin/sh
echo "gpg $*" >>"$FAKE_LOG"
out=
next=
for a; do
	if [ -n "$next" ]; then
		out=$a
		next=
	fi
	[ "$a" = --output ] && next=1
done
echo signed >"$out"
EOF
chmod 0755 "$WORK/fakebin/apt-ftparchive" "$WORK/fakebin/gpg"

printf 'deb1' >"$WORK/orbivisor_1.0_amd64.deb"
printf 'deb2' >"$WORK/orbivisor_1.1_amd64.deb"
R="$WORK/repo"
D="$R/dists/trixie"

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

# run <args…>: 가짜 도구로 build-repo.sh를 돌려 종료 코드(code)·stdout·stderr·calls를 남긴다.
run() {
	code=0
	: >"$WORK/calls"
	FAKE_LOG="$WORK/calls" APT_FTPARCHIVE="$WORK/fakebin/apt-ftparchive" GPG="$WORK/fakebin/gpg" \
		OPL_REPO_GPG_KEY_ID=${KEY-ABCD1234} \
		sh "$REPO_SCRIPT" "$@" >"$WORK/stdout" 2>"$WORK/stderr" || code=$?
}

# 인자 검사: 종료 코드 2, 아무것도 만들지 않음
expect_usage() {
	if [ "$code" -eq 2 ] && [ ! -e "$R" ] && [ ! -s "$WORK/calls" ]; then
		ok "$1"
	else
		fail "$1: 종료 코드 $code"
		cat "$WORK/stderr"
	fi
}
run "$R" stable
expect_usage "deb가 없으면 사용법"
run "$R" unstable "$WORK/orbivisor_1.0_amd64.deb"
expect_usage "모르는 채널은 거절"
grep -q 'stable|testing|enterprise' "$WORK/stderr" || fail "채널 거절 문구: $(cat "$WORK/stderr")"
run "$R" stable "$WORK/missing.deb"
expect_usage "없는 deb는 거절"
KEY='' run "$R" stable "$WORK/orbivisor_1.0_amd64.deb"
if [ "$code" -eq 1 ] && [ ! -e "$R" ] && grep -q OPL_REPO_GPG_KEY_ID "$WORK/stderr"; then
	ok "OPL_REPO_GPG_KEY_ID가 없으면 실패"
else
	fail "OPL_REPO_GPG_KEY_ID 없음: 종료 코드 $code"
fi

# 첫 채널 stable
run "$R" stable "$WORK/orbivisor_1.0_amd64.deb"
if [ "$code" -eq 0 ] && [ "$(cat "$R/pool/stable/orbivisor_1.0_amd64.deb")" = deb1 ] &&
	[ -f "$D/stable/binary-amd64/Packages" ] && [ -f "$D/Release" ] &&
	[ -f "$D/InRelease" ] && [ -f "$D/Release.gpg" ]; then
	ok "stable 레이아웃"
else
	fail "stable 레이아웃: 종료 코드 $code"
	cat "$WORK/stderr"
	find "$R" | sort
fi
# Packages의 Filename은 저장소 루트 기준(apt-ftparchive를 저장소 루트에서 돌림)
if grep -qx 'Filename: pool/stable/orbivisor_1.0_amd64.deb' "$D/stable/binary-amd64/Packages" &&
	gunzip -c "$D/stable/binary-amd64/Packages.gz" | cmp -s - "$D/stable/binary-amd64/Packages"; then
	ok "Packages와 Packages.gz"
else
	fail "Packages: $(cat "$D/stable/binary-amd64/Packages" 2>&1)"
fi
# Origin·Label은 일부러 opLite에 고정한다(바뀌면 설치된 노드의 apt-get update가 실패한다 — build-repo.sh).
EXPECTED_RELEASE='Origin=opLite
Label=opLite
Suite=trixie
Codename=trixie
Components=stable
Architectures=amd64
./stable/binary-amd64/Packages
./stable/binary-amd64/Packages.gz'
if [ "$(cat "$D/Release")" = "$EXPECTED_RELEASE" ]; then
	ok "Release 값"
else
	fail "Release: $(cat "$D/Release")"
fi
# 서명은 옮기기 전의 새 Release(dists/.trixie.new)에 한다.
S="$R/dists/.trixie.new"
if grep -q -- "--local-user ABCD1234 .*--clearsign --output $S/InRelease $S/Release" "$WORK/calls" &&
	grep -q -- "--local-user ABCD1234 .*--detach-sign --output $S/Release.gpg $S/Release" "$WORK/calls" &&
	grep -q -- '--batch' "$WORK/calls"; then
	ok "gpg 서명 인자"
else
	fail "gpg 인자: $(cat "$WORK/calls")"
fi
# 저장소에 임시 파일을 남기지 않는다.
if [ -z "$(find "$R" -name '*.new' -o -name '.*' -type f)" ]; then
	ok "임시 파일 없음"
else
	fail "임시 파일: $(find "$R" -name '*.new' -o -name '.*' -type f)"
fi

# 둘째 채널 testing: stable은 그대로, Release는 두 채널을 다시 묶는다.
cp "$D/stable/binary-amd64/Packages" "$WORK/stable.Packages"
run "$R" testing "$WORK/orbivisor_1.1_amd64.deb"
if [ "$code" -eq 0 ] && cmp -s "$WORK/stable.Packages" "$D/stable/binary-amd64/Packages" &&
	grep -qx 'Filename: pool/testing/orbivisor_1.1_amd64.deb' "$D/testing/binary-amd64/Packages" &&
	! grep -q 'pool/stable' "$D/testing/binary-amd64/Packages" &&
	! grep -q 'packages pool/stable' "$WORK/calls"; then
	ok "다른 채널의 Packages는 남긴다"
else
	fail "testing 추가: 종료 코드 $code"
	cat "$WORK/stderr" "$WORK/calls"
fi
if grep -qx 'Components=stable testing' "$D/Release" &&
	grep -qx './stable/binary-amd64/Packages' "$D/Release" &&
	grep -qx './testing/binary-amd64/Packages' "$D/Release"; then
	ok "Release가 모든 채널을 묶는다"
else
	fail "Release: $(cat "$D/Release")"
fi

# 같은 채널에 새 버전: 옛 패키지도 pool에 남고 Packages에 둘 다 있다.
run "$R" stable "$WORK/orbivisor_1.1_amd64.deb"
if [ "$code" -eq 0 ] && [ "$(grep -c '^Filename: pool/stable/' "$D/stable/binary-amd64/Packages")" -eq 2 ]; then
	ok "같은 채널에 더하면 옛 버전도 남는다"
else
	fail "stable 다시: $(cat "$D/stable/binary-amd64/Packages")"
fi

# enterprise도 채널이다. Components는 stable testing enterprise 순서.
run "$R" enterprise "$WORK/orbivisor_1.1_amd64.deb"
if [ "$code" -eq 0 ] && grep -qx 'Components=stable testing enterprise' "$D/Release" &&
	[ "$(tail -n1 "$WORK/stdout")" = "$R" ]; then
	ok "enterprise 채널"
else
	fail "enterprise: 종료 코드 $code, $(cat "$D/Release")"
fi

# 같은 이름에 내용이 다른 deb는 거절하고, 같은 deb는 넘어간다.
mkdir -p "$WORK/other"
printf 'rebuilt' >"$WORK/other/orbivisor_1.0_amd64.deb"
run "$R" stable "$WORK/other/orbivisor_1.0_amd64.deb"
if [ "$code" -eq 1 ] && grep -q 'version already published: ' "$WORK/stderr" &&
	[ "$(cat "$R/pool/stable/orbivisor_1.0_amd64.deb")" = deb1 ] && [ ! -s "$WORK/calls" ]; then
	ok "같은 이름의 다른 deb는 거절"
else
	fail "다시 올린 deb: 종료 코드 $code, $(cat "$WORK/stderr")"
fi
run "$R" stable "$WORK/orbivisor_1.0_amd64.deb"
if [ "$code" -eq 0 ]; then
	ok "같은 deb는 넘어간다"
else
	fail "같은 deb: 종료 코드 $code, $(cat "$WORK/stderr")"
fi

# 서명이 실패하면 실패하고, 색인과 서명은 옛 것 그대로다.
INDEX="Release InRelease Release.gpg stable/binary-amd64/Packages stable/binary-amd64/Packages.gz"
for f in $INDEX; do
	mkdir -p "$WORK/before/$(dirname "$f")"
	cp "$D/$f" "$WORK/before/$f"
done
unchanged() {
	for f in $INDEX; do
		cmp -s "$WORK/before/$f" "$D/$f" || return 1
	done
	[ ! -e "$R/dists/.trixie.new" ] && [ ! -e "$R/dists/.Release.new" ]
}
printf 'deb3' >"$WORK/orbivisor_1.2_amd64.deb"
cp "$WORK/fakebin/gpg" "$WORK/fakebin/gpg.ok"
printf '#!/bin/sh\nexit 3\n' >"$WORK/fakebin/gpg"
run "$R" stable "$WORK/orbivisor_1.2_amd64.deb"
if [ "$code" -ne 0 ] && unchanged; then
	ok "서명이 실패하면 실패하고 옛 색인과 서명이 남는다"
else
	fail "서명 실패: 종료 코드 $code, $(find "$R/dists" | sort)"
fi
# 둘째 서명(Release.gpg)만 실패해도 같다.
cat >"$WORK/fakebin/gpg" <<'EOF'
#!/bin/sh
for a; do [ "$a" = --detach-sign ] && exit 3; done
exec sh "$(dirname "$0")/gpg.ok" "$@"
EOF
run "$R" stable "$WORK/orbivisor_1.2_amd64.deb"
if [ "$code" -ne 0 ] && unchanged; then
	ok "Release.gpg 서명만 실패해도 옛 InRelease가 남는다"
else
	fail "Release.gpg 실패: 종료 코드 $code, $(find "$R/dists" | sort)"
fi

if [ "$FAILED" -ne 0 ]; then
	echo "build-repo-test: 실패"
	exit 1
fi
echo "build-repo-test: 모두 통과"
