#!/bin/bash
# shellcheck disable=SC2016 # 작은따옴표 안의 정규식·예시 줄은 $와 백틱을 글자 그대로 비교한다.
# 옛 제품 이름(opLite, oplite)이 허용 목록 밖에 남지 않았는지 본다(설계 2026-11-10-rename-orbivisor §3.10·§3.12).
# git grep -nI -i oplite의 줄마다 그 파일에 허용한 낱말을 지우고도 oplite(대소문자 무관)가 남으면 실패다.
# 이름에 oplite가 있는 파일도 전환 패키지 control과 V0 픽스처 밖에 있으면 실패다.
# 노드 위의 옛 이름(계획 74 설계 2026-11-26-naming-orbivisor §3.7 — /etc/opl·/var/lib/opl·/run/opl·/var/log/opl,
# opld.service, opl-ovn-*, OPL_* 환경 변수, 옛 크레이트 이름)도 호환 코드·그 시험·안내의 이름 바꾸기 절 밖에 있으면 실패다.
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
	'crates/orbivisor-api/src/oidc.rs|"oplite"'
	'crates/orbivisor-api/tests/common/idp.rs|"oplite"|`oplite`'
	'crates/orbivisor-api/tests/oidc_login.rs|oplite:client-secret'
	'crates/orbivisor-api/tests/realms.rs|"oplite"'
	'crates/orbivisor-types/src/realm.rs|"oplite"'
	'crates/orbivisorctl/src/cli.rs|"oplite"|client id: oplite'
	'web/src/realms/RealmsPage.test.tsx|"oplite"'
	# 옛 경로 짝(/etc/opl)이 이름이 비슷한 /etc/oplite를 옛 경로로 보지 않는지 보는 시험 자료(계획 74)
	'crates/orbivisor-types/src/layout.rs|"/etc/oplite/a"|\(`/etc/oplite`\)|/etc/oplite /etc/opl\.d x'
	'crates/orbivisord/src/layout.rs|# /etc/oplite'
	# 전환 패키지를 만들고 시험하는 파일에만 있는 모양(전환 패키지의 문서 디렉터리, 옛 V0 트리 등)
	'packaging/deb/build-deb.sh|usr/share/doc/oplite'
	'packaging/tests/build-deb-test.sh|usr/share/(doc/)?oplite|tree-oplite|Replaces oplite\)'
	'packaging/tests/repo-test.sh|usr/share/(doc/)?oplite|"oplite\(:amd64\)\?: |\$4 == "oplite(:amd64)?"'
	'packaging/tests/deb-test.sh|oplite\.\$script'
	'packaging/tests/maint-test.sh|ORBIVISOR_FAKE_OPLITE_CONFIG|mirror\.lan/oplite|-f \$\{Config-Version\} oplite'
	'.github/workflows/ci.yml|\(oplite\)'
	'.github/actions/build-deb/action.yml|transitional oplite package'
	# docs/guide/getting-started.md의 "이름 바꾸기" 절이 보이는 옛 이름과 그대로 둔 저장소 주소 예
	'docs/guide/getting-started.md|opLite → Orbivisor|#이름-바꾸기-oplite--orbivisor'
	'docs/guide/getting-started.md|\| `oplite` \||새 판의 `oplite`는|`oplite` 두 줄'
	'docs/guide/getting-started.md|`oplite\.iso`, "Install opLite"'
	'docs/guide/getting-started.md|`/usr/share/oplite/opld\.toml\.example`'
	'docs/guide/getting-started.md|/etc/issue\.d/oplite\.issue'
	'docs/guide/getting-started.md|s/opLite/'
	'docs/guide/getting-started.md|`apt-get remove oplite`'
	'docs/guide/getting-started.md|`dpkg -l oplite`'
	'docs/guide/getting-started.md|sudo apt install \./orbivisor_0\.1\.0_amd64\.deb \./oplite_0\.1\.0_amd64\.deb'
	'docs/guide/getting-started.md|"opLite cluster X CA"'
	'docs/guide/getting-started.md|apt\.example\.com/oplite'
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
expect clean crates/orbivisord/src/config.rs '"/etc/apt/sources.list.d/oplite.sources".into()'
expect clean packaging/tests/repo-test.sh "grep -qx 'Origin: opLite' \"\$REPO/dists/trixie/Release\""
expect clean crates/orbivisorctl/src/cli.rs '            "oplite",'
expect clean packaging/deb/build-deb.sh 'TRANS_DEB="$OUT_DIR/oplite_${VERSION}_amd64.deb"'
expect left crates/orbivisorctl/src/cli.rs 'about = "opLite command-line client"'
expect left crates/orbivisor-api/src/oidc.rs 'oplite.sources and opLite'
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
expect clean docs/guide/getting-started.md '| 설치 ISO | `oplite.iso`, "Install opLite" | `orbivisor.iso`, "Install Orbivisor" |'
expect left docs/guide/getting-started.md 'packaging/iso/build-iso.sh --deb dist/oplite_0.1.0_amd64.deb --out oplite.iso'
expect left docs/guide/getting-started.md '패키지 `oplite_0.1.0_amd64.deb`와 opLite 저장소'
expect clean docs/guide/getting-started.md '  `sudo apt install ./orbivisor_0.1.0_amd64.deb ./oplite_0.1.0_amd64.deb`. 새 deb 하나만 설치하면'
expect clean docs/guide/getting-started.md '| Debian 패키지 | `oplite` | `orbivisor`(옛 이름은 빈 전환 패키지) |'
expect left docs/guide/getting-started.md 'sudo dpkg -i ./oplite_0.1.0_amd64.deb'
expect left docs/guide/getting-started.md 'sudo cp /usr/share/oplite/opld.toml.example /etc/opl/opld.toml'
expect left docs/guide/getting-started.md '패키지 `oplite`를 설치한다'
expect left docs/guide/getting-started.md 'packaging/iso/build-iso.sh --deb dist/orbivisor_0.1.0_amd64.deb --out `oplite.iso`'

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

# 노드 위의 옛 이름(계획 74). 단어 경계(\b)는 macOS bash의 정규식에 없으므로 [^A-Za-z0-9_]로 쓴다.
W='([^A-Za-z0-9_]|$)'
OLD_RE="/etc/opl$W|/var/lib/opl$W|/run/opl$W|/var/log/opl$W|opld\.service|opl-ovn-|(^|[^A-Za-z0-9_])OPL_[A-Z]"
OLD_RE+="|opl[-_](types|api|cluster|driver|peer|qemu|reconciler|restore|store)$W|crates/opl(d|ctl)?$W"
# 계획 74가 바꾼 나머지(리뷰 1): 소켓·설정·잠금·CLI 로그인 파일, 일시 유닛 접두사, 쿠키, 크레이트 인자·경로, 개발 변수
OLD_RE+="|opld\.(sock|toml|lock)|oplctl\.json|\.config/opl$W|opl-(upgrade|bkserve|fr)-|opl_(session|oidc)$W"
OLD_RE+="|-p (opld|oplctl)$W|opl-devserver|(^|[^A-Za-z0-9_])OPLD_[A-Z]|(^|[^A-Za-z0-9_])(opld|oplctl)::|(^|[^A-Za-z0-9_./-])opld/[a-z]"

# 통째로 허용하는 파일: 옛 이름을 다루는 것이 일인 호환 코드와 유지 스크립트, 그것을 시험하는 패키징 시험
OLD_EXCLUDE=(
	':!packaging/tests/name-test.sh'
	':!docs/superpowers/plans'
	':!docs/superpowers/specs'
	# 옛 배치·옛 환경 변수 받기(옛↔새 짝 표와 그 시험)
	':!crates/orbivisor-types/src/layout.rs'
	':!crates/orbivisor-types/src/legacy.rs'
	':!crates/orbivisord/src/layout.rs'
	# OVN 유닛 옛 이름을 별칭 링크로 바꾸기
	':!crates/orbivisor-driver/src/ovn/units.rs'
	# 옛 판(V0) 픽스처(옛 판 그대로의 파일)
	':!packaging/tests/fixtures/oplite-v0'
)

# 허용 규칙 "<파일 glob>|<ERE>"(위 RULES와 같은 모양): 옛 이름을 일부러 받는 줄의 모양
OLD_RULES=(
	# 이름 바꾸기 앞에 저장된 ACME 훅 경로(spec에 들어 있다)
	'crates/orbivisor-types/src/acme.rs|LEGACY_HOOK_DIR: &str = "/etc/opl/acme-hooks/"|hook_(path_ok|rest)\("/etc/opl/[^"]*"\)'
	'crates/orbivisor-api/src/acme/dns/hook.rs|/etc/opl/acme-hooks/'
	'crates/orbivisor-api/src/acme/api.rs|옛 판은 `/etc/opl/acme-hooks/`만'
	'web/src/certs/dialogs.tsx|/etc/opl/acme-hooks/'
	# 섞인 클러스터에서 옛 이름으로 적는 경로, 옛 Task가 든 경로
	'crates/orbivisor-api/src/ceph.rs|옛 이름\(`/etc/opl/…`\)'
	'crates/orbivisor-api/src/tasks.rs|옛 경로\(`/var/lib/opl/uploads/…`\)'
	'web/src/ceph/ceph.ts|"/(etc|var/lib|run|var/log)/opl"|옛 이름\(`/etc/opl` 등\)'
	'web/src/ceph/ceph.test.ts|"/etc/opl[^"]*"|옛 /etc/opl\)'
	# OVN 유닛 옛 이름 옮기기와 그 시험
	'crates/orbivisor-driver/src/ovn/host.rs|앞의 단위\(`opl-ovn-\*\.service`\)'
	'crates/orbivisor-driver/src/ovn/host/tests.rs|opl-ovn-'
	# 옛 빌드 변수 이름으로 빌드한 값 받기
	'crates/orbivisor-types/src/version.rs|OPL_(VERSION|APT_URL|TELEMETRY_URL)'
	'crates/orbivisor-types/src/subscription.rs|OPL_LICENSE_KEYS'
	'crates/orbivisord/tests/version.rs|OPL_VERSION'
	'crates/orbivisorctl/tests/version.rs|OPL_VERSION'
	'packaging/deb/build-deb.sh|OPL_(LICENSE_KEYS|APT_KEY|BIN_DIR)'
	# 옛 실행 변수 받기와 그 시험
	'crates/orbivisorctl/src/main.rs|옛 이름 OPL_X'
	'crates/orbivisorctl/tests/legacy_env.rs|OPL_(SERVER|CONFIG)'
	'crates/orbivisord/src/bin/orbivisor-devserver.rs|옛 이름 OPL_LOG'
	'terraform/internal/provider/provider_test.go|OPL_LEGACY_T[0-9]+'
	# 옛 제어 소켓·옛 설정 경로로 물러서기
	'crates/orbivisorctl/src/cli.rs|옛 /run/opl/opld\.sock|"/run/opl/opld\.sock"'
	'crates/orbivisord/src/main.rs|옛 `/etc/opl/opld\.toml`'
	# 유닛: 옛 설정 경로 조건과 정적 별칭(옛 VM 스코프의 Before=opld.service)
	'packaging/deb/orbivisord.service|ConditionPathExists=[|]/etc/opl/opld\.toml|별칭 Before=opld\.service'
	'packaging/deb/build-deb.sh|유닛 opld\.service|Before=opld\.service|/usr/lib/systemd/system/opld\.service"'
	# 이름 바꾸기 앞에 뜬 swtpm이 옛 경로로 연 TPM 파일
	'packaging/deb/apparmor/orbivisor-swtpm|/(var/lib|run)/opl/vm/|\(/var/lib/opl, /run/opl\)'
	# 구조 문서가 적는 옛 이름 링크·별칭("옛 `…`", "별칭 링크 `opld.service`")
	'docs/architecture.md|옛 (이름 )?(링크 )?(디렉터리\()?`[^`]*`(·`[^`]*`)*|별칭 링크 `opld\.service`|`[|]/etc/opl/opld\.toml`|덮어쓰기\(`opld\.service\.d`\)'
	# 시작하기의 설치 안내: 패키지가 까는 별칭
	'docs/guide/getting-started.md|별칭 `opld\.service`'
	# 쿠키·잠금·일시 유닛의 옛 이름을 받는 코드(리뷰 1 — 새 이름을 쓰는 줄은 그대로 걸린다). 옛 문자열은 LEGACY_ 상수의
	# 정의 줄에만 허용한다(리뷰 2 — 새 이름의 상수·함수가 옛 문자열로 되돌아가면 걸린다).
	'crates/orbivisor-api/src/auth.rs|LEGACY_SESSION_COOKIE: &str = "opl_session"|cookies\("[^"]*"\)'
	'crates/orbivisor-api/src/oidc.rs|LEGACY_STATE_COOKIE: &str = "opl_oidc"'
	'crates/orbivisor-api/src/oidc_login.rs|옛 이름 `opl_oidc`도'
	'web/src/api/schema.d.ts|옛 이름 `opl_oidc`도'
	'crates/orbivisor-api/tests/auth.rs|opl_session'
	'crates/orbivisor-api/tests/audit.rs|opl_session=\{viewer\}'
	'crates/orbivisor-api/tests/oidc_login.rs|opl_(session|oidc)'
	'crates/orbivisor-api/src/updates.rs|`opl-upgrade-…`|LEGACY_UPGRADE_PREFIX: &str = "opl-upgrade-"|"opl-upgrade-[0-9a-f]{32}"'
	'crates/orbivisor-api/tests/updates.rs|opl-upgrade-'
	'crates/orbivisor-driver/src/backup_serve.rs|LEGACY_BACKUP_SERVE_PREFIX: &str = "opl-bkserve-"|"opl-bkserve-0{8}-0{4}-0{4}-0{4}-0{12}"'
	'crates/orbivisor-driver/src/file_restore.rs|LEGACY_UNIT_PREFIX: &str = "opl-fr-"|옛 `opl-fr-<id>(\.scope)?`'
	'crates/orbivisor-driver/src/file_restore/tests.rs|opl-fr-'
	'crates/orbivisor-driver/src/ovn/host.rs|옛 디렉터리 이름\(`/etc/opl` 등\)·옛 단위 이름\(`opl-ovn-`\)'
	'crates/orbivisor-driver/src/ovn/host/tests.rs|디렉터리 이름\(`/etc/opl` 등\)'
	'crates/orbivisord/src/*|옛 이름 (링크 ?)?\(?`opld\.(sock|lock)`|\(`opld\.lock`만 잠금\)'
	'crates/orbivisord/src/main.rs|LEGACY_LOCK: &str = "opld\.lock"'
	# 옛 로그인 파일 받기와 그 시험
	'crates/orbivisorctl/src/*|~/\.config/opl/oplctl\.json|"\.config/opl/oplctl\.json"'
	'crates/orbivisorctl/tests/legacy_env.rs|~/\.config/opl/oplctl\.json|"\.config/opl/oplctl\.json"'
	# 시험이 임시 디렉터리에 두는 파일 이름(그 자리의 설정·소켓·로그인 파일)과 옛 잠금 파일. src의 시험은 임시
	# 디렉터리(`dir.path()`) 아래만 — 데몬의 `self.run_dir.join("opld.sock")` 같은 줄은 걸린다(리뷰 2).
	'crates/*/tests/*|\.(path|join)\("(sub/)?(opld\.(toml|sock|lock)|oplctl\.json)"\)'
	'crates/*/src/*|dir\.path\(\)\.join\("(sub/)?(opld\.(toml|sock)|oplctl\.json)"\)'
	# 구조·안내 문서의 옛 이름 쿠키·잠금
	'docs/*|옛 이름 `opl_(session|oidc)`도|같은 값의 `opl_session`도|, `opld\.lock`'
	# 유지 스크립트(postinst·postrm): 옛 이름 유닛 파일·enable 링크·덮어쓰기의 경로와 그 안내(옛 이름을 systemctl에
	# 넘기는 줄은 걸린다)
	'packaging/deb/postinst|/opld\.service(\.d)?|\(opld\.service\.d\)|opld\.service는 그 별칭|/etc의 (진짜 )?opld\.service|돌고 있는 opld\.service가|the old opld\.service unit|opld\.service is masked|옛 /etc/opl/opld\.toml'
	'packaging/deb/postrm|/opld\.service"|\(옛 이름 링크 /var/lib/opl·/etc/opl\)|opld\.service가 진짜 파일'
	# 옛 판으로 되돌릴 때 OVN 중앙 단위 이름 되돌리기(리뷰 2 N1)
	'packaging/deb/postrm|old_name="opl-ovn-\$db\.service"|(/etc/systemd/system/|back to )opl-ovn-\*\.service'
	# 패키징 시험: 옛 판 배치·옛 유닛 경로를 만들거나 없음을 확인하는 줄
	'packaging/tests/build-deb-test.sh|OPL_(LICENSE_KEYS|APT_KEY|BIN_DIR)|"\$UNIT_DIR/opld\.service"|ConditionPathExists=[|]/etc/opl/opld\.toml|별칭 링크 opld\.service|! -e "\$T/usr/share/orbivisor/opld\.toml\.example"|옛 이름\(/var/lib/opl,|^# /run/opl — '
	'packaging/tests/deb-test.sh|=/usr/lib/systemd/system/opld\.service|! -e /usr/share/orbivisor/opld\.toml\.example|ConditionPathExists=[|]/etc/opl/opld\.toml|! -e /etc/systemd/system/multi-user\.target\.wants/opld\.service'
	'packaging/tests/maint-test.sh|/opld\.service(\.d)?|\(opld\.service\)|진짜 opld\.service|opld\.service는 그 링크|opld\.service가 (아직 있으면|진짜 파일|링크)|ln -s opld\.service '
	'packaging/tests/maint-test.sh|"?opl-ovn-(\$db|nb|sb|northd)\.service"?'
	'packaging/tests/repo-test.sh|/(etc|var/lib)/opl(/[A-Za-z0-9._-]*)?|/opld\.(service|toml|sock)(\.example)?|옛 이름 opld\.service는 별칭|/etc/orbivisor/opld\.toml|/run/orbivisor/opld\.sock|"opld\.toml이 링크가 아님"'
)

# 시작하기의 두 "이름 바꾸기" 절(계획 43·74)은 옛 이름을 설명한다: 첫 절 머리부터 둘째 절 다음 머리 앞까지
GUIDE=docs/guide/getting-started.md
rename_range() {
	awk '/^## 이름 바꾸기 \(opLite/ { from = NR }
		/^## 이름 바꾸기 2 / { two = 1; next }
		two && /^## / && !to { to = NR - 1 }
		END { if (!to) to = NR; if (from && two) print from, to }' "$ROOT/$GUIDE"
}

# old_left <파일> <줄>: 허용 모양을 지운 뒤 노드 위의 옛 이름이 남으면 참
old_left() {
	local path=$1 text=$2 rule glob re
	for rule in "${OLD_RULES[@]}"; do
		glob=${rule%%|*} re=${rule#*|}
		# shellcheck disable=SC2053 # glob으로 비교한다.
		[[ $path == $glob ]] || continue
		while [[ $text =~ $re ]]; do
			text=${text/"${BASH_REMATCH[0]}"/}
		done
	done
	[[ $text =~ $OLD_RE ]]
}

# expect_old <기대 left|clean> <파일> <줄>
expect_old() {
	local got=clean
	if old_left "$2" "$3"; then got=left; fi
	if [ "$got" = "$1" ]; then ok "옛 이름 규칙 $1: $2: $3"; else fail "옛 이름 규칙 $2: $3 → $got (기대 $1)"; fi
}
expect_old left docs/guide/cluster.md '# /etc/opl/opld.toml (노드마다)'
expect_old left docs/guide/storage.md '`rm -rf /var/lib/opl`을 하면'
expect_old left docs/guide/network.md 'journalctl -u opl-ovn-nb'
expect_old left docs/guide/cluster.md 'systemctl edit opld로 opld.service.d'
expect_old left docs/guide/backup.md 'export OPL_ESXI_PASSWORD=…'
expect_old left README.md 'cargo test -p opl-api'
expect_old left docs/architecture.md '`crates/opld/src/main.rs`'
expect_old left docs/architecture.md 'opl_types::acme'
expect_old left web/src/certs/i18n/en.json '"hook": "The hook must be a file under /etc/opl/acme-hooks/."'
expect_old left docs/guide/development.md 'sudo mkdir -p /etc/opl-ceph'
expect_old clean docs/guide/network.md 'oplv100, opl-r-<id>, opl_<태그>, opl-vm-1.scope'
expect_old clean docs/guide/cluster.md '/etc/oplite-x, /var/lib/oplx, ORBIVISOR_SERVER, XOPL_A, opl-apix'
expect_old clean docs/architecture.md '(기본 `/etc/orbivisor/orbivisord.toml`, 없고 옛 `/etc/opl/opld.toml`이 있으면 그것)'
expect_old clean docs/architecture.md '| 유닛(+ 정적 별칭 링크 `opld.service`) |'
expect_old left docs/architecture.md '옛 `/etc/opl/opld.toml`이 있으면 그것. 설정은 /etc/opl/opld.toml'
expect_old clean crates/orbivisorctl/src/cli.rs '                Path::new("/run/opl/opld.sock"),'
expect_old left crates/orbivisorctl/src/cli.rs '    /// 소켓 (기본: /run/opl/opld.sock)'
expect_old clean packaging/deb/orbivisord.service 'ConditionPathExists=|/etc/opl/opld.toml'
expect_old left packaging/deb/orbivisord.service 'ExecStart=/usr/bin/opld --config /etc/opl/opld.toml'
expect_old clean crates/orbivisor-types/src/version.rs '    _ => match option_env!("OPL_VERSION") {'
expect_old left crates/orbivisor-types/src/version.rs 'option_env!("OPL_SOCKET")'
# 계획 74 리뷰 1: 넓힌 옛 이름과 줄 모양 허용
expect_old left crates/orbivisor-api/src/auth.rs '    let c = format!("opl_session={value}; Path=/");'
expect_old left crates/orbivisor-api/src/updates.rs '    format!("opl-upgrade-{}", task)'
expect_old left crates/orbivisor-driver/src/backup_serve.rs '    format!("opl-bkserve-{}", id)'
expect_old left crates/orbivisorctl/src/cli.rs 'let path = home.join(".config/opl/x.json");'
expect_old left crates/orbivisord/src/main.rs 'socket.with_file_name("opld.sock")'
expect_old left docs/guide/cluster.md 'cargo test -p opld --features opld/qemu-it'
expect_old left docs/guide/development.md 'OPLD_URL=https://x pnpm -C web dev'
expect_old left docs/architecture.md 'opld::config, oplctl::cli'
expect_old left docs/guide/cluster.md 'cargo run --bin opl-devserver'
expect_old left packaging/deb/postinst '		systemctl restart opld.service || true'
expect_old left packaging/deb/postinst '		systemctl enable opld.service'
expect_old clean packaging/deb/postinst '		ln -s orbivisord.service.d "$SYSTEMD_ETC/opld.service.d" || return 2'
expect_old left packaging/tests/maint-test.sh 'restart opld.service" postinst configure 0.1.0'
expect_old clean packaging/tests/maint-test.sh 'ln -sf /usr/lib/systemd/system/opld.service "$WANTS/opld.service"'
expect_old clean crates/orbivisord/tests/smoke.rs '        .arg(d.path("opld.toml"))'
# 계획 74 리뷰 2: 옛 문자열은 LEGACY_ 상수 정의에만 — 새 이름 쪽이 옛 문자열로 되돌아가면 걸린다.
expect_old left crates/orbivisor-api/src/updates.rs '    format!("opl-upgrade-{}", task.simple())'
expect_old clean crates/orbivisor-api/src/updates.rs 'const LEGACY_UPGRADE_PREFIX: &str = "opl-upgrade-";'
expect_old left crates/orbivisor-api/src/auth.rs 'pub const SESSION_COOKIE: &str = "opl_session";'
expect_old clean crates/orbivisor-api/src/auth.rs 'pub const LEGACY_SESSION_COOKIE: &str = "opl_session";'
expect_old left crates/orbivisor-api/src/oidc.rs 'pub const STATE_COOKIE: &str = "opl_oidc";'
expect_old clean crates/orbivisor-api/src/oidc.rs 'pub const LEGACY_STATE_COOKIE: &str = "opl_oidc";'
expect_old left crates/orbivisor-types/src/acme.rs 'pub const HOOK_DIR: &str = "/etc/opl/acme-hooks/";'
expect_old clean crates/orbivisor-types/src/acme.rs 'pub const LEGACY_HOOK_DIR: &str = "/etc/opl/acme-hooks/";'
expect_old left crates/orbivisord/src/config.rs '            .unwrap_or_else(|| self.run_dir.join("opld.sock"))'
expect_old left crates/orbivisord/src/main.rs '        lock_file(&dir.join("opld.lock"))?,'
expect_old clean crates/orbivisord/src/main.rs 'const LEGACY_LOCK: &str = "opld.lock";'
expect_old clean crates/orbivisorctl/src/cli.rs '        let old = dir.path().join("opld.sock");'
expect_old clean crates/orbivisord/tests/release_vips.rs '    let lock = std::fs::File::create(data.join("opld.lock")).unwrap();'
expect_old left crates/orbivisor-driver/src/backup_serve.rs '    format!("opl-bkserve-{id}")'
expect_old clean crates/orbivisor-driver/src/backup_serve.rs 'const LEGACY_BACKUP_SERVE_PREFIX: &str = "opl-bkserve-";'
expect_old left crates/orbivisor-driver/src/file_restore.rs 'pub const UNIT_PREFIX: &str = "opl-fr-";'

range=$(rename_range)
if [ -n "$range" ]; then ok "시작하기의 이름 바꾸기 절: 줄 $range"; else fail "시작하기에서 두 이름 바꾸기 절을 찾지 못함"; fi
from=${range% *} to=${range#* }
code=0
lines=$(git grep -nIE "$OLD_RE" -- . "${OLD_EXCLUDE[@]}" </dev/null) || code=$?
if [ "$code" -gt 1 ]; then
	echo "name-test: git grep 실패(종료 코드 $code)"
	exit 1
fi
left=0
while IFS= read -r line; do
	[ -n "$line" ] || continue
	path=${line%%:*}
	rest=${line#*:}
	num=${rest%%:*}
	if [ "$path" = "$GUIDE" ] && [ -n "$range" ] && [ "$num" -ge "$from" ] && [ "$num" -le "$to" ]; then
		continue
	fi
	if old_left "$path" "${rest#*:}"; then
		echo "  $line"
		left=$((left + 1))
	fi
done <<<"$lines"
if [ "$left" -eq 0 ]; then ok "허용 목록 밖의 노드 옛 이름 없음"; else fail "허용 목록 밖의 노드 옛 이름 ${left}줄(위)"; fi

if [ "$FAILED" -ne 0 ]; then
	echo "name-test: 실패"
	exit 1
fi
echo "name-test: 모두 통과"
