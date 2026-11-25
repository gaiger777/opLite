# 구독과 텔레메트리

구독 키 발급·적용과 텔레메트리. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

## 구독과 텔레메트리

구독은 기능을 막지 않는다. 키가 없거나 만료돼도 모든 기능이 돈다. 구독 키는 지원과 `enterprise` 업데이트 채널에
대한 권리이고, 노드가 빌드에 들어 있는 공개 키로 오프라인 검증한다(어떤 서버에도 묻지 않는다).

```bash
orbivisorctl subscription show                   # state: active, customer, level, nodes: 2/3(사용/한도), expires: 2027-11-05
orbivisorctl subscription set opl1.eyJpZCI6…     # admin. 키를 넣거나 바꾸고 상태를 보인다
orbivisorctl subscription set --file key.txt     # 파일에서(끝의 줄바꿈은 다듬는다)
orbivisorctl subscription set --file - < key.txt  # 표준 입력에서. 키를 명령줄(셸 기록, ps)에 두지 않으려면 --file을 쓴다
orbivisorctl subscription remove                 # admin
orbivisorctl settings set updates.channel=enterprise  # admin. 구독이 active일 때만
```

- **키.** `opl1.<payload>.<signature>` 한 줄이다. `payload`는 JSON
  `{"id","customer","level":"standard"|"premium","nodes","issued_at","expires_at"}`(시각은 유닉스 초, 모르는 필드는
  무시)의 base64url(패딩 없음), `signature`는 `opl1.<payload>` 바이트에 대한 Ed25519 서명의 base64url이다. `id`는
  영문자·숫자·`.`·`_`·`-`로 된 1~64자(영문자나 숫자로 시작, APT 인증 파일의 로그인이 된다), `customer`는 1~200자,
  `nodes`(위트니스가 아닌 노드 수 한도)는 1 이상, `expires_at`은 `issued_at`보다 뒤이고 9999-12-31T23:59:59Z
  (`253402300799`)를 넘지 않는다.
- **상태.** `GET /api/v1/subscription`(로그인한 누구나)이 읽을 때마다 계산한다: 키가 없으면 `none`, 형식이나 서명이
  맞지 않으면 `invalid`(공개 키가 다른 빌드로 바꾼 경우), 만료됐으면 `expired`, 위트니스가 아닌 노드가 한도보다 많으면
  `over_limit`(같으면 괜찮다), 그 밖은 `active`다. 저장소의 개발 키로 검증된 키는 `development: true`이고 orbivisorctl은
  `state: active (development key)`, 웹은 "개발용 키"로 보인다. `PUT /api/v1/subscription`(`{"key":"opl1.…"}`)과
  `DELETE /api/v1/subscription`은 admin만 한다. 형식·서명이 틀리거나 이미 만료된 키는 **400**
  `invalid_subscription_key`이고 이유가 붙는다(`malformed subscription key: …`, `the subscription key signature does
  not match this build`, `the subscription key expired on 2026-01-01`). 키 문자열은 클러스터 설정
  (`Settings.spec.subscription`)에 저장하지만 API(`GET`·`PATCH /api/v1/settings`, watch)로는 내보내지 않는다. 감사
  로그(`detail` `id=<키 id>`, "감사 로그")와 orbivisord 로그에는 키 id만 남는다.
- **웹.** 설정 › 구독 탭이 상태·고객·수준·노드(사용/한도)·만료일을 보이고, admin은 키를 붙여 넣거나 지운다.
  상태가 `active`가 아니거나 만료 30일 전부터 홈 제목 아래에 안내가 보인다(`expired`·`over_limit`는 경고 색,
  그 밖은 정보 색). "7일 동안 숨기기"를 누르면 그 브라우저에서 그 안내만 7일 동안 숨긴다(예: "구독 없음"을 닫은 뒤
  구독이 만료되면 만료 안내는 보인다). 웹은 설정이 바뀌거나, 노드가 합류하거나 떠나거나, 다시 연결할 때와 한
  시간마다 상태를 다시 읽는다. 만료일은 UTC 날짜다.
- **enterprise 채널.** `updates.channel=enterprise`는 구독이 `active`일 때만 바꿀 수 있다(아니면 **400**
  `subscription_required`, 이미 enterprise여도 다시 보내면 검사한다). 채널이 enterprise이고 저장된 키의 서명이 맞으면
  노드마다 orbivisord가 소스 파일(`Components: enterprise`)과 함께 APT 인증 파일 `/etc/apt/auth.conf.d/oplite.conf`(0600)를
  쓴다:

  ```
  # managed by opld; edits are overwritten
  machine apt.example.com login <구독 id> password <키 문자열의 SHA-256 16진수>
  ```

  `machine`은 `apt_url`의 호스트(포트가 있으면 `호스트:포트`)다. 프로토콜 없는 `machine` 항목을 apt는 https에만 쓰므로
  `apt_url`이 `http://`면 인증이 가지 않는다. 다른 채널로 바꾸거나 키를 지우면 orbivisord가 쓴 파일을 지운다(첫 줄의 표시가
  없는 파일은 소스 파일처럼 건드리지 않고 경고만). 구독이 만료되거나 노드 수를 넘어도 채널과 인증 파일은 그대로다.
  저장소가 거절하면(401 등) `node updates`의 확인 오류에 보인다. 저장소 쪽에서 인증을 확인하는 일(예: 웹 서버가
  사용자 이름을 구독 id로, 비밀번호를 발급한 키의 SHA-256으로 확인하고 만료된 구독을 거절)은 Orbivisor 밖이다.
- **옛 노드.** 구독 넣기·지우기, enterprise 채널, 텔레메트리 바꾸기(`reset-id` 포함)는 모든 노드(위트니스 포함)가 이 orbivisord 빌드와
  같거나 새 버전일 때만 된다. 아니면 **409** `cluster_upgrading`("some nodes run an older Orbivisor (node3); …"):
  옛 orbivisord는 이 새 값이 든 설정을 풀지 못한다. 롤링 업그레이드를 끝낸 뒤 쓴다.

**키 발급.** 발급 도구 `orbivisor-license`는 패키지에 넣지 않는다. 발급하는 쪽에서 저장소를 빌드해 쓴다.

```bash
cargo build --release -p orbivisor-api --bin orbivisor-license
# 발급 키 쌍(한 번). 비밀 키(PKCS#8 base64 한 줄)는 --out 파일(0600, 있으면 거절)에, 공개 키 base64는 표준 출력에
target/release/orbivisor-license keygen --out license.key
# 키 발급: 표준 출력에 opl1.… 한 줄(--issued-at <유닉스 초>로 발급 시각을 정할 수 있다. 기본은 지금)
target/release/orbivisor-license issue --key license.key --id acme-2026 --customer 'Acme Corp' \
    --level premium --nodes 3 --days 365
# 비밀 키 파일의 공개 키(ORBIVISOR_LICENSE_KEYS의 항목)를 다시 보기
target/release/orbivisor-license pubkey --key license.key
```

`issue`는 `--nodes`·`--days`가 1 이상인지 먼저 보고("orbivisor-license: --days must be at least 1"), 내용이 키 규칙에 맞지
않으면 "orbivisor-license: invalid license: id must be …"처럼 멈춘다. `keygen`은 쓰다가 실패하면 만든 파일을 지운다.

빌드 값 `ORBIVISOR_LICENSE_KEYS`(base64 공개 키를 쉼표로)를 주고 빌드한 orbivisord만 그 키로 서명한 구독 키를 믿는다. 주지
않으면 저장소의 개발 키(`crates/orbivisor-types/keys/dev-license.pub`) 하나를 믿는데, 그 비밀 키가 시험 견본
(`crates/orbivisor-types/tests/fixtures/dev-license.key`)으로 저장소에 있어 누구나 키를 만들 수 있다. 그래서 정식 패키지는
반드시 `ORBIVISOR_LICENSE_KEYS`를 주고(위 "저장소 만들기"처럼 `cargo build`와 `build-deb.sh`에 함께), `build-deb.sh`는 이 값이
없으면 `ORBIVISOR_DEV_LICENSE=1`(개발 패키지, CI의 패키지가 이렇다)이 아닌 한 "build-deb: ORBIVISOR_LICENSE_KEYS is not set; …"로
멈춘다. 값이 있으면 세 가지를 더 본다: 쉼표로 나눈 항목마다 정규 base64(44자, `=` 끝)로 푼 32바이트 Ed25519 공개
키인지("build-deb: ORBIVISOR_LICENSE_KEYS entry is not a base64 Ed25519 public key: …")와 그런 항목이 하나 이상인지(공백·쉼표뿐이면
"build-deb: ORBIVISOR_LICENSE_KEYS has no keys"), 개발 키가 들어 있지 않은지("… contains the development
key; use ORBIVISOR_DEV_LICENSE=1 for a development package"), orbivisord 바이너리에 그 값이 그대로 들어 있는지(같은 값으로
빌드했는지, "build-deb: orbivisord was not built with this ORBIVISOR_LICENSE_KEYS"). 그래도 개발 키를 넣고 빌드한 바이너리는 그
키로 검증한 구독을 개발용으로 보인다. 빌드 값이 공백뿐이면 없는 것과 같지만(개발 키), 쉼표처럼 공백이 아닌 글자가 있는데
풀리는 키가 없으면 개발 키로 떨어지지 않고 어떤 구독 키도 믿지 않는다. 발급 키를
바꿀 때는 옛 공개 키와 새 공개 키를 함께 넣으면 이미 낸 키도 계속 풀린다. 이 키를 믿지 않는 빌드로 올리면 저장된 키는
`invalid`가 된다.

**텔레메트리.** 기본으로 꺼져 있다. 켜면 클러스터 리더가 하루에 한 번 사용 보고(수와 버전만)를 보낸다.

```bash
orbivisorctl telemetry preview                     # admin. 지금 보낼 JSON 그대로(꺼져 있어도)
orbivisorctl telemetry enable                      # admin. 저장된 끝점, 없으면 빌드 기본 끝점(ORBIVISOR_TELEMETRY_URL)으로
orbivisorctl telemetry enable --endpoint https://telemetry.example.com/v1/report
orbivisorctl telemetry show                        # enabled, endpoint, id, last sent, last error
orbivisorctl telemetry disable
orbivisorctl telemetry reset-id                    # 보고 id를 새로 만든다
```

- **설정.** `PATCH /api/v1/settings`의 `{"telemetry":{"enabled":true,"endpoint":"https://…"}}`(admin)다. 끝점은
  호스트가 있는 `https://` 주소여야 하고 사용자 정보(`user:pass@`)를 넣을 수 없다(설정은 로그인한 누구나 읽는다.
  아니면 **400** `invalid_request`). `""`를 보내면 지워 빌드 값
  `ORBIVISOR_TELEMETRY_URL`을 쓴다. 켰는데 끝점도 빌드 값도 없으면 **400** `telemetry_endpoint_required`. 처음 켤 때 무작위
  UUID v4 보고 id를 만든다(클러스터 이름·CA와 관계없다). 꺼도 id는 남고, `POST /api/v1/telemetry/reset-id`(admin)가
  새로 만든다. 웹 설정 › 일반의 텔레메트리 칸은 "켜기…"를 누르면 보낼 JSON(`GET /api/v1/telemetry/preview`, admin)을 대화상자로
  보이고 확인해야 켠다(끝점 입력란에 저장하지 않은 값이 있으면 함께 보낸다).
- **보내기.** 리더가 `[telemetry] tick`마다 확인해, 켜져 있고 마지막 전송(`status.telemetry.last_sent_at`)에서
  `interval`이 지났으면 끝점에 JSON을 `POST`한다(시간 제한 30초, https만, 리다이렉트는 따르지 않는다). 2xx면
  `last_sent_at`을 적고 `last_error`를 지운다. 아니면 `last_error`("HTTP 500 Internal Server Error", 3xx면 "HTTP 307
  Temporary Redirect" 같은 상태, 또는 연결 오류)를 적고 `retry` 뒤에 다시 보낸다. 켜져 있는데 설정 끝점도 빌드 기본
  끝점도 없으면(다른 빌드가 켠 경우) `last_error`가 "no telemetry endpoint: set telemetry.endpoint"다. 마지막 전송
  시각은 Raft에 있어 리더가 바뀌어도 `interval` 전에는 다시 보내지 않는다. 보낸 뒤 그 기록을 쓰지 못해도 같은 리더는
  메모리의 전송 시각으로 다시 보내지 않는다. 다만 다시 보낼 시각과 그 전송 시각은 리더의 메모리에 있어, 실패 뒤
  리더가 바뀌면 새 리더가 곧 다시 보낼 수 있고, 보낸 뒤 기록하기 전에 리더가 바뀌면 새 리더가 한 번 더 보낼 수 있다.
  이 빌드보다 오래된 노드가 남아 있으면 보내지 않는다.
- **프록시.** reqwest가 `HTTPS_PROXY` 환경 변수를 따른다. 패키지의 orbivisord는 systemd 서비스 `orbivisord`이므로
  `systemctl edit orbivisord`로 덮어쓰기 파일에 다음을 넣고 `systemctl restart orbivisord` 한다:

  ```ini
  [Service]
  Environment=HTTPS_PROXY=http://proxy.example.com:3128
  ```

- **보내는 내용**(`schema` 1). 이름, 주소, MAC, 키, 사용자, 고객 이름은 넣지 않는다:

  ```json
  {
    "schema": 1,
    "id": "6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
    "sent_at": 1793923200,
    "versions": { "0.2.0": 3 },
    "nodes": { "total": 3, "witness": 1 },
    "arch": { "x86_64": 2 },
    "hypervisor": { "qemu-kvm": 2 },
    "cpus": 64,
    "memory_gib": 251,
    "vms": { "total": 40, "running": 31, "ha": 6 },
    "pools": { "dir": 1, "nfs": 1, "rbd": 1 },
    "networks": { "bridge": 1, "vlan": 2, "vxlan": 1 },
    "security_groups": 3,
    "backup_jobs": 2,
    "imports": { "esxi": 5, "ova": 1 },
    "subscription": "premium"
  }
  ```

  `versions`는 버전별 노드 수(위트니스 포함, 버전을 모르면 `unknown`), `arch`·`hypervisor`·`cpus`·`memory_gib`(MiB 합을
  GiB로 내림)는 위트니스를 뺀 노드, `vms.running`은 지금 도는 VM, `vms.ha`는 HA를 켠 VM이다. `pools`는 백엔드 종류
  (`dir`, `nfs`, `lvm-thin`, `zfs`, `rbd`, `lvm-shared`), `networks`는 VXLAN이면 `vxlan`, 아니고 VLAN 태그가 있으면
  `vlan`, 그 밖은 `bridge`, `imports`는 가져오기의 원본 종류(`ova`, `esxi`)별 수다. `subscription`은 서명이 맞는
  구독 키의 수준(만료돼도), 키가 없거나 서명이 맞지 않으면 `none`이다.

설정(노드마다, 생략하면 아래 기본값):

```toml
[telemetry]
tick = "60s"         # 리더가 보낼 때가 됐는지 보는 간격
interval = "24h"     # 보고 간격
retry = "1h"         # 보내지 못한 뒤 다시 보내기까지
# ca_file = "/etc/orbivisor/telemetry-ca.pem"   # 시스템 루트에 더해 믿는 CA 인증서(PEM, 여럿 가능). 사설 수집 서버에 쓴다.
#                                         # 읽지 못하거나 인증서가 없으면 orbivisord가 시작하지 않는다
```

수집 서버는 Orbivisor 밖이다. 끝점의 기본값은 빌드할 때 `ORBIVISOR_TELEMETRY_URL`로 넣는다(없으면 기본 끝점이 없다).
