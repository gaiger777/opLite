# 이름 맞추기 2 — 서비스·경로·환경 변수·쿠키·크레이트를 orbivisor로 (설계)

상위 설계: `2026-11-10-rename-orbivisor-design.md`(계획 43). 계획 43은 사람이 보는 제품 이름·패키지·바이너리만 바꾸고 노드 위의
식별자를 그대로 두었다. 운영자는 지금 `systemctl status opld`, `/etc/opl/opld.toml`, `OPL_*`, 로그의 `opl_api::…`를 본다 —
제품 이름(Orbivisor)과 맞지 않는다는 지적(2026-11-26)에 따라 **운영자가 노드에서 만나는 이름**을 orbivisor로 맞춘다.
업그레이드는 계획 43과 같이 `apt-get dist-upgrade` 한 번으로 끝나야 하고, 돌고 있는 VM은 멈추지 않으며, 옛 이름을 쓰는
스크립트는 한 릴리스 동안(경고와 함께) 그대로 돈다.

## 1. 범위

| 바꿈 | 지금 | 새 이름 | 옛 이름 |
|---|---|---|---|
| 데몬 유닛 | `opld.service`(+ 별칭 `orbivisord.service`) | `orbivisord.service` | 정적 별칭 링크 `opld.service`(영구) |
| OVN 중앙 유닛 | `opl-ovn-{nb,sb,northd}.service` | `orbivisor-ovn-{nb,sb,northd}.service` | 별칭 링크(데몬이 바꿔 씀) |
| 일시 유닛 | `opl-upgrade-*`, `opl-bkserve-*`, `opl-fr-*` | `orbivisor-upgrade-*`, `orbivisor-bkserve-*`, `orbivisor-fr-*` | 찾기·멈추기는 두 접두사 다(한 릴리스) |
| 설정 | `/etc/opl/opld.toml` | `/etc/orbivisor/orbivisord.toml` | 디렉터리·파일 심볼릭 링크(영구) |
| 데이터·런타임·로그 | `/var/lib/opl`, `/run/opl`, `/var/log/opl` | `/var/lib/orbivisor`, `/run/orbivisor`, `/var/log/orbivisor` | 디렉터리 심볼릭 링크(영구) |
| 제어 소켓·잠금 | `/run/opl/opld.sock`, `{data_dir}/opld.lock` | `/run/orbivisor/orbivisord.sock`, `{data_dir}/orbivisord.lock` | 소켓은 링크 `opld.sock`(영구) |
| CLI 로그인 정보 | `~/.config/opl/oplctl.json` | `~/.config/orbivisor/orbivisorctl.json` | 새 파일이 없으면 옛 파일을 읽음(한 릴리스) |
| 환경 변수 | `OPL_*`(실행 때 읽는 것 12개 + 빌드·시험) | `ORBIVISOR_*` | 실행 때 읽는 것만 옛 이름도 받고 경고(한 릴리스) |
| 웹 쿠키 | `opl_session`, `opl_oidc` | `orbivisor_session`, `orbivisor_oidc` | 둘 다 쓰고, 값이 다르면 옛 이름(더 나중의 로그인) 먼저 읽음(한 릴리스, §3.6) |
| 크레이트 | `opl-*`, `opld`, `oplctl` | `orbivisor-*`, `orbivisord`, `orbivisorctl` | 없음(사용자에게 보이는 것은 로그 target뿐) |
| 보조 바이너리 | `opl-devserver` | `orbivisor-devserver` | 없음(개발용) |

그대로 둠(노드·클러스터에 이미 만들어져 저장된 이름 — 바꾸면 돌고 있는 VM·디스크·네트워크를 다시 만들어야 한다):
VM 스코프 `opl-vm-{id}.scope`(돌고 있는 QEMU가 그 안에 있다), 브리지·탭 `oplv*`/`oplx*`(IFNAMSIZ 15자), nft 표, LVM·ZFS·RBD
이름(`opl-{id}` 등), QMP 작업·노드 이름(`opl-backup-*` 등), OVN NB/SB 행 이름(`opl-lb-*` 등), API 토큰 형식
`opl_<id>_<secret>`(저장된 비밀·외부 도구가 들고 있다), 가입 토큰 `opl-join://`, APT 저장소 파일 이름·Origin(계획 43 §3.5),
Raft·저장 JSON 필드, `SOURCES_MARK`, 로그인 토큰 접두사 `oplctl-`(옛 CLI가 만든 토큰을 `logout`이 알아봐야 한다).
메트릭은 이미 `orbivisor_*`다(바꿀 것 없음). GitHub 저장소 이름 `opLite`도 그대로(계획 43 §3.11).

## 2. 견준 길과 고른 것

- **경로 옮기기: 데몬이 시작할 때 `renameat2(RENAME_EXCHANGE)`로 맞바꿈(채택) vs postinst의 `mv` + `ln` vs 새 경로를 옛 경로로의
  링크로만.** `mv`와 `ln` 사이에는 옛 경로가 없는 순간이 있다 — 업그레이드 중에도 옛 데몬·QEMU·swtpm·ovsdb가 `/var/lib/opl/…`,
  `/run/opl/vm/…/qmp.sock`을 이름으로 연다. 맞바꿈은 미리 `/var/lib/orbivisor → orbivisor`(자기 자신을 가리키는 링크, 아무도 쓰지
  않음)를 만든 뒤 두 이름을 원자적으로 바꾸므로, 바꾼 뒤 `/var/lib/opl`은 `orbivisor`를 가리키는 링크가 되어 어느 순간에도 옛
  경로가 풀린다(같은 파일 시스템 안의 이름 바꾸기라 열린 fd·inode는 그대로). 데몬에서 하면 deb·ISO·손 설치(실험실)가 모두 같은
  코드를 타고 Rust 시험(임시 뿌리)으로 검증된다. 새 경로를 옛 경로의 링크로만 두면 업그레이드한 노드에는 `opl`이 진짜 디렉터리로
  남아 이 계획의 목적에 못 미친다.
- **데몬 유닛: 실제 파일을 `orbivisord.service`로, `opld.service`를 정적 별칭 링크로(채택) vs 계획 43처럼 그대로.** 계획 43이 걱정한
  셋을 실험으로 풀었다(systemd 257, svc02, 2026-11-26): (1) 돌고 있는 유닛의 파일을 다른 유닛의 별칭 링크로 바꾸고
  `daemon-reload`하면 두 이름이 한 유닛으로 합쳐지고 MainPID가 그대로다, (2) `/etc/systemd/system/opld.service.d/` 덮어쓰기는 별칭
  이름으로도 그대로 읽힌다(`DropInPaths`에 보임), (3) `systemctl restart opld`·`orbivisord` 둘 다 된다. 남는 것은 enable 링크 —
  `multi-user.target.wants/opld.service`만 있으면 `is-enabled orbivisord`가 `disabled`다 — postinst가 옮긴다(§4.2). 업그레이드 Task의
  일시 유닛이 postinst를 돌리므로 데몬 재시작이 업그레이드를 끊지 않는 것은 지금과 같다.
- **설정 경로: `--config` 기본값 = 새 경로, 없으면 옛 경로(채택) vs 둘 다 받고 합침.** 맞바꿈 뒤에는 옛 경로가 새 파일을 가리키는
  링크이므로 "둘 다 있음"은 링크인 경우뿐이다. 진짜 파일이 둘이면 새 경로를 쓰고 경고한다.
- **환경 변수: 실행 때 읽는 것만 옛 이름을 받음(채택) vs 전부.** 빌드 변수(`OPL_VERSION` 등)와 시험 변수는 이 저장소의 CI·스크립트만
  쓴다 — 한 번에 바꾼다. 운영자·Terraform 사용자가 셸·CI에 넣어 둔 실행 변수(§3.3 표)만 한 릴리스 동안 옛 이름도 받는다.
- **쿠키: 두 이름을 함께 씀(채택) vs 새 이름만.** VIP나 프록시 뒤에서 한 브라우저가 업그레이드한 노드와 아직 옛 노드를 번갈아 만난다.
  새 노드가 `orbivisor_session`과 `opl_session`을 함께 쓰면 옛 노드도 같은 세션을 읽는다. 로그아웃은 둘 다 지운다.
- **크레이트 이름: 바꿈(채택) vs 계획 43처럼 둠.** 로그 줄의 target(`opl_api::tasks: …`)과 패닉 위치가 운영자에게 보인다. 직렬화 형식에는
  크레이트 경로가 들어가지 않는다(1단계가 `type_name`·`module_path!`을 확인한다). 큰 기계적 바꿈이라 다른 바꿈과 섞지 않고 첫 단계로
  혼자 한다.
- **VM 스코프: 그대로(채택) vs 새 VM부터 새 이름.** 돌고 있는 VM의 스코프는 다시 시작할 때까지 옛 이름이라 노드에 두 이름이 오래 섞이고,
  메트릭·멈추기·HA 펜싱·`Before=` 순서가 모두 두 이름을 찾아야 한다. 운영자는 VM을 웹·CLI로 다루고 스코프 이름을 칠 일이 드물다.

## 3. 자세히

### 3.1 크레이트(1단계)

`crates/opl-{types,api,cluster,driver,peer,qemu,reconciler,restore,store}` → `crates/orbivisor-*`, `crates/opld` → `crates/orbivisord`,
`crates/oplctl` → `crates/orbivisorctl`. 러스트 경로 `opl_types::` → `orbivisor_types::` 등, `oplctl::` → `orbivisorctl::`, 기능
`opld/qemu-it` → `orbivisord/qemu-it`, `-p opld` → `-p orbivisord`. 바이너리 `opl-devserver` → `orbivisor-devserver`. CI·문서
(`docs/guide`, `docs/architecture.md`, README)·스크립트를 함께. 지난 계획·설계 문서는 그대로(계획 43과 같은 규칙).
**바꾸지 않는 문자열**: 파일 이름 `opld.toml`·`opld.sock`·`opld.lock`(3단계가 따로), 유닛 이름, §1 "그대로 둠", 사용자 문장 속
`opld`·`oplctl`(이미 계획 43이 `orbivisord`·`orbivisorctl`로 바꿨다). 1단계는 동작을 바꾸지 않는다 — 시험 수·결과가 그대로다.

### 3.2 공통 도우미

`orbivisor-types::legacy`(새 모듈):
- `env(name) -> Option<String>`: `ORBIVISOR_{name}`을 읽고, 없으면 `OPL_{name}`을 읽어 한 번만 `warn!`("OPL_X is deprecated; use
  ORBIVISOR_X")한다. 둘 다 있으면 새 것.
- `LAYOUT`: (옛, 새) 짝 표 — `/etc/opl`↔`/etc/orbivisor`, `/var/lib/opl`↔`/var/lib/orbivisor`, `/run/opl`↔`/run/orbivisor`,
  `/var/log/opl`↔`/var/log/orbivisor`.

### 3.3 환경 변수(2단계)

실행 때 읽어 옛 이름도 받는 것: `OPL_CONFIG`, `OPL_SOCKET`, `OPL_SERVER`, `OPL_TOKEN`, `OPL_CA_PEM`, `OPL_CA_FINGERPRINT`,
`OPL_ACME_DNS_SECRET`, `OPL_ACME_EAB_HMAC`, `OPL_ESXI_PASSWORD`, `OPL_VMWARE_PASSWORD`, `OPL_LOG`, `OPL_QEMU_SHARE`(orbivisorctl·
orbivisord·devserver, Terraform 공급자 Go 코드 — Go 쪽은 같은 규칙의 작은 함수). 나머지(빌드 `OPL_VERSION`·`OPL_LICENSE_KEYS`·
`OPL_APT_URL`·`OPL_TELEMETRY_URL`·`OPL_WEB_DIST`, 패키징 `OPL_SYSTEMD_DIR` 등, 시험 `OPL_TEST_*`·`OPL_S3_*`·`OPL_RESTORE_*` 등)는
`ORBIVISOR_*`로 한 번에 바꾼다. 빌드 변수는 `option_env!` 두 이름을 차례로(릴리스 파이프라인이 옛 이름으로 빌드해도 값이 빠지지
않게) — GitHub 비밀(`secrets.*`) 이름은 저장소 설정이라 그대로 두고 env 이름만 바꾼다.

### 3.4 경로·설정·소켓(3단계)

- 기본값(`crates/orbivisord/src/config.rs`): `data_dir=/var/lib/orbivisor`, `run_dir=/run/orbivisor`, `log_dir=/var/log/orbivisor`,
  Ceph `client_dir=/etc/orbivisor/ceph`, OVN `etc=/etc/orbivisor/ovn` 등, 제어 소켓 `{run_dir}/orbivisord.sock`, 잠금
  `{data_dir}/orbivisord.lock`. ACME 훅 디렉터리 `HOOK_DIR=/etc/orbivisor/acme-hooks/`(옛 `/etc/opl/acme-hooks/…` 경로도 받는다 —
  저장된 인증서 spec에 들어 있다. 맞바꿈 뒤 같은 곳이다).
- `--config`는 선택이 된다: 없으면 `/etc/orbivisor/orbivisord.toml`, 그것이 없고 `/etc/opl/opld.toml`이 있으면 그것.
- **배치 맞추기**(`orbivisord::layout::migrate(root)` — 시험은 임시 뿌리): 설정을 읽기 전에, root(euid 0)이고 `--config`가 없거나
  기본 두 경로 중 하나일 때만. 짝마다:
  1. 옛 것이 진짜 디렉터리이고 새 것이 없음 → `ln -s <새 이름의 basename> <새>`(자기 링크) 뒤 `renameat2(옛, 새, RENAME_EXCHANGE)`.
     결과: 새 = 진짜 디렉터리, 옛 = 새를 가리키는 상대 링크. 맞바꿈이 안 되면(마운트 지점 EBUSY, 맞바꿈을 받지 않는 파일
     시스템 EINVAL, 옛 것이 다른 곳으로의 링크 — 리뷰 1 S1) 자기 링크를 지우고 거꾸로 새 이름을 옛 이름으로의 상대 링크
     (`orbivisor → opl`)로 만든 뒤 경고(링크로 둬도 된다. 마운트 지점이면 끝내는 절차를 안내 — 링크를 지우고 디렉터리를 만들어
     마운트를 옮김. fstab만 고치면 systemd가 링크를 따라 옛 자리에 다시 마운트한다, 리뷰 2 N4). 그것도 못 하면 데이터 디렉터리 짝은
     시작을 거부하고(빈 새 디렉터리로 새 노드가 되지 않게) 나머지 짝은 경고. 맞바꿈 직전에 멈춰 남은 자기 링크는 다음 시작에서
     지우고 다시 한다.
  2. 새 것이 진짜이고 옛 것이 없음(새로 설치) → 옛 이름 링크를 만든다(저장된 옛 경로·옛 스크립트가 풀리게).
  3. 둘 다 진짜 → 손대지 않고 경고. 다만 옛 것이 빈 디렉터리이고 마운트 지점이 아니면(마운트 지점을 새 이름으로 옮긴 뒤 남은
     것 — 리뷰 2) 지우고 옛 이름 링크를 만든다. 런타임 짝(`/run`)이 둘 다 진짜면 기본 경로는 옛 자리다(옛 판으로 되돌렸다가
     다시 올린 노드에서 옛 데몬이 띄운 VM의 소켓이 옛 디렉터리에 있다 — 다음 부팅에 `/run`이 비면 맞춰진다).
  그다음 `/etc/orbivisor/opld.toml`이 진짜 파일이고 `orbivisord.toml`이 없으면 같은 방식으로 맞바꿔 `opld.toml → orbivisord.toml`
  링크를 남긴다. 소켓: 데몬이 `orbivisord.sock`을 열고 `opld.sock → orbivisord.sock` 링크를 만든다(옛 CLI·스크립트).
- root인데 기본이 아닌 `--config`라 맞추기를 건너뛰었고 배치가 맞지 않으면 시작 로그에 그 까닭을 경고한다(리뷰 2).
- 맞추기를 건너뛰는 노드(root가 아님, 기본이 아닌 `--config`)와 맞추기 앞의 `--release-vips`: 경로 기본값(data·run·log·
  Ceph `client_dir`·OVN·ACME 훅 자리)은 새 디렉터리가 없고 옛 디렉터리가 있으면 옛 자리다(`layout::existing`). 설정 파일에 적은
  값은 그대로. 기능 `orbivisor-layout`은 `/etc`·`/var/lib` 짝이 같은 곳으로 풀리거나 옛 이름이 없을 때만 알린다.
- 설정 파일이 둘 다 진짜면(옛 경로를 원자적으로 바꿔 쓰는 도구가 링크를 파일로 바꿈) 더 늦게 고친 쪽을 읽고 어느 것인지 경고한다.
- 설정 파일 안에 옛 경로(`data_dir = "/var/lib/opl"`)가 적혀 있으면 그대로 돈다(링크) — 시작 로그에 한 번 안내.
- **섞인 클러스터**: Raft에 저장되고 다른 노드가 여는 경로(예: RBD 풀의 `{client_dir}/ceph.conf`)를 새 노드가 새 기본값으로 만들면 옛
  노드에는 그 경로가 없다. 3단계가 그런 값을 모두 찾아, 클러스터 기능 `orbivisor-layout`이 모든 노드에 있을 때까지는 옛 접두사로
  적는다(새 노드에도 옛 이름 링크가 늘 있으므로 어느 노드에서나 풀린다). 못 찾으면 "없음"을 기록한다.
- CLI 로그인 정보: `~/.config/orbivisor/orbivisorctl.json`, 없으면 `~/.config/opl/oplctl.json`을 읽고 다음 저장은 새 파일에.
  `--socket` 기본값 `/run/orbivisor/orbivisord.sock`, 그것이 없고 옛 것이 있으면 옛 것(옛 데몬 노드에서 새 CLI).

### 3.5 유닛(3단계)

- 패키지: `/usr/lib/systemd/system/orbivisord.service`(실제 파일: `Description=Orbivisor node daemon`,
  `ConditionPathExists=|/etc/orbivisor/orbivisord.toml`, `ConditionPathExists=|/etc/opl/opld.toml`, `ExecStart=/usr/bin/orbivisord`,
  `ExecStopPost=/usr/bin/orbivisord --release-vips`, 나머지 줄 그대로), `/usr/lib/systemd/system/opld.service → orbivisord.service`.
  `/usr/share/orbivisor/orbivisord.toml.example`(옛 이름 파일은 없앰). VM 스코프의 `Before=opld.service`는 별칭으로 같은 유닛을 가리킨다 —
  새 스코프는 `Before=orbivisord.service`.
- postinst(§4.2), prerm·postrm의 `systemctl` 대상은 `orbivisord.service`.
- OVN: 데몬이 `orbivisor-ovn-*.service`를 쓰고, `/etc/systemd/system/opl-ovn-*.service`가 Orbivisor 표시가 있는 파일이면
  새 이름으로의 링크로 바꾼 뒤 `daemon-reload` — 돌고 있는 ovsdb·northd가 멈추지 않는다(§2의 실험과 같은 합치기). 그 뒤의 비교·
  다시 시작 규칙은 지금 그대로.
- 일시 유닛: 새로 만드는 것은 `orbivisor-*`. 이름으로 찾는 곳(업그레이드 Task가 다시 시작한 데몬에서 자기 유닛을 찾기, 백업 서빙·파일
  복원 정리)은 두 접두사를 다 본다.

### 3.6 쿠키(4단계)

`SESSION_COOKIE=orbivisor_session`, `STATE_COOKIE=orbivisor_oidc`. 로그인·OIDC는 새 이름과 옛 이름을 같은 값·속성으로 둘 다
`Set-Cookie`한다. 읽기는 두 값이 같거나 하나만 있으면 그것, 다르면 옛 이름 → 새 이름(새 판 노드는 두 이름에 같은 값을 쓰므로
다른 값은 그 뒤 옛 판 노드에서 한 로그인 — 리뷰 1 M2). 고른 세션이 끝났거나 쓸 수 없으면(비밀번호 세대, 영역) 다른 이름을 본다.
옛 이름 쪽을 고르면 그 응답에서 새 이름 쿠키를 같은 값·속성으로 고쳐 쓴다(리뷰 2). OIDC 상태 쿠키는 새 이름 → 옛 이름 차례로
검증해 처음 맞는 것. 로그아웃·만료는 넷 다 지우고, 로그아웃 감사 주체는 인증과 같은 차례로 고른다.

### 3.7 문서·안내·이름 시험(4단계)

`docs/guide`·README·`docs/architecture.md`·웹 번역(경로·유닛을 보이는 문장)·CLI 도움말·오류 문구를 새 이름으로. getting-started에
"이름 바꾸기 2" 절: 무엇이 바뀌고 옛 이름이 언제까지 먹히는지, 그대로 둔 것과 그 까닭. `packaging/tests/name-test.sh`에 규칙을 더한다:
허용 목록(호환 코드·이 절·지난 문서) 밖에서 `/etc/opl\b`·`/var/lib/opl\b`·`/run/opl\b`·`/var/log/opl\b`·`opld\.service`·
`opl-ovn-`·`\bOPL_[A-Z]`·`opl_(types|api|cluster|driver|peer|qemu|reconciler|restore|store)\b`·`crates/opl`이 보이면 실패.

## 4. 업그레이드

### 4.1 차례(옛 판이 돌고 있는 노드에서 `apt-get dist-upgrade`)

1. unpack: 새 유닛 파일·별칭 링크·바이너리가 놓인다. 옛 데몬은 계속 돈다(`prerm upgrade`는 멈추지 않는다 — 지금과 같다).
2. postinst configure(§4.2): enable 링크·덮어쓰기 옮기기, `daemon-reload`(돌고 있는 `opld.service`가 `orbivisord.service`로 합쳐짐),
   `systemctl restart orbivisord.service`.
3. 새 데몬 시작: 배치 맞추기(§3.4) → 설정 읽기 → 소켓·잠금 → OVN 유닛 이름 바꾸기 → 평소대로. VM은 스코프에서 계속 돈다.
4. 다른 노드는 아직 옛 판: 경로는 노드 안의 일, 쿠키는 둘 다, 일시 유닛은 노드 안, `orbivisor-layout` 기능이 섞인 동안의 저장 경로를 막는다.

### 4.2 postinst(유닛 부분)

```
if [ -L $ETC/multi-user.target.wants/opld.service ]; then
    rm -f  $ETC/multi-user.target.wants/opld.service
    ln -s /usr/lib/systemd/system/orbivisord.service $ETC/multi-user.target.wants/orbivisord.service
fi
if [ -d $ETC/opld.service.d ] && [ ! -L $ETC/opld.service.d ] && [ ! -e $ETC/orbivisord.service.d ]; then
    mv $ETC/opld.service.d $ETC/orbivisord.service.d && ln -s orbivisord.service.d $ETC/opld.service.d
fi
daemon-reload    # 옮긴 뒤 한 번(리뷰 2 N3 — 옮기기 앞에 reload하면 /etc의 진짜 opld.service와 새 실제 파일이 두 단위가 된다)
is-enabled orbivisord → restart orbivisord / first → enable --now(설정 없으면 조건이 건너뜀)
```
systemd가 돌지 않을 때(chroot)는 링크만. 옛 oplite V0에서 바로 넘어오는 길(계획 43 §4)도 같은 규칙으로 덮인다(`maint-test.sh`에 줄을 더함).
swtpm AppArmor의 새 경로 규칙은 conffile 밖 `local/usr.bin.swtpm`에도 한 줄씩 한 번만 더한다(옛 판으로 되돌리거나 conffile을 고친
노드 — 리뷰 2 N2). purge는 그 줄들을 뺀다.

### 4.3 옛 판으로 되돌리기(리뷰 2 N1)

dpkg는 옛 판을 푼 뒤 새 판의 `postrm upgrade <옛 판>`을 부른다. 풀린 판의 `/usr/lib/systemd/system/opld.service`가 진짜 파일(이름
바꾸기 앞)이면 OVN 중앙 단위를 거꾸로 옮긴다: `orbivisor-ovn-*.service`(Orbivisor 표시, 옛 이름이 그것으로의 링크이거나 없음)를
옛 이름으로, 새 이름은 옛 이름으로의 링크로, enable 링크도 옛 이름으로, `daemon-reload` 한 번 — 옮길 때와 같은 합치기 전이라 도는
ovsdb·northd가 멈추지 않는다. 그러지 않으면 옛 데몬이 옛 이름 링크 자리에 진짜 파일을 써서 같은 DB를 쥔 단위가 둘이 된다. 옛
데몬은 단위 글을 옛 경로로 다시 쓰고 중앙을 한 번 다시 시작한다(받아들임). 다시 올리면 새 데몬의 adopt가 새 이름 링크를 진짜
파일로 바꾼 뒤 옛 이름을 링크로 둔다(서로 가리키는 링크가 되지 않게). 데몬 배치(디렉터리 링크)는 옛 판이 링크로 풀어 쓰므로
그대로 둔다.

## 5. 시험

- 1단계: 바뀐 이름으로 전체 시험이 같은 수로 통과, `cargo deny`, CI 작업 이름·`-p` 인자.
- `legacy::env`: 새만·옛만(경고 한 번)·둘 다.
- `layout::migrate`(임시 뿌리): 옛만 → 맞바꿈(옛 링크가 새로, 안의 파일·열린 fd 그대로), 새만 → 옛 링크 생김, 둘 다 진짜 → 그대로·경고,
  이미 맞춤 → 아무것도 안 함(몇 번 해도 같음), 설정 파일 맞바꿈, 맞바꿈 실패(EINVAL 흉내) → 자기 링크 지우고 그대로.
- 소켓 링크, `--config` 없음/옛/새, `--release-vips`가 새·옛 배치에서.
- OVN 유닛 바꾸기(가짜 systemctl), 일시 유닛 두 접두사 찾기.
- 쿠키: 로그인 응답에 넷, 옛 쿠키만으로 인증됨, 로그아웃이 넷 다 지움, OIDC 콜백이 옛 state 쿠키로도 됨.
- 패키징: `build-deb-test.sh`(새 유닛·별칭·예시 파일), `maint-test.sh`(enable 링크·덮어쓰기 옮기기, 이미 옮김, chroot, 옮긴 뒤
  reload 한 번, 되돌리기의 OVN 단위, swtpm local 규칙), `deb-test.sh`·`repo-test.sh`(옛 oplite V0와 계획 43~73의 orbivisor —
  `fixtures/orbivisor-v1` — 설치 → 새 판 업그레이드 뒤 `systemctl is-enabled orbivisord`, 유닛 파일·덮어쓰기, 새 경로가 진짜·옛
  경로가 링크) — GitHub `package` 작업. 컨테이너에서는 systemd가 돌지 않으므로 "VM 역할 프로세스가 같은 PID"와 "daemon-reload에서
  두 이름이 합쳐짐(MainPID 그대로)"은 실제 배포에서 확인한다.
- 실제: svc02 → svc01 배포를 업그레이드 길 그대로(옛 배치의 노드에서 새 바이너리·유닛) — test-vm PID 그대로, `/var/lib/opl`이 링크,
  `systemctl status orbivisord`, `journalctl -u orbivisord`, OVN 유닛 이름, 옛 쿠키 세션 유지, `OPL_SERVER`로 CLI 경고.

## 6. 알고 받아들이는 것

- `journalctl -u opld`는 이름 바꾸기 앞의 로그만 보인다(유닛의 원래 이름이 바뀌었다). 새 로그는 `-u orbivisord`. 안내에 적는다.
- 노드 위에 `opl`로 시작하는 이름이 남는다: VM 스코프, 브리지·탭, LVM·RBD 이름, nft 표, 옛 경로 링크. §1 "그대로 둠"과 안내 절이 까닭을 적는다.
- 섞인 기간에 한 브라우저에서 로그아웃 없이 사용자를 바꾸는 경우(리뷰 1 compat M2): 옛 노드는 옛 이름 쿠키만 쓰고 지운다.
  새 노드는 두 이름의 값이 다르면 더 나중의 로그인인 옛 이름을 먼저 보고 그 응답에서 새 이름 쿠키를 그 값으로 고쳐 쓰므로(리뷰 2),
  새 노드를 한 번이라도 거친 뒤에는 옛 노드에서 로그아웃해도 앞 사용자의 세션이 남지 않는다. 남는 빈틈은 로그인부터 로그아웃까지
  옛 노드만 거친 경우다 — 그때는 새 이름 쿠키에 앞 사용자의 세션이 남는다. 받아들이고 안내에 "섞인 기간에는 사용자를 바꿀 때 먼저
  로그아웃"을 적는다(한 릴리스 동안의 일).
- 옛 판으로 되돌리면 옛 데몬이 OVN 중앙 단위 글을 옛 경로로 다시 쓰고 중앙을 한 번 다시 시작한다(§4.3). postrm이 되돌리지 못하면
  안내의 손 절차.
- 옛 이름 받기(환경 변수·쿠키·CLI 로그인 파일·일시 유닛 접두사)는 다음 판에서 뺀다 — 그때 이 설계의 §1 "옛 이름" 열을 따른다. 경로 링크와
  유닛 별칭은 저장된 경로·운영 스크립트 때문에 없애지 않는다.
