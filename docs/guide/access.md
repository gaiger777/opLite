# 접근과 보안

웹 UI, 로그인과 세션, LDAP, SSO(OIDC), 감사 로그, 선언형 적용과 설정 기록의 권한. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.
폴더·객체 권한, 사용자 정의 역할, 그룹, 토큰 범위, 할당량, 2단계 인증은 [폴더와 권한](folders.md)에 있다.

- [웹 UI](#웹-ui)
- [로그인과 세션](#로그인과-세션)
- [외부 인증(LDAP)](#외부-인증ldap)
- [SSO(OIDC)](#ssooidc)
- [감사 로그](#감사-로그)
- [선언형 적용과 설정 기록의 권한](#선언형-적용과-설정-기록의-권한)

## 웹 UI

orbivisord를 `web-ui` 기능으로 빌드하면 API와 같은 주소(`https://<노드>:8006/`)에서 웹 UI가 열린다.
Node.js 22.12 이상과 pnpm이 필요하다.

```bash
pnpm -C web install
pnpm -C web build                                   # web/dist
cargo build -p orbivisord --release --features web-ui     # web/dist를 바이너리에 넣는다
```

브라우저는 클러스터 CA를 모르므로 처음 접속하면 인증서 경고가 뜬다. CA를 내려받아
(`curl -k https://<노드>:8006/api/v1/cluster/ca -o opl-ca.pem`, `openssl x509 -in opl-ca.pem -noout
-fingerprint -sha256`의 지문을 `cluster init` 출력과 비교) 운영체제나 브라우저의 신뢰할 인증서로
가져오면 경고가 사라진다. 로그인한 뒤에는 클러스터 화면에서 CA를 내려받고 지문을 볼 수 있다.

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 홈, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, Ceph(만들기 마법사·상태·mon·mgr·OSD·풀 더하기), 백업(목록·증분·복원·지우기·원격으로 복제)과 백업 작업(증분 주기·원격 복제), VMware 가져오기(OVA·ESXi)와 VMware 이전(원본 연결·계획 마법사·전환)·OVA 내보내기, 네트워크, 보안 그룹, 노드(유지보수 모드, 비우기, 레이블, 디스크, 얼림 풀기, 전원 끄기 펜싱과 시험, 죽은 노드 제거), HA 그룹과 배치 규칙, HA 용량 예약, 작업, 감사 로그(admin),
사용자(admin), 인증 영역(LDAP·OIDC, admin), 폴더와 권한·역할·그룹·2단계 인증 정책([폴더와 권한](folders.md)), 내 2단계 인증, API 토큰, 클러스터(멤버, CA, 조인 토큰), 원격 클러스터(상태 카드, 원격 VM 목록과 동작), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
`orbivisorctl cluster join`을 실행한다(클러스터 화면이 명령을 만들어 준다). 이미지는 지금 접속한 노드로 올라가므로
그 노드에서 쓸 수 있는 풀에만 가져올 수 있다. 화면 구성(영역과 탭), ⌘K 팔레트, 일괄 동작, 단축키는
[웹 화면 둘러보기](web-ui.md)에 있다.

- **작업 보존.** 끝난(성공·실패) 작업은 만든 지 7일이 넘으면 지워지고, 그래도 1000개를 넘으면 오래된 것부터
  지워진다(orbivisord 설정 `[janitor]`의 `task_max_age_days`·`task_max_finished`, 리더 노드가 지운다). 어느 쪽이든 끝난
  뒤 10분은 남겨 결과를 볼 수 있게 한다. 처리 중인 작업은 지우지 않는다. 작업 화면과 VM 상세의 작업 탭은 50개씩 페이지로 나눠 보여 준다.
- **막힌 동작과 이유.** 할 수 없는 메뉴 항목·버튼은 숨기지 않고 막힌 채로 이유를 보인다(예: "VM이 정지되어
  있습니다.", "내 계정은 다른 관리자가 삭제합니다."). 키보드로 닿고 화면 읽기 프로그램이 이유를 읽는다. VM에 처리
  중인 시작·정지·재시작 작업이 있으면 웹에서는 시작·정지·재시작이 막힌다(강제 정지는 된다. CLI·API에서는 새 요청이
  앞 작업을 대체한다).
- **VMware 이전과 내보내기(계획 72).** VMware 원본 연결은 보기 `cluster.read`, 만들기·바꾸기·지우기 `cluster.admin`(admin)이다 —
  vCenter 비밀번호는 Raft에만 두고 API 응답·watch·감사 로그·`Debug`·Task에 없으며, 지문이 틀리면 비밀번호를 보내기 전에 끊는다.
  이전 계획·항목은 보기 루트 `system.read`, 계획 동작 루트 `vm.create`, 전환·되돌리기·마무리는 대상 폴더의 `vm.create` + `vm.power`다.
  OVA 내보내기·내려받기·지우기는 그 VM의 새 권한 **`vm.export`**(내장 `operator`에 없음 — 디스크 전체를 클러스터 밖으로 가져가므로
  `backup.file_restore`처럼 따로 준다)다([vCenter 일괄 이전](backup.md#vcenter-일괄-이전과-따뜻한-이전), [OVA로 내보내기](vms.md#ova로-내보내기)).
- **캐시.** 웹 파일에는 내용 해시로 만든 ETag가 붙는다. `index.html`은 매번 다시 확인하지만 바뀌지 않았으면
  **304**로 본문 없이 끝난다.

## 로그인과 세션

- **역할과 권한.** 사용자마다 전역 역할(`admin`·`operator`·`viewer`, 또는 권한 없는 `none`)이 있고, 이것은 클러스터 전체(루트)에
  그 내장 역할을 준 것과 같다. 폴더나 객체 하나에만 권한을 주려면 grant를 쓴다(그룹·사용자 정의 역할 포함). 역할과 grant는
  요청마다 다시 보므로 바꾸면 바로 반영된다. 읽을 수 없는 객체는 **404**, 읽을 수 있지만 권한이 없는 동작은 **403**이다.
  자세한 것은 [폴더와 권한](folders.md#권한).
- **2단계 인증.** 사용자가 인증 앱(TOTP)이나 패스키를 등록했거나 정책이 요구하면, 비밀번호가 맞아도 5분짜리 대기 세션만 받고
  둘째 단계(`POST /api/v1/auth/login/second-factor`)를 마쳐야 로그인이 끝난다. orbivisorctl은 `login --code`로 주거나 터미널에서
  묻는다. API 토큰은 둘째 단계를 거치지 않는다([2단계 인증](folders.md#2단계-인증)).
- **시도 제한.** 같은 사용자 이름·IP로 5번 연속 실패하면 30초, 같은 IP에서 (사용자 이름과 상관없이) 20번
  연속 실패하면 30초 동안 막힌다. 풀린 뒤에도 실패가 이어지면 실패할 때마다 막는 시간이 두 배가 된다(최대 15분).
  막힌 동안에는 맞는 비밀번호도 **429** `too_many_attempts`("try again in N seconds", `Retry-After` 헤더)를
  받는다. 로그인에 성공하면 그 사용자 이름·IP의 기록은 지워지지만, IP의 기록에서는 그 시도 하나만 빠진다(자기
  계정으로 로그인해 IP의 실패 수를 지울 수 없다). 맞는 로그인이 IP를 새로 잠그지는 않는다. 마지막 실패 뒤 15분이 넘게 지나면 기록이 지워진다. IPv6 주소는
  /64 단위로 센다. 자기 비밀번호를 바꿀 때의 현재
  비밀번호 확인도 같은 기록으로 센다. 노드마다 따로 세고, orbivisord를 다시 시작하면 기록이 사라진다.
- **프록시 뒤.** IP는 연결의 주소다. 리버스 프록시 뒤에 두면 모든 요청이 프록시 IP로 보이므로, 한 사람이 20번 틀리면
  프록시를 거치는 모든 사용자가 함께 막힌다. `[api] trusted_proxies`에 프록시 주소를 적으면 그 프록시가 보낸
  `X-Forwarded-For`의 클라이언트 주소로 센다([믿는 프록시와 허용 출처](cluster.md#믿는-프록시와-허용-출처)). 믿지 않는 주소에서 온
  `X-Forwarded-For`는 보지 않는다.
- **비밀번호 변경.** 비밀번호를 바꾸면(본인이든 admin이든) 모든 노드에서 그 사용자의 로그인 세션이 끝나 다시
  로그인해야 한다. 역할만 바꾸면 세션은 그대로다. **API 토큰은 끊기지 않는다.** 토큰이 새었을 수 있으면
  `orbivisorctl token list`·`orbivisorctl token delete`(웹: API 토큰 화면)로 따로 지운다. 관리자의 2단계 인증 초기화(`orbivisorctl user
  2fa-reset`)도 세션만 끝내고 토큰은 남긴다. 토큰을 좁게 쓰려면 [범위 토큰](folders.md#api-토큰의-범위)을 만든다. 열려 있는 콘솔(WebSocket)은 연결할
  때 한 번 인증하므로 비밀번호를 바꿔도 끊기지 않는다.

## 외부 인증(LDAP)

인증 영역(`Realm`)을 만들면 로컬 사용자 말고도 바깥 LDAP 디렉터리(OpenLDAP, Active Directory 등)의 계정으로
로그인할 수 있다(OpenID Connect IdP로 로그인하는 OIDC 영역은 다음 절 "SSO(OIDC)"). 영역은 admin만 보고
만들고 고친다(operator·viewer는 읽기도 **403**). 영역 이름은 리소스 이름 규칙(1~63자의 영문 소문자·숫자·`-`)이고
`local`은 로컬 사용자를 뜻하는 예약 이름이다. UUID로 읽히는 이름(예: `0192f0c1-5d2e-7000-8000-000000000000`)도
API가 id로 읽어 이름으로 부를 수 없으므로 **400**이다.

- **외부 사용자.** 영역 `corp`에 `alice`로 처음 로그인하면 Orbivisor 사용자 `alice@corp`가 생긴다(로컬 사용자 `alice`와는
  다른 사용자다). 이름의 사용자 부분은 소문자로 바꾸고(디렉터리에도 소문자로 찾는다) 1~64자의 `a-z`·`0-9`·`.`·`_`·`-`만
  된다. 이 형식이 아닌 계정은 로그인할 수 없다(**401**). 비밀번호는 Orbivisor에 저장하지 않고 늘 디렉터리가 확인한다.
  외부 사용자의 비밀번호를 바꾸려 하면 **400** `external_user`다(웹에는 그 메뉴가 없다). 외부 사용자도 API 토큰을
  만들 수 있다. 외부 사용자를 지우면(`orbivisorctl user delete alice@corp`) 세션과 토큰이 함께 끝나고, 디렉터리가 받아 주면
  다음 로그인에 다시 생긴다.
- **역할 매핑.** 로그인할 때마다 사용자 항목의 그룹 속성(기본 `memberOf`) 값들을 영역의 그룹 → 역할 표와 비교한다.
  앞뒤 공백을 떼고 ASCII 대소문자만 무시한 문자열 비교다(DN을 정규화하지 않으므로 `cn=ops, dc=example`과
  `cn=ops,dc=example`은 다르다 — `orbivisorctl realm test`가 보이는 값을 그대로 쓴다). 여러 그룹이 맞으면 가장 높은 역할
  (admin > operator > viewer > none), 맞는 것이 없으면 기본 역할, 그것도 없으면 **403** `no_role`("no role is mapped for this
  user")이다. 매핑이나 기본 역할에 `none`을 주면 전역 권한 없이 로그인시키고 폴더 grant로만 일하게 한다. 정해진 역할이 지금과 다르면 사용자를 고치고 그 사용자의 다른 로그인 세션을 끝낸다(같으면 아무것도 쓰지
  않는다). admin이 외부 사용자의 역할을 바꿀 수는 있지만 다음 로그인에 매핑으로 다시 정해진다.
- **그룹 연결.** 로그인 때 읽은 그룹 값은 사용자에 저장되고(바뀌었을 때만 쓴다), 로컬 그룹에 연결한 외부 그룹(`영역:그룹 DN`)과
  같으면 그 그룹의 grant를 받는다([그룹과 외부 그룹](folders.md#그룹과-외부-그룹)). 비교는 위 역할 매핑과 같다.
- **2단계 인증.** 디렉터리 바인드가 맞으면 그 외부 사용자(`alice@corp`)에 등록한 로컬 요소(TOTP·패스키)를 묻는다. 정책의
  `realms`에 영역 이름을 넣으면 그 영역 사용자에게 요구한다.

```bash
# OpenLDAP(memberof 오버레이): 서비스 계정으로 찾는다. --group은 <그룹 DN>=<role>(마지막 =에서 나눔, 여러 번)
orbivisorctl realm add-ldap corp --display "Corp LDAP" \
  --url ldap://ldap.example.com --base-dn ou=people,dc=example,dc=com \
  --bind-dn cn=orbivisor,ou=services,dc=example,dc=com --bind-password-file /root/corp-bind.pw \
  --ca-file /etc/ssl/certs/corp-ca.pem \
  --group cn=admins,ou=groups,dc=example,dc=com=admin \
  --group cn=ops,ou=groups,dc=example,dc=com=operator --default-role viewer
# Active Directory: sAMAccountName으로 찾는다(그룹은 memberOf 그대로)
orbivisorctl realm add-ldap ad --url ldaps://dc1.corp.example.com --base-dn "DC=corp,DC=example,DC=com" \
  --bind-dn "CN=svc-orbivisor,OU=Service Accounts,DC=corp,DC=example,DC=com" --bind-password-file /root/ad-bind.pw \
  --user-filter "(&(objectClass=user)(sAMAccountName={username}))" \
  --group "CN=orbivisor-admins,OU=Groups,DC=corp,DC=example,DC=com=admin"
orbivisorctl realm list
# NAME  DISPLAY    KIND  URL
# corp  Corp LDAP  ldap  ldap://ldap.example.com
orbivisorctl realm show corp      # 설정(bind password: set|none, ca: custom (PEM)|system roots, 그룹 표, 기본 역할)
orbivisorctl realm test corp --user alice  # 비밀번호는 터미널에서 묻는다(--password-file <파일|->)
# STEP          RESULT  MESSAGE
# connect       ok      connected to ldap://ldap.example.com with StartTLS
# service_bind  ok      bound as cn=orbivisor,ou=services,dc=example,dc=com
# search        ok      found uid=alice,ou=people,dc=example,dc=com
# user_bind     ok      bound as uid=alice,ou=people,dc=example,dc=com
# groups        ok      2 value(s) of memberOf
# dn: uid=alice,ou=people,dc=example,dc=com
# groups: cn=admins,ou=groups,dc=example,dc=com, cn=staff,ou=groups,dc=example,dc=com
# role: admin
orbivisorctl realm set-ldap corp --timeout 5      # 준 것만 바꾼다(읽은 revision으로 — 그 사이 바뀌었으면 412)
orbivisorctl realm set-ldap corp --bind-password-file /root/corp-bind.pw  # 새 서비스 계정 비밀번호
# URL·바인드 DN·CA를 바꿀 때는 비밀번호도 다시 준다(주지 않으면 서버가 400)
orbivisorctl realm set-ldap corp --url ldaps://ldap2.example.com --bind-password-file /root/corp-bind.pw
orbivisorctl realm set-ldap corp --clear-bind-dn    # 익명 검색으로(저장된 비밀번호도 지움)
orbivisorctl realm set-ldap corp --clear-ca --clear-default-role --clear-groups  # 시스템 루트, 기본 역할·그룹 표 없음
orbivisorctl realm remove corp
orbivisorctl login --server https://10.0.0.11:8006 --realm corp --user alice  # 기본 --realm local
```

- **설정 항목**(API `backend`의 필드, orbivisorctl 선택):
  - `url`(`--url`): `ldap://호스트[:포트]` 또는 `ldaps://호스트[:포트]`. 서버 하나만 쓴다.
  - `base_dn`(`--base-dn`): 사용자를 찾을 기준 DN(그 아래 전체, subtree).
  - `bind_dn`·`bind_password`(`--bind-dn`, `--bind-password-file <파일|->`): 사용자를 찾을 서비스 계정. 둘은 함께 있어야
    한다 — `bind_dn`에 비밀번호가 없거나 빈 글이면 **400**("bind_dn needs bind_password; remove bind_dn for an anonymous
    search"), 비밀번호만 있어도 **400**이고, 둘 다 없을 때만 익명으로 찾는다. 비밀번호는 API 응답에 없고
    `has_bind_password`로 있는지만 보인다. 바꾸기(`PUT`)에서 필드를 빼면 저장된 값을 두는데, `url`·`bind_dn`·`ca_pem`이
    모두 그대로일 때만이다: 하나라도 바꾸면서 빼면 **400** `invalid_request`("bind_password must be given again when url,
    bind_dn or ca_pem changes" — 저장된 비밀번호를 다른 서버나 계정으로 보내지 않는다). `""`이면 지우므로 `bind_dn`도 함께
    빼야 한다(`--clear-bind-dn`, 웹은 "저장된 비밀번호 지우기"와 바인드 DN 비우기).
  - 서버는 저장·비교하기 전에 `url`·`bind_dn`의 앞뒤 공백을 떼고, `ca_pem`은 CRLF를 LF로 바꾼 뒤 앞뒤 공백을 뗀다(만들기·바꾸기
    모두). 그래서 끝 줄바꿈이 있는 CA 파일(`--ca-file`)로 만든 영역을 웹에서 고쳐도 같은 CA로 보고 비밀번호를 그대로 둔다.
  - `user_filter`(`--user-filter`, 기본 `(&(objectClass=person)(uid={username}))`): `{username}`이 정확히 한 번 있어야
    하고, 로그인 이름을 RFC 4515로 이스케이프해 넣는다. Active Directory는 `(&(objectClass=user)(sAMAccountName={username}))`,
    UPN으로 찾으려면 `(userPrincipalName={username}@corp.example.com)`처럼 쓴다. 로그인할 수 있는 사람을 줄이는 조건도
    필터에 넣는다(예: AD의 `(memberOf:1.2.840.113556.1.4.1941:=CN=orbivisor,…)`).
  - `group_attr`(`--group-attr`, 기본 `memberOf`): 그룹 DN을 담은 사용자 속성. 서비스 계정(또는 익명)이 읽을 수 있어야 한다.
    OpenLDAP는 memberof 오버레이가 있어야 `memberOf`가 생긴다.
  - `ca_pem`(`--ca-file`): 서버 인증서를 확인할 CA(PEM). 있으면 그 인증서들만 믿고, 없으면 시스템 루트를 쓴다.
  - `timeout_secs`(`--timeout`, 기본 10, 1~30): 연결과 LDAP 작업마다의 시간 제한(초). 확인 전체(연결·바인드·검색·바인드)는
    `timeout_secs × 4 + 1`초(최대 121초) 안에 끝나고, 넘으면 **503**이다(시험 동작은 그 단계가 "timed out").
  - `display`(`--display`): 로그인 화면에 보일 이름. `roles`: `groups`(`--group`, `set-ldap`에서 주면 표 전체를 바꾼다)와
    `default`(`--default-role`).
  - `set-ldap`은 영역을 읽어 준 것만 바꾼다. 없애는 선택은 `--clear-bind-dn`(바인드 DN과 저장된 비밀번호 — 익명 검색),
    `--clear-ca`(시스템 루트), `--clear-default-role`, `--clear-groups`이고, 같은 값을 정하는 선택과 함께 줄 수 없다.
    `--clear-bind-password`는 바인드 DN이 남으면 보내지 않고 오류다("--clear-bind-password needs --clear-bind-dn (or give
    --bind-password-file)"). URL·바인드 DN·CA를 바꾸면서 `--bind-password-file`을 주지 않으면 서버의 **400**을 그대로 보인다.
  - 나쁜 값(형식이 아닌 URL, 빈 기준 DN, `{username}`이 없거나 둘, 시간 제한 범위 밖, 빈 그룹 DN 등)과 이름 `local`은 **400**
    `invalid_request`다.
- **TLS.** 평문으로는 바인드하지 않는다. `ldap://`는 늘 StartTLS를 쓰고(서버가 StartTLS를 하지 못하면 연결 실패),
  `ldaps://`는 처음부터 TLS다. 서버 인증서는 CA와 **호스트 이름**을 모두 확인한다: URL의 호스트(이름이나 IP)가 인증서의
  SAN에 있어야 한다. 시스템 루트는 노드의 인증서 저장소(Debian: `ca-certificates`)이고, 읽지 못하면 믿는 루트가 없어 모든
  연결이 실패한다. 사설 CA면 `--ca-file`을 준다.
- **로그인 순서.** 빈 비밀번호는 디렉터리에 묻지 않고 거절한다(익명 바인드로 통과하는 것을 막는다). 연결 → 서비스 계정
  바인드(없으면 생략) → `base_dn` 아래에서 `user_filter`로 찾기(`group_attr`만 읽음, 크기 제한 2·시간 제한 `timeout_secs`;
  서버가 크기 제한을 지키지 않아도 둘째 항목에서 멈추고 검색을 버린다) → 찾은 항목이 정확히 하나가 아니면(결과 코드 4
  sizeLimitExceeded도 여럿으로 봄) 실패 → 그 DN과 비밀번호로 바인드 → 그룹 값으로 역할 → 사용자 만들기·고치기 → 세션.
  끝나면 연결을 끊는다(unbind). 사용자 확인이 자격 증명 실패로 끝나면 시작부터 적어도 300ms 뒤에 답한다. 디렉터리가
  300ms 안에 답하면 없는 사용자와 틀린 비밀번호의 응답 시간 차가 가려지지만, 그보다 느린 디렉터리에서는 차이가 남을 수
  있다(시험 동작은 기다리지 않는다).
- **오류.** 없는 사용자, 여러 항목, 틀린 비밀번호(LDAP 결과 49)는 모두 로컬과 같은 **401** `invalid_credentials`다.
  연결·TLS 실패, 믿지 않는 인증서, 서비스 계정 바인드·검색 실패, 사용자 바인드의 49가 아닌 실패는 **503**
  `realm_unavailable`("the directory of realm corp is unavailable")이고 원인은 orbivisord 로그(`journalctl -u orbivisord`)에 경고로
  남는다. 모르는 영역은 **400** `invalid_request`, 매핑되는 역할이 없으면 **403** `no_role`이다. 롤링 업그레이드 중(옛 orbivisord
  노드가 있을 때) 영역 로그인과 영역 쓰기는 **409** `cluster_upgrading`이다(로컬 로그인은 된다).
- **시도 제한.** 로컬 로그인과 같은 기록이고 키는 외부 사용자 이름(`alice@corp`)과 IP다. **401뿐 아니라 403 `no_role`과
  503 `realm_unavailable`도 실패로 센다**: 디렉터리가 멈춘 동안 되풀이해 로그인하면 잠시 막힐 수 있다. 형식이 아닌 이름은
  IP만 센다.
- **감사 로그.** 영역 로그인의 `detail`은 `username=alice realm=corp`이다(이름 형식이 아니면 `<invalid>`). 영역 만들기·고치기·
  지우기·시험은 다른 바꾸는 요청처럼 적히고, 서비스 계정·사용자 비밀번호는 어디에도 넣지 않는다.
- **시험 동작.** `orbivisorctl realm test`(웹: 영역의 "시험", API `POST /api/v1/realms/{key}/actions/test {"username","password"}`,
  admin)는 로그인과 같은 순서를 `connect`·`service_bind`·`search`·`user_bind`·`groups` 단계로 돌려 단계마다 성공·실패와
  내용, 찾은 DN, 그룹 값, 정해질 역할을 보인다. 실패한 단계에서 멈춘다. 사용자·세션을 만들지 않고 시도 제한에도 세지
  않으며 비밀번호는 응답에 없다. orbivisorctl은 실패한 단계가 있으면 오류로 끝난다(역할만 없으면 성공 — `role: none`).
- **웹.** 로그인 화면은 `local` 말고도 영역이 있으면 영역 고르기를 보인다(기본 "로컬 사용자", 영역은 표시 이름이나 이름).
  "인증 영역" 메뉴(admin에게만, 사용자 다음): 목록(이름, 표시 이름, 종류, URL), LDAP 영역 만들기·고치기(바인드 비밀번호는
  비워 두면 그대로, "저장된 비밀번호 지우기"로 지움 — 서버와 같은 규칙으로 URL·바인드 DN·CA를 바꾸면 비밀번호를 다시
  넣으라고(바인드 DN만 비웠으면 "저장된 비밀번호 지우기"를 고르라고), 바인드 DN에 비밀번호가 없거나 지우기에 바인드 DN이
  남으면 저장 전에 알린다, 그룹 → 역할 표 더하기·빼기,
  기본 역할), 지우기, 시험 대화상자(단계 표,
  DN, 그룹, 정해질 역할). 사용자 목록에 영역 열(로컬 사용자는 `local`)이 있다.
- **API.** `GET`·`POST /api/v1/realms`, `GET`·`PUT`·`DELETE /api/v1/realms/{key}`(`PUT`은 통째로 바꾸고 `If-Match`를 따른다,
  이름은 바꿀 수 없다), 로그인 전 화면용 `GET /api/v1/auth/realms`(인증 없음, `[{"name","display","kind"}]`, `local`이 먼저이고
  나머지는 이름 순), `POST /api/v1/auth/login {"username","password","realm"}`(`realm`이 없거나 `local`이면 로컬 로그인,
  영역 로그인의 `username`은 `@영역` 없는 이름). 영역은 watch로 내보내지 않는다(서비스 계정 비밀번호가 들어 있다).
- **영역을 지울 때.** 그 영역의 사용자는 로그인할 수 없고(모르는 영역 **400**), 모든 노드에서 그 사용자들의 세션과 API 토큰이
  다음 요청부터 **401**이다. `alice@corp` 같은 사용자와 토큰은 남으므로 같은 이름의 영역을 다시 만들면 다시 쓸 수 있다
  — 새 영역의 디렉터리·매핑과 상관없이 **옛 세션(12시간 안)과 토큰이 지울 때의 역할 그대로 되살아난다**(아래 한계).
  완전히 없애려면 사용자도 지운다. 이미 열린 콘솔(WebSocket)은 끊기지 않는다.

한계:

- 디렉터리는 로그인할 때만 본다. 디렉터리에서 계정을 막거나 지우거나 그룹을 빼도 이미 있는 세션(최대 12시간)과 API 토큰은
  남고 역할과 그룹 grant는 다음 로그인에 바뀐다(토큰만 쓰는 사용자는 마지막 로그인의 그룹 그대로). 바로 막으려면 Orbivisor에서 그 사용자를 지운다(토큰도 함께 지워진다).
- 그룹은 사용자 항목의 속성 값만 본다. 그룹 항목을 찾는 방식(`posixGroup`의 `memberUid` 등)과 중첩 그룹 풀기(AD의
  `memberOf`는 직접 속한 그룹만)는 없다. DN은 문자열로만 비교한다.
- 서버 주소는 하나다(장애 조치·여러 서버 없음). 참조(referral)는 따라가지 않는다. 단순 바인드만 한다(SASL·Kerberos·클라이언트
  인증서 없음).
- 서비스 계정 비밀번호는 Raft에 평문으로 있다(구독 키와 같이 노드의 root만 읽는다).
- 영역을 지우고 같은 이름으로 다시 만들면(다른 디렉터리를 가리켜도) 옛 영역 사용자의 세션과 API 토큰이 지울 때의 역할로
  다시 동작한다. 영역을 다른 용도로 다시 쓰기 전에는 그 영역의 사용자(`orbivisorctl user list`의 `<사용자>@<영역>`)를 먼저 지운다.
- 같은 사용자가 거의 동시에 두 번 로그인하고 그 사이 디렉터리의 그룹이 바뀌면, 옛 그룹을 읽은 로그인이 나중에 써서 역할이
  옛 것으로 되돌아갈 수 있다(드문 경쟁 — 다음 로그인에 바로잡힌다).
- 사용자 부분이 `a-z`·`0-9`·`.`·`_`·`-`가 아닌 계정(공백·`@`·한글 등)은 로그인할 수 없다. 사용자·그룹 동기화(디렉터리를
  주기적으로 다시 읽기)는 없다. IdP로 로그인하는 영역은 아래 "SSO(OIDC)".

## SSO(OIDC)

OIDC 영역을 만들면 사용자가 OpenID Connect IdP(Keycloak, Okta, Microsoft Entra ID 등)에서 로그인하고 Orbivisor로
돌아온다(인가 코드 흐름과 PKCE). 영역의 공통 규칙은 "외부 인증(LDAP)"과 같다: admin만 보고 만들고 고치며, 이름 규칙,
외부 사용자 `<사용자>@<영역>`, 로그인할 때마다 그룹 → 역할 매핑(앞뒤 공백을 떼고 ASCII 대소문자 무시, 가장 높은 역할,
기본 역할(`none` 포함), 없으면 `no_role`), 역할이 바뀌면 그 사용자의 다른 세션이 끝남, 그룹 값 저장과 로컬 그룹 연결, 외부 사용자의
API 토큰, 영역을 지울 때가 모두 같다. 그룹 값은 그룹 클레임의 문자열이다. OIDC 사용자에게는 로컬 2단계 인증을 묻지 않는다(IdP가
MFA를 맡는다 — 2단계 인증 정책에 OIDC 영역을 넣으면 **400**). OIDC 영역 사용자는 비밀번호로 로그인하지 않는다: `POST /auth/login`에 OIDC
영역을 주면 이름 모양과 관계없이(이메일 모양 이름도) 늘 **400** `invalid_request`("realm okta signs in through its
identity provider" — 시도 제한에 세지 않음)다.

- **이름.** OIDC 영역의 사용자 부분은 이름 클레임을 소문자로 바꾼 것이고 1~128자의 `a-z`·`0-9`·`.`·`_`·`+`·`-`와 안쪽
  `@` 하나까지다(이메일 모양 — 앞뒤가 비어 있지 않음, 공백·`/`·`:`·`=` 없음). 그래서 Okta·Entra ID의 `preferred_username`
  `Alice@Corp.com`은 그대로 Orbivisor 사용자 `alice@corp.com@okta`가 된다(영역은 마지막 `@` 뒤). LDAP 영역의 규칙(1~64자,
  `@` 없음)은 그대로다.
- **IdP 사용자에 묶기.** 외부 사용자를 처음 만들 때 IdP의 사용자(`"<issuer> <sub>"`)를 적어 둔다(`subject`). 뒤에 같은
  이름으로 로그인한 사람의 `sub`가 다르면(IdP에서 이름을 다른 사람에게 다시 준 경우 등) `sso_failed`로 거절하고 사용자는
  그대로 둔다(orbivisord 로그에 그 이름과 "subject mismatch"). `subject`가 없는 사용자(이 기능 전에 만들어졌거나 LDAP 영역이었던
  이름)는 다음 OIDC 로그인의 것을 적는다.

- **IdP에 등록.** IdP에 Orbivisor를 웹 애플리케이션(인가 코드 흐름)으로 등록한다.
  - 리디렉트 URI는 `https://<브라우저가 연 노드 주소>/api/v1/auth/oidc/callback`이다(예:
    `https://10.0.0.11:8006/api/v1/auth/oidc/callback`). orbivisord는 로그인을 시작할 때 요청의 `Host` 헤더로 이 주소를 만들므로
    사용자가 들어오는 주소(노드마다, 이름과 IP도 따로)를 모두 등록한다. 리버스 프록시 뒤라면 프록시가 `Host`로 넘기는
    주소를 등록한다. 웹의 영역 대화상자가 지금 노드의 값을 복사 버튼과 함께 보이고, `orbivisorctl realm show`는 노드 주소 뒤에
    붙일 경로(`callback path`)를 보인다.
  - 클라이언트는 기밀 클라이언트(클라이언트 비밀, 토큰 끝점에 `client_secret_basic`)나 공개 클라이언트(비밀 없음)다.
    PKCE(S256)는 둘 다 늘 쓴다.
  - 사용자 이름 클레임(기본 `preferred_username`)은 위 이름 규칙에 맞는 문자열이어야 한다(이메일 모양도 된다). 그룹
    클레임(기본 `groups`)은 문자열 배열이다(없으면 그룹 없음).
  - 노드도 IdP에 직접 닿아야 한다(discovery·JWKS·토큰·userinfo 끝점, https만). 브라우저는 IdP와 노드 둘 다에 닿아야 한다.

```bash
# Keycloak(영역 corp): 클라이언트 orbivisor(Client authentication 켬 — 끄면 공개 클라이언트), Valid redirect URIs에
# 위 주소. 클라이언트 범위에 "Group Membership" 매퍼(Token Claim Name groups, Add to ID token 켬, Full group path
# 끔 — 켜면 값이 /admins 꼴). preferred_username은 Keycloak 사용자 이름 그대로다.
orbivisorctl realm add-oidc kc --display "Keycloak" \
  --issuer https://kc.example.com/realms/corp --client-id orbivisor --client-secret-file /root/kc-secret \
  --group admins=admin --group ops=operator --default-role viewer
# Okta(사용자 정의 인가 서버 default): 앱(OIDC Web Application)의 Sign-in redirect URIs에 위 주소. 인가 서버의 Claims에
# groups(Groups 필터, ID Token·Always). preferred_username은 로그인 이메일이라 사용자는 alice@example.com@okta가 된다.
orbivisorctl realm add-oidc okta --display "Okta" \
  --issuer https://example.okta.com/oauth2/default --client-id 0oa1b2c3d4 --client-secret-file /root/okta-secret \
  --group opl-admins=admin --group opl-operators=operator
# Microsoft Entra ID: 앱 등록(웹 플랫폼)의 리디렉트 URI에 위 주소, "인증서 및 비밀"의 클라이언트 비밀, "토큰 구성"의
# 그룹 클레임 추가(보안 그룹 — ID 토큰의 groups에 그룹 개체 ID가 들어간다). preferred_username은 UPN이라 사용자는
# alice@contoso.com@entra가 된다. 그룹이 200개를 넘는 사용자는 groups 클레임이 오지 않아 그룹이 없다.
orbivisorctl realm add-oidc entra --issuer https://login.microsoftonline.com/<테넌트 ID>/v2.0 \
  --client-id <애플리케이션 ID> --client-secret-file /root/entra-secret \
  --group 5f2c9a1e-0000-4000-8000-000000000001=admin
orbivisorctl realm list
# NAME  DISPLAY   KIND  URL
# kc    Keycloak  oidc  https://kc.example.com/realms/corp
orbivisorctl realm show kc  # issuer·client id·client secret: set|none·scopes·클레임·ca·timeout과 callback path
orbivisorctl realm test kc  # OIDC는 사용자·비밀번호 없이 IdP만 확인한다(--user는 경고하고 무시)
# STEP       RESULT  MESSAGE
# discovery  ok      issuer https://kc.example.com/realms/corp, authorization endpoint …, token endpoint …
# jwks       ok      1 RS256 and 0 ES256 signing keys
orbivisorctl realm set-oidc kc --timeout 5            # 준 것만 바꾼다(읽은 revision으로 — 그 사이 바뀌었으면 412)
orbivisorctl realm set-oidc kc --client-secret-file /root/kc-secret  # 새 클라이언트 비밀
# issuer·client ID·CA를 바꿀 때는 비밀도 다시 준다(주지 않으면 서버가 400)
orbivisorctl realm set-oidc kc --issuer https://sso.example.com/realms/corp --client-secret-file /root/kc-secret
orbivisorctl realm set-oidc kc --clear-client-secret  # 공개 클라이언트로(IdP 쪽도 바꾼다)
orbivisorctl realm set-oidc kc --scope openid --scope profile --scope groups  # scope 목록 전체를 바꾼다
orbivisorctl realm set-oidc kc --clear-ca --clear-default-role --clear-groups
```

- **설정 항목**(API `backend`의 필드 — `"kind": "oidc"`, orbivisorctl 선택):
  - `issuer`(`--issuer`): IdP의 issuer. 호스트가 있는 `https://` URL이고 쿼리·조각·사용자 정보·공백이 없어야 한다.
    discovery 문서(`<issuer>/.well-known/openid-configuration` — issuer 끝의 `/`는 하나로 합친다)의 `issuer`, ID 토큰의
    `iss`와 **글자 그대로** 같아야 하므로 끝 `/`까지 IdP가 알리는 대로 쓴다(Keycloak `https://<호스트>/realms/<영역>` —
    옛 판은 `/auth/realms/<영역>`, Okta `https://<조직>.okta.com/oauth2/default`, Entra ID
    `https://login.microsoftonline.com/<테넌트 ID>/v2.0`).
  - `client_id`(`--client-id`): 비어 있으면 안 된다.
  - `client_secret`(`--client-secret-file <파일|->`): 없으면 공개 클라이언트. API 응답에 없고 `has_client_secret`으로 있는지만
    보인다. 바꾸기(`PUT`)에서 빼면 저장된 값을 두는데 `issuer`·`client_id`·`ca_pem`이 모두 그대로일 때만이다: 하나라도
    바꾸면서 빼면 **400** `invalid_request`("client_secret must be given again when issuer, client_id or ca_pem changes" —
    저장된 비밀을 다른 IdP·클라이언트로 보내지 않는다). 저장된 비밀이 없으면(공개 클라이언트) 이 규칙은 없다. `""`이면
    지운다(`--clear-client-secret`, 웹은 "저장된 비밀 지우기(공개 클라이언트)").
  - 서버는 저장·비교 전에 `issuer`·`client_id`의 앞뒤 공백을 떼고 `ca_pem`은 LDAP처럼 다듬는다(CRLF → LF, 앞뒤 공백).
  - `scopes`(`--scope`, 여러 번, 기본 `openid` `profile` `email`): `openid`가 있어야 하고 값마다 공백 없는 비어 있지 않은 글.
    authorization 요청에 공백으로 이어 보낸다. `set-oidc`에서 주면 목록 전체를 바꾼다.
  - `username_claim`(`--username-claim`, 기본 `preferred_username`), `groups_claim`(`--groups-claim`, 기본 `groups`):
    비어 있으면 안 된다.
  - `ca_pem`(`--ca-file`): IdP 인증서를 확인할 CA(PEM). 있으면 그 인증서들만 믿고, 없으면 시스템 루트를 쓴다. 호스트
    이름도 확인한다.
  - `timeout_secs`(`--timeout`, 기본 10, 1~30): IdP 요청마다의 시간 제한(초).
  - `display`(`--display`): 로그인 화면 버튼의 이름. `roles`: `groups`(`--group <그룹 클레임 값>=<role>`, 여러 번)와
    `default`(`--default-role`).
  - Entra ID 메모: 클레임 변환(예: `ExtractMailPrefix`로 이메일 앞부분만)을 쓰려면 앱 매니페스트에
    `acceptMappedClaims: true`가 필요하고, 앱 전용 서명 키는 discovery 주소에 `?appid=`가 붙어야 하는데 Orbivisor는
    `<issuer>/.well-known/openid-configuration`만 읽으므로 맞지 않는다 — 쓰지 않는다. 이메일 모양 이름을 그대로 쓰므로 보통은
    변환이 필요 없다.
  - `set-oidc`는 영역을 읽어 준 것만 바꾸고, 없애는 선택은 `--clear-client-secret`·`--clear-ca`·`--clear-default-role`·
    `--clear-groups`다(같은 값을 정하는 선택과 함께 줄 수 없다). `set-ldap`·`set-oidc`는 종류가 다른 영역을 거절한다
    ("realm kc is not an LDAP realm (its kind is oidc)"). 나쁜 값은 **400** `invalid_request`다.
- **흐름.**
  1. 로그인 화면의 "<표시 이름>(으)로 로그인"이 `GET /api/v1/auth/oidc/<영역>/start`로 간다(페이지 이동). 노드는 IdP의
     discovery 문서를 읽고(노드 메모리에 10분), 요청의 `Host`로 리디렉트 URI를 만들고, 무작위 `state`·`nonce`·PKCE 검증자와
     그 리디렉트 URI를 로그인 상태 쿠키에 넣은 뒤 IdP의 authorization endpoint로 302한다(`response_type=code`,
     `client_id`, `redirect_uri`, `scope`, `state`, `nonce`, `code_challenge`, `code_challenge_method=S256`).
  2. 사용자가 IdP에서 로그인하면 IdP가 `/api/v1/auth/oidc/callback?code=…&state=…`로 돌려보낸다. IdP는 시작할 때
     보낸 리디렉트 URI(그 주소)로 돌려보내는데, 상태가 쿠키에 있으므로 그 주소 뒤의 어느 노드가 받아도 된다(노드들 앞의
     로드 밸런서·DNS 이름).
  3. 콜백은 쿠키를 확인하고(서명, 10분 안, 쿼리의 `state`와 같음), 인가 응답의 `iss`를 본다(RFC 9207: discovery 문서가
     `authorization_response_iss_parameter_supported: true`면 있어야 하고, 있으면 늘 issuer와 같아야 한다 — 다른 IdP의
     응답을 섞는 공격 막기). 토큰 끝점에 코드·쿠키의 `redirect_uri`·`code_verifier`·`client_id`를 폼으로 보내(기밀
     클라이언트는 `client_secret_basic`) ID 토큰을 받는다. 콜백은 `Host`를 보지 않는다.
  4. ID 토큰을 확인한다: 서명은 RS256(RSA 2048~8192비트)·ES256(P-256)만이고 `none`·`HS256` 등은 거절, 헤더에 `crit`가
     있으면 거절, 키는 IdP의 JWKS(노드 메모리에 10분, `use`가 있으면 `sig`만; 모르는 `kid`면 다시 받는데 마지막으로 받은 지
     10초가 지났을 때만 — IdP 키 교체를 따라감; 다시 받아도 그 `kid`가 없으면 `kid` 없이 내보인 키로 확인), `iss`가 issuer와
     같음, `aud`에 client ID(여럿이거나 `azp`가 있으면 `azp`가 client ID), `exp`·`iat`·(있으면) `nbf`는 5분 오차, `nonce`가
     쿠키의 것과 같음, `sub`가 있음.
  5. 이름·그룹 클레임 가운데 ID 토큰에 없는 것이 있고 IdP에 userinfo 끝점이 있으면 액세스 토큰으로 userinfo를 한 번 읽어
     빈 것만 채운다(userinfo의 `sub`가 ID 토큰과 다르면 실패). 이름 → `<이름>@<영역>`, 그룹 → 역할, 사용자 만들기·고치기
     (LDAP과 같음, IdP 사용자에 묶기) → 세션 쿠키(로컬 로그인과 같은 12시간 세션), 로그인 상태 쿠키 지우기 → `/`로 302.
     로그인 전에 열려던 주소로는 돌아가지 않는다.
- **IdP 요청.** discovery·JWKS를 받지 못하면 그 실패를 같은 영역(같은 revision)에서 15초 동안 기억해 다시 받지 않고 같은
  오류를 낸다(막힌 IdP로 요청을 쏟지 않게). 같은 영역을 동시에 여러 로그인이 물어도 한 번만 받는다. IdP 응답 본문은 1 MiB까지만
  읽는다(넘으면 실패).
- **쿠키.** 로그인 상태 쿠키 `orbivisor_oidc`(옛 이름 `opl_oidc`도 같은 값으로 — "이름 바꾸기 2")의 값은 `{영역, state, nonce, PKCE 검증자, 리디렉트 URI, 만든 시각}` JSON의 base64url과 그 HMAC-SHA256
  이다. HMAC 키는 클러스터 CA 비밀 키(모든 노드에 있음)에서 HKDF-SHA256(정보 `opl-oidc-state`)으로 만들므로 새로 저장하는
  값 없이 어느 노드나 같은 키를 얻는다. 속성은 `HttpOnly; Secure; SameSite=Lax; Path=/api/v1/auth/oidc; Max-Age=600`이다
  (IdP에서 돌아오는 최상위 이동에 쿠키가 가도록 `Lax`). 콜백은 성공·실패 모두 이 쿠키를 지운다. 브라우저가 쿠키를 막으면
  SSO 로그인은 늘 실패한다.
- **오류.** 실패하면 `/login?error=<코드>`로 돌아가고 로그인 화면이 문구를 보인다. 원인은 orbivisord 로그에 경고("an OIDC login
  failed" 또는 "cannot start an OIDC login", `journalctl -u orbivisord`)로 남는다.
  - `sso_failed`: 쿠키 없음·서명이 틀림·10분 지남·`state` 다름, IdP가 `error`로 돌려보냄(사용자가 거절 등), 인가 응답의
    `iss`가 다르거나(지원한다는 IdP인데) 없음, 토큰 끝점이 코드를 거절함(4xx `invalid_grant` — 코드가 틀렸거나 이미 썼거나
    만료), ID 토큰 확인 실패, userinfo가 액세스 토큰을 거절함(401·403), 이름 클레임이 없거나 글이 아니거나 이름 규칙에 맞지
    않음, 그룹 클레임이 문자열 배열이 아님, userinfo `sub` 다름, 같은 이름의 사용자가 다른 IdP 사용자에 묶여 있음, 시작할 때
    OIDC 영역이 아니거나 없음, 롤링 업그레이드 중(옛 orbivisord 노드가 있을 때 — LDAP의 409 대신).
  - `no_role`: 매핑되는 역할이 없음.
  - `realm_unavailable`: IdP에 닿지 못함(연결·TLS·믿지 않는 인증서·시간 제한) 또는 IdP 끝점(discovery·JWKS·토큰·userinfo)이
    2xx가 아닌 응답(위의 `invalid_grant`·userinfo 401·403 빼고 — 토큰 끝점의 `invalid_client` 같은 클라이언트 인증 거절도
    여기다), 시작이나 콜백에서 discovery 문서를 읽지 못하거나 맞지 않음(issuer 다름, https가 아닌 끝점).
  - 시작 요청의 `Host` 헤더가 없거나 호스트 글자가 아니면 **400**(JSON)이다. 콜백은 `Host`를 보지 않으므로 늘 302이고
    로그인 상태 쿠키를 지운다.
- **시도 제한.** OIDC 로그인(시작·콜백)은 로그인 시도 제한을 쓰지 않는다. 맞힐 비밀이 없기 때문이다: `state`는 서명된
  쿠키와 같아야 하고, 코드는 IdP가 한 번만 받으며 PKCE 검증자에 묶인다. 같은 NAT·프록시 뒤의 정상 실패(쿠키 만료 등)가
  그 주소의 모든 비밀번호 로그인을 막지 않게 한다.
- **감사 로그.** 콜백 `GET /auth/oidc/callback`을 적는다(다른 `GET`처럼 빼지 않음). `path`는 쿼리를 뺀
  `/auth/oidc/callback`이다. `detail`은 성공이면 `realm=kc username=alice`(사용자는 그 외부 사용자), 실패면
  `realm=kc error=no_role`이고 쿠키를 확인하기 전 실패는 `realm=<unknown>`이다. 실패 원인은 적지 않고 코드만이다(원인은 orbivisord
  로그). 쿼리(코드·`state`)와 토큰은 어디에도 적지 않는다. 시작은 적지 않는다.
- **시험 동작.** `orbivisorctl realm test <영역>`(웹: 영역의 "시험", API `POST /api/v1/realms/{key}/actions/test`를 본문 없이)은
  `discovery`(문서 받기, issuer 같음, 끝점 https)와 `jwks`(쓸 수 있는 RS256·ES256 키 수, 없으면 실패) 단계를 캐시 없이
  돌린다. 아무도 로그인하지 않으므로 클라이언트 비밀·리디렉트 URI·클레임은 확인하지 않는다 — 처음 한 번은 브라우저로
  로그인해 본다. LDAP 영역 시험은 `--user`(API `username`·`password`)가 없으면 **400**이다.
- **웹.** 로그인 화면은 OIDC 영역마다 아이디·비밀번호 폼 아래 "또는" 뒤에 "<표시 이름 또는 이름>(으)로 로그인" 버튼을
  보이고, 영역 고르기에는 로컬과 LDAP 영역만 있다. "인증 영역" 화면은 만들기에서 종류(LDAP/OIDC)를 고르고(고치기에서는
  종류를 바꾸지 않음), OIDC 폼(issuer, 클라이언트 ID, 클라이언트 비밀 — 비워 두면 그대로, issuer·클라이언트 ID·CA를
  바꾸면 다시 넣으라고 알림, scope는 공백으로 나눈 한 줄, 클레임, CA, 시간 제한, 그룹 → 역할, 기본 역할)과 이 노드의
  리디렉트 URI(복사 버튼), 입력 없는 시험(단계 표)이 있다. 목록의 URL 열은 issuer다.
- **API.** 영역 API는 LDAP과 같고(`backend`의 `kind`가 `oidc`, 응답은 `has_client_secret`), `GET /api/v1/auth/realms`의
  `kind`가 `oidc`다. 로그인 흐름은 `GET /api/v1/auth/oidc/{realm}/start`와 `GET /api/v1/auth/oidc/callback`(둘 다 인증
  없음, 302)이다.
- **orbivisorctl.** CLI는 브라우저 SSO를 하지 않는다: `orbivisorctl login --realm <OIDC 영역>`은 비밀번호를 묻기 전에 "sign in to kc in
  the web UI, create an API token there and run `orbivisorctl login --token-file`"로 끝난다(종료 1). SSO 사용자는 웹으로 로그인해
  API 토큰 화면에서 토큰을 만들고 파일에 넣은 뒤 `orbivisorctl login --token-file`로 저장한다:

  ```bash
  orbivisorctl login --server https://10.0.0.11:8006 --ca-fingerprint sha256:... --token-file /root/opl-token
  # logged in to https://10.0.0.11:8006 as alice@example.com@okta
  ```

  `--token-file <파일|->`(`-`면 표준 입력)는 `--user`·`--realm`·`--password-file`과 함께 줄 수 없다. 서버 CA는 비밀번호 로그인처럼
  `--ca-fingerprint`로 고정하고(같은 서버에 다시 로그인하면 저장한 CA), 그 토큰으로 `GET /api/v1/auth/whoami`를 불러
  확인한 뒤(받지 않으면 저장하지 않음) 로그인 정보 파일에 넣는다. 다시 로그인할 때 이전 토큰은 새 토큰과 다르고 이 CLI의
  비밀번호 로그인이 만든 토큰(이름 `oplctl-…`)일 때만 서버에서 지운다 — `--token-file`로 넣은 웹 토큰은 두므로 필요 없으면
  웹이나 `orbivisorctl token delete`로 지운다. `orbivisorctl logout`도 같다: login이 만든 토큰(`oplctl-…`)만 서버에서 지우고, 웹에서
  만든 토큰이면 로컬 설정에서만 지운 뒤 "the token stays valid on the server; delete it in the web UI"라고 알린다.

한계:

- 로그아웃은 Orbivisor 세션만 끝낸다(IdP 로그아웃·RP-initiated·back-channel 로그아웃 없음). IdP에 아직 로그인되어 있으면 다시
  누르면 바로 들어온다.
- IdP는 로그인할 때만 본다. IdP에서 계정을 막거나 그룹을 빼도 이미 있는 세션(최대 12시간)과 API 토큰은 남고 역할은 다음
  로그인에 바뀐다. 바로 막으려면 Orbivisor에서 그 사용자를 지운다.
- 노드마다 주소가 다르면 IdP에 리디렉트 URI를 주소 수만큼 등록해야 한다. 리버스 프록시 뒤면 프록시가 넘기는 `Host`의 주소가
  리디렉트 URI가 되고 scheme은 늘 `https`다.
- 외부 사용자는 처음 로그인한 IdP 사용자(`sub`)에 묶인다: IdP에서 계정을 지우고 같은 이름으로 다시 만들면(새 `sub`) 로그인이
  `sso_failed`다 — 같은 사람이면 Orbivisor에서 그 사용자를 지운 뒤(토큰도 지워짐) 다시 로그인한다. 영역의 issuer를 바꾸면
  모든 사용자가 같은 이유로 거절된다. Entra ID는 `sub`가 앱 등록마다 다르므로 앱 등록을 바꾸면(새 `client_id`) `sub`가
  모두 바뀌어 기존 사용자가 모두 `sso_failed`다 — 그 사용자들을 지워야 한다. `subject`가 없는 옛 사용자는 그 이름으로 처음 OIDC 로그인한 사람에게 묶인다.
- discovery·JWKS 받기가 실패하면 IdP가 돌아와도 15초 동안은 같은 오류다.
- discovery·JWKS는 노드마다 10분 둔다: IdP에서 지운 서명 키도 그동안은 그 노드에서 믿는다. 영역을 고치면 바로 새로 받는다.
- 이름 규칙(1~128자, 공백·한글 등 없음)에 맞지 않는 이름 클레임은 쓸 수 없다. 그룹은 클레임 값만 보고(IdP의 그룹 API를
  부르지 않음) Entra ID의 그룹 초과(groups 대신 `_claim_names`)는 그룹 없음이다.
- 클라이언트 인증은 `client_secret_basic`과 공개 클라이언트뿐이다(`client_secret_post`·`private_key_jwt`·mTLS 없음).
  `response_mode=form_post`, 요청 객체·PAR, 서명 알고리즘 RS256·ES256 밖(PS256·EdDSA 등)과 암호화한 ID 토큰은 없다.
- 클라이언트 비밀은 Raft에 평문으로 있다(서비스 계정 비밀번호와 같음). orbivisorctl은 브라우저 SSO를 하지 않는다(웹에서 만든 토큰을
  `orbivisorctl login --token-file`로).

## 감사 로그

노드마다 그 노드가 받은 바꾸는 API 요청(`POST`·`PUT`·`PATCH`·`DELETE`, 성공·실패와 401·403 포함), 로그인 성공·실패,
로그아웃, VM 콘솔 열기(`GET /api/v1/vms/{key}/console`), OIDC 로그인 콜백(`GET /api/v1/auth/oidc/callback`)을 한 줄 JSON으로 적는다. 다른 `GET`(감사 로그 읽기도),
웹 정적 파일, 노드 사이 RPC, 제어 소켓(`cluster init`·`join`·`leave`), 맞는 라우트가 없는 요청은 적지 않는다.
요청 본문은 적지 않는다.

```bash
orbivisorctl audit                               # admin. 모든 노드의 지난 24시간, 새 것부터 200개
orbivisorctl audit --since 1h --user alice       # --since: 90s·30m·1h·7d(지금부터 거슬러) 또는 RFC 3339
orbivisorctl audit --since 2026-11-06T00:00:00Z --until 2026-11-07T00:00:00Z --node node1 --limit 1000
orbivisorctl audit --action web-1                # 동작이나 경로에 든 문자열(VM 동작은 경로로 가른다)
# TIME                  NODE   USER   IP        ACTION                            TARGET     STATUS
# 2026-11-06T09:30:05Z  node1  alice  10.0.0.5  POST /vms/{key}/actions/{action}  vms/web-1     202
# 2026-11-06T09:29:58Z  node2  -      10.0.0.9  POST /auth/login                  username=bob  401
orbivisorctl -o json audit --since 7d            # 응답 그대로(항목의 모든 필드)
```

- **항목.** `{"id","time","node","user","user_id","role","auth","ip","method","path","action","target","status",
  "duration_ms","detail"}`. `id`는 UUID v7, `time`은 유닉스 밀리초, `node`는 적은 노드의 이름이다. `user`·`user_id`·
  `role`은 인증 전이거나 실패하면 `null`, `auth`는 `session`·`token`·`none`(`Authorization: Bearer`가 있으면 인증에
  실패해도 `token`)이다. `ip`는 시도 제한과 같은 클라이언트 IP(TLS 연결의 주소, "로그인과 세션"), `path`는
  `/api/v1` 아래의 실제 경로이고 쿼리는 넣지 않는다(쿼리에 비밀이 올 수 있다). `action`은 `"<메서드> <라우트
  템플릿>"`(예: `POST /vms/{key}/actions/{action}`, `DELETE /users/{key}`, `POST /auth/login`)이라 새 API도 저절로
  이름을 가진다. `target`은 템플릿의 첫 조각과 첫 매개변수 값(`{"kind":"vms","key":"web-1"}`)이고, 매개변수가 없는
  경로는 `null`이다. `detail`은 핸들러가 남긴 짧은 문자열이다: 로그인은 `username=<이름>`(이름 형식이 아니면
  `username=<invalid>` — 비밀번호를 이름 칸에 넣었을 수 있다, 영역 로그인은 뒤에 ` realm=<영역>`), OIDC 콜백은 `realm=<영역> username=<이름>` 또는 `realm=<영역> error=<코드>`, 원격 클러스터 중계는 `remote=<이름> <메서드> /<경로>`, 구독 키 넣기는 `id=<키 id>`. 비밀번호·키·토큰은
  어디에도 넣지 않는다. 로그인에 성공하면 그 사용자, 로그아웃은 세션의 사용자가 `user`다. `path`·`target.key`·
  `detail`·`user`는 512바이트까지만 적는다(넘으면 글자 경계에서 잘라 끝에 `…`).
- **파일.** `{data_dir}/audit/audit.log`(패키지는 `/var/lib/orbivisor/audit/audit.log`). 디렉터리는 0700, 파일은 0600이다.
  요청은 디스크를 기다리지 않는다: 쓰기 스레드가 덧붙이고 1초마다 묶어 `fsync`한다. 줄을 더하면 `max_file_mib`를
  넘을 때 `audit.log` → `audit.log.1` → `.2` …로 밀고, 지금 파일을 포함해 `max_files`개를 넘는 가장 오래된 것을 지운다.
  쓰기가 밀려 대기열(1만 개)이 차면 항목을 버리고, 다음 항목 앞에 `"action":"audit.dropped"`, `"detail":"<N> entries
  dropped"` 항목을 적는다. 인증 없는 항목(인증 실패·로그인 실패·익명, `user`가 `null`)은 IP마다 1분에 60개까지만
  적고(IPv6는 /64 네트워크 하나로 센다), 넘친 것은 그 1분 창이 끝나고 1초 안에(또는 orbivisord를 멈출 때)
  `"action":"audit.suppressed"`, `"ip"`(IPv6면 `<네트워크>/64`), `"detail":"<N> unauthenticated requests
  not recorded since <창 시작 RFC 3339>"` 항목으로 수만 남긴다(IP는 1만 개까지 세고, 넘치면 가장 오래된 것의
  수를 바로 적고 버린다). 인증된 요청은 한도가 없다. orbivisord가 열 때 디렉터리는 0700, 파일은 0600으로 다시 맞추고,
  `audit.log`가 심볼릭 링크면 열지 않으며(orbivisord가 시작하지 않음), `max_files`를 줄였으면 넘는 옛 파일을 지운다.
- **journald.** 같은 JSON을 tracing 대상 `opl_audit`(INFO)로도 내보낸다. 패키지의 orbivisord는 systemd 서비스라 journald에
  `… INFO opl_audit: {"id":…}` 줄로 남는다(`journalctl -u orbivisord -g opl_audit`). `RUST_LOG`를 바꿀 때는 `opl_audit=info`가
  남게 한다(파일은 `RUST_LOG`와 상관없다). orbivisord는 로그가 터미널로 갈 때만 색 코드를 넣으므로 journald 줄에는 색 코드가
  없다(`NO_COLOR`를 따로 줄 필요가 없다). 밖으로 보내려면 rsyslog로 전달한다. Debian 13은 rsyslog가 기본으로 없다
  (`apt install rsyslog`):

  ```
  # /etc/rsyslog.d/60-opl-audit.conf
  # rsyslog가 이미 journald에서 받으면(/var/log/syslog에 orbivisord 줄이 보이면) 다음 module 줄은 뺀다.
  module(load="imjournal" StateFile="imjournal.state")
  # 이름 바꾸기(Orbivisor) 전 판의 노드는 프로그램 이름이 orbivisord다.
  if ($programname == 'orbivisord' or $programname == 'opld') and $msg contains 'opl_audit:' then {
      action(type="omfwd" target="siem.example.com" port="514" protocol="tcp")
  }
  ```

  `systemctl restart rsyslog` 뒤 바꾸는 요청 하나를 보내 수집 서버에 줄이 오는지 본다.
- **읽기.** `GET /api/v1/audit?since&until&user&node&action&limit`(루트 `cluster.admin` — admin만, 다른 역할과 폴더 권한만 있는 사용자는 **403**)는 받은 노드가
  위트니스가 아닌 모든 멤버의 감사 로그를 읽어(자기 파일은 직접, 다른 노드는 노드 사이 RPC로 노드마다 5초 제한)
  시간 내림차순으로 합친다. `since`·`until`은 유닉스 밀리초나 RFC 3339(기본: 지금까지 24시간, 양 끝 포함), `user`·
  `node`는 정확히 같은 이름, `action`은 `action`이나 `path`에 든 문자열이다. `limit`은 1~5000(기본 200), 아니거나 시각이
  틀리면 **400** `invalid_request`다. 응답은 `{"entries":[…],"unreachable":["node3"],"disabled":["node4"],"truncated":false}`:
  읽지 못한 노드(꺼짐, 제한 시간, 이 기능이 없는 옛 orbivisord)는 `unreachable`에, 감사 로그를 끈(`[audit] enabled = false`)
  노드는 `disabled`에 이름 순으로, 조건에 맞는 항목이 `limit`보다 많으면 `truncated: true`다. 같은 항목은 한 번만 온다
  (읽는 중에 회전해도). orbivisorctl은 표 뒤 stderr에 `warning: no audit records from: node3`, `note: the audit log is disabled
  on: node4`, `note: more entries match; narrow the range or raise --limit`을 쓴다(`-o json`이면 쓰지 않는다). 표의
  `TARGET`은 대상이 없으면 `detail`(로그인의 `username=…`, `audit.dropped`의 수)이다. 기간 `--since`(`1h` 등)와 `--until`은
  함께 줄 수 없다(`a relative --since cannot be combined with --until; give a time` — `--since`에 시각을 준다).
- **웹.** 기록 › 감사 로그(admin에게만 보인다 — 같은 묶음의 변경 기록·코드로 관리(YAML)는 [웹 화면](web-ui.md#설정-기록선언-묶음인증서-화면)): 시간 범위(지난 1시간·24시간·7일, 기본 24시간), 사용자·동작 입력(적용을
  눌러야 거른다), 표(시각은 브라우저의 지역 시간, 대상이 없으면 `detail`, 4xx·5xx 상태는 경고 색), 읽지 못한 노드 경고,
  감사 로그를 끈 노드 안내와 잘림 안내, 행을 누르면 항목 JSON. 자동으로 다시 읽지 않는다(새로고침 버튼). 다시 읽기가
  실패하면 마지막 표를 두고 위에 오류를 보인다.

설정(노드마다, 생략하면 아래 기본값):

```toml
[audit]
enabled = true       # 끄면 이 노드는 파일도 journald 줄도 남기지 않는다
max_file_mib = 16    # 한 파일의 최대 크기(MiB, 1~256 — 읽을 때 파일을 통째로 메모리에 올린다)
max_files = 8        # 지금 파일을 포함해 남기는 파일 수(1 이상). 0이면 orbivisord가 시작하지 않는다
```

감사 디렉터리를 만들거나 파일을 열지 못하면 orbivisord가 시작하지 않는다. orbivisord를 멈출 때는 API를 닫은 뒤 남은 항목을 쓰고
`fsync`한다.

- **한계.** 꺼진 노드의 기록은 그 노드가 돌아와야 보이고, 디스크를 잃으면 사라진다(journald 전달로 보완한다). 변조
  방지(해시 체인·서명)는 없어 노드의 root는 파일을 고칠 수 있다. 롤링 업그레이드 중 옛 orbivisord 노드는 `unreachable`이다.
  orbivisord가 갑자기 죽으면 대기열에 남은 항목을, 전원이 나가면 마지막 1초쯤을 잃을 수 있다. 버린 항목은 수만 남는다.
  파일 쓰기가 실패하면(디스크가 참 등) orbivisord 로그에 경고만 남기고 요청은 그대로 처리한다. 리버스 프록시 뒤에서는
  `ip`가 프록시 주소다. 콘솔은 연 것만 적고 닫은 때와 입력은 적지 않는다. 알림과 날짜로 지우기는 없다(크기와 개수로만
  지운다). 위트니스 노드는 API가 없어 감사 로그도 없다. 인증 없는 요청은 IP(IPv6는 /64)마다 1분에 60개까지만 적고 나머지는
  창이 끝나고 1초 안에 적히는 `audit.suppressed`의 수만 남는다(같은 IP에서 많은 로그인 실패가 오면 그 시각·이름은
  다 남지 않는다). 각 문자열 필드는 512바이트까지만 적는다. 로그인의 사용자 이름 칸에 비밀번호를 잘못 넣으면 이름 형식(1~63자의 영문 소문자·숫자·`-`)이면
  `username=<그 값>`으로 그대로 적힌다. 감사 로그를 끈 노드는 `disabled`로 보일 뿐 그 노드의 요청은 어디에도 남지 않는다.
  `max_file_mib`는 256까지이고, 읽을 때 파일 하나를 통째로 메모리에 올린다(한 번 읽기에 최대 `max_file_mib`쯤).

## 선언형 적용과 설정 기록의 권한

선언형 적용·되돌리기·설정 기록(계획 73, [선언형 운영](declarative.md))은 **새 권한을 더하지 않는다.**

- `orbivisorctl apply`·`restore`(`POST /apply`·`/history/…/restore`)는 어딘가의 `system.read`가 있으면 계획을 세울 수 있고, 단계마다
  그 종류·대상의 지금 권한으로 쓴다(VM은 그 폴더의 `vm.create`·`vm.config`·`vm.power`…, grant는 `permission.modify`와 위임 규칙, ACME는
  루트 `cluster.admin`). 권한이 없는 단계는 `blocked: forbidden`이고 아무것도 쓰지 않는다. 볼 수 없는 리소스·폴더는 계획에서도
  없는 것과 같은 문구다. 단계는 적용 Task가 돌 때 다시 본다(계획 뒤 권한이 빠지면 그 단계가 실패). 저장한 계획은 그것을 세운
  자격(세션 또는 같은 API 토큰)만 적용한다 — 좁은 토큰이 넓은 세션의 계획을 쓰지 못한다.
- 설정 기록 보기는 그 리소스를 **지금** 읽을 수 있고 그 줄의 그때 폴더도 읽을 수 있는 줄만 준다(지운 리소스는 마지막 폴더 기준).
  다른 줄은 목록에 없고 하나로 물으면 404다(있는데 읽을 수 없는 것과 없는 것은 거르기와 관계없이 같은 404). 적용 묶음 보기는 루트
  `system.read`, 묶음 지우기·드리프트 받아들이기는 그 객체 모두의 바꾸기 권한이다(객체를 고르지 않은 받아들이기는 볼 수 있고 바꿀 수
  있는 것만).
- 비밀(사용자 비밀번호 해시, realm 비밀, 펜스 BMC 비밀번호, 원격 토큰, 알림 SMTP·webhook 비밀, 백업 저장소 키·S3 비밀, VMware 비밀번호,
  ACME EAB·계정 키·DNS 비밀·공유 인증서 키, cloud-init 비밀번호 해시)은 설정 기록·감사 로그·계획 diff에 평문·해시 없이 "설정됨·같음·
  바뀜·지움" 표시만 남는다. 보기가 가리는 webhook URL(경로·질의)·헤더 값과 이미지 카탈로그 URL의 서명·사용자 정보도 기록과 적용 묶음
  선언에 남지 않는다. 기록이 없으니 되돌리기도 비밀을 되돌리지 않는다.
- 감사 로그에는 적용 요청 한 줄(`apply (set web: 3 create, 5 update, 1 delete, plan …)`)과 단계마다 `apply.create|update|delete` 줄이
  사용자·계획 id와 함께 남는다. 설정 기록 줄의 `request`가 감사 줄 `id`다.
