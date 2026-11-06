# opLite 엔터프라이즈 기능 설계 (하위 프로젝트 10: 계획 34~38)

작성일: 2026-11-06
상위 문서: `2026-10-20-opl-node-core-design.md`(하위 프로젝트 분해 §1.1), `2026-10-28-api-security-design.md`(인증·시도 제한·세션 세대), `2026-11-04-backup-design.md`(백업·백업 작업), `2026-11-05-install-update-subscription-design.md`(설정·`require_current` 게이트)

## 1. 목표와 범위

처음 분해에서 하위 프로젝트 10은 엔터프라이즈 기능이다: LDAP/SSO, 감사, 다중 클러스터 관리, 고급 DR(Proxmox의 realm·Datacenter Manager, vCenter의 이벤트·SSO·Linked Mode·Site Recovery). 지금 opLite는 로컬 사용자만 로그인하고, 누가 무엇을 했는지 남지 않으며, 클러스터 하나만 보고, 백업은 그 클러스터의 풀에만 있다.

| 계획 | 내용 |
|---|---|
| 34 | 감사 로그: 노드마다 그 노드가 처리한 바꾸는 요청·로그인·콘솔 열기를 로컬 파일(JSON Lines, 회전)에 적고 journald에도 보냄, 모든 노드를 모아 읽는 `GET /audit`(노드 사이 RPC), oplctl·웹 |
| 35 | 인증 영역과 LDAP: 새 리소스 `Realm`(종류 `ldap`), 로그인에 영역 고르기, LDAP 바인드·사용자 찾기·그룹으로 역할 정하기, 외부 사용자는 첫 로그인에 `User`를 만듦(`alice@corp`), 시험용 가짜 LDAP 서버와 CI의 실제 slapd 시험, oplctl·웹 |
| 36 | OIDC SSO: `Realm` 종류 `oidc`, 인가 코드 흐름(PKCE·state·nonce, 상태는 브라우저 쿠키라 어느 노드로 돌아와도 됨), ID 토큰 서명 확인(RS256·ES256, ring), 클레임으로 역할, 시험용 가짜 IdP, 웹 로그인 버튼 |
| 37 | 원격 클러스터: 새 리소스 `Remote`(주소·CA 지문·API 토큰), 리더의 상태 수집(버전·노드·VM 수), 원격 API 중계(`/remotes/{key}/api/...`, 읽기는 누구나·바꾸기는 admin), 웹 "원격 클러스터" 화면(요약과 VM 목록·전원), oplctl |
| 38 | 원격 복제(DR): 백업 작업의 `replicate`(원격 클러스터와 그 백업 풀), 백업이 끝나면 `backup-replicate` Task가 원격의 받기 API로 파일을 보냄(SHA-256 확인), 원격 사본은 그 클러스터의 보통 `Backup`(출처 표시)이라 거기서 복원, 보존 규칙으로 지울 때 원격 사본도 지움, 수동 복제, oplctl·웹 |

넣지 않는 것: 감사 로그의 변조 방지(해시 체인·서명)와 원격 syslog 직접 전송(journald로 보낸 것을 관리자가 전달), 요청 본문 기록, SAML, Kerberos·SPNEGO, LDAP 사용자·그룹 동기화(로그인할 때만 봄), 세분화된 RBAC(역할은 그대로 셋), 외부 사용자의 2단계 인증(IdP가 맡음), oplctl의 SSO 로그인(웹에서 API 토큰을 만들어 씀), 원격 클러스터의 전체 웹 화면·콘솔 중계·클러스터 사이 라이브 마이그레이션, 디스크 단위 연속 복제(낮은 RPO), 자동 장애 조치와 복구 계획 실행, 복제의 대역폭 제한·증분 전송.

### 견준 길

- **감사 로그 저장: 노드 로컬 파일(채택) vs Raft 리소스.** Raft에 두면 한 곳에 순서대로 모이지만, 로그인 실패마다 Raft에 쓰면 공격이 곧 쓰기 부하이고(계획 13이 같은 이유로 시도 제한을 노드마다 셈), 스냅샷이 모든 객체를 JSON 하나로 만들어 항목이 쌓이면 무거워지며, 정족수를 잃은 동안의 요청은 남지 않는다. 노드 로컬 파일은 이 셋이 없고, 읽을 때 노드 사이 RPC로 모은다. 꺼진 노드의 기록은 그 노드가 돌아와야 보인다(한계로 적음).
- **감사 지점: 라우터 미들웨어(채택) vs 핸들러마다 기록.** 미들웨어 하나가 모든 바꾸는 요청을 빠짐없이 적는다(새 API가 기록을 잊을 수 없음). 누가 했는지는 인증 추출기가 요청마다 둔 칸에 적어 미들웨어가 읽는다.
- **외부 사용자: 첫 로그인에 `User` 리소스를 만듦(채택) vs 세션에만 둠.** API 토큰, 세션 세대, 권한 확인이 모두 `User` id를 쓴다. 리소스를 만들면 이 경로를 그대로 쓰고, 관리자가 사용자 목록에서 외부 사용자를 보고 지울 수 있다. 역할은 로그인할 때마다 그룹·클레임으로 다시 정한다.
- **OIDC 구현: reqwest와 ring으로 직접(채택) vs `openidconnect` 크레이트.** 필요한 것은 discovery, 인가 코드 교환, JWKS로 RS256·ES256 서명 확인뿐이다. ring은 이미 의존성에 있다. `openidconnect`는 의존성이 많고 구조가 커서 얻는 것보다 다루는 비용이 크다.
- **LDAP: `ldap3` 크레이트(채택).** MIT/Apache, tokio 기반, StartTLS·LDAPS를 지원한다. 시험의 가짜 서버는 같은 프로젝트의 `lber`(BER 인코딩)로 짧게 만든다. 실제 OpenLDAP(slapd) 시험은 CI Linux에서만 기능 `ldap-it`로.
- **다중 클러스터: 각 클러스터에 원격을 등록하는 대등 구조(채택) vs 별도 관리 서버.** 따로 설치할 것이 없고 어느 클러스터에서도 다른 클러스터를 본다. 원격 쪽은 그 클러스터의 보통 API 토큰으로 권한을 정하므로 원격 클러스터에 새 개념이 필요 없다.
- **DR: 백업을 원격 클러스터로 복제(채택) vs 공유 NFS의 백업 받아들이기 vs 디스크 연속 복제.** 백업(계획 27·28)과 원격(계획 37)을 그대로 이어 쓰고, 원격 사본이 그 클러스터의 보통 백업이라 복원도 이미 있는 기능이다. 공유 NFS는 두 사이트가 같은 저장소를 보는 경우만 되고, 연속 복제는 스토리지 백엔드마다 다른 큰 일이다.

## 2. 감사 로그 (계획 34)

### 2.1 무엇을 적나

- 모든 `POST`·`PUT`·`PATCH`·`DELETE` API 요청(성공·실패 모두, 인증 실패 401·403 포함), `GET`의 VM 콘솔 열기(`/vms/{key}/console`), 로그인 성공·실패, 로그아웃.
- 적지 않는 것: 다른 `GET`, `/auth/whoami`, `/watch`, 웹 정적 파일, 노드 사이 RPC(사람의 요청이 아님).
- 항목(JSON 한 줄, `AuditEntry`):
  - `id`(UUID v7), `time`(유닉스 밀리초), `node`(노드 이름)
  - `user`(이름, 없으면 null — 인증 전·실패), `user_id`, `role`, `auth`(`session`·`token`·`none`)
  - `ip`(계획 13의 클라이언트 IP), `method`, `path`(쿼리 제외 — 쿼리에 비밀이 올 수 있음), `status`, `duration_ms`
  - `target`: 경로에서 읽은 `{ kind, key }`(예: `/vms/web-1/actions/start` → `vm`, `web-1`), 없으면 null
  - `action`: 경로 패턴 이름(예: `vm.start`, `user.create`, `auth.login`) — 라우트 표에서 정함
  - `detail`: 핸들러가 남긴 짧은 문자열(선택). 로그인은 시도한 사용자 이름과 영역, 구독 `PUT`은 키 id. 비밀(비밀번호·키·토큰)은 넣지 않는다.
- 요청 본문은 적지 않는다.

### 2.2 어떻게 적나

- `opl_api::router`에 미들웨어 `audit::layer`(axum `from_fn_with_state`). 요청 확장에 빈 칸 `AuditSlot`(`Arc<Mutex<..>>`)을 넣고 다음으로 넘긴 뒤, 응답 상태와 칸의 주체·`detail`로 항목을 만든다. 인증 추출기(`Principal`)는 확장에 칸이 있으면 주체와 인증 방식을 적는다. 로그인 핸들러는 성공하면 그 사용자를 칸에 적는다.
- 쓰기는 `AuditLog`(노드에 하나, `AppState`가 가짐)의 채널로 넘겨 별도 태스크가 파일에 덧붙인다(요청이 디스크를 기다리지 않음). 채널이 가득 차면(1만 개) 버리고 버린 수를 세어 다음 항목 앞에 `audit.dropped` 항목을 적는다.
- 파일: `{data_dir}/audit/audit.log`, 한 줄에 한 항목, 0600. 16 MiB를 넘으면 `audit.log.1`… 로 밀고 8개를 넘는 것은 지운다(opld 설정 `[audit] max_file_size`, `max_files`). 쓸 때마다 fsync하지 않고 1초마다 묶어서 한다.
- 같은 항목을 `tracing` 대상 `opl_audit`로도 내보낸다(info). systemd 아래에서는 journald로 가고, 관리자가 rsyslog 등으로 밖에 보낼 수 있다(README에 방법).
- opld 설정 `[audit] enabled`(기본 true). 끄면 파일도 tracing도 없다.

### 2.3 읽기

- `GET /api/v1/audit?since&until&user&node&action&limit`(admin). `since`·`until`은 RFC 3339 또는 유닉스 밀리초, 기본은 최근 24시간, `limit` 기본 200·최대 5000.
- 받은 노드가 노드 사이 RPC `ReadAudit`(필터와 한도)로 위트니스를 포함한 모든 멤버에게 묻고(자기는 직접), 시간 내림차순으로 합쳐 `limit`만큼 돌려준다. 각 노드는 회전한 파일까지 거꾸로 읽어 조건에 맞는 최신 `limit`개만 보낸다. 노드마다 5초 제한, 닿지 않은 노드는 응답 `unreachable: [이름]`에 넣는다. 응답 `{ entries, unreachable, truncated }`.
- 감사 로그 읽기 자체는 적지 않는다(GET).

### 2.4 oplctl과 웹

- `oplctl audit [--since 1h|<시각>] [--until] [--user] [--node] [--action] [--limit] [-o json]`: 표(시각, 노드, 사용자, IP, 동작, 대상, 상태).
- 웹 `/audit`(admin만 메뉴에 보임): 시간 범위(1시간·24시간·7일), 사용자·동작 거르기, 표, 닿지 않은 노드 경고, 행을 누르면 항목 JSON.

### 2.5 시험

- 단위: 경로 → `action`·`target` 표, 회전(크기·개수), 거꾸로 읽기와 거르기, 넘친 채널의 `audit.dropped`.
- API: 성공·403·401 요청이 주체와 함께 적힘, 로그인 실패에 사용자 이름이 `detail`에 있고 비밀번호는 어디에도 없음, 토큰 요청의 `auth: token`, GET은 적지 않음, `GET /audit` 권한과 거르기, 두 노드(`two_nodes.rs`)에서 합치기와 닿지 않는 노드.
- opld E2E: 실제 opld 둘에서 각 노드에 바꾸는 요청 → 한 노드의 `oplctl audit`이 둘을 모두 보임, 파일 권한 0600.
- 웹 Vitest: 표, 거르기, 경고, 권한에 따른 메뉴.

### 2.6 구현(계획 34)

- 타입(`opl_types::audit`): `AuditEntry`(§2.1의 필드), `AuditAuth{Session, Token, None}`(snake_case), `AuditTarget{kind, key}`, `AuditQuery{since, until, user, action, limit}`(`MAX_LIMIT` 5000, `matches`: 시각은 양 끝 포함, `user`는 정확히 같음), `AuditResponse{entries, unreachable, disabled, truncated}`. 설계와 다른 점:
  - `action`은 라우트 표의 이름(`vm.start`)이 아니라 `"<METHOD> <라우트 템플릿>"`(axum `MatchedPath`에서 `/api/v1`을 뗀 것, 예: `POST /vms/{key}/actions/{action}`, `POST /auth/login`)이다. 표를 따로 두지 않아 새 API가 저절로 이름을 가진다. VM 동작은 템플릿이 `/vms/{key}/actions/{action}` 하나라 `action`만으로는 가를 수 없으므로 `AuditQuery`의 `action` 거르기는 `action` **또는 `path`**의 부분 문자열이다.
  - `target`은 템플릿의 첫 조각(복수형 그대로 `vms`)과 템플릿의 첫 `{…}` 자리의 실제 경로 조각이다. 매개변수가 없는 템플릿은 None.
  - 넘친 기록은 `action: "audit.dropped"`, `detail: "<N> entries dropped"`, method·path·ip 빈 문자열, status 0.
  - (설계에 없음) 인증 없는 항목(`user` None — 인증 실패·로그인 실패·익명)은 IP마다 1분 창에 60개까지만 적는다. IP는 IPv4면 주소, IPv6면 /64 네트워크로 센다(계획 13 `limit::counted_ip`와 같은 규칙, IPv4-mapped는 IPv4). 넘친 것은 그 IP의 카운터로 세고, 창이 끝나면 쓰기 스레드의 1초 틱이 끝난 창을 훑어 `action: "audit.suppressed"`, `ip`(세는 단위 — IPv6는 `<네트워크>/64`), `detail: "<N> unauthenticated requests not recorded since <창 시작 RFC 3339>"`(`time`은 적는 시각), method·path 빈 문자열, status 0 항목을 적고 그 기록을 지운다(넘친 것이 없는 끝난 창도 지움). 그래서 넘친 수는 창이 끝나고 1초 안에 적힌다. 훑기 전에 그 IP가 다시 오면 그 항목 앞에, `shutdown` 때는 남은 것을 모두 적는다. 기록표는 IP 1만 개까지이고(끝난 창이 곧 지워지므로 1분 안에 서로 다른 IP 1만 개가 와야 닿음) 넘치면 가장 오래된 창을 버린다(그 카운터가 0이 아니면 그 `audit.suppressed`를 바로 적음). 가장 오래된 창과 끝난 창은 `(창 시작, 키)` `BTreeSet` 색인으로 찾아 표 전체를 훑지 않는다. 인증된 항목은 한도가 없다. 인증 없는 요청이 쏟아져도 파일이 회전으로 밀려 나가지 않게 하려는 것이다.
  - (설계에 없음) `path`·`target.key`·`detail`·`user`는 각각 512바이트까지만 적는다(UTF-8 글자 경계에서 자르고 끝에 `…`).
- 쓰기(`opl_api::audit`, `AuditConfig{dir, node, max_file_size, max_files}`·`AuditLog`): `disabled()`(아무것도 적지 않고 읽으면 빈 목록), `open(cfg)`(디렉터리 0700), `record`(동기 `try_send`, `node`는 설정의 노드 이름으로 덮음, 필드 자르기와 인증 없는 항목의 IP별 한도 — 시계는 `Instant`, 시험은 비공개 `record_at`), `read`(`spawn_blocking`), `shutdown`(채널에 남은 것과 남은 `audit.suppressed`를 쓰고 fsync, 5초 안에 끝나지 않으면 `warn!`하고 돌아감 — 종료 메시지는 막히는 `send` 대신 `try_send`와 10 ms async sleep을 마감까지 되풀이해 넘기므로 시간이 지나도 막힌 blocking 태스크가 남지 않는다), `is_enabled`. 쓰기 스레드가 끝난 뒤의 `record`는 처음 한 번 `warn!`하고 그 뒤는 버린 수로 센다. 설계와 다른 점: 쓰기는 tokio 태스크가 아니라 전용 스레드 `opl-audit`(std `sync_channel` 1만)다 — 파일 쓰기·fsync가 동기라서. `open`은 이미 있던 디렉터리도 0700으로 맞추고, `audit.log`를 `O_NOFOLLOW`로 열어(심볼릭 링크면 실패 — opld 시작 오류) 0600으로 맞추며(`libc`를 opl-api의 직접 의존으로), `max_files` 이상의 옛 `audit.log.N`(`max_files`를 줄인 경우)을 지운다. 파일 `audit.log`는 0600, 쓰기 전 크기 + 줄 길이가 한도를 넘으면(빈 파일이 아닐 때) 회전(`.{max_files-1}` 지우고 `.N` → `.N+1`, `audit.log` → `.1`), 쓴 것이 있으면 1초마다와 회전·종료 때 `sync_data`. 쓰기 실패는 `warn!`만 하고 다음 쓰기에서 파일을 다시 연다. tracing `opl_audit`(info, 메시지는 JSON) 줄은 파일에 쓸 때 낸다(버린 항목은 journald에도 없다). 읽기는 새 파일부터 통째로 읽어 줄을 거꾸로 훑고(망가진 줄 건너뜀), 파일의 가장 새 줄이 `since` 앞이면 멈추고 가장 오래된 줄이 `until` 뒤면 그 파일을 건너뛰며, `limit + 1`개까지 모은다(합칠 때 `truncated` 판단). 읽는 중에 회전하면 같은 항목이 두 파일에서 보일 수 있어 `id`로 중복을 없앤다.
- 기록 지점: `router()`가 `api()`에 `route_layer(from_fn_with_state(state, audit::layer))`를 붙인다(라우트가 맞은 요청만 — 맞는 라우트가 없는 404와 `/openapi.json`, opld의 제어 소켓·웹 정적 파일은 적지 않음). 메서드가 GET·HEAD·OPTIONS가 아니거나 템플릿이 `/vms/{key}/console`이면 요청 확장에 `AuditSlot`을 넣고 응답 뒤 항목을 만든다(`ip`는 `ClientIp`의 `to_canonical`, `path`는 쿼리 없는 `uri.path()`에서 `/api/v1`을 뗀 것). `Principal` 추출기가 `audit::note_auth`로 주체와 auth를 적는다 — `Authorization: Bearer `가 있으면 인증에 실패해도 늘 `token`. 핸들러는 추출기 `AuditNote`(실패 없음, 미들웨어 없으면 아무 일 안 함)의 `.detail(s)`·`.principal(p, auth)`로 남긴다: 로그인은 이름을 확인하기 전에 `username=<이름>`(이름 형식이 아니면 `username=<invalid>`)을 적어 401·429에도 있고 성공하면 그 사용자와 `session`, 로그아웃은 세션을 지우기 전에 그 사용자, 구독 `PUT`은 성공하면 `id=<키 id>`. 설계와 다른 점: 로그인 `detail`에 영역은 없다(영역은 계획 35).
- 읽기 API: `GET /api/v1/audit`(`audit::list_audit`, `AuditParams{since, until, user, node, action, limit}` `deny_unknown_fields`). 거절 순서 401 → 403(admin 아님, `ApiError::forbidden`) → 400 `invalid_request`(시각이 밀리초도 RFC 3339도 아님, `limit`이 1~5000 밖). `since > until`은 빈 목록. 설계와 다른 점: 위트니스는 API가 없어 감사 로그도 없으므로 `NodeSpec.witness`인 멤버는 묻지 않는다(`unreachable`에도 넣지 않음). 이름은 Node 리소스의 이름, 없으면 노드 id 문자열. 자기 노드는 `state.audit.read`(실패하면 `unreachable`), 다른 노드는 `Peers::read_audit`를 노드마다 5초 제한으로 동시에. 오류·제한 시간·`peers` 없음·`ReadAudit`이 없는 옛 opld(unimplemented)는 `unreachable`(정렬), 감사 로그를 끈 노드는 응답의 `disabled`(정렬 — 설계에 없음)다: 이 노드는 `AuditLog::is_enabled`로, 다른 노드는 `ReadAudit`의 `failed_precondition`으로 안다. 합친 뒤 `(time, id)` 내림차순, `limit`을 넘으면 자르고 `truncated`.
- 노드 사이 RPC: `build.rs`의 JSON 메서드 `ReadAudit`(`AuditQuery` → `opl_peer::AuditReply{entries}`), 트레이트 `opl_peer::AuditSource`(async_trait, `MigrationReceiver` 모양, `Result<Vec<AuditEntry>, String>`), `PeerService::with_audit(Arc<dyn AuditSource>)`(없으면 `Status::unimplemented`, `AuditSource::enabled`(기본 참)가 거짓이면 `Status::failed_precondition("audit log disabled")`, 받는 쪽이 `limit`을 5000으로 자름), `Peers::read_audit(addr, query, timeout)`(꺼진 노드면 `Ok(None)`). opld는 끈 노드에도 `AuditLog::disabled`를 넘긴다. `AuditLog`가 `AuditSource`를 구현한다.
- opld `[audit]`(`AuditSection`): 설계의 `max_file_size` 대신 `max_file_mib`(기본 16)이고 `enabled`(true)·`max_files`(8). `max_file_mib`가 0이거나 256을 넘거나(읽기가 파일을 통째로 메모리에 올림) `max_files`가 0이면 시작 오류, 모르는 키도 오류(`deny_unknown_fields`). `audit_config(data_dir, node)`가 `{data_dir}/audit`의 설정(꺼졌으면 None). `member.rs`는 감사 로그를 `PeerService`보다 먼저 열어(열지 못하면 시작 오류) `AppState.audit`과 `PeerService::with_audit`에 같은 `Arc<AuditLog>`를 넘기고, 종료 때 API를 멈춘 뒤 `shutdown`한다. opld의 tracing 출력은 stdout이 터미널일 때만 색 코드를 넣는다(journald로 가는 `opl_audit` 줄을 그대로 전달할 수 있게).
- oplctl `audit [--since] [--until] [--user] [--node] [--action] [--limit]`: 준 거르기만 쿼리에 넣고(없으면 서버 기본 24시간·200), `--since`의 `90s`·`30m`·`1h`·`7d`는 클라이언트가 지금에서 뺀 밀리초로, 그 밖(RFC 3339·밀리초)은 그대로 보내 서버가 푼다. 기간 `--since`와 `--until`을 함께 주면 보내지 않고 오류(`a relative --since cannot be combined with --until; give a time`). 표 `TIME`(UTC `YYYY-MM-DDTHH:MM:SSZ`)·`NODE`·`USER`·`IP`·`ACTION`·`TARGET`(`kind/key`, 없으면 `detail`, 둘 다 없으면 `-`)·`STATUS`, 표 뒤 stderr에 `warning: no audit records from: …`, `note: the audit log is disabled on: …`, `note: more entries match; narrow the range or raise --limit`, `-o json`은 응답 그대로이고 stderr에 쓰지 않는다.
- 웹: `web/src/audit/AuditPage.tsx`, 라우트 `/audit`, 메뉴 항목의 `admin?: boolean`(admin에게만 보임 — 다른 역할이 주소로 오면 읽지 않고 "관리자만" 안내), i18n `nav.audit`·`audit.*`. 범위는 `since`만 보내(읽을 때의 지금에서 뺀 밀리초) 새로고침이 창을 앞으로 민다. 사용자·동작은 적용을 눌러야, 다듬어 비어 있지 않을 때만 보낸다. 노드 거르기는 없다. 대상 열은 target이 없으면 detail, `disabled`는 정보 색 안내(`audit.disabled`), 데이터가 있는데 다시 읽기가 실패하면 표 위에 오류(`audit.refreshFailed`).
- 시험: opl-types 단위(`matches`의 범위 경계·사용자·action·path 부분 문자열), `opl_api::audit` 단위(쓰고 읽기, 0600·0700, 크기·개수 회전과 가장 오래된 것 지움, 새 것부터·거르기·`limit + 1`, 망가진 줄, 파일 순서로 멈춤, 비공개 `AuditLog::start(cfg, capacity, gate)`로 채운 채널 뒤 `audit.dropped`, disabled, `target`, 필드 512바이트 자르기, 인증 없는 항목의 IP별 한도와 `audit.suppressed`, 새 요청 없이 끝난 창의 수가 1초 틱에 적힘(61초 전 시계), 훑기는 끝난 창만, IPv6 /64, 1만 IP에서 가장 오래된 창 축출, 두 파일의 같은 `id` 한 번, `max_files` 넘는 옛 파일 지움, 0700·0600으로 맞추기와 링크 거절, 쓰기 스레드가 막힌 채 `shutdown`이 마감 뒤 돌아오고 런타임을 떨어뜨려도 끝남), `opl-api/tests/audit.rs`(시험 `Env`는 늘 `{dir}/audit`에 노드 "node1"·1 MiB·2개의 감사 로그: 주체·auth·템플릿·target·status, 403·401·틀린 토큰, 로그인 성공·실패·`<invalid>`와 비밀번호가 파일에 없음, 로그아웃, GET은 안 적고 콘솔 GET은 적음, 쿼리가 path에 없음, 구독 `id=`, `GET /audit`의 권한·시각·limit 경계·거르기·truncated, 감사 로그를 끈 이 노드는 `disabled`), `opl-peer/tests/grpc.rs`(`ReadAudit`·unimplemented·limit 자르기), `two_nodes.rs`(두 노드 합치기, 한 노드를 멈춘 뒤 `unreachable`, 감사 로그를 끈 다른 노드는 `unreachable`이 아니라 `disabled`), opld 설정 단위(`max_file_mib` 256·257), oplctl `fake_api`(`disabled` 안내, TARGET의 detail, 기간 `--since`와 `--until`을 함께 주면 오류), opld E2E `crates/opld/tests/audit.rs`(노드 셋 중 `[audit] enabled = false` 하나 — 노드마다 바꾸는 요청 뒤 한 노드의 `oplctl -o json audit`가 두 노드 기록을 함께 보이고 `unreachable` 없음, `--node`, 표, `audit.log` 0600, 꺼진 노드는 디렉터리 없고 `disabled`에 있음), 웹 Vitest(`AuditPage.test.tsx` — 다른 역할은 요청 없음, 데이터가 있을 때 다시 읽기 오류, `disabled` 안내, 대상 열 detail — 와 메뉴).

## 3. 인증 영역과 LDAP (계획 35)

### 3.1 영역

- 새 리소스 `Realm`(이름 = 영역 이름, 소문자 이름 규칙, `local`은 예약). spec은 종류별 enum `RealmSpec { display: Option<String>, backend: RealmBackend, roles: RoleMapping }`, `RealmBackend::Ldap(LdapRealm)`(계획 35), `RealmBackend::Oidc(OidcRealm)`(계획 36). 비밀(`bind_password`, `client_secret`)은 API 응답·watch에서 지운다(설정의 구독 키와 같은 방식, `has_bind_password: true`로만 보임).
- `RoleMapping { groups: Vec<{ group: String, role: Role }>, default: Option<Role> }`: 사용자의 그룹 가운데 맞는 것들 중 가장 높은 역할(admin > operator > viewer), 없으면 `default`, 그것도 없으면 로그인 거절(403 `no_role`).
- `Realm`을 만들고 고치고 지우는 것은 admin. 새 Kind라 쓰기 전에 `require_current`(옛 노드는 새 Kind의 명령을 풀지 못함).
- `GET /api/v1/auth/realms`(인증 없음): 로그인 화면용 `[{ name, display, kind }]`, 맨 앞에 `local`.

### 3.2 외부 사용자

- 이름은 `<사용자>@<영역>`(사용자 부분은 소문자로 바꾼 1~64자 `[a-z0-9._-]`, 이것이 아니면 거절). `validate_name`을 쓰는 로컬 사용자 만들기 API는 그대로이고, 외부 사용자 이름은 `validate_external_name`.
- `UserSpec.realm: Option<String>`(None이면 직렬화하지 않음), 외부 사용자의 `password_hash`는 빈 문자열이고 로컬 로그인 경로에서 늘 실패한다. 외부 사용자의 비밀번호는 API로 바꿀 수 없다(400 `external_user`).
- 로그인에 성공하면 `User`를 만들거나(첫 로그인) 역할이 바뀌었으면 고친다. 역할이 바뀌면 `session_epoch`도 올린다. 관리자가 외부 사용자를 지울 수 있고, 다음 로그인에 다시 생긴다. 외부 사용자도 API 토큰을 만들 수 있다. 토큰은 디렉터리에서 사용자를 지워도 남는다(관리자가 지움 — 한계로 적음).
- 영역을 지우면 그 영역 사용자의 세션은 다음 요청에서 끊고(사용자를 읽을 때 영역이 없으면 401), `User`·토큰은 남긴다(다시 만들면 이어 씀).

### 3.3 로그인

- `POST /auth/login { username, password, realm? }`: `realm`이 없거나 `local`이면 지금 그대로. LDAP 영역이면 시도 제한(키는 `<사용자>@<영역>`과 IP) → LDAP 확인 → `User` 만들기·고치기 → 세션. OIDC 영역이면 400(`/auth/oidc/...`를 써야 함).
- LDAP 확인(`LdapRealm { url, bind_dn: Option, bind_password: Option, base_dn, user_filter, group_attr, ca_pem: Option, timeout }`):
  1. `ldaps://`이거나 `ldap://`이면 StartTLS가 반드시(평문 바인드는 하지 않음). `ca_pem`이 있으면 그것만 믿고 없으면 시스템 루트.
  2. 서비스 계정으로 바인드(없으면 익명) → `base_dn` 아래에서 `user_filter`(기본 `(&(objectClass=person)(uid={username}))`, `{username}`은 RFC 4515 이스케이프)로 정확히 한 항목을 찾음(0개·2개 이상은 실패).
  3. 찾은 DN과 사용자 비밀번호로 다시 바인드(빈 비밀번호는 거절 — 익명 바인드로 통과하는 것을 막음).
  4. 그룹: 항목의 `group_attr`(기본 `memberOf`) 값(DN)들을 `RoleMapping.groups`의 `group`과 대소문자 무시로 비교.
  - 실패는 모두 401 `invalid_credentials`로 같고, 서버에 닿지 못하거나 서비스 바인드가 실패하면 503 `realm_unavailable`(로그에 원인).
- `POST /api/v1/realms/{key}/actions/test { username, password }`(admin): 위 과정을 돌려 단계별 결과(서비스 바인드, 찾은 DN, 그룹, 정해질 역할)를 돌려준다. `User`는 만들지 않는다.

### 3.4 oplctl과 웹

- `oplctl realm list|show|add-ldap|set|remove|test`, `oplctl login --realm <이름>`(비밀번호 로그인과 같이 토큰을 만듦).
- 웹 로그인 화면에 영역 고르기(영역이 `local`뿐이면 숨김). 설정 아래 "인증 영역" 화면(admin): 목록, LDAP 영역 만들기·고치기, 역할 매핑 표, 시험 대화상자. 사용자 목록에 영역 열.

### 3.5 시험

- 단위: 이름 규칙, 필터 이스케이프, 역할 매핑(가장 높은 역할·기본·없음).
- API: `tests/common/ldap.rs`의 가짜 LDAP 서버(`lber`로 Bind·Search·Unbind·ExtendedRequest(StartTLS)만, 시험 CA 인증서로 TLS): 첫 로그인에 사용자 생김·역할, 그룹이 바뀌면 역할과 세션 세대, 틀린 비밀번호·빈 비밀번호·없는 사용자·두 항목, 서버 없음 503, 시도 제한, 비밀이 응답·watch에 없음, 권한, 옛 노드 409, 영역 지우기와 세션, 시험 동작.
- CI Linux 기능 `ldap-it`: apt의 slapd로 실제 LDAPS·StartTLS 로그인.
- 웹 Vitest: 로그인의 영역 고르기, 영역 화면.

## 4. OIDC SSO (계획 36)

### 4.1 설정

- `OidcRealm { issuer, client_id, client_secret: Option, scopes (기본 ["openid","profile","email"]), username_claim (기본 "preferred_username"), groups_claim (기본 "groups"), ca_pem: Option }`. 공개 클라이언트(비밀 없음)도 PKCE로 된다.
- 리디렉트 URI는 `https://<사용자가 연 노드 주소>/api/v1/auth/oidc/callback`. 노드마다 주소가 다르면 IdP에 모두 등록해야 한다(영역 화면이 이 노드의 값을 보여 줌).
- discovery(`<issuer>/.well-known/openid-configuration`)와 JWKS는 노드 메모리에 10분 둔다. 모르는 `kid`면 한 번 다시 받는다. issuer는 `https://`만(시험은 시험 CA).

### 4.2 흐름

1. 웹이 `GET /api/v1/auth/oidc/{realm}/start`로 간다(최상위 이동). 서버는 `state`·`nonce`·PKCE 검증자를 만들어 쿠키 `opl_oidc`(HttpOnly, Secure, SameSite=Lax, 10분, `Path=/api/v1/auth/oidc`)에 넣고 — 값은 영역·state·nonce·검증자·만든 시각을 클러스터 CA 비밀 키(이미 모든 노드의 `ClusterSpec`에 있음)에서 HKDF-SHA256(정보 `opl-oidc-state`)으로 만든 키로 HMAC한 것 — 새 저장 값이 없고 어느 노드나 같은 키를 얻는다 — IdP의 authorization endpoint로 302.
2. IdP가 `/auth/oidc/callback?code&state`로 돌려보낸다. 서버는 쿠키를 확인(HMAC·10분·state 같음)하고 지운 뒤, token endpoint에 코드와 검증자로 교환한다(`client_secret_basic`, 비밀이 없으면 `client_id`만).
3. ID 토큰 확인: JWS `alg`는 RS256·ES256만(`none`·HS* 거절), JWKS의 키로 서명, `iss`·`aud`(client_id 포함, 여러 개면 `azp`)·`exp`·`iat`(5분 오차)·`nonce`.
4. 사용자 이름은 `username_claim`(소문자, 규칙에 맞지 않으면 실패), 그룹은 `groups_claim`(문자열 배열, 없으면 빈 것) → 역할. 클레임이 ID 토큰에 없으면 userinfo endpoint에서 한 번 더 읽는다.
5. `User` 만들기·고치기(3.2와 같음) → 세션 쿠키 → `/`로 302. 실패하면 `/login?error=<코드>`로 302(코드는 `sso_failed`·`no_role`·`realm_unavailable`), 원인은 로그와 감사 로그 `detail`에.
- 시도 제한: 콜백 실패도 IP 키로 센다.
- 로그아웃은 opLite 세션만 끝낸다(IdP 로그아웃 없음).

### 4.3 웹·oplctl

- 로그인 화면: OIDC 영역마다 "<표시 이름>으로 로그인" 버튼. `?error=` 문구.
- 영역 화면에 OIDC 만들기·고치기, 이 노드의 리디렉트 URI 보이기. `oplctl realm add-oidc`.

### 4.4 시험

- `tests/common/idp.rs`의 가짜 IdP(HTTPS, discovery·authorize(바로 코드로 돌려보냄)·token·jwks·userinfo, RS256·ES256 키): 성공 흐름(쿠키·세션), state 다름·쿠키 없음·쿠키 만료·HMAC 바뀜, nonce 다름, aud·iss·exp 틀림, `alg: none`, 모르는 kid 뒤 키 바뀜, 그룹 → 역할·`no_role`, userinfo로 클레임, 다른 노드로 돌아온 콜백(`two_nodes.rs`).
- 웹 Vitest: 버튼, 오류 문구.

## 5. 원격 클러스터 (계획 37)

### 5.1 리소스

- 새 리소스 `Remote`(이름은 사용자가 붙임): `RemoteSpec { url, ca_fingerprint, token }`(token은 원격 클러스터에서 만든 API 토큰, 응답·watch에서 지움), `RemoteStatus { reachable, checked_at, error, cluster_name, version, nodes: {total, online}, vms: {total, running}, cpus, memory_gib }`.
- 만들 때(admin, `require_current`) 서버가 바로 연결해 본다: `GET <url>/api/v1/cluster/ca`의 CA 지문이 `ca_fingerprint`와 같은지(다르면 400 `remote_fingerprint_mismatch`), 그 CA로 TLS를 믿고 토큰으로 `GET /auth/whoami`(401이면 400 `remote_unauthorized`). 자기 클러스터(같은 CA 지문)는 400 `remote_is_local`.
- 연결은 그 CA 하나만 믿는 rustls 클라이언트(호스트 이름은 확인하지 않고 CA 서명만 — 노드 인증서의 이름은 노드마다 다름), 시간 제한 10초, 리다이렉트 없음.

### 5.2 상태 수집

- 리더의 `RemotePoller`(`TelemetryReporter`와 같은 모양): 30초마다(opld `[remotes] interval`) 원격마다 `GET /cluster`, `/nodes`, `/vms`를 읽어 상태를 계산하고, 바뀌었을 때만 `update_status`(같으면 쓰지 않음 — Raft 부하). `checked_at`은 상태가 바뀔 때만 바뀌므로, 웹은 `reachable`과 `error`만 믿는다.

### 5.3 중계

- `/api/v1/remotes/{key}/api/{*path}`: 그 원격의 `/api/v1/{path}`로 메서드·쿼리·본문·`Content-Type`·`If-Match`를 그대로 보내고 응답을 그대로 돌려준다(스트리밍, 본문 한도 없음). `Authorization`은 저장한 토큰으로 바꾸고, 쿠키·`Origin` 등은 보내지 않는다.
- 권한: `GET`은 로그인한 누구나(Remote 읽기), 그 밖은 admin. 원격 쪽 권한은 토큰 주인의 역할이 한 번 더 정한다(관리자는 viewer 토큰을 등록해 읽기만 하게 할 수 있음).
- `watch`·콘솔(WebSocket)은 중계하지 않는다(404 `remote_path_unsupported`).
- 중계한 바꾸는 요청은 이 클러스터의 감사 로그에 `action: remote.proxy`, `target: remote/<이름>`, `detail: <메서드> <경로>`로 남는다(원격 쪽에는 토큰 주인으로 남음).

### 5.4 oplctl과 웹

- `oplctl remote add <이름> --url --ca-fingerprint --token-file|- `, `list`, `show`, `remove`, `oplctl remote vms <이름>`, `oplctl remote vm <이름> start|stop|shutdown <VM>`.
- 웹 "원격 클러스터"(메뉴): 카드 목록(이름, 클러스터 이름, 버전, 노드 online/total, VM running/total, CPU·메모리, 닿지 않으면 오류), admin은 추가(지문은 CA를 먼저 받아 보여 주고 확인)·삭제. 카드를 누르면 그 클러스터의 VM 목록(중계로 읽음, 10초마다), admin은 시작·끄기·종료.

### 5.5 시험

- API: 두 `Env`(서로 다른 클러스터)로 등록(지문 틀림·토큰 틀림·자기 자신), 비밀이 응답에 없음, 중계 GET·POST와 권한, 토큰이 원격에서 viewer면 원격이 403, watch 404, 상태 수집과 바뀔 때만 쓰기, 원격이 꺼짐.
- opld E2E: opld 클러스터 둘(각 한 노드), 한쪽에 다른 쪽을 등록 → 상태 `reachable`, `oplctl remote vms`.
- 웹 Vitest: 카드, 추가 대화상자, VM 목록·동작 권한.

## 6. 원격 복제 (계획 38)

### 6.1 받는 쪽 API

- `POST /api/v1/backups/receive { pool, manifest, origin }`(Backup 쓰기): 받는 풀(백업 풀)에 `Backup`을 phase `receiving`, `spec.origin = { cluster: <보낸 클러스터 이름>, ca_fingerprint, backup: <원본 id>, job: <원본 작업 이름> }`으로 만든다. 같은 `origin.ca_fingerprint`·`origin.backup`의 사본이 있으면 409 `replica_exists`(그 id를 `details`에).
- `PUT /api/v1/backups/{key}/files/{name}`(`name`은 매니페스트의 디스크 파일·`vars.fd`만): 본문을 `.part`에 쓰고 SHA-256을 응답 `{ sha256, size }`로 준다. 같은 파일을 다시 보내면 덮는다.
- `POST /api/v1/backups/{key}/actions/complete`: 모든 파일이 있으면 `manifest.json`을 쓰고 phase `ready`. 받는 중 24시간 손대지 않은 `receiving` 백업은 janitor가 지운다.
- 받은 사본은 그 클러스터의 보통 백업이다(목록·복원·지우기). `job`이 없으므로 받는 쪽 보존 규칙은 사본을 지우지 않는다.

### 6.2 보내는 쪽

- `BackupJobSpec.replicate: Option<{ remote, pool }>`(None이면 직렬화 안 함, 넣을 때 `require_current`). 작업이 만든 백업이 `ready`가 되면 리더의 스케줄러가 `backup-replicate` Task(대상 백업, 백업 풀을 읽을 수 있는 노드에서)를 만든다.
- Task: 원격(계획 37의 클라이언트, 저장한 토큰)에 `receive` → 파일마다 읽으면서 SHA-256을 계산해 `PUT`, 응답 해시가 다르면 실패 → `complete`. 결과를 원본 `BackupStatus.replicas: [{ remote, backup, state: ready|failed, error, at }]`에 적는다. 실패하면 다음 작업 실행 때 한 번 더(원본이 아직 있으면). 진행률은 Task 진행률.
- 수동: `POST /backups/{key}/actions/replicate { remote, pool }`(Backup 쓰기, 원격은 등록돼 있어야 함).
- 지우기: 보존 규칙이나 사용자가 원본을 지우면 `replicas`의 원격 사본도 지운다(원격 `DELETE /backups/{id}`, 404는 성공). 실패하면 원본 삭제는 그대로 하고 경고를 로그와 Task 오류에 남긴다(원격에 남은 사본은 원격에서 지움 — 한계).
- 원격 쪽 토큰 주인은 operator 이상이어야 한다(Backup 쓰기).

### 6.3 DR 절차(README)

1. 주 사이트 클러스터 A, DR 사이트 클러스터 B. B에 백업 풀을 만들고 operator 토큰을 만든다.
2. A에 B를 원격으로 등록하고, 백업 작업에 `replicate: { remote: B, pool: … }`.
3. A를 잃으면 B의 웹·oplctl에서 사본(출처 A)을 골라 복원한다(네트워크·보안 그룹은 B에 같은 이름이 있어야 하고, 없으면 복원 화면에서 바꿈 — 계획 27).

### 6.4 oplctl과 웹

- `oplctl backup-job set <이름> --replicate <원격>/<풀>|--no-replicate`, `oplctl backup replicate <백업> --remote --pool`, `oplctl backup list`에 출처·사본 열.
- 웹: 백업 작업 화면의 "원격 복제" 고르기, 백업 목록의 사본 상태와 출처, 백업 상세의 "원격으로 복제".

### 6.5 시험

- API: 두 `Env`로 받기 API(권한, 파일 이름 제한, 해시, 같은 출처 409, complete 조건, 받는 중 지우기), 작업 → 복제 Task → 원격에 ready 사본, 해시 다름 실패, 원본 지우면 원격 사본 지움, 원격 꺼짐의 실패와 다음 실행 때 다시, 원격 사본 복원.
- opld E2E: 클러스터 둘, 꺼진 VM 백업 복제 → B에서 복원해 같은 디스크 내용(가짜 하이퍼바이저·dir 풀).
- 웹 Vitest: 작업 화면, 목록 표시.

## 7. 오류

| 코드 | HTTP | 언제 |
|---|---|---|
| `no_role` | 403 | 외부 사용자의 그룹·클레임에 맞는 역할이 없음 |
| `realm_unavailable` | 503 | LDAP 서버·IdP에 닿지 못함, 서비스 바인드 실패 |
| `external_user` | 400 | 외부 사용자의 비밀번호 바꾸기 |
| `sso_failed` | (리디렉트) | OIDC 콜백 확인 실패 |
| `remote_fingerprint_mismatch` | 400 | 원격 CA 지문이 다름 |
| `remote_unauthorized` | 400 | 원격 토큰이 받아들여지지 않음 |
| `remote_is_local` | 400 | 자기 클러스터를 원격으로 |
| `remote_unreachable` | 502 | 중계·복제 중 원격에 닿지 못함 |
| `remote_path_unsupported` | 404 | watch·콘솔 중계 |
| `replica_exists` | 409 | 같은 출처의 사본이 이미 있음 |

새 Kind(`Realm`, `Remote`)와 새 필드(`UserSpec.realm`, `BackupJobSpec.replicate`, `BackupSpec.origin`, `BackupStatus.replicas`)를 쓰는 모든 곳은 쓰기 전에 `require_current`(409 `cluster_upgrading`), 새 필드는 기본값이면 직렬화하지 않는다.

## 8. 나누기

- 계획 34: 2장. 이후 계획의 새 API는 미들웨어로 저절로 감사 로그에 남는다.
- 계획 35: 3장(`Realm` 리소스와 LDAP).
- 계획 36: 4장(계획 35의 `Realm`에 `oidc`).
- 계획 37: 5장.
- 계획 38: 6장(계획 37의 원격 클라이언트를 씀).

## 9. 알려진 틈

- 꺼진 노드의 감사 기록은 그 노드가 돌아와야 보이고, 디스크를 잃으면 사라진다(journald 전달로 보완). 루트 권한을 가진 사람은 파일을 고칠 수 있다(변조 방지 없음).
- (계획 34) 롤링 업그레이드 중 `ReadAudit`이 없는 옛 노드는 `GET /audit`의 `unreachable`이다. opld가 갑자기 죽으면 대기열에 남은 항목을, 전원이 나가면 fsync 전 마지막 1초쯤을 잃을 수 있고, 대기열이 차서 버린 항목은 `audit.dropped`의 수만 남는다(journald에도 없음). 파일 쓰기가 실패하면 `warn!`만 남기고 요청은 그대로 처리한다. 감사 로그를 끈 노드의 요청은 어디에도 남지 않는다(그 노드는 `disabled`로 보인다). 인증 없는 요청은 IP마다 1분에 60개까지만 적고 나머지는 `audit.suppressed`의 수만 남는다. 필드는 512바이트까지만 적는다. 로그인의 사용자 이름 칸에 비밀번호를 잘못 넣으면 이름 형식이면 `username=`에 그대로 적힌다. 읽기는 파일 하나를 통째로 메모리에 올리므로 `max_file_mib`는 256까지다. 콘솔은 연 것만 적는다(닫은 때·입력 없음). 리버스 프록시 뒤에서는 `ip`가 프록시 주소다(계획 13과 같음). 보존은 크기·개수로만 하고 날짜로 지우지 않는다.
- 외부 사용자를 디렉터리에서 지워도 이미 있는 세션(최대 12시간)과 API 토큰은 남는다.
- 노드마다 주소가 다르면 IdP에 리디렉트 URI를 노드 수만큼 등록해야 한다.
- 원격 등록의 토큰은 Raft에 평문으로 있다(구독 키·바인드 비밀번호와 같음 — Raft 데이터는 노드의 루트만 읽음).
- 복제는 전체 백업을 매번 보낸다(증분 없음). 원본을 지울 때 원격에 닿지 못하면 사본이 남는다.
