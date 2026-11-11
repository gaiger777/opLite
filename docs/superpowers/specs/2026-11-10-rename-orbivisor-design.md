# Orbivisor 이름 바꾸기 설계 (계획 43)

작성일: 2026-11-10
상위 문서: `2026-10-20-opl-node-core-design.md`(크레이트·바이너리·데이터 경로), `2026-11-05-install-update-subscription-design.md`(Debian 패키지·ISO·APT 저장소·업데이트), `2026-10-23-web-ui-design.md`(웹 UI)
기준 트리: 계획 42까지 들어간 main(`7b7dc13`, 로컬 디스크 마이그레이션). 이 문서의 경로·수는 그 트리에서 셌다.

## 1. 목표와 범위

제품 이름을 opLite에서 **Orbivisor**로 바꾼다. 사람이 보는 이름(문서, 웹 UI, 패키지, ISO, 바이너리, CLI 도움말, 콘솔 안내)은 바꾸고, 노드에 저장된 상태·노드 위의 이름·주고받는 형식에 들어간 내부 식별자는 업그레이드 호환을 위해 그대로 둔다. 이미 설치된 `oplite` 패키지는 `apt-get dist-upgrade`(웹·`oplctl node upgrade`의 업그레이드 Task) 한 번으로 새 패키지로 넘어가야 하고, 그 뒤에도 옛 명령(`opld`, `oplctl`)·유닛(`opld.service`)·설정 경로를 쓰는 스크립트가 그대로 돌아야 한다.

| 바꿈 | 그대로 둠 |
|---|---|
| 제품 이름 opLite → Orbivisor: README, 웹 제목·머리 브랜드·로그인 화면, 이름을 보이는 번역 문자열, OpenAPI 제목, 크레이트 `description` | 크레이트 이름 `opl-*`·`opld`·`oplctl`, Rust 모듈 이름 |
| Debian 패키지 `oplite` → `orbivisor`(옛 이름은 빈 전환 패키지) | 데이터·설정·런타임 경로 `/var/lib/opl`, `/etc/opl/opld.toml`, `/run/opl/opld.sock`, `/var/log/opl` |
| 바이너리 `opld` → `orbivisord`, `oplctl` → `orbivisorctl`(옛 이름은 패키지의 심볼릭 링크) | systemd 유닛 이름 `opld.service`(새 이름 `orbivisord.service`는 별칭) |
| ISO 이름·부트 항목 "Install Orbivisor"·preseed 파일 이름·설치 로그·콘솔 안내 파일 | 장치·nft·QMP·LVM·ZFS·RBD 이름, 비트맵, 쿠키, 토큰·키 접두사, `opl-join://`, 환경 변수 `OPL_*`, Raft·저장 JSON 필드 |
| CLI·데몬의 도움말과 사람에게 보이는 메시지에서 프로그램 이름 | 코드가 비교하는 문자열(APT 관리 표시, `CREDENTIALS_LOST`, 로그인 토큰 접두사 `oplctl-`) |
| CI 아티팩트·deb 파일 이름, 패키징 시험 | APT 저장소의 파일 이름(`oplite.sources`, `oplite.conf`, `oplite-archive-keyring.gpg`)과 Release `Origin`/`Label` |
| 새 클러스터 CA의 CN "Orbivisor cluster X CA" | 지금 클러스터의 CA(다시 만들지 않는다) |

넣지 않는 것: GitHub 저장소 이름 바꾸기와 그 링크(저장소를 바꾼 뒤 따로 — §3.11), 내부 식별자 바꾸기(이 문서 §3.12가 남기는 이유를 적는다), 새 로고·아이콘 디자인(지금 favicon에는 글자가 없다), 지난 설계·계획 문서 고쳐 쓰기, 설정 파일 이름 `orbivisord.toml`, `ORBIVISOR_*` 환경 변수.

정해진 결정(이 문서는 다시 묻지 않는다): 위 표의 "바꿈" 줄 전부, 바이너리 옛 이름을 패키지 안의 호환 링크로 남김, 내부 식별자 유지. 이 문서가 고른 것: 유닛 이름(§3.3), 전환 패키지(§3.4), 설정 경로(§3.6), APT 저장소 이름·키(§3.5), GitHub 링크 시기(§3.11).

### 견준 길

- **유닛: `opld.service`를 그대로 두고 `orbivisord.service`를 정적 별칭 링크로 더함(채택) vs 새 유닛 `orbivisord.service`에 `Alias=opld.service`와 postinst 옮기기.** 새 유닛으로 옮기면 노드마다 (1) enable 링크 `/etc/systemd/system/multi-user.target.wants/opld.service`를 옮겨야 하고, (2) 관리자가 README대로 만든 덮어쓰기 파일 `/etc/systemd/system/opld.service.d/`(텔레메트리·업데이트의 `HTTPS_PROXY`, README "구독과 텔레메트리")가 새 이름에는 적용되지 않을 수 있어 프록시 뒤 노드의 업데이트가 조용히 깨지고, (3) 업그레이드는 opld가 띄운 일시 유닛의 `apt-get dist-upgrade`가 하므로 postinst가 옛 유닛을 멈추고 새 유닛을 띄우다 실패하면 되돌릴 유닛이 없다. 유닛 이름은 노드 위의 식별자(`journalctl -u opld`, `oplite-ci-init.service`의 `Requires=opld.service`, 운영 스크립트)이므로 내부 식별자 규칙대로 둔다. `/usr/lib/systemd/system/orbivisord.service → opld.service` 심볼릭 링크는 systemd가 별칭으로 읽어(같은 방식: `autovt@.service → getty@.service`) `systemctl status|restart orbivisord`가 된다.
- **바이너리: 크레이트 이름은 두고 `[[bin]] name`만 바꿈(채택) vs 크레이트 이름까지 `orbivisord` vs 빌드 이름은 `opld`로 두고 패키징에서만 이름 바꿈.** 크레이트 이름을 바꾸면 `-p opld`(CI·README·계획 문서의 수백 곳), 기능 이름 `opld/qemu-it`, `Cargo.lock`, `use oplctl::…`가 모두 바뀌는데 사용자에게 보이는 것은 없다. 패키징에서만 이름을 바꾸면 `target/release`의 이름과 설치된 이름이 달라 `--version`·도움말의 이름을 맞추기 어렵고 개발 문서가 둘로 갈린다.
- **옛 패키지: 빈 전환 패키지 `oplite`(채택) vs `orbivisor`에 `Provides: oplite`만 vs 이름을 바꾸지 않음.** `Provides`만으로는 `apt-get dist-upgrade`가 설치된 `oplite`를 새 패키지로 바꾸지 않는다(새 판의 `oplite`가 없으면 그대로 남는다). 업그레이드 Task는 `dist-upgrade`만 하므로, 새 판의 `oplite`가 `orbivisor`에 의존해야 자동으로 넘어간다. `Provides`는 그 전환 패키지와 겹쳐 apt가 어느 쪽을 지울지 헷갈리게 할 뿐이고 `oplite`에 의존하는 패키지도 없으므로 넣지 않는다.
- **APT 저장소: 파일 이름·키 경로·Release `Origin`/`Label`을 그대로 둠(채택) vs 모두 `orbivisor`로.** apt는 Release의 `Origin`·`Label`이 바뀌면 `apt-get update`를 오류로 끝낸다("changed its 'Origin' value … must be accepted explicitly"). opld의 업데이트 확인(`crates/opl-api/src/updates.rs`, `apt-get update -q`)이 옛 판에서 그 오류로 멈추므로 새 판을 받을 길이 없어진다. 소스·인증 파일 이름을 바꾸면 옛 파일이 남아 같은 저장소가 두 번 설정되고(apt 경고), postrm의 정리 규칙이 둘로 갈린다. 키 경로는 지금 노드의 소스 파일 `Signed-By`가 가리킨다.
- **설정: `/etc/opl/opld.toml`만(채택) vs `/etc/orbivisor/orbivisord.toml`도 받음.** 두 곳을 받으면 어느 쪽이 이기는지·둘 다 있을 때를 정해야 하고, 유닛의 `ConditionPathExists=/etc/opl/opld.toml`·ISO 설치·시험이 둘로 갈린다. 데이터 경로 규칙(`/etc/opl`)과 같이 둔다.
- **GitHub 링크: 저장소 이름을 바꾼 뒤 바꿈(채택) vs 지금 바꿈.** 지금 `…/Orbivisor`로 바꾸면 저장소를 바꾸기 전까지 404다. 이름을 바꾸면 GitHub가 옛 주소를 넘겨주므로 지금 링크는 그 뒤에도 돈다.
- **지난 설계·계획 문서: 그대로 둠(채택) vs 모두 고침.** `docs/superpowers/plans`(41개 파일, opLite 1247번)와 `specs`(21개, 138번)는 그때의 기록이다. 고치면 커밋 기록과 어긋나고 얻는 것이 없다. 노드 코어 설계 §1.1에 이름이 바뀌었다는 메모만 더한다.

## 2. 지금 있는 것 (계획 42 끝)

`git grep -i oplite`: 163개 파일, 1948번. `opld` 4732번(197개 파일), `oplctl` 3896번(118개 파일) — 대부분 `docs/superpowers`(opld 3944, oplctl 3312)와 README(opld 242, oplctl 283)다.

| 영역 | opLite/oplite (파일/번) | 대표 경로 | 처리 |
|---|---|---|---|
| 지난 계획·설계 문서 | 41/1247, 21/138 | `docs/superpowers/plans/2026-11-05-plan31-packaging-iso.md` | 그대로(§3.11) |
| README | 1/93 | `README.md` 1행 `# opLite`, 73행 "Install opLite" | 바꿈 |
| 크레이트 | 50/154 | `crates/opl-api/src/updates.rs`(29), `crates/oplctl/src/cli.rs`(19), `crates/opl-api/src/oidc.rs`(8) | 사람이 보는 것만(§3.7, §3.12) |
| 웹 | 13/48 + `index.html` 1 + `package.json` 1 | `web/src/i18n/{ko,en}.json`(각 12), `web/src/layout/AppLayout.tsx:96`, `web/src/pages/LoginPage.tsx:94` | 바꿈(§3.8) |
| 패키지 | 7/22 | `packaging/deb/control.in`, `build-deb.sh`, `postinst`·`prerm`·`postrm`, `opld.service` | §3.3·§3.4 |
| ISO | 5/90 | `packaging/iso/build-iso.sh`, `preseed/oplite.cfg`·`oplite-setup.sh`·`oplite-ci-init.service`·`unattended.cfg` | 바꿈(§3.9) |
| APT 저장소 | 1/3 | `packaging/repo/build-repo.sh` 90·91행 `Origin=opLite`·`Label=opLite` | 그대로(§3.5) |
| 패키징 시험 | 17/128 | `packaging/tests/{deb,repo,build-deb,build-repo,iso,setup,maint}-test.sh`, `fixtures/oplite.issue`, `fixtures/opld-*.toml` | 바꿈·더함(§5) |
| CI | 4/22 | `.github/workflows/{ci,iso}.yml`, `.github/actions/build-deb/action.yml`, `.github/scripts/ldap-setup.sh` | §3.10 |
| 워크스페이스 | 1/1 | `Cargo.toml` `repository` | 나중(§3.11) |

- 바이너리: `crates/opld`(`[package] name = "opld"`, `src/main.rs`, clap `#[command(version = BUILD, about = "opLite node daemon")]` — `name`이 없어 clap이 크레이트 이름을 쓴다)와 `crates/oplctl`(`#[command(name = "oplctl", …, about = "opLite command-line client")]`). 따로 `opl-api`의 `opl-license`(구독 키 발급, 패키지에 없음)와 `opl-api-openapi`. 시험의 `CARGO_BIN_EXE_opld`(`crates/opld/tests/common/mod.rs`, `smoke.rs` 2번, `version.rs`), `CARGO_BIN_EXE_oplctl`(`crates/oplctl/tests/version.rs`).
- 패키지(`packaging/deb/build-deb.sh`): `oplite_<버전>_amd64.deb`, `/usr/bin/{opld,oplctl}`, `/usr/lib/systemd/system/opld.service`, `/usr/share/oplite/opld.toml.example`, `/usr/share/doc/oplite/copyright`, `OPL_APT_KEY`이 있으면 `/usr/share/keyrings/oplite-archive-keyring.gpg`. 바이너리 `--version`이 `"<bin> <버전>"`인지, 정식 빌드면 `opld` 안에 `OPL_LICENSE_KEYS`가 그대로 있는지 본다. conffile은 없다. `control.in`: `Package: oplite`, `Maintainer: opLite developers`, `Homepage: https://github.com/gaiger777/opLite`.
- 유지 스크립트: `postinst` configure의 `$2`가 비면 처음 설치(enable·start), 아니면 enable돼 있을 때 restart. `prerm`은 remove·deconfigure에만 stop. `postrm`은 purge에 disable과 enable 링크 지우기, remove·purge에 첫 줄이 `# managed by opld; edits are overwritten`인 `/etc/apt/sources.list.d/oplite.sources`를 지움.
- 업데이트(`crates/opl-api/src/updates.rs`): `KEYRING`·`AUTH_FILE` 상수, `crates/opld/src/config.rs:675` 소스 파일 기본값, `SOURCES_MARK`, 일시 유닛 `opl-upgrade-<task>`와 로그 표시 `opl-upgrade: start`·`opl-upgrade: exit `. 패키지 이름을 비교하는 코드는 없다(`apt-get -s dist-upgrade`의 `Inst` 줄을 모두 보인다).
- 웹: `web/index.html` `<title>opLite</title>`, favicon `web/public/favicon.svg`(글자 없는 막대 둘), 머리 `AppLayout.tsx:96`, 로그인 `LoginPage.tsx:94` `<h1>opLite</h1>`, 웹 UI 없이 빌드한 opld의 안내 페이지 `crates/opld/src/web.rs:68`. 번역 키에는 제품 이름이 없고 값에만 있다.
- 텔레메트리(`crates/opl-api/src/telemetry.rs` `TelemetryReport`, `schema` 1)에 제품 이름 필드는 없다. 어느 HTTP 클라이언트도 `User-Agent`를 정하지 않는다(`reqwest` 기본은 보내지 않음). 바꿀 것이 없다.
- 클러스터 CA: `crates/opl-peer/src/pki.rs:62` `"opLite cluster {cluster_name} CA"`. CN을 읽거나 비교하는 코드는 없다.

## 3. 설계

### 3.1 이름

| 무엇 | 옛 | 새 |
|---|---|---|
| 제품(문장 속) | opLite | Orbivisor |
| Debian 패키지 | `oplite` | `orbivisor`(+ 전환 `oplite`) |
| 노드 데몬 | `opld` | `orbivisord`(+ 링크 `opld`) |
| 명령줄 | `oplctl` | `orbivisorctl`(+ 링크 `oplctl`) |
| 유닛 | `opld.service` | `opld.service`(+ 별칭 `orbivisord.service`) |
| ISO | `oplite.iso`, "Install opLite" | `orbivisor.iso`, "Install Orbivisor" |
| 공유 파일 디렉터리 | `/usr/share/oplite` | `/usr/share/orbivisor` |
| 웹 패키지(비공개) | `oplite-web` | `orbivisor-web` |

### 3.2 바이너리와 호환 링크

- `crates/opld/Cargo.toml`: `[package]`에 `autobins = false`, `[[bin]] name = "orbivisord", path = "src/main.rs"`. `crates/oplctl/Cargo.toml`: 같은 방식으로 `orbivisorctl`(라이브러리 `oplctl`은 그대로 — opld E2E가 `oplctl::` 를 쓴다). 크레이트 이름·기능 이름·`-p opld`는 그대로다.
- clap: opld `#[command(name = "orbivisord", version = BUILD, about = "Orbivisor node daemon")]`, oplctl `#[command(name = "orbivisorctl", …, about = "Orbivisor command-line client")]`. `--version`은 `orbivisord <버전>`. 링크 이름으로 부르면 clap이 사용법 줄의 이름을 `argv[0]`에서 가져올 수 있다(`--version`의 이름이 그 영향을 받는지는 1단계에서 확인해 `deb-test.sh`의 기대값을 정한다 — §5).
- `opl-license`는 운영자 도구라 이름을 두고 `about`만 "Issue Orbivisor subscription keys".
- 시험의 `CARGO_BIN_EXE_opld` → `CARGO_BIN_EXE_orbivisord`, `CARGO_BIN_EXE_oplctl` → `CARGO_BIN_EXE_orbivisorctl`, `crates/{opld,oplctl}/tests/version.rs`의 기대값.
- 패키지: `/usr/bin/orbivisord`, `/usr/bin/orbivisorctl`(실제 파일), `/usr/bin/opld → orbivisord`, `/usr/bin/oplctl → orbivisorctl`(상대 심볼릭 링크). 링크는 `md5sums`에 넣지 않는다(파일만, 지금 `find usr -type f`).
- 프로세스 이름(`comm`)과 journald의 `SYSLOG_IDENTIFIER`는 `orbivisord`가 된다. 코드에서 프로세스 이름을 찾는 곳은 없다(`pgrep`·`pidof` 없음).

### 3.3 systemd 유닛

- 유닛 파일은 지금 경로 `/usr/lib/systemd/system/opld.service` 그대로. 바뀌는 줄: `Description=Orbivisor node daemon`, `ExecStart=/usr/bin/orbivisord --config /etc/opl/opld.toml`. `ConditionPathExists=/etc/opl/opld.toml`, `KillMode=process`(VM의 QEMU는 자기 scope `opl-vm-{id}.scope`), `Documentation`(§3.11)은 그대로.
- 별칭: 패키지에 `/usr/lib/systemd/system/orbivisord.service → opld.service` 심볼릭 링크. `systemctl status|restart|stop orbivisord`가 된다. `[Install] Alias=`는 쓰지 않는다(enable 때만 링크가 생겨 지금 노드에서는 다시 enable해야 한다).
- 유지 스크립트의 `systemctl` 대상은 계속 `opld.service`다(`maint-test.sh`의 기대 그대로).
- 로그는 유닛의 원래 이름으로 남는다: `journalctl -u opld`. 설계 때 systemd 255(Ubuntu 24.04 시험 노드)에서 확인한 것: `/usr/lib/systemd/system`의 별칭 링크로 `systemctl start|restart|status <별칭>`은 원래 유닛을 따라가고(`systemctl show -p Names`에 두 이름), `journalctl -u <별칭>`은 "No entries"다. 그래서 README에 "로그·덮어쓰기(`systemctl edit`)는 `opld`로"라고 적는다(§6).

### 3.4 Debian 패키지

`build-deb.sh <version> <out dir>`가 두 파일을 만든다.

1. `orbivisor_<version>_amd64.deb` — 지금 `oplite`의 내용에 §3.2·§3.3의 이름과 링크를 더한 것.

   ```
   Package: orbivisor
   Version: @VERSION@
   Architecture: amd64
   Maintainer: Orbivisor developers <gaiger777@gmail.com>
   Depends: (지금과 같음)
   Recommends: (지금과 같음)
   Breaks: oplite (<< @VERSION@)
   Replaces: oplite (<< @VERSION@)
   Homepage: https://github.com/gaiger777/opLite
   Description: Orbivisor virtualization platform node
    … orbivisord (with the web UI), orbivisorctl, the opld systemd unit
    and the compatibility names opld and oplctl.
   ```

   파일: `/usr/bin/{orbivisord,orbivisorctl}`, 링크 `/usr/bin/{opld,oplctl}`, `/usr/lib/systemd/system/opld.service`와 링크 `orbivisord.service`, `/usr/share/orbivisor/opld.toml.example`, `/usr/share/doc/orbivisor/copyright`, (있으면) `/usr/share/keyrings/oplite-archive-keyring.gpg`. 옛 패키지와 같은 경로의 파일(`/usr/bin/opld`·`oplctl` 자리, 유닛, 키)은 `Replaces`로 넘겨받는다. `Provides`는 없다(§1 견준 길).

2. `oplite_<version>_amd64.deb` — 빈 전환 패키지. `Depends: orbivisor (= @VERSION@)`, `Section: oldlibs`, `Priority: optional`, `Description: transitional package for orbivisor` / "It can be removed.". 파일은 `/usr/share/doc/oplite/copyright`만, **유지 스크립트 없음**(옛 `postrm`의 purge가 하던 `systemctl disable opld.service`·소스 파일 지우기를 나중에 전환 패키지를 지울 때 하면 안 된다). `Architecture: amd64`(저장소 `Architectures: amd64`와 `build-iso.sh`의 이름 규칙에 맞춤).

`orbivisor`의 `postinst` configure는 "처음 설치"를 `$2`만으로 정하지 않는다. `oplite`에서 넘어올 때 `orbivisor`는 처음 설치(`$2` 빔)지만 유닛은 이미 enable돼 돌고 있으므로 다시 시작해야 새 바이너리가 돈다.

```
systemd가 돎:
  daemon-reload
  opld.service가 enable돼 있음 → restart        (업그레이드, oplite에서 넘어옴, 재설정)
  아니고 처음 설치($2 빔)       → enable, start  (새 설치)
systemd가 돌지 않음(컨테이너·설치 프로그램 chroot):
  처음 설치이고 enable 링크가 없음 → enable
```

옛 `oplite`가 dpkg의 `--auto-deconfigure`로 `prerm deconfigure`를 받아 opld를 멈춘 경우에도 enable 링크는 남으므로 위 규칙이 다시 시작한다. `prerm`·`postrm`은 지금과 같다(`APT_SOURCES` 기본값 `oplite.sources` 그대로).

### 3.5 APT 저장소와 업데이트

- `build-repo.sh`: 두 deb를 같은 채널에 넣는다(호출하는 쪽이 둘 다 넘긴다). `Origin=opLite`·`Label=opLite`는 **그대로 둔다** — 바꾸면 모든 노드의 `apt-get update`가 실패한다(§1 견준 길). 주석에 그 이유를 적는다. 첫 줄 주석만 "Orbivisor APT 저장소".
- 저장소 URL(`OPL_APT_URL`, 운영자가 빌드 때 줌)은 이 계획이 정하지 않는다. README 예시 `https://apt.example.com/oplite`는 그대로 두고, 새 URL을 쓰려면 옛 URL에서도 같은 색인을 내야 한다고 적는다(옛 판 opld가 쓴 소스 파일로 새 판을 받기 때문).
- 그대로: `/usr/share/keyrings/oplite-archive-keyring.gpg`(`updates.rs` `KEYRING`), `/etc/apt/sources.list.d/oplite.sources`(`config.rs:675`), `/etc/apt/auth.conf.d/oplite.conf`(`AUTH_FILE`), `SOURCES_MARK` `# managed by opld; edits are overwritten`(postrm과 `updates.rs`가 비교), 일시 유닛 `opl-upgrade-<task>`와 로그 표시. 문서 주석의 "opLite 저장소"만 "Orbivisor 저장소"로.
- 업데이트 화면·`oplctl node updates`에는 넘어가는 업그레이드에서 `orbivisor`(현재 없음 — 새로 설치)와 `oplite`(전환) 두 줄이 보인다. 코드 변경은 없다.

### 3.6 설정 파일과 orbivisorctl 설정

- 노드 설정은 `/etc/opl/opld.toml` 하나(`--config` 기본값, 유닛, ISO). 새 경로를 받지 않는다. 오류 문구의 `opld.toml`(`crates/opld/src/bootstrap.rs` 88·92행)은 파일 이름이므로 그대로.
- `orbivisorctl` 로그인 정보는 `~/.config/opl/oplctl.json`(`crates/oplctl/src/config.rs:20`)과 `OPL_CONFIG` 그대로. 옮기지 않는다: 같은 사용자가 `oplctl`·`orbivisorctl`을 섞어 써도 같은 로그인이 보이고, 옮기기를 넣으면 두 파일 중 무엇이 이기는지를 정해야 한다. `--socket` 기본값 `/run/opl/opld.sock`(`OPL_SOCKET`)도 그대로.
- 로그인 토큰 이름 접두사 `oplctl-`(`cli.rs:5435` `LOGIN_TOKEN_PREFIX`)는 그대로: `logout`이 이 접두사로 "login이 만든 토큰"을 알아보고 지운다. 바꾸면 옛 CLI가 만든 토큰을 지우지 못한다.

### 3.7 데몬·CLI 문자열

규칙: 문장 속 제품 이름 opLite → Orbivisor. 사람에게 실행하라고 알려 주는 명령 `oplctl …` → `orbivisorctl …`, 데몬을 가리키는 "opld" → "orbivisord". 파일 이름(`opld.toml`, `opld.sock`, `opld.lock`)·유닛 이름·코드가 비교하는 문자열은 그대로. 코드 주석의 `opld`·`oplctl`은 크레이트 이름이므로 그대로 두고, 주석의 opLite는 바꾼다(OpenAPI 설명으로 나가는 doc 주석이 섞여 있어 규칙을 하나로).

바꾸는 것(대표):
- 도움말·about: `crates/opld/src/main.rs:47`, `crates/oplctl/src/cli.rs:20`, `crates/opl-api/src/bin/opl-license.rs:16`, `crates/opl-api/src/lib.rs:72` OpenAPI `title = "Orbivisor API"`·설명.
- API·Task 오류: `cluster.rs:455` "some nodes run an older Orbivisor (…)", `remote_client.rs:106` "not an Orbivisor cluster", `tasks/snapshot.rs:856` "left by an Orbivisor snapshot that orbivisord removes…", `tasks.rs:609` "interrupted: orbivisord restarted…", `cluster.rs:636`·`remote_proxy.rs:97`의 `` `orbivisorctl …` ``, `opl-driver/src/net.rs` 174·437 "not managed by this Orbivisor cluster", `opl-types/src/network.rs:174`, `opl-types/src/remote.rs:174` "must be an Orbivisor API token (opl_<id>_<secret>)"(형식은 그대로), `opl-reconciler/src/ha.rs:370`.
- 데몬 로그·제어 소켓 응답: `crates/opld/src/main.rs` 91·304, `member.rs` 488·606·706, `control.rs` 85·145.
- oplctl 출력: "follow it with `orbivisorctl task get …`"(cli.rs 314·338·3764), "review it with `orbivisorctl import get …`"(4008·4183), cluster init의 로그인 안내(4349), 로그인·지문 안내(2008·3222·5359·5428), `client.rs` 124·184·208·298·305, 버전 경고 5153.
- 새 클러스터 CA CN `"Orbivisor cluster {cluster_name} CA"`(`pki.rs:62`). 있는 CA는 그대로다.
- 크레이트 `description`(opl-api·opl-cluster·opl-driver·opl-peer·opl-qemu·opl-types·oplctl·opld).

그대로 두는 것:
- `vm_imports.rs:54` `CREDENTIALS_LOST`("…(opld restarted or the request was interrupted)…") — VM 가져오기 `status.error`에 저장되고 웹 `web/src/imports/imports.ts:11`이 그 문자열과 같은지 비교해 번역한다. 사람에게는 번역 `imports.credentialsLost`가 보이므로 그 값만 바꾼다.
- `SOURCES_MARK`, `opl-upgrade:` 표시, `LOGIN_TOKEN_PREFIX`, `http://opld/v1`(유닉스 소켓 요청의 호스트, `client.rs:294`).
- 시험 자료의 IdP `client_id: "oplite"`(`opl-api/src/oidc.rs`, `tests/common/idp.rs`, `opl-types/src/realm.rs`, `oplctl/src/cli.rs`, `RealmsPage.test.tsx`) — 관리자가 정하는 값의 예일 뿐이다. 패키지 이름 예 `"oplite"`(`updates.rs` 시험, `opl-types/src/node.rs:274`, `NodesPage.test.tsx`)는 실제 출력 모양을 보이는 것이므로 `orbivisor`로 바꾼다.

### 3.8 웹

- `web/index.html` `<title>Orbivisor</title>`, `AppLayout.tsx:96`·`LoginPage.tsx:94` "Orbivisor", `crates/opld/src/web.rs:68` 안내 페이지 제목과 "This orbivisord was built without the web UI".
- favicon: 글자가 없어 그대로(새 로고는 범위 밖).
- 번역 값(키는 그대로), ko·en 각각 25개. opLite가 든 12개 — `errors.version_too_old`, `errors.no_role`, `login.version`(`"Orbivisor {{version}}"`), `login.sso.no_role`, `imports.pageHint`, `imports.networksHint`, `pools.sharedHint`, `pools.makeSharedDescription`, `remotes.pageHint`, `remotes.addDescription`, `dashboard.mixedVersions`, `settings.channelHint`; opld·oplctl이 든 13개 — `errors.node_online`, `imports.forceHint`·`esxiDescription`·`credentialsLost`, `networks.kindHint.vxlan`, `networks.deleteDescription`, `nodes.joinNote`·`removeNote`·`reason.online`, `nodes.updates.upgradeDescription`, `tokens.note`, `cluster.issueDescription`·`commandNote`(소켓 경로 `/run/opl/opld.sock`은 그대로).
- `web/src/cluster/ClusterPage.tsx:45` 조인 명령 `orbivisorctl cluster join '…'`(새 노드에는 두 이름이 다 있다).
- `web/package.json` `"name": "orbivisor-web"`. `vite.config.ts`의 `OPLD_URL`은 그대로(개발 환경 변수).
- `web/src/api/schema.d.ts`: doc 주석의 opLite를 바꾼 뒤 `opl-api-openapi`로 다시 만든다(CI "API types match the Rust API"가 확인).
- 시험: `App.test.tsx`(83 머리 제목, 85 `"Orbivisor 0.9.0~ci7"`, 162·224 no_role), `DashboardPage.test.tsx:230`, `SettingsPage.test.tsx` 512·524(서버 문구), `PoolsPage.test.tsx:193`, `ClusterPage.test.tsx`(조인 명령), `NodesPage.test.tsx`(패키지 이름 예).

### 3.9 ISO와 설치

- `packaging/iso/preseed/`: `oplite.cfg` → `orbivisor.cfg`, `oplite-setup.sh` → `orbivisor-setup.sh`, `oplite-ci-init.service` → `orbivisor-ci-init.service`(CI 전용, `Description=Orbivisor CI first-boot cluster init`, `Requires=opld.service`·`After=opld.service` 그대로, `orbivisorctl cluster init`, 표시 파일 `/var/lib/orbivisor-ci-init.done`), `unattended.cfg`의 `preseed/include`.
- `build-iso.sh`: CD 디렉터리 `/orbivisor`, deb 이름 규칙 `orbivisor_*_amd64.deb`(전환 패키지는 넣지 않는다 — 새 설치에는 필요 없다), isolinux `label orbivisor`·`menu label ^Install Orbivisor`·`default orbivisor`, GRUB `menuentry --hotkey=o 'Install Orbivisor'`, 검사 문구, 사용법의 `--out <orbivisor.iso>`.
- `orbivisor-setup.sh`: `orbivisor` 패키지 설치(`/var/cache/orbivisor`), 로그 `/root/orbivisor-setup.log`, 콘솔 표시 `orbivisor-setup: `, opld.toml 첫 줄 `# Written by the Orbivisor installer. Example with all settings: /usr/share/orbivisor/opld.toml.example`, 콘솔 안내 `/etc/issue.d/orbivisor.issue`:

  ```
  Orbivisor: https://\4{vmbr0}:8006/ (web UI and API, after the node is in a cluster)
    First node:  orbivisorctl cluster init --name <cluster-name>
    Other nodes: on a logged-in client run `orbivisorctl cluster join-token`,
                 then here run `orbivisorctl cluster join '<opl-join://... URL>'`
  ```

  `/etc/opl/opld.toml`, 브리지 `vmbr0`, `opl-join://`은 그대로.
- 이미 설치된 노드의 `/etc/issue.d/oplite.issue`와 opld.toml 주석은 패키지 파일이 아니라 바꾸지 않는다(§6). 그 안의 `oplctl` 명령은 링크로 계속 된다.

### 3.10 CI

- `.github/actions/build-deb/action.yml`: 이름 "Build orbivisor deb", 출력 `deb` = `dist/orbivisor_${OPL_VERSION}_amd64.deb`, 새 출력 `transitional` = `dist/oplite_${OPL_VERSION}_amd64.deb`. 빌드 명령 `cargo build --release -p opld --features web-ui -p oplctl`은 그대로(크레이트 이름).
- `ci.yml` package 작업: 아티팩트 `orbivisor-deb`, `path: dist/*_amd64.deb`(두 파일), `dpkg-deb --info/--contents`를 둘 다, `deb-test.sh`·`repo-test.sh`에 두 경로. 버전 시험 `cargo test -p opl-types -p opld -p oplctl --lib --test version` 그대로. setup-test의 busybox 단계는 파일 이름만.
- `iso.yml`: `$RUNNER_TEMP/orbivisor.iso`, `-cdrom orbivisor.iso`, 직렬 로그 grep `orbivisor-setup:`·`orbivisor-setup: FAILED`, `Failed to start .*Orbivisor CI first-boot`, 로그인 단계의 `oplctl=target/release/orbivisorctl`, 노드 이름 `opl-ci`·`OPL_CONFIG`는 그대로.
- `.github/scripts/ldap-setup.sh`의 시험 조직·CA 이름 "opLite test"는 시험 자료라 "Orbivisor test"로 바꿔도 되고 두어도 된다 — 바꾼다(검색에 남지 않게).
- 남은 이름 검사: `packaging/tests/name-test.sh`(새, bash): `git grep -nI -i oplite`에서 허용 목록(§3.12의 파일 이름·저장소 값, `docs/superpowers/plans`·지난 `specs`, 전환 패키지, 시험 자료 `client_id`, GitHub 링크)을 뺀 나머지가 없어야 한다. CI lint 작업(shellcheck 옆)에서 돈다.

### 3.11 문서

- README: 제목 `# Orbivisor`, 문장 속 opLite 93번, 명령 예 `oplctl` → `orbivisorctl`, 프로그램을 가리키는 `opld` → `orbivisord`. 유닛·설정·소켓·로그(`systemctl restart opld`, `journalctl -u opld`, `opld.toml`, `/run/opl/opld.sock`)는 그대로. 새 절 "이름 바꾸기(opLite → Orbivisor)": 옛 명령·유닛·경로가 그대로 되는 것, 전환 패키지와 지우기, 남긴 내부 이름의 표(§3.12 요약), 이미 설치된 노드의 콘솔 안내 바꾸는 한 줄.
- 설계 문서: 노드 코어 설계 §1.1에 "계획 43: 제품 이름 Orbivisor(이 문서의 opLite·opld·oplctl은 그대로 내부 이름)" 메모, 이 문서의 "구현" 절. 지난 계획·설계는 고치지 않는다.
- GitHub 링크 4곳(`Cargo.toml` `repository`, `control.in` `Homepage`, `copyright` `Source`, `opld.service` `Documentation`)과 `copyright`의 `Upstream-Name`은 저장소 이름을 바꾸는 커밋에서 함께 바꾼다. `Upstream-Name`·`Copyright: … developers`는 이 계획에서 Orbivisor로.

### 3.12 그대로 두는 내부 식별자

수는 `crates`·`packaging`·`web/src`·`.github`에서 `git grep -o`로 센 번수다.

| 분류 | 식별자 (대표 경로) | 수 | 남기는 이유 |
|---|---|---|---|
| 데이터·설정·런타임 경로 | `/var/lib/opl`(35), `/etc/opl`(34), `/run/opl`(20), `/var/log/opl`(2) — `opld/src/config.rs`, `opl-qemu/src/cmdline.rs` | 91 | Raft 데이터·풀·볼륨·마이그레이션 기록 파일이 그 아래에 있다. 실행 중 QEMU의 QMP 소켓·pidfile 경로(`/run/opl`)를 업그레이드한 데몬이 이어받는다 |
| 데몬 파일 이름 | `opld.toml`, `opld.sock`, `opld.lock`(`config.rs:1050`, `main.rs:293`) | — | 유닛 조건, `--socket` 기본값, 운영 스크립트 |
| CLI 설정 | `~/.config/opl/oplctl.json`, `OPL_CONFIG` | 3 | §3.6 |
| 유닛·scope | `opld.service`, `opl-vm-{id}.scope`(`opl-driver/src/qemu.rs:1804`), `opl-upgrade-{task}`(`updates.rs:424`) | — | enable 링크·덮어쓰기 파일, 돌고 있는 VM의 scope를 새 데몬이 알아봄, 업그레이드 중 옛·새 데몬이 같은 유닛 이름을 봄 |
| dirty bitmap | `BITMAP_PREFIX = "opl-"`(`opl-types/src/backup.rs:25`) | — | qcow2에 저장된 비트맵과 증분 백업 체인 |
| QMP 객체·작업 | `opl-mig-tls`, `opl-nbd-tls`(`opl-driver/src/migration.rs` 81·84), `opl-backup-`, `opl-mirror-`, `opl-export-`(`qemu.rs` 68–74) | — | 업그레이드 전에 시작한 QEMU의 남은 작업을 새 데몬이 치운다. 혼합 버전 마이그레이션의 양쪽이 같은 이름을 쓴다 |
| 블록 장치 이름 | LV `opl-{id}`·`opl-{id}-s-`·`-revert-`, 태그 `opl-zero-from-`(`storage/lvm.rs` 376–386·1060), zvol `{dataset}/opl-{id}`(`zfs.rs:314`), RBD 이미지 `opl-…` | — | 지금 볼륨의 이름이다(바꾸면 디스크를 찾지 못한다) |
| 네트워크 장치 | 브리지 `oplv<vni>`(138), VXLAN `oplx<vni>`(88), tap `op<10자리>n`(`opl-types/src/network.rs:143`), ifalias `opl:<tag>`(20, `net.rs:277`) | 246+ | 노드 위의 장치를 "이 클러스터가 관리함"으로 알아보는 표시. VM NIC 정의·`bridges` 상태에 저장됨 |
| nft | `table bridge opl_<tag>`(`opl-driver/src/firewall.rs:31`) | — | 지금 적용된 규칙 표를 바꿔 넣고 지운다 |
| 쿠키·토큰·키 | 세션 `opl_session`(`auth.rs:24`), OIDC `opl_oidc`(13), API 토큰 `opl_`(`auth.rs:26`, `remote.rs:13`), 조인 토큰 `oplj_`(9), 구독 키 `opl1.`(25), 조인 URL `opl-join://`(17), 로그인 토큰 이름 `oplctl-` | — | 발급한 토큰·키·로그인 세션, 원격 클러스터에 저장한 토큰, 고객이 가진 구독 키가 계속 맞아야 한다 |
| gRPC·로그 대상 | 메타데이터 `opl-vm`(`opl-peer/src/lib.rs:36`), 감사 로그 대상 `opl_audit`(`audit.rs:833`) | — | 혼합 버전 노드 사이 통신. 감사 줄을 거르는 rsyslog·`journalctl -g opl_audit` 설정 |
| 환경 변수 | `OPL_VERSION`, `OPL_APT_URL`, `OPL_LICENSE_KEYS`, `OPL_SOCKET`, `OPL_CONFIG`, `OPL_TEST_*`, `OPLD_URL` 등 | 336 | 빌드·CI·운영 스크립트 |
| 저장 JSON·Raft | 리소스·Task·상태 필드 이름(제품 이름 없음) | — | 바꿀 것이 없다(확인: 필드 이름에 opLite·opld가 없다) |
| APT | `oplite.sources`, `oplite.conf`, `oplite-archive-keyring.gpg`, `SOURCES_MARK`, Release `Origin`/`Label` `opLite` | — | §3.5 |
| 비교되는 문구 | `CREDENTIALS_LOST`(웹이 비교), `opl-upgrade: start`·`exit ` | — | §3.7 |
| 크레이트·모듈 | `opl-*`, `opld`, `oplctl`, `opl_types::…` | — | 빌드 이름만, 사용자에게 보이지 않는다 |

## 4. 호환·업그레이드

**이미 설치된 노드(oplite V0 → V1).** 운영자가 노드마다 업그레이드(웹·`oplctl node upgrade`)를 누르면 옛 opld가 일시 유닛 `opl-upgrade-<task>`에서 `apt-get dist-upgrade`를 돌린다.

1. `apt-get update`: 같은 저장소, 같은 `Origin`/`Label`이라 그대로 된다.
2. `dist-upgrade`가 `oplite` V1(전환, `Depends: orbivisor (= V1)`)을 고르며 `orbivisor` V1을 새로 설치한다. `orbivisor`의 `Breaks: oplite (<< V1)` 때문에 apt가 옛 `oplite`를 함께 올리거나 먼저 deconfigure한다.
3. `orbivisor` unpack: `Replaces`로 유닛·키를 넘겨받고 `/usr/bin/orbivisord`·링크를 둔다. `oplite` V1 unpack: 옛 파일 중 넘겨받지 않은 것(`/usr/share/oplite/…`, `/usr/share/doc/oplite`의 옛 내용)이 지워진다. 옛 `prerm upgrade`·`postrm upgrade`는 아무것도 하지 않는다.
4. `orbivisor` configure: enable 링크 `opld.service`가 있으므로 restart → `orbivisord`가 돈다. VM의 QEMU는 자기 scope에 있어 계속 돈다(지금 업그레이드와 같다). HA watchdog은 지금 restart와 같은 짧은 간격만 본다.
5. 새 데몬이 일시 유닛의 로그(`opl-upgrade: exit 0`)를 읽어 업그레이드 Task를 끝낸다(지금과 같다 — 유닛·표시 이름이 그대로다).

**혼합 버전.** 이름 바꾸기는 저장 형식·RPC·기능 목록을 바꾸지 않으므로 롤링 업그레이드 중 V0·V1 노드가 그대로 섞여 돈다. 새 노드 기능(`features`)도 더하지 않는다. 옛 노드가 낸 오류 문구는 옛 이름(opLite)으로 보일 수 있다.

**스크립트.** `opld …`, `oplctl …`(링크), `systemctl … opld`, `journalctl -u opld`, `/etc/opl/opld.toml`, `/run/opl/opld.sock`, `~/.config/opl/oplctl.json`, `OPL_*`는 모두 그대로 된다. 바뀌는 것은 `--version`·도움말에 보이는 이름과 journald의 식별자(`journalctl -t opld` → `-t orbivisord`).

**새 설치.** ISO는 `orbivisor`만 설치한다. `apt-get install orbivisor`(저장소)도 같다. `apt-get install oplite`는 전환 패키지를 거쳐 `orbivisor`를 설치한다.

**되돌리기.** V1 → V0 내리기는 지원하지 않는다(지금도 업그레이드만 한다). 손으로 하려면 `apt-get install oplite=V0 orbivisor-`(전환 패키지 대신 옛 실제 패키지, `orbivisor` 지움)다. `orbivisor`의 `prerm remove`가 opld를 멈추고 옛 `oplite`의 postinst가 다시 시작한다. README에 적지 않고 이 문서에만 둔다.

**전환 패키지 지우기.** `apt-get remove oplite`는 안전하다(파일·스크립트 없음, `orbivisor`가 의존하지 않음). 저장소에는 다음 Debian 판(trixie 다음)으로 옮길 때까지 매 판의 전환 패키지를 낸다.

## 5. 시험

- **빌드·버전:** `crates/opld/tests/version.rs`·`crates/oplctl/tests/version.rs`가 `orbivisord <버전>`·`orbivisorctl <버전>`, CI `OPL_VERSION=9.9.9~t` 판 그대로. `CARGO_BIN_EXE_orbivisord`로 E2E(`tests/common/mod.rs`, `smoke.rs`).
- **문구 시험:** `opl-api/tests`(`settings.rs:205`, `subscription.rs:236`, `telemetry.rs:327` older Orbivisor, `remote_client.rs:205`, `snapshot_tasks.rs:1655`, `tasks.rs:52`, `remotes.rs:970`), `opld/tests`(`smoke.rs` 26·195 `orbivisorctl login`), oplctl `cli.rs` 출력 시험들. `CREDENTIALS_LOST`의 시험(`vm_imports.rs`, `vm_import_tasks.rs`, `opld/tests/vm_import.rs`, `web/src/imports/imports.test.ts`)은 문자열이 그대로인지 계속 본다.
- **build-deb-test.sh:** 가짜 바이너리 이름 `orbivisord`·`orbivisorctl`, 두 deb가 생김(`orbivisor_0.1.0~ci7_amd64.deb`, `oplite_…`), `orbivisor`의 트리에 링크 넷(`usr/bin/opld`, `usr/bin/oplctl`, `orbivisord.service`)과 그 대상, md5sums에 링크가 없음, 전환 패키지 control(`Depends: orbivisor (= V)`, 유지 스크립트 없음), 키 경로 그대로, `OPL_LICENSE_KEYS`를 `orbivisord`에서 찾음.
- **maint-test.sh:** 새 분기 "처음 설치인데 enable돼 있음(oplite에서 넘어옴) → restart", "처음 설치이고 enable 안 됨 → enable·start", systemd 없음에서 링크가 이미 있으면 다시 enable하지 않음. 지금의 기대(대상 `opld.service`)는 그대로.
- **deb-test.sh(Debian 13 컨테이너):** `orbivisor` 설치, `orbivisord --version`·`orbivisorctl --version`, `opld --version`·`oplctl --version`이 성공하고 버전이 같음(이름 부분은 1단계에서 정한 기대), 유닛 파일과 별칭 링크, `dpkg --verify orbivisor`, `/usr/share/orbivisor/opld.toml.example`, enable 링크, purge 동작 그대로.
- **repo-test.sh(Debian 13 컨테이너) — 업그레이드 시험 더함:** stable에 옛 모양 `oplite` V0(시험이 `build-deb.sh`의 옛 동작을 흉내 내 만든 실제 파일을 담은 패키지 — 옛 control·유지 스크립트를 픽스처로 둠), testing에 V1의 `orbivisor`·`oplite`. V0를 설치하고 enable 링크를 확인한 뒤 testing으로 바꿔 `apt-get -s dist-upgrade`에 `Inst orbivisor`·`Inst oplite [V0]`가 보이고, 실제 `apt-get dist-upgrade -y` 뒤 `orbivisor` V1·`oplite` V1이 설치됨, `/usr/bin/opld`가 `orbivisord`로 가는 링크, 유닛 파일이 있고 enable 링크가 남음, `dpkg --verify orbivisor`, `apt-get remove oplite` 뒤에도 유닛·링크·소스 파일이 그대로. `Origin`/`Label`이 그대로라 `apt-get update`가 성공함.
- **build-repo-test.sh:** 기대 Release의 `Origin=opLite`·`Label=opLite`는 그대로(바뀌면 실패 — 의도한 고정), deb 이름만.
- **iso-test.sh·setup-test.sh:** 부트 항목 문구·라벨, CD 디렉터리, deb 이름 규칙(`orbivisor_` 아님이면 거절), preseed 파일 목록, 픽스처 `fixtures/orbivisor.issue`, `fixtures/opld-*.toml` 첫 줄, `/var/cache/orbivisor`, 로그 표시.
- **iso.yml(실제 설치):** 무인 설치 → 첫 부팅 cluster init → `orbivisorctl login`·`node list`(버전).
- **웹:** Vitest 위 §3.8 목록, `pnpm -C web build`, `schema.d.ts` 차이 없음.
- **이름 검사:** §3.10 `name-test.sh`.

## 6. 알려진 한계

- 유닛의 원래 이름은 `opld.service`다. 로그는 `journalctl -u opld`로 본다(`journalctl -u orbivisord`는 빈 결과 — §3.3). `systemctl edit`도 `opld`로 한다.
- 이미 설치된 노드의 콘솔 안내(`/etc/issue.d/oplite.issue`)와 opld.toml 첫 줄 주석(옛 예시 경로 `/usr/share/oplite/opld.toml.example`)은 옛 이름으로 남는다. README에 바꾸는 한 줄을 적는다.
- 지금 클러스터의 CA CN은 "opLite cluster X CA"로 남는다(브라우저 인증서 정보에 보임). 새 클러스터만 Orbivisor다.
- 내부 이름(장치 `oplv`·`oplx`·tap `op…`, `/var/lib/opl`, `OPL_*`, 토큰 `opl_` 등)에 옛 이름의 흔적이 남는다. 바꾸려면 노드 위 상태를 옮기는 별도 계획이 필요하다.
- APT Release의 `Origin`/`Label`이 opLite로 남는다(`apt policy`에 보임). 바꾸려면 새 저장소 URL과 옛 URL을 함께 내는 별도 절차가 필요하다.
- 롤링 업그레이드 중 옛 노드의 오류 문구와 옛 CLI의 출력은 옛 이름이다.
- dpkg 실행이 `oplite` V1 unpack과 `orbivisor` unpack 사이에서 끊기면 유닛 파일이 잠시 없을 수 있다(apt가 남은 일을 알리고 `apt-get -f install`로 끝낸다). 데몬은 계속 돈다.
- GitHub 링크는 저장소 이름을 바꾸기 전까지 `…/opLite`다.

## 7. 구현 단계 (계획 43)

각 단계는 따로 빌드되고 그 단계의 시험이 통과한다.

1. **바이너리·호환 링크·패키지.** 환경 확인(clap이 링크 이름으로 부를 때 `--version`의 이름, `autobins = false`와 `[[bin]]` — systemd 별칭은 §3.3에 확인 결과가 있다). `crates/{opld,oplctl}/Cargo.toml`의 `[[bin]]`, clap `name`·`about`, `CARGO_BIN_EXE_*`, version 시험. `build-deb.sh`(두 패키지, 링크, `/usr/share/orbivisor`), `control.in`과 전환 패키지 control, `opld.service`의 `Description`·`ExecStart`, `postinst`의 enable 기준. `build-deb-test.sh`, `maint-test.sh`, `deb-test.sh`, `repo-test.sh`의 넘어가는 업그레이드, `build-repo.sh` 주석과 `build-repo-test.sh`, `.github/actions/build-deb`·`ci.yml` package 작업의 파일·아티팩트 이름.
2. **데몬·CLI 문자열.** §3.7의 바꿀 문구(API·Task 오류, 로그, oplctl 출력·도움말, OpenAPI 제목·doc 주석, CA CN, 크레이트 `description`, `opl-license` about)와 그 Rust 시험, `CREDENTIALS_LOST` 등 그대로 둘 문구의 주석("비교되는 문자열 — 바꾸지 않는다"). `schema.d.ts` 다시 만들기.
3. **웹.** 제목·머리·로그인·`web.rs` 안내 페이지, 번역 ko·en, 조인 명령, `package.json` 이름, Vitest.
4. **ISO·CI.** preseed 파일 이름과 내용, `build-iso.sh`, `orbivisor-setup.sh`의 패키지·로그·issue, 픽스처, `iso-test.sh`·`setup-test.sh`, `iso.yml`, `ldap-setup.sh`, `name-test.sh`와 CI lint 단계.
5. **문서.** README(제목, 문장, 명령 예, 새 절 "이름 바꾸기", 1단계에서 확인한 로그·덮어쓰기 안내), 노드 코어 설계 §1.1 메모, 이 문서의 "구현" 절. GitHub 링크는 저장소 이름을 바꿀 때 따로.

## 8. 구현(계획 43)

- **환경 확인(이 Mac).** clap은 링크 이름으로 불러도 `--version`에 `#[command(name)]`(새 이름)을 쓴다: `ln -s orbivisord opld`의 `opld --version`은 `orbivisord <버전>`이고 도움말의 사용법 줄만 `argv[0]`(`Usage: opld …`)을 따른다. 그래서 `deb-test.sh`·`repo-test.sh`는 옛 이름의 `--version`도 `orbivisord <버전>`·`orbivisorctl <버전>`으로 본다. `autobins = false`와 `[[bin]]`으로 `target/release/orbivisord`·`orbivisorctl`이 생기고 `-p opld`·`-p oplctl`·기능 이름·`cargo run -p opld`는 그대로 된다. systemd 별칭은 §3.3의 확인을 다시 하지 않았다.
- **바이너리·패키지.** §3.2·§3.4대로. `build-deb.sh`는 두 줄을 찍는다(`orbivisor_<v>_amd64.deb`, 그다음 `oplite_<v>_amd64.deb`). 전환 패키지의 control은 새 파일 `packaging/deb/oplite.control.in`(설명 "opLite was renamed to Orbivisor. … It can be removed."), 내용은 `/usr/share/doc/oplite/copyright`만이다. 링크 넷(`/usr/bin/opld`·`oplctl`, `orbivisord.service`)은 상대 링크이고 `md5sums`에 넣지 않는다. `copyright`의 `Upstream-Name`·`Copyright`는 Orbivisor, `Source`(GitHub)는 그대로. build-deb 액션의 출력은 `deb`(orbivisor)와 새 `transitional`, CI 아티팩트는 `orbivisor-deb`(`dist/*_amd64.deb`). `deb-test.sh`·`repo-test.sh`는 인자가 둘(`<orbivisor deb> <oplite deb>`)이다.
- **postinst.** §3.4의 표대로 enable돼 있는지를 처음 설치보다 먼저 본다. systemd가 없을 때의 "enable 링크가 이미 있음"은 `/etc/systemd/system/multi-user.target.wants/opld.service` 링크로 보고, 시험이 그 디렉터리를 바꾸도록 `OPL_SYSTEMD_ETC`(기본 `/etc/systemd/system`)를 더했다.
- **repo-test의 옛 V0.** 이름 바꾸기 전 `build-deb.sh`의 control·유지 스크립트·유닛을 픽스처 `packaging/tests/fixtures/oplite-v0/`로 복사해 두고, 시험이 버전을 찍는 가짜 `opld`·`oplctl` 실제 파일로 `oplite_<V>~v0` 패키지를 만든다(stable). testing에 V1의 두 deb를 넣어 `apt-get -s dist-upgrade`의 `Inst oplite [V0] (V1 …`·`Inst orbivisor (V1 …`, 실제 `dist-upgrade`(시험 시간 때문에 `--no-install-recommends`) 뒤 링크·`--version`·유닛·별칭·enable 링크·소스 파일·`/usr/share/orbivisor`·`dpkg --verify orbivisor`, `/usr/share/oplite`가 없어짐, `apt-get remove oplite` 뒤에도 같음을 본다.
- **문자열(2단계).** §3.7대로. 그대로 둔 문자열(`CREDENTIALS_LOST`, `SOURCES_MARK`와 "has no opld mark" 로그, `opl-upgrade:` 표시, `LOGIN_TOKEN_PREFIX`, `KEYRING`·`AUTH_FILE`·`default_sources_file`)에 바꾸지 않는 이유를 주석으로 달았다. clap 도움말이 되는 `///` 주석의 opld·oplctl은 orbivisord·`orbivisorctl …`로 바꿨고 그 밖의 코드 주석의 opld·oplctl(크레이트 이름)은 그대로다. 시험의 예시 저장소 URL은 `https://apt.example.com/orbivisor`로 바꿨고, 흉내 낸 apt 출력의 `opLite:trixie`(Release `Origin`)와 시험 IdP의 `client_id`/`aud` `"oplite"`는 그대로다.
- **웹(3단계).** §3.8대로(번역 ko·en 각 25개). `web/`에 남은 oplite는 `RealmsPage.test.tsx`의 시험 IdP `client_id`뿐이다. `CREDENTIALS_LOST` 사본, `OPLD_URL`, `playwright.config.ts`의 개발 안내(`opld` 크레이트)는 그대로.
- **ISO·CI(4단계).** §3.9·§3.10대로. 설계에 없던 것: ISO 볼륨 ID `ORBIVISOR`, `build-iso.sh`는 전환 패키지 이름(`oplite_…`)을 종료 코드 2로 거절한다(`--deb`는 `orbivisor_<v>_amd64.deb`만). 시험 훅 환경 변수 `OPLITE_*`는 내부 식별자가 아니라 시험 전용이라 `OPL_ISO_LIB`·`OPL_SETUP_LIB`·`OPL_CDROM`·`OPL_CONSOLE`·`OPL_INSTALLER_ROOT`로 바꿨다(§3.12의 `OPL_*` 규칙에 맞춤). CI 첫 부팅 표시 파일은 `/var/lib/orbivisor-ci-init.done`.
- **name-test.sh.** `git grep -nI -i oplite`의 줄마다 그 파일의 허용 규칙(`"<파일 glob>|<ERE>"`, 맞는 부분을 지움)을 적용하고 oplite가 남으면 실패한다. 규칙 자체도 시험한다(`expect clean|left`). 통째로 빼는 것: 이 시험, `docs/superpowers/plans`·`specs`, 전환 control, V0 픽스처. 규칙: APT 파일 이름 셋, GitHub 링크, Release `Origin`/`Label`과 `opLite:trixie`, 시험 IdP `client_id`(파일마다), 전환 패키지를 만들고 시험하는 패키징·CI 파일의 소문자 `oplite`, README(아래). CI `shell` 작업에서 shellcheck 다음에 돈다.
- **문서(5단계).** README: 제목 `# Orbivisor`, 문장의 opLite, 명령 예 `orbivisorctl …`, 데몬을 가리키는 orbivisord(`sudo orbivisord --config …`, `pidof orbivisord`), 패키지·ISO·설치 스크립트·로그·콘솔 안내·예시 설정의 새 이름, `build-repo.sh` 예는 두 deb(`dist/*_<v>_amd64.deb`), 새 절 "이름 바꾸기 (opLite → Orbivisor)"(옛·새 이름 표, 옛 명령·유닛·경로가 그대로 됨, `--version`, 유닛 이름과 `journalctl -u opld`·`systemctl edit opld`·`journalctl -t orbivisord`, 넘어가는 업그레이드의 두 줄과 Recommends 다시 설치, 전환 패키지 지우기, APT 이름·`Origin`/`Label`과 저장소 주소, 이미 설치한 노드의 콘솔 안내를 바꾸는 한 줄, 남긴 내부 이름). 그대로 둔 것: `cargo … -p opld|oplctl`, `opld 시험`(크레이트의 시험), 유닛·설정·소켓·로그 명령, `# managed by opld` 표시, `CREDENTIALS_LOST` 인용, 로그인 토큰 `oplctl-…`, `~/.config/opl/oplctl.json`, 예시 저장소 URL `https://apt.example.com/oplite`(§3.5), 지난 계획 문서 링크의 제목. 노드 코어 설계 §1.1에 메모.
- **설계와 다른 점.**
  - README의 감사 로그 rsyslog 예는 `$programname == 'opld'`였는데, 프로세스 이름이 `orbivisord`가 되어(§3.2) journald 식별자도 바뀌므로 `($programname == 'orbivisord' or $programname == 'opld')`로 바꿨다(롤링 업그레이드 중 옛 노드도 거름). §3.11에 없던 변경이다.
  - README의 LDAP·OIDC 예(서비스 계정 `cn=oplite`·`svc-oplite`·그룹 `oplite-admins`, Keycloak 클라이언트 `oplite`·`--client-id oplite`)는 관리자가 정하는 값의 예라 `orbivisor`로 바꿨다. 코드 시험 자료의 `client_id: "oplite"`(§3.7)는 그대로다.
  - name-test의 README 규칙은 이름 바꾸기 절이 쓰는 모양만 허용한다: `opLite → Orbivisor`, 코드 표기 `` `oplite` ``·`` `oplite.iso` ``, `"Install opLite"`, `/usr/share/oplite/`, `/etc/issue.d/oplite.issue`, `s/opLite/`, `apt-get remove oplite`, `"opLite cluster X CA"`, `apt.example.com/oplite`. 그 밖의 README 줄에 옛 이름이 생기면 실패한다.
  - README에 계획 43 설계 문서 링크를 더했다(계획 문서 링크는 실행 기록과 함께).
- **1차 리뷰에서 고친 것.**
  - postinst(§3.4 표의 "처음 설치" 판단): configure의 `$2`가 비어도 `dpkg-query -W -f '${Config-Version}' oplite`가 비지 않으면(옛 `oplite`가 설정된 적이 있음 — 넘어갈 때 `oplite` V1은 unpack만 된 상태라 옛 판이 나온다) 처음 설치로 보지 않는다. 그래서 enable돼 있으면 restart, 아니면 아무것도 하지 않는다: 관리자가 꺼 둔 opld는 넘어간 뒤에도 꺼진 채다. systemd가 없는 분기도 같다. `maint-test.sh`는 가짜 `dpkg-query`(`OPL_FAKE_OPLITE_CONFIG`)로 두 경우를 본다.
  - OpenAPI로 나가는 doc 주석(`opl-api`·`opl-types`)의 실행할 명령 `oplctl …`은 `orbivisorctl …`, 프로그램으로서의 opld는 orbivisord로 바꿨다(`schema.d.ts`). 파일 이름(`opld.toml`·`opld.sock`)과 크레이트를 뜻하는 곳, OpenAPI에 나가지 않는 주석은 그대로다.
  - 넘어가는 업그레이드 중 `oplite` V1 unpack과 `orbivisor` unpack 사이에는 옛 파일(`/usr/bin/opld`, 유닛 파일, 저장소 키)이 잠깐 없을 수 있다(§6). 이미 도는 데몬과 enable 링크(`/etc`, 패키지 파일 아님)는 영향이 없다.
  - `repo-test.sh`: V0에 저장소 키를 싣고, V1 `orbivisor`를 받은 deb의 바이너리로 `build-deb.sh`(`OPL_APT_KEY`)를 다시 돌려 키와 함께 만든다. 업그레이드 뒤 키가 남고 `dpkg -S`가 `orbivisor`, `dpkg.log`에서 `upgrade oplite <V0> <V1>`이 `install orbivisor`보다 앞(deconfigure 단계는 dpkg.log 표기가 확실하지 않아 순서만 본다), `apt-mark showmanual`에 `orbivisor`(apt의 `APT::Move-Autobit-Sections` `oldlibs` — 수동 표시가 전환 패키지의 새 의존으로 옮겨 감), `apt-get remove oplite` 뒤 `apt-get -s autoremove`에 `orbivisor`가 없음을 본다.
  - `name-test.sh`: 패키징·CI 파일의 소문자 `oplite` 통째 허용을 패키지 이름으로 쓰는 모양(deb 이름, `Breaks`·`Replaces`, `Package:`, 패키지를 다루는 명령, 문장 속 패키지 이름 등)으로, README 규칙을 README의 실제 줄 모양(표 칸, `` `/usr/share/oplite/opld.toml.example` ``, deb 두 개의 `apt install` 등)으로 좁혔다. 이름에 oplite가 있는 파일은 전환 control과 V0 픽스처뿐인지도 본다. 작은따옴표 안의 `$`·백틱 때문에 shellcheck SC2016을 파일 전체에서 끈다.
  - README: deb 파일로 옮길 때는 두 deb를 함께 설치한다(새 deb 하나만이면 apt가 옛 `oplite`를 지우며 opld를 멈추고 관리 소스 파일을 지운다 — 데몬은 다시 켜지지만 그동안 멈춤), `apt autoremove`가 전환 패키지를 지울 수 있음(안전), 꺼 둔 opld는 꺼진 채.
- **2차 리뷰에서 고친 것.**
  - `maint-test.sh`의 가짜 `dpkg-query`는 인자가 정확히 `-W -f ${Config-Version} oplite`가 아니면 종료 코드 2로 끝난다(postinst의 패키지 이름·필드 오타를 잡음).
  - README: 새 deb 하나만 설치하거나 옮기기 전에 `apt remove oplite`를 하면 옛 `oplite` V0가 `rc`로 남는데, 나중에 그것을 purge하면(`apt purge`, `apt autoremove --purge`, `dpkg --purge` 정리) 옛 `postrm purge`가 `opld.service`를 disable하고 enable 링크를 지워 다음 재부팅에 orbivisord가 시작하지 않는다. "패키지만 설치하기"와 "이름 바꾸기"에 `dpkg -l oplite`가 `rc`면 purge하지 말고, purge했다면 `sudo systemctl enable opld`를 하라고 적었다.
