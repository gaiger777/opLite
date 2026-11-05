#!/bin/bash
# build-deb.sh가 바이너리의 --version이 패키지 버전과 같은지 확인하는지, OPL_APT_KEY의 키링을 싣는지,
# OPL_LICENSE_KEYS 없이는 OPL_DEV_LICENSE=1일 때만 만드는지, OPL_LICENSE_KEYS의 항목·개발 키·opld에 든 값을
# 확인하는지 본다.
# 가짜 opld·oplctl(버전을 찍는 스크립트)과 가짜 dpkg-deb(만들 파일과 패키지 트리 사본만 만듦)를 쓴다. bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/build-deb-test.sh
set -eu

# 사용자 셸의 값에 결과가 흔들리지 않게 한다(아래 run이 OPL_DEV_LICENSE 기본 1을 준다).
unset OPL_DEV_LICENSE OPL_LICENSE_KEYS

DEB_DIR="$(cd "$(dirname "$0")/../deb" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/fakebin"
# 가짜 dpkg-deb: 마지막 인자(만들 .deb)를 만들고, 그 앞 인자(패키지 트리)를 $WORK/tree로 복사한다.
cat >"$WORK/fakebin/dpkg-deb" <<'EOF'
#!/bin/sh
for a; do
	root=${out-}
	out=$a
done
echo deb >"$out"
rm -rf "$(dirname "$out")/../tree"
cp -Rp "$root" "$(dirname "$out")/../tree"
EOF
chmod 0755 "$WORK/fakebin/dpkg-deb"

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

# fake_bins <opld 버전> <oplctl 버전> [opld에 넣을 문자열]: <bin> --version이 "<bin> <버전>"을 찍는
# 가짜 바이너리. 세 번째 인자는 OPL_LICENSE_KEYS로 빌드한 opld처럼 바이너리 안에 둔다.
fake_bins() {
	rm -rf "${WORK:?}/bin"
	mkdir -p "$WORK/bin"
	printf '#!/bin/sh\necho "opld %s"\n# %s\n' "$1" "${3-}" >"$WORK/bin/opld"
	printf '#!/bin/sh\necho "oplctl %s"\n' "$2" >"$WORK/bin/oplctl"
	chmod 0755 "$WORK/bin/opld" "$WORK/bin/oplctl"
}

# run <버전>: build-deb.sh를 돌려 종료 코드(code)·stdout·stderr를 남긴다.
# OPL_DEV_LICENSE는 따로 주지 않으면 1(개발 패키지)이다.
run() {
	rm -rf "$WORK/out"
	code=0
	PATH="$WORK/fakebin:$PATH" OPL_BIN_DIR="$WORK/bin" OPL_DEV_LICENSE="${OPL_DEV_LICENSE-1}" \
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

# OPL_APT_KEY: 저장소 공개 키를 /usr/share/keyrings/oplite-archive-keyring.gpg(0644)로 싣고 md5sums에 넣는다.
KEYRING=usr/share/keyrings/oplite-archive-keyring.gpg
fake_bins 0.1.0 0.1.0
run 0.1.0
if [ "$code" -eq 0 ] && [ ! -e "$WORK/tree/usr/share/keyrings" ]; then
	ok "OPL_APT_KEY가 없으면 키링을 싣지 않는다"
else
	fail "키링 없음: 종료 코드 $code"
fi
printf 'public key' >"$WORK/archive.gpg"
chmod 0600 "$WORK/archive.gpg"
OPL_APT_KEY="$WORK/archive.gpg" run 0.1.0
if [ "$code" -eq 0 ] && cmp -s "$WORK/archive.gpg" "$WORK/tree/$KEYRING" &&
	[ -n "$(find "$WORK/tree/$KEYRING" -perm 0644)" ] &&
	grep -q " $KEYRING\$" "$WORK/tree/DEBIAN/md5sums"; then
	ok "OPL_APT_KEY의 키링을 싣는다"
else
	fail "키링: 종료 코드 $code"
	cat "$WORK/stderr"
fi
OPL_APT_KEY="$WORK/missing.gpg" run 0.1.0
if [ "$code" -eq 1 ] && grep -q OPL_APT_KEY "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
	ok "OPL_APT_KEY 파일이 없으면 실패"
else
	fail "없는 OPL_APT_KEY: 종료 코드 $code"
fi
printf -- '-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmQINBG\n-----END PGP PUBLIC KEY BLOCK-----\n' >"$WORK/archive.asc"
OPL_APT_KEY="$WORK/archive.asc" run 0.1.0
if [ "$code" -eq 1 ] && grep -q 'OPL_APT_KEY must be a binary keyring (gpg --export, not --armor)' "$WORK/stderr" &&
	[ ! -e "$WORK/out" ]; then
	ok "armor 키는 거절"
else
	fail "armor OPL_APT_KEY: 종료 코드 $code"
fi

# 구독 키 공개 키: OPL_LICENSE_KEYS도 OPL_DEV_LICENSE=1도 없으면 만들지 않는다.
fake_bins 0.1.0 0.1.0
OPL_LICENSE_KEYS='' OPL_DEV_LICENSE='' run 0.1.0
if [ "$code" -eq 1 ] && grep -q 'build-deb: OPL_LICENSE_KEYS is not set; set it for a release build or OPL_DEV_LICENSE=1 for a development package' "$WORK/stderr" &&
	[ ! -e "$WORK/out" ]; then
	ok "OPL_LICENSE_KEYS 없이 개발 패키지가 아니면 실패"
else
	fail "OPL_LICENSE_KEYS 없음: 종료 코드 $code"
fi
OPL_LICENSE_KEYS='' OPL_DEV_LICENSE=0 run 0.1.0
if [ "$code" -eq 1 ] && [ ! -e "$WORK/out" ]; then
	ok "OPL_DEV_LICENSE가 1이 아니면 실패"
else
	fail "OPL_DEV_LICENSE=0: 종료 코드 $code"
fi
# 정식 키: opl-license keygen과 같은 형식(base64 32바이트)의 개발 키가 아닌 키 둘
RELEASE_KEYS='Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyM=,mR8vT2wX4yZ6aB8cD0eF2gH4iJ6kL8mN0oP2qR4sT6U='
DEV_KEY=$(cat "$DEB_DIR/../../crates/opl-types/keys/dev-license.pub")
fake_bins 0.1.0 0.1.0 "$RELEASE_KEYS"
OPL_LICENSE_KEYS=$RELEASE_KEYS OPL_DEV_LICENSE='' run 0.1.0
if [ "$code" -eq 0 ] && [ -f "$WORK/out/oplite_0.1.0_amd64.deb" ]; then
	ok "OPL_LICENSE_KEYS가 있으면 만든다"
else
	fail "OPL_LICENSE_KEYS 있음: 종료 코드 $code"
	cat "$WORK/stderr"
fi
# check_key_error <이름> <문구>: 종료 코드 1, 그 문구, 패키지 없음
check_key_error() {
	if [ "$code" -eq 1 ] && grep -qF -- "$2" "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
		ok "$1"
	else
		fail "$1: 종료 코드 $code"
		cat "$WORK/stderr"
	fi
}
fake_bins 0.1.0 0.1.0 "$DEV_KEY"
OPL_LICENSE_KEYS=$DEV_KEY OPL_DEV_LICENSE='' run 0.1.0
check_key_error "OPL_LICENSE_KEYS의 개발 키는 거절" \
	'build-deb: OPL_LICENSE_KEYS contains the development key; use OPL_DEV_LICENSE=1 for a development package'
fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*},$DEV_KEY"
OPL_LICENSE_KEYS="${RELEASE_KEYS%%,*},$DEV_KEY" OPL_DEV_LICENSE='' run 0.1.0
check_key_error "목록에 섞인 개발 키도 거절" 'OPL_LICENSE_KEYS contains the development key'
# 패딩 없는 키, 끝 비트가 0이 아닌(정규형이 아닌) 키도 거절한다.
for bad in 'not-base64!' 'AAAA' 'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyMA' \
	'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyM' 'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyN='; do
	fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*},$bad"
	OPL_LICENSE_KEYS="${RELEASE_KEYS%%,*},$bad" OPL_DEV_LICENSE='' run 0.1.0
	check_key_error "32바이트 base64가 아닌 항목 거절: $bad" \
		"build-deb: OPL_LICENSE_KEYS entry is not a base64 Ed25519 public key: $bad"
done
# 공백·쉼표뿐이면 키가 하나도 없다.
for none in ' ' ','; do
	fake_bins 0.1.0 0.1.0 "$none"
	OPL_LICENSE_KEYS=$none OPL_DEV_LICENSE='' run 0.1.0
	check_key_error "키가 없는 OPL_LICENSE_KEYS 거절: '$none'" 'build-deb: OPL_LICENSE_KEYS has no keys'
done
fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*}"
OPL_LICENSE_KEYS=$RELEASE_KEYS OPL_DEV_LICENSE='' run 0.1.0
check_key_error "opld에 OPL_LICENSE_KEYS가 그대로 없으면 거절" \
	'build-deb: opld was not built with this OPL_LICENSE_KEYS'

if [ "$FAILED" -ne 0 ]; then
	echo "build-deb-test: 실패"
	exit 1
fi
echo "build-deb-test: 모두 통과"
