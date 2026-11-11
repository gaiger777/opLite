#!/bin/bash
# shellcheck disable=SC2016 # 작은따옴표 안의 정규식·예시 줄은 $와 백틱을 글자 그대로 비교한다.
# 옛 제품 이름(opLite, oplite)이 허용 목록 밖에 남지 않았는지 본다(설계 2026-11-10-rename-orbivisor §3.10·§3.12).
# git grep -nI -i oplite의 줄마다 그 파일에 허용한 낱말을 지우고도 oplite(대소문자 무관)가 남으면 실패다.
# 이름에 oplite가 있는 파일도 전환 패키지 control과 V0 픽스처 밖에 있으면 실패다.
# bash(3.2도 됨)와 git만 있으면 된다. CI shell 작업에서 shellcheck 옆에 돈다.
# 쓰는 법: bash packaging/tests/name-test.sh
set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

# 통째로 허용하는 파일·디렉터리(git grep에서 뺀다)
EXCLUDE=(
	# 이 시험(허용 목록)
	':!packaging/tests/name-test.sh'
	# 지난 계획·설계 문서는 고쳐 쓰지 않는다(이름 바꾸기 설계도 옛 이름을 설명한다).
	':!docs/superpowers/plans'
	':!docs/superpowers/specs'
	# 전환 패키지 oplite의 control과 이름 바꾸기 전 패키지(V0)를 흉내 내는 픽스처
	':!packaging/deb/oplite.control.in'
	':!packaging/tests/fixtures/oplite-v0'
)

# 허용 규칙 "<파일 glob>|<ERE>": 파일이 glob에 맞으면 줄에서 ERE에 맞는 부분을 지운다(대소문자 구분).
RULES=(
	# APT 저장소 파일 이름(설치된 노드에 있다 — §3.5)
	'*|oplite\.sources'
	'*|oplite\.conf'
	'*|oplite-archive-keyring\.gpg'
	# GitHub 저장소 링크(저장소 이름을 바꿀 때 함께 바꾼다 — §3.11)
	'*|github\.com/gaiger777/opLite'
	# APT Release의 Origin·Label(바뀌면 설치된 노드의 apt-get update가 실패한다 — §3.5)과 그것을 흉내 낸 apt 출력
	'*|(Origin|Label)[=:] ?opLite'
	'*|opLite:trixie'
	'packaging/repo/build-repo.sh|Origin·Label은 제품 이름이 바뀌어도 opLite'
	'packaging/tests/build-repo-test.sh|Origin·Label은 일부러 opLite'
	# 시험 IdP의 client_id·aud(시험 자료)
	'crates/opl-api/src/oidc.rs|"oplite"'
	'crates/opl-api/tests/common/idp.rs|"oplite"|`oplite`'
	'crates/opl-api/tests/oidc_login.rs|oplite:client-secret'
	'crates/opl-api/tests/realms.rs|"oplite"'
	'crates/opl-types/src/realm.rs|"oplite"'
	'crates/oplctl/src/cli.rs|"oplite"|client id: oplite'
	'web/src/realms/RealmsPage.test.tsx|"oplite"'
	# 전환 패키지를 만들고 시험하는 파일에만 있는 모양(전환 패키지의 문서 디렉터리, 옛 V0 트리 등)
	'packaging/deb/build-deb.sh|usr/share/doc/oplite'
	'packaging/tests/build-deb-test.sh|usr/share/(doc/)?oplite|tree-oplite|Replaces oplite\)'
	'packaging/tests/repo-test.sh|usr/share/(doc/)?oplite|"oplite\(:amd64\)\?: |\$4 == "oplite(:amd64)?"'
	'packaging/tests/deb-test.sh|oplite\.\$script'
	'packaging/tests/maint-test.sh|OPL_FAKE_OPLITE_CONFIG|mirror\.lan/oplite|-f \$\{Config-Version\} oplite'
	'.github/workflows/ci.yml|\(oplite\)'
	'.github/actions/build-deb/action.yml|transitional oplite package'
	# README의 "이름 바꾸기" 절이 보이는 옛 이름과 그대로 둔 저장소 주소 예
	'README.md|opLite → Orbivisor'
	'README.md|\| `oplite` \||새 판의 `oplite`는|`oplite` 두 줄'
	'README.md|`oplite\.iso`, "Install opLite"'
	'README.md|`/usr/share/oplite/opld\.toml\.example`'
	'README.md|/etc/issue\.d/oplite\.issue'
	'README.md|s/opLite/'
	'README.md|`apt-get remove oplite`'
	'README.md|`dpkg -l oplite`'
	'README.md|sudo apt install \./orbivisor_0\.1\.0_amd64\.deb \./oplite_0\.1\.0_amd64\.deb'
	'README.md|"opLite cluster X CA"'
	'README.md|apt\.example\.com/oplite'
)

# 전환 패키지 oplite(소문자 패키지 이름)를 만들고 넘기고 시험하는 파일에서 패키지 이름으로 쓰는 모양
PKG_FILES=(
	packaging/deb/build-deb.sh
	packaging/deb/control.in
	packaging/deb/postinst
	packaging/repo/build-repo.sh
	packaging/tests/build-deb-test.sh
	packaging/tests/deb-test.sh
	packaging/tests/repo-test.sh
	packaging/tests/maint-test.sh
	packaging/tests/iso-test.sh
	.github/workflows/ci.yml
	.github/actions/build-deb/action.yml
)
PKG_FORMS=(
	# deb 파일 이름, control, 전환 control 파일과 V0 픽스처 경로
	'oplite_[^ ]*_amd64\.deb'
	'<oplite deb>'
	'(Breaks|Replaces): oplite \(<< '
	'Package: oplite'
	'oplite\.control\.in'
	'fixtures/oplite-v0'
	# 패키지를 다루는 명령과 apt 출력
	'(apt-get [a-z -]+|dpkg --verify|dpkg-query -W -f [^ ]+|Inst) oplite'
	# 문장 속 패키지 이름
	'(옛|전환|전환 패키지|설치된|orbivisor·) ?oplite'
	'oplite(가|를|는|에서|\(전환\)| V[01]| \$V0| \$VERSION| 버전| Depends| 업그레이드)'
)
for f in "${PKG_FILES[@]}"; do
	for form in "${PKG_FORMS[@]}"; do
		RULES+=("$f|$form")
	done
done

# leftover <파일> <줄>: 허용 낱말을 지운 뒤 oplite가 남으면 참
leftover() {
	local path=$1 text=$2 rule glob re found=1
	for rule in "${RULES[@]}"; do
		glob=${rule%%|*} re=${rule#*|}
		# shellcheck disable=SC2053 # glob으로 비교한다.
		[[ $path == $glob ]] || continue
		while [[ $text =~ $re ]]; do
			text=${text/"${BASH_REMATCH[0]}"/}
		done
	done
	shopt -s nocasematch
	[[ $text == *oplite* ]] && found=0
	shopt -u nocasematch
	return $found
}

# name_left <경로>: 파일 이름에 옛 이름이 있으면 참. 전환 패키지 control과 V0 픽스처만 허용한다.
name_left() {
	local found=1
	case $1 in
	packaging/deb/oplite.control.in | packaging/tests/fixtures/oplite-v0/*) return 1 ;;
	esac
	shopt -s nocasematch
	[[ $1 == *oplite* ]] && found=0
	shopt -u nocasematch
	return $found
}

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

# 규칙 자체: 허용한 낱말만 지우고 나머지 옛 이름은 남긴다.
# expect <기대 left|clean> <파일> <줄>
expect() {
	local got=clean
	if leftover "$2" "$3"; then got=left; fi
	if [ "$got" = "$1" ]; then ok "규칙 $1: $2: $3"; else fail "규칙 $2: $3 → $got (기대 $1)"; fi
}
expect clean crates/opld/src/config.rs '"/etc/apt/sources.list.d/oplite.sources".into()'
expect clean packaging/tests/repo-test.sh "grep -qx 'Origin: opLite' \"\$REPO/dists/trixie/Release\""
expect clean crates/oplctl/src/cli.rs '            "oplite",'
expect clean packaging/deb/build-deb.sh 'TRANS_DEB="$OUT_DIR/oplite_${VERSION}_amd64.deb"'
expect left crates/oplctl/src/cli.rs 'about = "opLite command-line client"'
expect left crates/opl-api/src/oidc.rs 'oplite.sources and opLite'
expect left packaging/deb/build-deb.sh 'Description: opLite node'
expect left web/src/App.tsx '"oplite"'
expect clean packaging/deb/control.in 'Breaks: oplite (<< @VERSION@)'
expect clean packaging/tests/repo-test.sh 'apt-get install -y --no-install-recommends oplite'
expect clean packaging/deb/postinst '		# 옛 oplite에서 넘어옴: 그때 oplite V1은 unpack만 된 상태라 Config-Version이 옛 판이다.'
expect left packaging/deb/postinst 'systemctl restart oplite.service'
expect left packaging/deb/build-deb.sh 'install -m 0644 "$DEB_DIR/opld.toml.example" "$STAGE/usr/share/oplite/opld.toml.example"'
expect left packaging/tests/deb-test.sh '[ "$(oplite --version)" = "orbivisord $VERSION" ]'
expect left .github/workflows/ci.yml '          name: oplite-deb'
expect left packaging/repo/build-repo.sh '# opLite APT 저장소'

# 파일 이름 규칙 자체
# expect_name <기대 left|clean> <경로>
expect_name() {
	local got=clean
	if name_left "$2"; then got=left; fi
	if [ "$got" = "$1" ]; then ok "파일 이름 $1: $2"; else fail "파일 이름 $2 → $got (기대 $1)"; fi
}
expect_name clean packaging/deb/oplite.control.in
expect_name clean packaging/tests/fixtures/oplite-v0/postinst
expect_name left packaging/iso/oplite.cfg
expect_name left packaging/tests/fixtures/oplite.issue
expect_name left web/src/OpLiteLogo.tsx
expect left packaging/iso/build-iso.sh 'cp "$DEB" "$tree/oplite/"'
expect clean README.md '| 설치 ISO | `oplite.iso`, "Install opLite" | `orbivisor.iso`, "Install Orbivisor" |'
expect left README.md 'packaging/iso/build-iso.sh --deb dist/oplite_0.1.0_amd64.deb --out oplite.iso'
expect left README.md '패키지 `oplite_0.1.0_amd64.deb`와 opLite 저장소'
expect clean README.md '  `sudo apt install ./orbivisor_0.1.0_amd64.deb ./oplite_0.1.0_amd64.deb`. 새 deb 하나만 설치하면'
expect clean README.md '| Debian 패키지 | `oplite` | `orbivisor`(옛 이름은 빈 전환 패키지) |'
expect left README.md 'sudo dpkg -i ./oplite_0.1.0_amd64.deb'
expect left README.md 'sudo cp /usr/share/oplite/opld.toml.example /etc/opl/opld.toml'
expect left README.md '패키지 `oplite`를 설치한다'
expect left README.md 'packaging/iso/build-iso.sh --deb dist/orbivisor_0.1.0_amd64.deb --out `oplite.iso`'

cd "$ROOT"
code=0
lines=$(git grep -nI -i oplite -- . "${EXCLUDE[@]}" </dev/null) || code=$?
# git grep: 0 찾음, 1 없음, 그 밖은 오류
if [ "$code" -gt 1 ]; then
	echo "name-test: git grep 실패(종료 코드 $code)"
	exit 1
fi
left=0
while IFS= read -r line; do
	[ -n "$line" ] || continue
	path=${line%%:*}
	rest=${line#*:}
	if leftover "$path" "${rest#*:}"; then
		echo "  $line"
		left=$((left + 1))
	fi
done <<<"$lines"
if [ "$left" -eq 0 ]; then ok "허용 목록 밖의 oplite 없음"; else fail "허용 목록 밖의 oplite ${left}줄(위)"; fi

# 파일 이름(지난 계획·설계 문서는 뺀다)
names=$(git ls-files -- . ':!docs/superpowers/plans' ':!docs/superpowers/specs' </dev/null)
left=0
while IFS= read -r path; do
	[ -n "$path" ] || continue
	if name_left "$path"; then
		echo "  $path"
		left=$((left + 1))
	fi
done <<<"$names"
if [ "$left" -eq 0 ]; then ok "이름에 oplite가 있는 파일은 전환 control과 V0 픽스처뿐"; else fail "이름에 oplite가 있는 파일 ${left}개(위)"; fi

if [ "$FAILED" -ne 0 ]; then
	echo "name-test: 실패"
	exit 1
fi
echo "name-test: 모두 통과"
