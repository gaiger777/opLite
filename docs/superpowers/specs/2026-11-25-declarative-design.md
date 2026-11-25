# 선언형 운영 설계 — 선언형 적용(apply·diff·plan·export), Terraform/OpenTofu 공급자, ACME 인증서, 설정 이력과 되돌리기·드리프트 (계획 73)

작성일: 2026-11-25
상위 문서: `2026-11-25-dnd-builder-design.md`(계획 71 — `POST /vm-checks` 미리 확인, `prepare_with(Planned)`, "권한 먼저, 첫 거절에서 멈춤", JSON·CLI 미리 보기), `2026-11-25-vmware2-design.md`(계획 72 — 비밀을 Raft에 두고 보기·watch·감사에서 빼기, 리더 루프 꼴 `VmwareScheduler`, 이어 가는 Task), `2026-11-18-folders-permissions-quotas-2fa-design.md`(계획 62 — 폴더·권한·할당량), `2026-11-11-availability-scale-design.md`(계획 46 — `api_names`, 내부 CA로 서명한 API 인증서), `2026-11-18-ui-ux-redesign-design.md`(계획 61 화면 규칙)
기준 트리: 계획 72 끝(`bdf186b2`). 이 문서는 구현 전 설계다. 스테이지 보고는 구현하면서 §18에 덧붙인다. 로드맵의 "선언형 적용 + Terraform + ACME"와 격차 분석의 "API 우선·선언형 운영으로 앞서기"를 이 문서가 맡고, 설정 이력·되돌리기·드리프트를 더한다.

## 1. 목적

Orbivisor는 API 우선으로 만들었지만, 운영자는 지금 **명령을 하나씩** 부른다(`orbivisorctl vm create …`, 웹 폼). 수십 개 리소스(폴더·네트워크·보안 그룹·볼륨·VM·백업 작업·권한)를 git에 두고 리뷰하고, 같은 모양을 다른 클러스터에 다시 만들고, 누가 무엇을 언제 바꿨는지 보고, 잘못 바꾼 설정을 **이전 모양으로 되돌리는** 일은 할 수 없다. 공개 API에 쓰는 인증서는 내부 CA가 서명해 브라우저가 믿지 않는다. 계획 73은 넷을 더한다.

- **선언형 적용**: `orbivisorctl apply -f 디렉터리|파일`이 YAML 매니페스트(여러 종류, 여러 문서)를 서버로 보내고, 서버가 **지금 상태와 비교해 계획**(만들기·바꾸기·그대로·지우기·막힘)을 세운 뒤 **의존 차례**(폴더·풀·네트워크 → 볼륨 → VM → 규칙·백업 작업·권한)로 지금 API와 같은 길로 적용한다. 몇 번 해도 같다(idempotent). `diff`·`plan`은 서버 쪽 미리 확인(쓰지 않음 — 권한·기능·이름·참조·할당량·용량·MAC/IP)이고, `plan --out`으로 저장한 계획은 그사이 바뀐 것이 있으면 적용을 거절한다. **적용 묶음**(apply set)이 마지막에 선언한 것을 기억해 매니페스트에서 빠진 리소스를 지우고(`--prune`, 데이터 있는 종류는 따로 허락), 선언과 실제의 차이(드리프트)를 본다. `orbivisorctl export`가 지금 클러스터를 같은 YAML로 내보낸다.
- **Terraform/OpenTofu 공급자**: `terraform/`의 Go 공급자(terraform-plugin-framework)가 같은 매니페스트 API를 써서 VM·볼륨·네트워크(SDN 포함)·라우터·보안 그룹·폴더·권한·백업 작업 등을 관리한다. `terraform plan` 때 서버 미리 확인을 불러 **계획 단계에서** 할당량·용량·권한 오류를 보인다. 같은 저장소·같은 판으로 나가고, 수용 시험(acceptance test)이 진짜 opld(개발 서버)에 대해 terraform과 tofu 둘로 돈다.
- **ACME 인증서**: Let's Encrypt·ZeroSSL·사내 ACME(step-ca 등)에서 API·웹 리스너 인증서를 받는다. HTTP-01(선택 포트 80 리스너 — 어느 노드든 답함)과 DNS-01(Cloudflare·Route 53·RFC 2136·PowerDNS·훅), EAB, 갱신 정보(ARI, RFC 9773)·프로필, 노드마다 인증서 또는 VIP 이름을 나눠 쓰는 인증서. 주문은 리더가 Raft로 조율하고(한 인증서에 주문 하나, 속도 한도·물러서기), 노드는 다시 시작 없이 바꿔 끼운다. CI는 Pebble로 시험한다.
- **설정 이력과 되돌리기**: 모든 설정 리소스의 spec 바뀜을 Raft 상태 머신이 **누가(사용자·토큰)·언제·어디서(웹·CLI·apply·Terraform·Task)·왜(사유)** 와 함께 기록한다. 리소스마다 타임라인과 revision 사이 비교, **리소스 하나를 이전 revision으로 되돌리기**, **지운 리소스 살리기**(같은 id), **여러 리소스를 한 시점으로 되돌리기**(폴더·적용 묶음·종류 단위 — 계획을 보고 적용), 보존 한도. 적용 묶음의 드리프트는 바꾼 이력 줄까지 짚어 준다.

2026-11 공개 문서로 아는 것(제품 판마다 다를 수 있다 — 이 저장소에서 확인하지 않았다):

| | Proxmox VE 9 | vSphere 8 (vCenter) | Orbivisor 지금(계획 72) | Orbivisor 계획 73 |
|---|---|---|---|---|
| 선언형 파일 적용 | 없음(`pvesh`·API 하나씩, Ansible 모듈은 커뮤니티) | 없음(PowerCLI·Ansible·Aria Automation 별도 제품). 8.0의 "구성 프로필"은 클러스터·호스트 설정만 | 없음(명령·폼) | **`apply`·`diff`·`plan`·`export`**, 서버 미리 확인, 저장한 계획, 가지치기 |
| Terraform | **공식 공급자 없음**(커뮤니티 `bpg/proxmox`·`Telmate/proxmox`) | 공식 `hashicorp/vsphere`(HashiCorp 유지, 제품과 판이 따로) | 없음 | **공식 공급자를 같은 저장소·같은 판으로**, 계획 때 서버 미리 확인, terraform·tofu 수용 시험 |
| ACME | 있음(노드마다, HTTP-01은 포트 80 단독 리스너, DNS-01은 acme.sh 플러그인) | 없음(VMCA·손으로 바꾸기·외부 CA 서명 요청) | 없음(내부 CA·손으로 PEM 파일) | **클러스터가 조율하는 ACME**, 노드마다·VIP 공유, ARI·프로필, EAB, 만료 경보 |
| 설정 바뀜 이력 | 없음(Task 기록에 "누가 무엇을 했다"까지, 설정 diff 없음) | 이벤트·Task("누가 언제 Reconfigure"), VM 설정 diff·되돌리기 없음 | 감사 로그(노드 파일, 요청 줄, 전과 후 없음) | **리소스마다 타임라인·diff·사유·출처**, 감사 줄과 이음 |
| 설정 되돌리기 | 없음(VM 스냅샷은 디스크+설정을 함께) | 없음(구성 프로필 되돌리기는 클러스터 설정만) | 없음 | **리소스 하나·지운 것 살리기·한 시점으로 여러 개** |
| 드리프트 | — | 구성 프로필·호스트 프로필 준수 검사(호스트 설정만) | — | **선언 묶음 대 실제**, 바꾼 사람까지 |

## 2. 범위

| 포함 | 제외 |
|---|---|
| 매니페스트 `orbivisor.io/v1`(종류·metadata·spec·VM `state`) — 스물다섯 종류(§8.1), 이름·폴더 경로로 참조 | 템플릿 언어(Helm·Kustomize 같은 덮어쓰기·변수 — 그런 도구는 매니페스트를 만들면 된다), 사용자 정의 종류 |
| 서버 쪽 계획·미리 확인(`POST /apply?dry_run`), 의존 차례, 세 갈래 비교(적용 묶음의 마지막 선언), 저장한 계획 토큰, 적용 Task, 가지치기(데이터 종류는 따로 허락) | 여러 리소스를 한 Raft 거래로(원자 적용 — §15), 자동 바꿔 만들기(지우고 다시 만들기) |
| 매니페스트 내보내기(`GET /manifests`), `orbivisorctl apply·diff·plan·export`, 비밀은 환경 변수·파일에서(`secretFrom`) | 서버가 YAML을 읽음(서버는 JSON만 — YAML은 CLI가 바꿈) |
| Go Terraform 공급자(리소스 열다섯·데이터 소스 여섯), 가져오기, 쓰기 전용 비밀 칸, 개발 서버 `opl-devserver`, GitHub `terraform` 작업(terraform·tofu) | 레지스트리 게시(서명 키·`terraform-provider-orbivisor` 저장소 — 사용자 몫, 절차만 가이드에), Pulumi·Crossplane |
| ACME 계정(EAB)·DNS 공급자·인증서 리소스, 손으로 짠 RFC 8555 클라이언트, HTTP-01 리스너, DNS-01 다섯 갈래, ARI·프로필, 노드마다·공유, 리더 조율, 바꿔 끼우기, 만료 경보, GitHub `acme` 작업(Pebble·challtestsrv·BIND) | 내부 클러스터 인증서(8007 mTLS) 갱신(§16), TLS-ALPN-01, 손 PEM 파일 다시 읽기 |
| 설정 이력(상태 머신 표, 출처를 단 명령), 보존 한도(설정), 타임라인·비교·되돌리기·살리기·시점 되돌리기, 비밀은 "바뀜/같음"만 | 상태(status)·디스크 데이터의 이력(데이터는 스냅샷·백업), 감사 로그를 Raft로 옮기기 |
| 적용 묶음 드리프트(리더 루프·경보·"실제를 선언으로 받아들이기") | 적용 묶음 없는 리소스의 드리프트(선언이 없으면 비교할 것이 없다 — 이력은 있다) |
| 웹: 활동 › 감사 탭 안 보기 셋(감사·설정 기록·선언 묶음), 리소스 ⋯ 메뉴 "설정 기록…" 서랍, 설정 › 보안 탭 안 "인증서" 보기, "YAML로 내보내기" | 새 사이드바 항목·탭, 웹 YAML 편집기(붙여 넣어 적용 — §16) |

## 3. 지금 있는 것 (계획 72 끝)

- **리소스**(`opl-types/src/resource.rs:10-19`): `Resource<S,T> { id(v7), name, revision, labels, spec, status }`. `revision`은 마지막으로 바꾼 Raft 로그 인덱스(전역 수)이고 spec·status 쓰기가 모두 올린다. 만든 시각·바꾼 시각·주인 칸은 없다(폴더는 `spec.folder: Option<Uuid>`). `Kind::ALL` 43(새 종류는 끝에만 — 시험), 이름 규칙 DNS 레이블 63자, UUID 꼴 이름 거절.
- **Raft 명령**(`opl-cluster/src/types.rs:42-68`, 태그 `op`): `Create`·`UpdateSpec{expected_revision?, labels, spec}`(spec 통째)·`UpdateStatus`·`UpdateStatusIf`·`Delete{expected_revision?}`. 옛 노드는 모르는 명령을 오류로 처리한다(호환 규칙 "새 명령은 기능 뒤에서"). 쓰기는 `Cluster::write` → `LeaderRequest::Write` → 리더가 제안(`cluster.rs:723`). 명령에 **누가·언제가 없다**.
- **상태 머신**(`state_machine.rs`): 묶음 하나를 redb 거래 하나로, 표 `objects`((종류, id) → JSON)·`names`. `insert`가 덮어쓰고 `Delete`가 줄을 지운다 — **옛 값은 어디에도 남지 않는다**(watch의 `deleted` 사건이 지우기 직전 값을 한 번 실을 뿐). 스냅샷 `SnapshotData { schema_version, objects }`. 독점 키(`exclusive.rs` — 볼륨 이름·MAC·IP·…)와 폴더 할당량·호스트 자원을 적용 때 결정적으로 본다.
- **watch**(`opl-cluster/src/watch.rs`): 메모리에 적용 묶음 1024개, `put`·`deleted`·`reset`, `since`가 지나면 409 `expired`. API는 종류별 가림(`redact_json` 갈래), 폴더 권한 거르기.
- **감사**(`opl-api/src/audit.rs`): 노드마다 `audit.log`(JSON 줄, 16 MiB × 8 돌림), GET 아닌 요청마다 `{id, time, node, user, auth, ip, method, path, action, target{kind,key}, status, duration_ms, detail}`. **전과 후 값이 없다**. `GET /audit`은 `cluster.admin`, 노드를 모아 읽는다. 처리기 메모 `AuditNote`, 비밀 뺀 요약 `audit_summary` 꼴.
- **낙관적 동시성**: `If-Match: <revision>`(`resources.rs:83`) → 다르면 **412** `revision_mismatch`. 바꾸기 본문은 종류마다 다르다(VM은 PUT 통째, 풀·네트워크·보안 그룹은 종류별 PATCH 구조체 — JSON merge patch 아님).
- **일반 리소스 API 없음**: 종류마다 처리기(`/vms`·`/volumes`·`/networks`·`/storage-pools`·`/folders`·`/grants`·`/access-roles`·`/groups`·`/backup-jobs`·`/backup-stores`·`/security-groups`·`/routers`·`/floating-ips`·`/load-balancers`·`/ha-groups`·`/ha-checks`·`/placement-rules`·`/remotes`·`/alert-rules`·`/notification-targets`·`/realms`·`/settings`). SDN은 클러스터 하나 `sdn` + 네트워크 종류의 SDN 칸(존·VNet 종류 따로 없음). 처리기 안 만들기 함수 일부는 이미 꺼내 있다(`vms::create_vm` — 가져오기·VMware 이전이 부름).
- **미리 확인**: `POST /vm-checks`(`vm_checks.rs:210`, 기능 `vm-check`) — 권한 먼저, `prepare_with(state, vm, scorer, Planned{volumes, rules, preview})`(`vms.rs:295`)로 아직 없는 볼륨·배치 규칙을 얹어 같은 검사. `?dry_run=true`는 `network-migrations`·`sdn/lockdown`에만.
- **기능·권한**: `FEATURES` 끝 `vm-export`(`node.rs:533`, 끝 시험 `:1246`), `require_feature` → 409 `cluster_upgrading`(`cluster.rs:469`). 경로표 `ROUTES`(`authz_routes.rs:79-658`, `Rule::{Public,Handler,List,Root,Read,Object}`, 빠진 경로 403·시험). 권한 이름 28개(마지막 `vm.export`), 역할 admin·operator·viewer·none + `AccessRole`, 토큰 범위.
- **비밀**: Raft 스펙 평문(`vmware::Secret` — `Debug` 가림), 종류별 가림(`backup_stores::redact_json`·`redact_source_json`·`redact_vm_json`(cloud-init `********`)), realm bind 비밀번호·OIDC client secret·BMC 비밀번호·원격 토큰·SMTP·웹훅 토큰·2FA 비밀·사용자 비밀번호 해시.
- **TLS**(`opl-peer/src/tls.rs`): 공개 리스너 `0.0.0.0:8006`(rustls ring, ALPN http/1.1), 인증서는 `ApiCert`(`RwLock<Arc<CertifiedKey>>`, `replace`로 **다시 시작 없이 바꿔 끼움**, SNI 무시). 출처는 `[api] tls_cert`·`tls_key`(시작 때 한 번 읽음) 또는 내부 CA 노드 인증서 / `ApiCertAgent`(`api_cert.rs`)가 설정 `api_names`를 보고 내부 CA로 서명한 API 인증서(메모리에만). **포트 80 리스너는 없다.** 내부 8007은 tonic mTLS + Raft의 클러스터 CA(`ClusterSpec.ca_key_pem`), 노드 인증서 824일·자동 갱신 없음.
- **리더 루프**: `VmwareScheduler` 꼴(틱·리더 확인·`read_barrier`), `member.rs:1064-1092`에서 띄움. **경보**(`alerts/engine.rs`) `AlertCondition`에 인증서 만료가 없다. **설정** `Kind::Settings` 하나(`"cluster"`), 새 칸은 `skip_serializing_if`.
- **CLI**(`crates/oplctl`, 바이너리 `orbivisorctl`): clap, `-o table|json|haproxy`, **YAML 의존성 없음**. 파일 입력은 `node fence set -f FILE`(읽기 전용 칸을 빼고 `revision`이 있으면 `If-Match`)뿐, `vm create --dry-run` → `/vm-checks`. 시험은 `fake_api`(요청 기록).
- **웹**: 활동 영역 탭 `/tasks`·`/alerts`·`/audit`(admin), 감사 화면은 대상 거르기가 없고 리소스 상세에 이력 탭이 없다(VM 상세 `?tab=json`은 `JSON.stringify`). diff·YAML 라이브러리 없음. 설정 영역 `/settings/security` 탭.
- **의존성**: `Cargo.lock`에 YAML·ACME·DNS·prost가 없다. `similar` 2.7은 `insta`(시험)로만. tonic은 손 코덱(`opl-peer/build.rs`의 `tonic_build::manual`)이고 `.proto`·prost가 없다. SigV4 서명은 `opl-store/src/backend/sigv4.rs`(계획 68 S3)에 있다. Go 1.26은 이 Mac에 있다.

## 4. 확인한 사실 (ACME·Terraform·YAML·우리 저장소)

공개 문서(RFC 8555·8738·9773·7638·2136·8945, Let's Encrypt 문서·공지, Pebble README, HashiCorp terraform-plugin-framework·terraform-plugin-testing·plugin protocol 문서, OpenTofu 문서)로 확인한 것과 가정을 나눠 적는다. 가정은 §14·§18에서 메운다.

- **ACME 흐름(RFC 8555)**: 디렉터리 → `newNonce`(HEAD) → `newAccount`(JWS에 `jwk`, 그 뒤는 `kid` = 계정 URL) → `newOrder{identifiers}` → 인가(authz)마다 도전 고름 → 도전 URL에 빈 객체 POST → 인가 `valid`까지 물음(`Retry-After`) → `finalize`(CSR DER base64url) → 주문 `valid` → `certificate` URL을 POST-as-GET으로 PEM 사슬. 모든 POST는 JWS(`alg`·`nonce`·`url` 보호 머리), 서버는 `badNonce` 오류를 언제든 낼 수 있다(새 nonce로 다시). 계정 키 ES256(P-256)·키 지문은 RFC 7638 JWK thumbprint, HTTP-01 응답은 `token.thumbprint`, DNS-01 TXT는 `base64url(sha256(token.thumbprint))`.
- **EAB**: ZeroSSL·Google Trust Services·사내 CA는 `newAccount`에 `externalAccountBinding`(HS256 JWS — 보호 머리 `kid` = EAB 키 id, 내용 = 계정 공개 JWK)이 필요하다.
- **HTTP-01**: 검증자는 `http://{이름}:80/.well-known/acme-challenge/{token}`만 본다(포트 바꿀 수 없음, 리다이렉트는 따라감 — 443 리다이렉트도 됨). 와일드카드는 DNS-01만. 이름이 VIP를 가리키면 **어느 노드가 받아도** 답해야 한다.
- **DNS-01**: `_acme-challenge.{이름}` TXT. CNAME을 따라가므로 위임(`_acme-challenge.web CNAME web.acme.example.net`)이 된다. 검증자는 권한 있는 서버에 물으므로 우리 쪽 전파 확인도 권한 서버(NS)에 직접 묻는 것이 맞다(재귀 서버 캐시를 피함).
- **ARI(RFC 9773)**: 디렉터리 `renewalInfo` → `GET {renewalInfo}/{base64url(AKI)}.{base64url(serial)}` → `{suggestedWindow{start,end}, explanationURL?}`, `Retry-After`. 새 주문에 `replaces`(옛 인증서 id)를 넣으면 CA가 속도 한도에서 봐준다. Let's Encrypt·Pebble이 지원한다. 창 안의 무작위 시각에 갱신하는 것이 권장이다.
- **프로필**: Let's Encrypt는 2025년부터 `newOrder`의 `profile`(`classic`·`tlsserver`·`shortlived` 6일)을 받는다(디렉터리 `meta.profiles`). 인증서 유효 기간은 짧아지는 쪽이다(90일 → 공지된 45일 일정) — 갱신은 고정 일수가 아니라 **ARI 창 또는 유효 기간의 1/3 남았을 때**로 정한다. 가정: 디렉터리에 `profiles`가 없으면 `profile`을 보내지 않는다.
- **속도 한도(Let's Encrypt)**: 같은 이름 묶음 주에 5장, 계정당 실패한 검증 시간당 5회, 새 주문 3시간에 300 등. 그래서 노드 수만큼 같은 이름 묶음을 따로 주문하면 안 된다(공유 인증서는 한 번 받아 나눔 — §11.2), 실패는 물러선다.
- **Pebble**(Let's Encrypt의 시험 서버, MPL-2.0 Go): `-strict`, 무작위 5% `badNonce`(`PEBBLE_WFE_NONCEREJECT`), 검증 지연 끔 `PEBBLE_VA_NOSLEEP=1`, 인가 재사용 끔, HTTP-01 검증 포트 설정(`httpPort` — 시험에서 80 대신), DNS는 `pebble-challtestsrv`(관리 HTTP `POST /set-txt`·`/clear-txt`, DNS 응답)에 묻게 `-dnsserver`. ARI·프로필·EAB(`externalAccountBindingRequired`)를 지원한다. 루트는 시작마다 새로 만들어 `GET /roots/0`(관리 포트 15000)으로 받는다.
- **Terraform 공급자 프로토콜**: 공급자는 별도 실행 파일이고 terraform이 go-plugin 방식으로 띄운다(마법 쿠키 환경 → 표준 출력 첫 줄 `1|6|tcp|127.0.0.1:포트|grpc|{인증서}` → gRPC + 자동 mTLS). 메시지는 `tfplugin6.proto`(MPL-2.0)이고 값은 cty msgpack(`DynamicValue`). OpenTofu는 같은 프로토콜 5·6을 쓰고 레지스트리만 다르다(`registry.opentofu.org`는 GitHub 릴리스를 가리키는 색인). 
- **terraform-plugin-framework**(Go, MPL-2.0, HashiCorp 유지): 스키마·plan modifier·검증기·가져오기·상태 판 올리기·민감 칸·**쓰기 전용 칸**(Terraform 1.11+, 상태에 남지 않음)·ModifyPlan(계획 때 공급자가 API를 불러도 됨)을 준다. 수용 시험 `terraform-plugin-testing`은 `TF_ACC=1`에서 진짜 terraform(또는 `TF_ACC_TERRAFORM_PATH`·`TF_ACC_PROVIDER_NAMESPACE`로 tofu) 실행 파일로 `plan`·`apply`·`import`·`refresh`·`destroy`를 돌린다. 가정: OpenTofu의 쓰기 전용 칸 지원 판(1.11 계열로 앎) — 안 되면 그 칸을 `sensitive`로 내린다(§18).
- **Rust로 tfplugin6를 짜려면**: 우리 tonic은 손 JSON 코덱이라 protobuf가 없다 — prost·protoc(또는 미리 만든 코드)·msgpack(cty 타입 체계: unknown·null·동적 타입)·go-plugin 손맞춤·자동 mTLS를 더 들이고, 프레임워크가 주는 계획 의미(unknown 전파·plan modifier·import·상태 판)를 우리가 소유해야 한다. 커뮤니티 Rust 프레임워크(`tf-provider` 등)는 유지가 얕다(가정 — 2026-11 crates.io 기준 최근 판 없음).
- **YAML(Rust)**: `serde_yaml`은 2024년 유지 중단(RUSTSEC 유지 중단 권고), `serde_yml`은 유지 중단·건전성 권고가 있었다. 유지되는 갈래 `serde_yaml_ng`(MIT, `unsafe-libyaml` — libyaml을 옮긴 것, 같은 API)를 CLI에만 쓴다(§15). deny.toml 허용 라이선스 안이다(가정: 스테이지 2가 `cargo deny check`로 확인).
- **우리 저장소**: `similar`(diff)는 시험 의존성뿐이라 서버 비교는 JSON 구조 비교를 직접 짠다(경로 목록 — 순수, 작다). ring은 ECDSA P-256 서명(`EcdsaKeyPair::sign` — 고정 길이 r‖s가 JWS ES256 꼴 그대로)·HMAC(EAB·TSIG)을 준다. rcgen은 CSR(`CertificateParams::serialize_request`)을, x509-parser는 AKI·일련번호·만료(ARI id)를 준다.

## 5. 전체 구조

```
orbivisorctl apply -f dir/  ──YAML→JSON(serde_yaml_ng), secretFrom 풀기──→  POST /apply {documents, apply_set?, prune?, dry_run?, plan?}
terraform/tofu ─ terraform-provider-orbivisor(Go) ──────────────────────→  POST /apply {documents:[하나], manager:"terraform"} · GET /manifests/{kind}/{key}
웹 ⋯ "YAML로 내보내기" · 설정 기록 서랍 ───────────────────────────────→  GET /manifests · GET /history… · POST /history/…/restore

opl-api/src/declarative/
  manifest.rs   매니페스트 꼴·검증·정규화(normalize — 기본값 채움·순서 고정)
  kinds/*.rs    종류마다 Declarable 어댑터: 열쇠·참조 뽑기·이름↔id·만들기/바꾸기/지우기 = 지금 처리기 안 함수
  plan.rs       참조 풀기(실제 + 계획 안) → 그래프·차례 → 세 갈래 비교 → 단계 + 미리 확인(prepare_with(Planned) 등)
  apply.rs      Task "apply": 단계마다 권한 다시 보기 → If-Match → 지금 길로 쓰기 → 만든 Task 기다리기 → 묶음 갱신
  export.rs     실제 → 매니페스트(id → 이름·폴더 경로, 비밀 → secretRef, 기본값 빼기 --minimal)
opl-api/src/history.rs   GET /history…, 비교, restore(하나·살리기·시점) → declarative 계획으로
opl-api/src/drift.rs     리더 DriftChecker(5분 + watch 몰아 받기) → apply-set status.drift, 경보
opl-api/src/acme/        client.rs(RFC 8555 JWS·nonce·EAB·ARI) · dns/{cloudflare,route53,rfc2136,pdns,hook}.rs · http01.rs
                         renewer.rs(리더 AcmeRenewer 60초 → Task acme-order) · install.rs(ApiCertAgent가 고름)

Raft(opl-cluster)
  Command::Attributed { meta{at_ms(리더가 찍음), actor, source, reason, request}, command }   ← 기능 config-history 뒤에서만
  상태 머신 표 history((kind,id,revision) → 기록 lz4) · history_seq(revision → (kind,id)) · meta history_bytes
  새 종류: apply-set · acme-account · acme-dns · certificate   (Kind::ALL 43 → 47)

opld: [api] http_listen = "0.0.0.0:80"(선택) → /.well-known/acme-challenge/* 만(Raft의 도전 값), 나머지는 https로 308
      공개 8006 인증서 차례: [api] tls_cert 파일 > 이 노드를 덮는 ACME 인증서 > 내부 CA(api_names)
```

- **서버가 계획한다.** CLI·Terraform·웹은 문서를 보내고 계획을 받는다. 같은 비교·검증·차례가 한 곳(`declarative/`)에 있어 세 손님이 갈라지지 않는다(계획 71이 "웹이 서버 규칙을 흉내 내면 갈라진다"로 미리 확인을 서버에 둔 것과 같은 까닭).
- **쓰기는 지금 길로만.** 어댑터는 종류의 지금 처리기에서 꺼낸 함수(검증·권한·할당량·독점 키·감사 메모·Task 만들기 그대로)를 부른다. 매니페스트 엔진이 Raft 명령을 직접 내지 않는다 — 새 규칙이 생겨도 apply가 비껴가지 않는다.
- **이력은 Raft 안에.** 상태 머신이 적용하면서 기록하므로 어느 노드에서 읽어도 같고, 새 노드는 스냅샷으로 받는다. 감사 로그(노드 파일)는 그대로 두고 이력 줄이 감사 id로 잇는다.

## 6. 타입 (opl-types·opl-cluster)

새 파일 `crates/opl-types/src/{manifest,apply_set,acme,history}.rs`. 모든 새 요청 타입은 `deny_unknown_fields`·`ToSchema`, 빈 값은 쓰지 않는다. 지금 타입의 직렬화는 그대로다(바이트 호환 시험).

- **`Manifest`**(저장하지 않는 꼴 — 요청·내보내기): `{ api_version: "orbivisor.io/v1", kind: ManifestKind, metadata: { name, folder?: "/prod/web"(경로), labels?, annotations?: { "orbivisor.io/prune": "false" 등 } }, spec: Value(종류의 만들기 본문 spec과 같은 꼴, 참조는 이름), state?: Running | Stopped(VM만) }`. `ManifestKind`는 PascalCase(`Vm`·`Volume`·`Network`·`Router`·`FloatingIp`·`SecurityGroup`·`LoadBalancer`·`StoragePool`·`Folder`·`PlacementRule`·`HaGroup`·`HaCheck`·`BackupJob`·`BackupStore`·`Remote`·`Grant`·`AccessRole`·`Group`·`AlertRule`·`NotificationTarget`·`Realm`·`Settings`·`Certificate`·`AcmeAccount`·`AcmeDns`) — `Kind`와 표로 잇는다.
- **비밀 칸 값**: `SecretValue = Plain(String) | Ref { secret_ref: "<종류>/<이름>/<칸>" }`. 서버는 `Plain`만 받는다(`Ref`는 "그대로 둠" — 내보낸 파일을 다시 적용해도 비밀이 바뀌지 않음). CLI는 `secretFrom: { env: X } | { file: path }`를 보내기 전에 `Plain`으로 푼다.
- **`ApplySet = Resource<ApplySetSpec, ApplySetStatus>`**(`apply-set`): `ApplySetSpec { description?, protect_data: true(가지치기에서 VM·볼륨·백업 저장소는 `--prune-data` 없이는 지우지 않음) }`. `ApplySetStatus { applied_at, applied_by, applied_revision, objects: Vec<AppliedObject { kind, id, name, declared: Value(정규화한 선언 칸만 — 비밀은 지문 없이 `"$secret"` 표시), revision }>, drift?: DriftReport { checked_at, revision, items: Vec<{ kind, id, name, state: Modified | Missing | Unmanaged?, paths: Vec<String>, by?: HistoryRef(그 칸을 처음 바꾼 이력 줄) }> }, last_error? }`. 한도: 객체 1 000개·`declared` 합 4 MiB(넘으면 400 `apply_set_too_large` — 묶음을 나눈다). 묶음에 든 리소스에는 레이블 `orbivisor.io/apply-set={이름}`을 붙인다(가지치기 대상 = 레이블과 `objects` 둘 다에 있는 것).
- **`AcmeAccount = Resource<AcmeAccountSpec, AcmeAccountStatus>`**(`acme-account`): `AcmeAccountSpec { directory: URL(별명 `letsencrypt`·`letsencrypt-staging`·`zerossl`·`google` 표), email?, agree_tos: bool(참이어야 등록), eab?: { kid, hmac_key: Secret }, ca_bundle?: PEM(사내 ACME·Pebble 루트), key: Secret(PKCS#8 PEM — 서버가 만듦, 요청에 없음) }`, `AcmeAccountStatus { url?, registered_at?, terms?, profiles: Vec<String>(디렉터리 `meta.profiles`), error? }`.
- **`AcmeDns = Resource<AcmeDnsSpec, ()>`**(`acme-dns`): `AcmeDnsSpec { provider: Cloudflare { api_token: Secret, zone_id? } | Route53 { access_key_id, secret_access_key: Secret, hosted_zone_id?, region: "us-east-1" } | Rfc2136 { server: "ip:53", key_name, algorithm: HmacSha256 | HmacSha512, secret: Secret(base64), zone? } | PowerDns { url, api_key: Secret, server_id: "localhost" } | Hook { path(절대 — `/etc/opl/acme-hooks/` 아래만) }, propagation: { timeout_s: 180, check: Authoritative | Skip }, zones?: Vec<String>(이 공급자가 맡는 영역 — 인증서가 이름으로 고름) }`.
- **`Certificate = Resource<CertificateSpec, CertificateStatus>`**(`certificate`): `CertificateSpec { account, names: Vec<String>(자리 `{node}`·`{node_address}` 허용 — 노드마다 펼침, 와일드카드 `*.` 허용(DNS-01만)), scope: PerNode { nodes: All | Some(Vec<Uuid>) } | Shared, challenge: Http01 | Dns01 { dns: 이름 }, key: EcdsaP256 | EcdsaP384 | Rsa2048, profile?, renew: Ari(기본 — 없으면 1/3) | DaysBefore(u16), preferred_chain?: 루트 CN, install: Api(공개 리스너에 씀 — 기본) | None(받기만) }`. `CertificateStatus { nodes: BTreeMap<Uuid, IssuedCert>, shared?: IssuedCert + key: Secret(공유 키 — 보기·watch에서 뺌), challenges: Vec<{ token, key_authorization, until }>(HTTP-01 답 — 짧게 삶), next_attempt_at?, failures: u32, error? }`, `IssuedCert { serial, not_before, not_after, names, chain_pem(공개), ari?: { start, end, checked_at }, renew_at, order?: 진행 중 주문 URL }`. **노드마다 인증서의 키는 그 노드 디스크에만**(`{data_dir}/pki/acme/{인증서 id}.key`, 0600) — Raft에 없다.
- **이력 기록**(Raft 표, 종류 아님 — `opl-types/src/history.rs`): `HistoryRecord { revision, kind, id, name, op: Create | Update | Delete, at_ms, actor: Actor { user?, user_id?, auth: Session | Token { name } | System, task?: Uuid }, source: Api | Web | Cli | Apply | Terraform | Task | System, reason?: String(200자), request?: Uuid(감사 id), apply_set?, labels, spec: Value(비밀 칸은 `{"$secret": "same" | "changed" | "set" | "cleared"}`), restored_from?: u64 }`. `ChangeMeta { at_ms, actor, source, reason?, request?, apply_set?, restored_from? }`.
- **Raft 명령**: `Command::Attributed { meta: ChangeMeta, command: Box<Command> }` — 안의 명령을 그대로 적용하고 이력을 쓴다. `at_ms`는 **리더가 제안 직전에 찍는다**(`LeaderRequest::Write`를 받은 리더 — 따르미는 빈 값을 보냄), 그래서 상태 머신 적용은 결정적이다. 기능 `config-history`가 모든 노드에 있을 때만 쓰고, 없으면 지금 명령 그대로(옛 노드가 모르는 명령을 받지 않음).
- **Task**(`TaskOp`): `Apply { set?: Uuid, plan: Uuid }`(`apply` — 계획을 실행, 받은 노드에서, 시간 제한 없음, 이어 가지 않음 — 끊기면 실패로 끝나고 다시 적용하면 남은 것만 함), `AcmeOrder { certificate, node? }`(`acme-order` — 노드마다면 그 노드에서, 공유면 리더가 고른 노드). 줄은 대상(인증서 하나에 주문 하나).
- **기능**: `declarative`(apply·manifests·apply-set), `config-history`(Attributed·이력 API·restore), `acme`(새 ACME 종류·HTTP-01 리스너·설치). `FEATURES` 끝에 셋. `Kind::ALL` 43 → 47(`apply-set`·`acme-account`·`acme-dns`·`certificate`), `WATCHABLE`에 넷(ACME 셋은 가린 JSON).
- **설정**: `SettingsSpec.history?: { max_per_resource: 100(10~1000), max_age_days: 365(7~3650), max_total_mib: 64(8~512) }`, `SettingsSpec.acme?: { http_redirect: true, renew_jitter: true }`.
- **경보**: `AlertCondition::CertificateExpiring { days: 14 }`(ACME·손 파일·내부 노드 인증서 모두 — 노드 평가), `CertificateRenewFailed { failures: 3 }`(리더), `ApplySetDrift`(리더).
- **권한**: **새 권한은 없다.** apply·restore의 쓰기는 단계마다 그 종류·대상의 지금 권한(VM이면 폴더의 `vm.create`·`vm.config`·…)을 본다. 이력 읽기는 그 리소스를 지금 읽을 수 있으면(폴더 종류는 `system.read`, 그 밖은 `kind_privilege(Read)`), 지운 리소스는 마지막 폴더에서. ACME 셋은 `cluster.admin`(쓰기)·`cluster.read`(가린 읽기), 적용 묶음 만들기·지우기는 `cluster.admin`이 아니라 루트 `system.read` + 단계 권한(§13).

## 7. 변경 기록 — 출처를 단 명령과 상태 머신 이력 표

### 7.1 출처 잇기

- `opl-cluster`에 작업 지역 값 `ChangeContext`(tokio `task_local!`)를 두고 `Cluster::write`가 읽어 `Attributed`로 싼다. API 미들웨어(인증 뒤, 감사 층과 같은 곳)가 요청마다 `{actor(사용자·토큰 이름), source, reason, request(감사 id — 감사 층이 요청 시작에 id를 만들어 함께 씀)}`를 채운다. `source`는 머리 `Orbivisor-Client`(`cli/0.1.0`·`terraform/0.1.0`·`web`, 64자, 영숫자·`/._-`만 — 아니면 `api`), `reason`은 머리 `Orbivisor-Change-Reason`(200자, 제어 문자 뺌). 웹은 세션이면 `web`.
- Task가 쓰는 spec(VM 마이그레이션의 `node` 등)은 Task 실행기가 `{actor: Task를 만든 사용자, task: id, source: task}`로 감싸고, 리더 루프·reconciler의 spec 쓰기는 `System`. status 쓰기(`UpdateStatus*`)는 이력이 없으므로 싸지 않는다(로그 크기 그대로).
- 값이 없는 쓰기(옛 길·시험)는 `System`으로 기록된다 — 이력이 빠지지 않는다.

### 7.2 무엇을 기록하나

- 적용한 명령이 `Create`·`UpdateSpec`·`Delete`이고 종류가 **기록 종류**이면 줄 하나: `Create` — 만든 spec, `UpdateSpec` — **spec이나 labels가 실제로 바뀐 때만**(같은 값 쓰기는 줄 없음 — 정규 JSON 비교), `Delete` — 지우기 직전 spec(살리기에 씀). 이름 바꾸기도 줄이다.
- 기록 종류: vm, storage-pool, volume, network, security-group, backup-job, settings, realm, remote, ha-group, placement-rule, node-fence, ha-check, alert-rule, notification-target, folder, grant, access-role, group, router, floating-ip, load-balancer, backup-store, vmware-source, apply-set(spec만), acme-account, acme-dns, certificate, user. 빼는 것: task, session, join-token, node, cluster(CA 키), vip-holder, snapshot·backup·vm-import·vm-export·vmware-migration·vmware-vm·network-migration(작업 기록 — 자기 status가 이력), api-token·two-factor(비밀 덩어리), sdn·ceph·cpu-catalog(클러스터 설치 상태). 표는 `Kind::history_tracked()` 하나(시험이 `Kind::ALL` 전부를 넣었는지 뺐는지 정하게 함 — 새 종류가 슬쩍 빠지지 않음).
- **비밀 칸**: `opl-types` 순수 함수 `history_mask(kind, prev: Option<&Value>, next: &Value) -> Value`가 종류별 비밀 경로(사용자 `password_hash`, realm `bind_password`·`client_secret`, 펜스 BMC 비밀번호, 원격 토큰, 알림 SMTP·웹훅 토큰, 백업 저장소 키·S3 비밀, VMware 비밀번호, ACME EAB·계정 키·DNS 비밀, VM cloud-init 비밀번호 해시)를 `{"$secret": "set|same|changed|cleared"}`로 바꾼다 — 앞 줄의 평문과 지금 평문을 상태 머신 안에서 견주므로 **해시·지문조차 남지 않는다**(낮은 엔트로피 비밀번호의 오프라인 대입을 막음). 경로 표는 보기 가림 함수와 같은 곳에 두고, 시험이 "가림 함수가 가리는 칸 ⊆ 이력이 가리는 칸"을 본다.

### 7.3 저장과 보존

- redb 표 `history`((종류, id u128, revision) → lz4 압축 JSON), `history_seq`(revision → (종류, id) — 시간 차례 타임라인·전역 한도), `meta.history_bytes`·`history_count`. 상태 머신은 이 표를 같은 거래에서 쓴다.
- 보존은 **적용 때 결정적으로**: 줄을 넣은 뒤 그 리소스의 줄이 `max_per_resource`를 넘으면 가장 오래된 것부터 지움(지운 리소스의 마지막 `Delete` 줄은 나이 한도까지 남김 — 살리기), 그리고 묶음마다 `history_seq` 앞에서 최대 256줄을 보며 `at_ms < 지금 묶음의 at_ms − max_age` 또는 `history_bytes > max_total`인 줄을 지운다. "지금"은 명령의 `at_ms`(리더가 찍음)이지 노드 시계가 아니다. 한도는 Raft의 설정 리소스에서 읽는다(같은 상태라 결정적).
- **스냅샷**: `SnapshotData`에 `history: Vec<(Kind, Uuid, u64, Bytes)>`(`#[serde(default, skip_serializing_if = "Vec::is_empty")]`). 기능이 켜지기 전에는 비어 옛 노드는 이 칸을 보지 않는다. 기본 한도 64 MiB는 스냅샷 보내기(1 MiB 조각)에 몇 초 더한다(§14에서 잰다). 스키마 판은 1 그대로(새 표는 열 때 만든다 — 없으면 빈 이력).
- **잰 값 목표**: VM spec 평균 2 KiB(lz4 뒤 ~0.7 KiB) — 64 MiB면 약 9만 줄. 1 000번 바꾸기 적용 비용 증가 < 5%(시험이 잰다 — §14).

## 8. 선언형 적용 엔진 (`crates/opl-api/src/declarative/`)

### 8.1 종류와 어댑터

```rust
trait Declarable {
    const KIND: Kind; const MANIFEST: ManifestKind;
    fn key(m: &Manifest) -> Key;                       // 이름(+ 폴더 종류는 폴더 경로가 아니라 이름이 전역 고유 — 지금 규칙)
    fn refs(spec: &Value) -> Vec<Ref>;                  // 참조: 폴더·네트워크·풀·볼륨·보안 그룹·라우터·이미지·노드·역할·사용자…
    fn normalize(spec: Value, ctx: &Resolve) -> Result<Value>;  // 이름 → id, 기본값 채움, 목록 순서 고정
    fn to_manifest(live: &RawResource, ctx: &Names) -> Manifest; // id → 이름, 비밀 → secret_ref
    fn immutable() -> &'static [&'static str];          // 바꿀 수 없는 칸(JSON 경로)
    async fn check(...), create(...), update(...), delete(...);  // 지금 처리기 안 함수
}
```

| 종류 | 의존(먼저) | 바꿀 수 없는 칸 → 막힘 | 비고 |
|---|---|---|---|
| Folder | 부모 Folder | — | 할당량 칸 포함(`inventory.quota`) |
| StoragePool | — | `kind`·노드·경로 | 만들기는 풀 종류마다 지금 검사 |
| Network | Folder, Router(SDN 외부) | `kind`(simple/SDN)·VLAN id | SDN 서브넷·DHCP·QoS 칸 |
| Router·FloatingIp | Network | 외부 네트워크 | |
| SecurityGroup | Folder | — | 규칙 목록 통째 |
| LoadBalancer | Network, Vm(구성원) | VIP 서브넷 | |
| Volume | StoragePool, Folder | `pool`·`format`, `size` 줄이기 | 늘리기는 `resize` 길, 이미지·ISO 출처 |
| Vm | Folder, Volume, Network, SecurityGroup, 이미지·템플릿 | `arch`, 템플릿 여부 | 핫플러그 아닌 바꿈은 "다음 켤 때"(계획 54 미뤄진 변경) 표시, `state` |
| PlacementRule·HaGroup·HaCheck | Vm, Node | — | |
| BackupStore·Remote | — | 저장소 백엔드 종류 | 비밀 칸 |
| BackupJob | Vm·Folder 선택, BackupStore | — | |
| AccessRole·Group·Grant | Folder·User·Group·AccessRole | Grant는 전체가 열쇠(바꾸기 = 지우고 만들기 — 데이터 없음이라 허용) | |
| AlertRule·NotificationTarget·Realm | NotificationTarget | realm 종류 | 비밀 칸 |
| Settings | — | — | 하나뿐(`metadata.name: cluster`), 지우기 없음 |
| AcmeAccount·AcmeDns·Certificate | AcmeAccount·AcmeDns | 계정 `directory` | 계정 `key`는 서버가 만듦 |

뺀 것(내보내기에도 없음): 노드·Task·스냅샷·백업·가져오기·이전·내보내기·SDN 설치·Ceph·사용자 비밀번호·토큰·2FA. 사용자(`User`)는 이름·역할·realm 칸만 다루는 갈래를 나중으로 미룬다(§16) — 비밀번호를 파일에 두는 길을 만들지 않는다.

### 8.2 계획 세우기

1. **문서 검증**: `api_version`·`kind`, 종류의 spec 꼴(만들기 본문 타입으로 `deny_unknown_fields` 역직렬화 — 칸 이름 틀리면 400 `manifest_invalid` + 문서 번호·경로), 같은 (종류, 이름) 두 번이면 400 `manifest_duplicate`. 문서 1 000개·본문 8 MiB.
2. **권한 먼저**(계획 71 리뷰 2 교훈): 단계마다 그 종류·대상 폴더의 쓰기 권한을 보고 없으면 그 단계를 `blocked: forbidden`으로 두고 **그 단계의 나머지 검사를 하지 않는다**(숨은 참조 대상의 존재를 새지 않음 — 볼 수 없는 네트워크 이름은 "없음"과 같은 문구).
3. **참조 풀기**: 이름 → id를 실제 상태 + **같은 계획 안에서 만들 것**에서. 없으면 `blocked: missing_reference{kind,name}`. 그래프(참조 = 먼저)를 위상 정렬하고, 고리는 400 `manifest_cycle`(경로를 보임). 같은 층은 §8.1 표의 종류 차례.
4. **비교**: 지금 객체가 없으면 `create`. 있으면 `normalize(선언)`과 `normalize(to_manifest(실제))`를 **선언한 칸만** 견준다(선언하지 않은 칸은 서버 값 그대로 — 사람이 웹에서 고친 칸을 apply가 덮지 않음). 단, 적용 묶음의 `declared`에 있었는데 이번 문서에서 빠진 칸은 기본값으로 되돌린다(**세 갈래** — kubectl의 last-applied와 같은 뜻, 저장 자리는 묶음 status). 목록(디스크·NIC·규칙)은 통째로 견준다. 비밀 칸: `secret_ref`면 견주지 않음, 평문이면 서버가 지금 평문과 견줘 "바뀜/같음"만 알림. 바꿀 수 없는 칸이 다르면 `blocked: immutable{path}` + 손으로 할 일 문구.
5. **미리 확인**(쓰지 않음): 종류의 `check` — VM은 `prepare_with(Planned{volumes: 이 계획이 만들 볼륨, rules: 만들 배치 규칙})`(할당량·용량·노드·MAC/IP·CPU 모델), 볼륨은 풀 여유·형식, 네트워크는 독점 키(VLAN·physnet·서브넷 겹침)를 지금 상태 + 앞선 단계로, 이름 겹침. 기능 게이트(종류의 기능이 없으면 `blocked: cluster_upgrading`).
6. **가지치기**: `apply_set`과 `prune`이면 묶음의 `objects`에 있고 레이블이 남아 있는데 문서에 없는 것 → `delete`(역순). VM·볼륨·백업 저장소는 `protect_data`면 `--prune-data` 없이는 `blocked: protected`. 주석 `orbivisor.io/prune: "false"`인 것은 건너뜀.
7. **충돌**: 레이블 `orbivisor.io/managed-by=terraform`인 객체를 apply가 바꾸려 하면 `blocked: managed_elsewhere`(`--force-conflicts`로 넘김), 반대로 Terraform이 다른 묶음의 객체를 바꾸면 같은 갈래. 관리자는 객체 통째 단위(k8s managedFields 같은 칸 단위는 §15).

응답: `{ plan: Uuid, base_revision, steps: [{ n, kind, name, action: create|update|unchanged|delete|blocked, diff: [{ path, op: add|remove|replace, old?, new? }], problems: [{ code, message, path? }], restart?: true, after: [단계 번호] }], summary: { create, update, delete, unchanged, blocked } }`. 비밀은 diff에 `"(changed)"`만.

### 8.3 저장한 계획과 적용

- 미리 확인 응답의 `plan`은 리더 노드 메모리가 아니라 **받은 노드 메모리**에 30분(문서 해시·단계·각 단계가 본 `revision`·사용자). `POST /apply {plan}`은 같은 사용자만, 문서 해시가 같아야 하고, 단계가 본 객체의 revision이 하나라도 바뀌었으면 412 `plan_stale`(다시 계획하라 — Terraform 저장 계획과 같은 약속). 다른 노드로 가면 404 `plan_not_found` → CLI가 같은 서버 주소로(로드밸런서 뒤면 `--plan` 대신 바로 적용을 권함 — 가이드).
- `dry_run` 없이 `plan` 없이 보내면 계획 → 막힌 단계가 있으면 하나도 하지 않고 409 `plan_blocked` + 계획, 없으면 Task `apply`.
- Task `apply`: 단계를 차례로 — 권한 다시 보기(그사이 권한이 빠졌으면 그 단계 실패) → `If-Match`(계획 때 revision) → 어댑터 쓰기(`ChangeContext{source: apply|terraform, apply_set}`) → 그 쓰기가 만든 Task(볼륨 만들기·VM 켜기)를 기다림 → 다음. 첫 실패에서 멈추고 Task `progress`에 "n/m, 실패 단계, 오류". 성공한 단계는 되돌리지 않는다 — **다시 적용하면 남은 것만** 한다(idempotent, 가이드에 적음). 끝나면 묶음 status(`declared`·`objects`·`applied_revision`)를 갱신하고 레이블을 붙인다(묶음 갱신은 성공한 단계까지만 — 실패한 단계의 선언은 다음 적용이 다시 본다).
- VM `state: running|stopped`: 만든 뒤 켜기·끄기(게스트 종료, 600초 넘으면 실패), 이미 그 상태면 할 일 없음. 핫플러그 아닌 칸을 바꾼 켜진 VM은 `restart: true`로 알리고 다시 켜지 않는다(`--restart`가 있으면 그 VM만 껐다 켬 — 기본 없음).

### 8.4 내보내기

`GET /manifests?kinds=&folder=&apply_set=&minimal=` → `{ revision, documents: [Manifest] }`(읽을 수 있는 것만 — 목록 권한과 같음), `GET /manifests/{kind}/{key}`(하나 — Terraform Read). 순서는 적용 차례, id 대신 이름·폴더 경로, 비밀은 `secret_ref`, status는 뺀다. 서버가 채운 MAC은 **늘 내보낸다**(다른 클러스터에 다시 만들어도 같은 MAC — DHCP 예약·라이선스, 계획 29의 `keep_macs`와 같은 까닭; 새 MAC을 원하면 `--minimal`이 서버가 만든 MAC을 뺀다). `minimal`은 기본값과 같은 칸을 뺀다(사람이 읽을 파일). 내보낸 것을 그대로 같은 클러스터에 `apply`하면 모든 단계가 `unchanged`(시험 — 왕복 불변식).

## 9. 매니페스트 파일과 CLI

```yaml
apiVersion: orbivisor.io/v1
kind: Network
metadata: { name: web-net, folder: /prod/web }
spec:
  sdn: { subnets: [{ cidr: 10.20.0.0/24, gateway: 10.20.0.1, dhcp: true }] }
---
apiVersion: orbivisor.io/v1
kind: Volume
metadata: { name: web01-root, folder: /prod/web }
spec: { pool: ceph-rbd, size: 40Gi, source: { image: rocky-10-cloud } }
---
apiVersion: orbivisor.io/v1
kind: Vm
metadata: { name: web01, folder: /prod/web, labels: { tier: web } }
state: running
spec:
  vcpus: 4
  memory: 8Gi
  disks: [{ volume: web01-root, bus: virtio-blk }]
  nics: [{ network: web-net, security_groups: [web-sg] }]
  cloud_init: { user: rocky, ssh_keys: ["ssh-ed25519 AAAA…"], password: { secretFrom: { env: WEB_PW } } }
```

- 크기 칸은 `40Gi`·`512Mi` 문자열도 받는다(CLI가 바이트 수로 바꿈 — 서버는 숫자만). 파일은 `*.yaml`·`*.yml`·`*.json`, 디렉터리는 이름 차례로 다시 내려가지 않음(`-R`이면 내려감), `-f -`는 표준 입력.
- `orbivisorctl apply -f PATH… [--set NAME] [--prune [--prune-data]] [--force-conflicts] [--restart] [--reason TEXT] [--yes] [--wait/--no-wait]` — 계획을 보이고(색 `+`·`~`·`-`·`!`) 묻고(`--yes`면 안 물음, 단말이 아니면 `--yes` 필요) 적용. `orbivisorctl plan -f … [--out plan.json]`(계획 id·문서 해시·서버 주소를 파일에), `orbivisorctl apply --plan plan.json`. `orbivisorctl diff -f …`(끝 코드 0 같음·1 다름·2 오류 — `kubectl diff`와 같은 약속, CI에서 씀). `orbivisorctl export [--kind K…] [--folder /p] [--set S] [--minimal] [-o DIR|-]`(DIR이면 종류마다 파일 `10-folders.yaml`·`20-networks.yaml`… — 적용 차례 번호). `-o json`은 모든 명령에 그대로.
- `orbivisorctl set list|get|delete NAME`(묶음 지우기는 리소스를 지우지 않고 레이블만 뗀다), `drift [SET]`(§12.5).
- YAML은 CLI의 `serde_yaml_ng`가 JSON 값으로 바꿔 보낸다(서버·웹은 YAML을 읽지 않는다). 한 파일 여러 문서(`---`), 앵커·별칭은 받되 펼친 크기 8 MiB 한도(빌리언 래프 막기).

## 10. Terraform/OpenTofu 공급자 (`terraform/`)

### 10.1 언어 결정: Go terraform-plugin-framework(채택)

- **까닭**: 계획 의미(unknown 전파·plan modifier·import·상태 판 올리기·쓰기 전용 칸·정체성)를 HashiCorp가 유지하는 프레임워크가 지고, 수용 시험 도구(`terraform-plugin-testing`)·문서 생성(`tfplugindocs`)·릴리스 도구(GoReleaser — 레지스트리 꼴 zip·SHA256SUMS·서명)가 그대로 있다. 사용자·기여자가 아는 꼴이라 유지 비용이 낮다.
- **Rust tfplugin6(버림)**: tonic이 있어도 우리 것은 protobuf 없는 손 코덱이다 — prost·protoc·cty msgpack·go-plugin 손맞춤·자동 mTLS를 더하고, 프레임워크가 주는 의미를 3~5천 줄로 우리가 소유한다. 같은 타입(`opl-types`)을 쓸 수 있다는 이점은 **서버가 계획한다(§5)** 는 설계로 대부분 사라진다 — 공급자는 매니페스트 꼴을 옮기는 얇은 층이다.
- **대가**: 두 번째 언어(Go 1.24+ — 모듈 `go.mod`의 `toolchain`으로 고정), 따로 도는 CI 작업, 의존성(프레임워크·plugin-go·plugin-testing — MPL-2.0, 공급자 자체는 Apache-2.0 — 따로 빌드되는 실행 파일이라 섞이지 않음, 가이드·`terraform/LICENSE`에 적음). Rust 쪽 `cargo deny`와 별개로 `go mod verify`·`govulncheck`를 작업에 둔다.

### 10.2 꼴

- `terraform/`(모듈 `github.com/gaiger777/opLite/terraform`, 실행 파일 `terraform-provider-orbivisor`, 주소 `registry.terraform.io/gaiger777/orbivisor` — 게시는 사용자 몫, 개발은 `dev_overrides`). `internal/client`(매니페스트 API 손님 — `POST /apply`·`GET /manifests/{kind}/{key}`·Task 기다리기·`Orbivisor-Client: terraform/{판}` 머리, 토큰은 `api_token`·`OPL_TOKEN`, CA는 `ca_pem`·`insecure` 없음 — 지문 고정 `ca_fingerprint` 선택), `internal/provider`(공급자 설정 `endpoint`·`api_token`·`ca_pem`·`timeout`), `internal/resources`(종류마다 스키마 + 공통 CRUD 하나 — 스키마만 다르고 동작은 "매니페스트 하나 적용").
- **리소스(15)**: `orbivisor_folder`, `orbivisor_network`(simple·SDN 서브넷), `orbivisor_router`, `orbivisor_floating_ip`, `orbivisor_security_group`, `orbivisor_load_balancer`, `orbivisor_volume`, `orbivisor_vm`(`state` 칸 포함), `orbivisor_placement_rule`, `orbivisor_ha_group`, `orbivisor_backup_job`, `orbivisor_backup_store`, `orbivisor_grant`, `orbivisor_access_role`, `orbivisor_certificate`. **데이터 소스(6)**: `orbivisor_node(s)`, `orbivisor_storage_pool`, `orbivisor_image`, `orbivisor_template`, `orbivisor_network`, `orbivisor_folder`.
- **CRUD**: Create·Update = `POST /apply {documents:[하나], manager: "terraform"}` → Task를 기다림 → `GET /manifests/{kind}/{id}`로 상태. Read = `GET /manifests/{kind}/{id}`(404면 상태에서 뺌 — 밖에서 지운 것). Delete = 매니페스트 지우기 `DELETE /manifests/{kind}/{id}`(지금 처리기 지우기 — VM은 끄고 지우는 지금 규칙, 볼륨은 붙어 있으면 409). Import = 이름 또는 id(`terraform import orbivisor_vm.web web01`, 정체성(identity) 스키마 `{name}` — Terraform 1.12+ `import` 블록).
- **계획 때 확인**: `ModifyPlan`에서 `POST /apply?dry_run {documents:[하나]}`를 불러 `blocked` 문제를 진단(error)으로, `restart: true`를 경고로 낸다. 같은 계획 안 다른 리소스가 아직 unknown이면(만들 네트워크 이름 등) 그 검사는 건너뛴다 — 참조 이름이 알려졌을 때만.
- **비밀**: VM cloud-init 비밀번호·백업 저장소 키·S3 비밀·ACME EAB는 **쓰기 전용 칸**(`password_wo` + `password_wo_version`, 상태에 남지 않음 — Terraform 1.11+). 가정이 틀린 판(OpenTofu)에서는 `sensitive` 칸으로 내린다(§18).
- **관리자 표시**: 공급자가 만든 객체에 레이블 `orbivisor.io/managed-by=terraform`(§8.2 충돌). 웹은 그 객체 상세에 "Terraform이 관리" 칩과 "바꾸면 다음 plan에서 되돌려짐" 안내.
- **스키마 맞춤 시험**: Go 시험이 `opl-api-openapi`가 낸 OpenAPI의 매니페스트 스키마(§13)를 읽어 각 리소스 스키마의 칸 이름·타입·필수 여부가 맞는지 본다 — 서버 칸이 늘었는데 공급자가 모르면 실패(빠진 칸 목록).

### 10.3 개발 서버 `opl-devserver`

opld 수용 시험·Terraform·웹 데모가 함께 쓰는 실행 파일(`crates/opld`, 기능 `devserver`, `required-features`): 임시 디렉터리에 노드 하나(가짜 스토리지 `FakeStorage`·가짜 드라이버 — QEMU 없이 VM이 "켜짐"까지 감, opl-api 시험과 같은 바탕), admin 토큰과 CA를 만들어 `{"url","token","ca_pem"}` 한 줄을 찍는다(`fake-vsphere`와 같은 꼴). 기능 셋이 켜져 있다. Go `TestMain`이 이것을 띄우고 `TF_ACC=1`로 시험한다. 실제 QEMU 시험은 §14의 opld `qemu-it`가 맡는다.

## 11. ACME 인증서 (`crates/opl-api/src/acme/`)

### 11.1 클라이언트(손으로 짠 RFC 8555)

- `client.rs`: 디렉터리(별명 표 + URL), nonce 통(응답 `Replay-Nonce`를 모아 둠, `badNonce`면 3번까지 새 nonce로), JWS(`ring` `ECDSA_P256_SHA256_FIXED_SIGNING` — r‖s 64바이트가 ES256 그대로, `alg`·`nonce`·`url`·`jwk|kid`), JWK thumbprint(RFC 7638 — 사전 차례 `{"crv","kty","x","y"}`), EAB(HS256), `newAccount`(이미 있으면 200 + 같은 URL — 다시 해도 같음), `newOrder{identifiers, profile?, replaces?}`, 인가·도전·물음(`Retry-After` 존중, 최대 10분), `finalize`(rcgen CSR — 키는 인증서 `key` 종류), 사슬 받기(`Link: rel="alternate"`의 다른 사슬 중 `preferred_chain` 루트 CN), ARI(`renewalInfo` — id는 x509-parser로 AKI·일련번호). HTTP는 reqwest(우리 꾸러미의 native-tls), `ca_bundle`이 있으면 그 루트만 믿음(`tls_certs_only` — 알림 웹훅과 같은 길). 응답 본문 1 MiB 한도.
- 오류는 ACME problem document(`type` urn:ietf:params:acme:error:…)를 `AcmeError::{RateLimited{retry_after}, BadNonce, Unauthorized, Dns, Connection, Incorrect, Caa, RejectedIdentifier, ExternalAccountRequired, Server, Http}`로 나눠 status `error`에 사람 문구로("the CA could not reach http://web01.example.com/.well-known/acme-challenge/…: connection refused — is port 80 open and http_listen set on every node this name points to?").

### 11.2 누가 언제 주문하나

- **리더 `AcmeRenewer`(60초)**: 인증서마다 "할 일"을 순수 함수 `due(cert, nodes, now)`로 정한다 — 받은 것이 없음, 이름 바뀜(노드 이름·주소 펼침 포함), `renew_at` 지남(ARI 창이 있으면 창 안의 무작위 시각 — 처음 정할 때 한 번 뽑아 status에, 없으면 `not_after − 유효 기간/3`, `DaysBefore`면 그대로), 키 종류 바뀜. 할 일이 있고 `next_attempt_at`이 지났으면 Task `acme-order`를 만든다(인증서·노드 줄 — 겹치지 않음). 클러스터 전체 동시 주문 2개. ARI는 하루 한 번(또는 `Retry-After`) 다시 묻는다.
- **노드마다(`PerNode`)**: Task는 그 노드에서 돈다 — 키를 그 노드 디스크에 만들고(없거나 키 종류가 바뀌었을 때만), CSR, 주문, 사슬을 status `nodes[id]`에. 노드가 오프라인이면 건너뛰고 돌아오면 잇는다. 이름 예: `{node}.pve.example.com`·`{node_address}` — 노드마다 다른 인증서라 속도 한도의 "같은 이름 묶음"에 걸리지 않는다.
- **공유(`Shared`)**: VIP 이름(`api.example.com`)처럼 모든 노드가 같은 이름을 낼 때. Task는 리더가 고른 노드 하나에서 돌고, 키는 **Raft status의 `Secret`**(보기·watch·감사·이력에서 뺌 — 원격 토큰·S3 비밀과 같은 자리)이며 모든 노드가 읽어 끼운다. 한 번 받아 나누므로 속도 한도가 노드 수와 무관하다.
- **물러서기**: 실패마다 `failures += 1`, `next_attempt_at = now + min(5분 × 2^(failures−1), 6시간)`, `RateLimited{retry_after}`면 그 값. 3번이면 경보 `CertificateRenewFailed`. 받은 인증서가 아직 살아 있으면 계속 쓴다.
- **리더 바뀜**: 할 일은 Raft status로만 정하므로 새 리더가 다음 틱에 잇는다. 진행 중 주문 URL을 status에 적어 두어 Task가 다시 뜨면 새 주문 대신 그 주문을 이어 본다(주문은 CA 쪽에 7일 산다).

### 11.3 HTTP-01

- 선택 리스너 `[api] http_listen = "0.0.0.0:80"`(opld 설정 — 노드가 기능 `acme`와 함께 `http01` 능력을 노드 status에 알림). 하는 일은 둘뿐: `GET /.well-known/acme-challenge/{token}` → 인증서 status `challenges`에서 찾은 `key_authorization`(text/plain, 없으면 404), 그 밖은 설정 `acme.http_redirect`면 `308 https://{Host}:8006{path}`, 아니면 404. 본문을 읽지 않고, 요청 머리 8 KiB·연결당 한 요청·초당 100 요청 한도.
- 도전 값은 Raft에 둔다 — 이름이 VIP·DNS 라운드 로빈으로 **어느 노드를 가리켜도** 답한다(Proxmox는 주문한 노드가 포트 80을 잠깐 연다 — VIP 이름이면 그 노드가 VIP를 들고 있어야 한다). 주문 Task가 값을 적고(`until` = 10분 뒤) 검증이 끝나면 지운다.
- 미리 확인: HTTP-01 인증서를 만들 때 대상 노드(공유면 모두)가 `http01`을 알리지 않으면 400 `http01_unavailable`("set [api] http_listen on node X"). 스스로 확인: 주문 전에 각 이름을 우리 쪽에서 `http://{이름}/.well-known/acme-challenge/{무작위}`로 불러 보고(값을 먼저 적음) 실패하면 CA에 묻지 않고 실패(검증 실패 속도 한도를 아낌, 끄는 칸 `precheck: false`).

### 11.4 DNS-01

- 공급자 trait `DnsProvider { add(fqdn, value) -> Handle, remove(Handle) }`, 다섯 갈래: **Cloudflare**(API 토큰, 영역 찾기 `GET /zones?name=`, TXT 만들기·지우기), **Route 53**(SigV4 — `opl-store` `backend::sigv4`를 공개 함수로 꺼내 씀, `ChangeResourceRecordSets` UPSERT/DELETE, `GetChange` INSYNC까지), **RFC 2136**(손으로 짠 DNS UPDATE 메시지 + TSIG(RFC 8945) HMAC-SHA256/512 — ring, TCP 53, 영역은 SOA 질의로 찾음), **PowerDNS**(`PATCH /api/v1/servers/{id}/zones/{zone}` rrsets), **훅**(`{path} add|remove {fqdn} {value}`를 그 노드에서 — 경로는 `/etc/opl/acme-hooks/` 아래 root 소유·다른 사용자 쓰기 불가만, 환경은 비우고 60초 한도, 표준 출력·오류 4 KiB만 기록 — acme.sh·lego의 수많은 공급자를 이 길로 잇는다, 가이드에 예).
- **전파 확인**(`Authoritative`): 이름의 영역 NS를 찾아 그 서버들에 직접 TXT를 묻고(손으로 짠 최소 DNS 질의 — UDP, 잘리면 TCP; RFC 2136 코드와 메시지 부호화를 함께 씀) 모두 보이면 도전을 알린다. CNAME 위임을 따라간다(위임된 이름의 공급자 `zones`로 고름). `timeout_s` 넘으면 실패.
- 와일드카드·내부망(포트 80이 밖에서 닿지 않는) 클러스터는 DNS-01이 기본 권고다(가이드).

### 11.5 끼우기

- `ApiCertAgent`가 인증서 차례를 정한다: (1) `[api] tls_cert` 파일(손 — 늘 이김, 지금과 같음), (2) `install: Api`이고 이 노드를 덮는(노드마다면 `nodes[이 노드]`, 공유면 `shared`) 살아 있는 ACME 인증서 — 키는 노드 파일 또는 Raft, (3) 내부 CA 인증서(`api_names`). watch로 인증서·설정 바뀜을 보고 `ApiCert::replace` — 열린 연결은 그대로, 새 손맞춤부터. 사슬과 키가 맞지 않거나 이 노드 이름이 빠졌으면 끼우지 않고 경고(status `nodes[id].error`).
- 한 리스너에 인증서 하나(SNI 무시 — 지금 그대로). 노드마다 인증서 + VIP 공유 인증서를 **둘 다** 내려면 SNI 고르기가 필요하다 — `ResolvesServerCert`가 `client_hello.server_name()`으로 고르게 바꾼다(이름이 맞는 ACME 인증서 → 그 밖은 차례 1~3). 작은 바꿈이라 이 계획에 넣는다.
- 내부 8007(mTLS)은 건드리지 않는다. `orbivisorctl login`은 공개 CA 인증서면 `--system-ca`로 붙는다(지금 칸).

### 11.6 만료 경보

노드 평가 `CertificateExpiring{days}`: 이 노드 공개 리스너의 지금 인증서(출처가 무엇이든)와 **내부 노드 인증서**(824일 — 지금 자동 갱신 없음)의 `not_after`가 `days` 안이면 경보. 내부 인증서 갱신 자체는 §16이지만, 적어도 모르고 지나가지 않게 한다.

## 12. 설정 이력 보기·되돌리기·드리프트

### 12.1 API

- `GET /history?kind=&key=&folder=&user=&source=&apply_set=&since=&until=&before=&limit=`(기본 100, 최대 1 000) — 타임라인(`history_seq` 거꾸로). 줄마다 `{revision, kind, id, name, op, at, actor, source, reason, request, apply_set, changed: [경로 최대 20]}`(spec 본문 없음 — 가벼움). 지금 읽을 수 있는 것만(지운 것은 마지막 폴더 기준), 거른 뒤 수를 채운다(쪽 열쇠 `before=revision`).
- `GET /history/{kind}/{key}` — 그 리소스의 줄(지운 리소스는 id로). `GET /history/{kind}/{key}/{revision}?compare=prev|live|{revision}` — 전체 spec(보기 가림 그대로 — 비밀은 `$secret` 표시) + 비교(`[{path, op, old, new}]` — §8.2와 같은 비교 함수). 감사 줄 링크 `request`.

### 12.2 하나 되돌리기

`POST /history/{kind}/{key}/restore { revision, dry_run?, reason? }`: 그 줄의 spec → 매니페스트(`to_manifest`) → §8의 계획(문서 하나, 묶음 없음, 이 리소스 하나만) → 막힘이 없으면 Task `apply`(meta `restored_from = revision`, source 유지). 비밀 칸: 그 줄이 `same`·`set`이면 **지금 비밀을 그대로 둔다**(이력에 평문이 없으니 되돌릴 수 없음 — 경고 "secrets are not restored; the current password stays"). 지금 객체의 `If-Match`. 바꿀 수 없는 칸이 다르면 막힘(예: 볼륨 풀). 응답은 계획과 같은 꼴 — 웹·CLI가 diff를 보이고 확인을 받는다.

### 12.3 지운 리소스 살리기

마지막 줄이 `Delete`인 리소스에 restore: 그 직전 spec으로 **같은 id**의 `Create`(어댑터 `create_with_id` — 다른 리소스가 id로 가리키던 참조가 다시 살아남), 이름이 이미 쓰이면 409 `name_taken`(본문 `name`으로 바꿔 살림 — 그러면 새 id). **데이터 종류는 막는다**: 볼륨(디스크 내용이 없다 — "restore it from a backup")·백업 저장소(저장소 초기화가 따로)·VM은 그 디스크 볼륨이 모두 살아 있을 때만(지워졌으면 그 볼륨들을 백업에서 먼저 — 막힘 문구에 목록).

### 12.4 한 시점으로 되돌리기(여러 개)

`POST /history/restore { at: revision | time, scope: { kinds?, folder?(그 아래 모두), apply_set?, keys? }, recreate_deleted: true, delete_created: false, dry_run? }`: 범위 안 리소스마다 `at` 시점의 spec(그 시점 이전 마지막 줄; 그때 없었으면 "그 뒤 만든 것")으로 **문서 묶음**을 만들고 §8 계획을 세운다 — 바뀐 것은 update, 그 뒤 지운 것은 살리기(§12.3 규칙), 그 뒤 만든 것은 `delete_created`면 delete(VM·볼륨은 `--prune-data` 같은 확인 칸 `allow_data_loss`). 의존 차례·권한·미리 확인·저장한 계획 토큰이 그대로다. 보존 한도로 `at` 시점 줄이 없는 리소스는 `blocked: history_expired`. **vCenter·Proxmox에 없는 기능**이다(§1 표) — "어제 17시의 /prod/web 폴더 설정으로" 같은 일.

### 12.5 드리프트

- 리더 `DriftChecker`: 5분마다 + 묶음에 든 리소스의 watch 사건을 10초 몰아 받아, 묶음마다 `declared`와 실제(`to_manifest` 정규화)를 §8.2의 "선언한 칸만" 비교로 견준다. 결과가 바뀔 때만 status `drift`를 쓴다(쓰기 폭주 없음). 상태: `Modified{paths}`, `Missing`(밖에서 지움), 비밀은 `secret_ref`가 아니면 "바뀜"만.
- **누가 바꿨나**: 다른 칸마다 `applied_revision` 뒤로 그 경로를 처음 바꾼 이력 줄을 찾아 `by`에(사용자·시각·출처·사유) — 화면이 "admin이 웹에서 11-24 15:02에 vcpus 4 → 8"로 보인다.
- 경보 `ApplySetDrift`(묶음 이름·수), CLI `orbivisorctl drift [SET]`(끝 코드 0 없음·1 있음), 해결 셋: 다시 `apply`(선언대로 되돌림), `POST /apply-sets/{key}/adopt { objects? }`(실제를 선언으로 받아들임 — 묶음 `declared`만 바꿈, 그 뒤 `export --set`으로 git 파일을 갱신하라는 안내), 그대로 둠.
- 묶음 없는 리소스는 드리프트가 없다(이력·타임라인은 있다). Terraform 관리 객체의 드리프트는 `terraform plan`이 본다(공급자 Read).

## 13. 권한·감사·버전 섞임·API·CLI·웹

| 것 | 권한 | 없을 때 |
|---|---|---|
| `POST /apply`(계획·적용) | 루트 `system.read` + **단계마다** 그 종류·대상의 지금 쓰기 권한(만들기·바꾸기·지우기), 묶음 만들기·가지치기는 묶음 단계에서 같은 규칙 | 그 단계 `blocked: forbidden`(숨은 것 새지 않음) |
| `GET /manifests` | 목록 권한(보이는 것만) | 목록에 없음 |
| 적용 묶음 보기·지우기·adopt | 보기 루트 `system.read`(묶음 안 객체는 보이는 것만), 지우기·adopt는 묶음 객체 모두의 쓰기 권한 | 403 |
| 이력 보기 | 그 리소스 읽기(지운 것은 마지막 폴더) | 목록에 없음·404 |
| 되돌리기·살리기·시점 되돌리기 | 단계마다 쓰기 권한(살리기 = 만들기 권한) | 단계 막힘 |
| ACME 계정·DNS·인증서 | 쓰기 루트 `cluster.admin`, 읽기 `cluster.read`(가린 보기) | 403 |

- **감사**: `apply wave1 (set web-prod: 3 create, 5 update, 1 delete, plan 0193…)`, 단계마다 지금 처리기의 감사 줄(같은 요청 id로 묶임 — Task가 쓰는 감사는 `system_entry`에 요청 id), `history restore vm web01 → r81234`, `history restore folder /prod/web at 2026-11-24T17:00 (12 objects)`, `acme-account create le (letsencrypt, tos agreed)`, `certificate create api-vip (shared, dns-01 cf)`, `acme-order api-vip issued (serial …, until …)`. EAB·계정 키·DNS 비밀·공유 키는 적지 않는다.
- **버전 섞임**: `apply`·`manifests`·`apply-sets`는 `declarative`, 이력·restore는 `config-history`, ACME 경로·HTTP-01 설치는 `acme`가 없는 노드가 있으면 409 `cluster_upgrading`. `Attributed` 명령은 `config-history`가 모두 있을 때만 쓴다 — 그 전 바뀜은 이력에 없다(가이드: "이력은 업그레이드를 마친 때부터"). 새 종류는 기능 뒤에서만 만들어진다.
- **API**(OpenAPI 태그 `declarative`·`history`·`acme`): `/apply`(POST, `?dry_run`), `/manifests`(GET), `/manifests/{kind}/{key}`(GET·DELETE), `/apply-sets`(GET), `/apply-sets/{key}`(GET·DELETE), `/apply-sets/{key}/adopt`, `/history`(GET), `/history/{kind}/{key}`(GET), `/history/{kind}/{key}/{revision}`(GET), `/history/{kind}/{key}/restore`(POST), `/history/restore`(POST), `/acme-accounts`·`/acme-dns`·`/certificates`(GET·POST, `{key}` GET·PATCH·DELETE), `/certificates/{key}/actions/renew`(지금 주문), `/acme-accounts/{key}/actions/check`. 매니페스트 스키마는 종류마다 `Manifest{Kind}`로 OpenAPI에 낸다(Terraform 스키마 맞춤 시험·웹 타입이 씀). 경로표에 모두(빠진 경로 시험).
- **CLI**: §9 + `orbivisorctl history [KIND NAME] [--user U] [--source S] [--since 24h] [--folder /p] [--set S]`(표 `REVISION TIME USER SOURCE OP KIND NAME CHANGED`), `history show KIND NAME REV [--compare prev|live|REV]`(색 diff), `restore KIND NAME --revision REV [--dry-run] [--yes] [--name NEW]`, `restore --at TIME|REV [--folder /p] [--set S] [--kind K…] [--delete-created --allow-data-loss] [--dry-run]`, `cert account add NAME --directory letsencrypt --email E --agree-tos [--eab-kid K --eab-hmac-file F] [--ca-bundle F]`, `cert dns add NAME --provider cloudflare|route53|rfc2136|pdns|hook …(비밀은 --*-file·환경 변수·프롬프트)`, `cert add NAME --account A --name N… [--per-node|--shared] [--http01|--dns01 DNS] [--key p256|p384|rsa2048] [--profile P]`, `cert list|get|renew|delete`. 모든 쓰기 명령에 `--reason`(머리 `Orbivisor-Change-Reason`), 머리 `Orbivisor-Client: cli/{판}`.
- **웹**(계획 61 규칙 — 새 사이드바 항목·탭 없음):
  - 활동 › 감사 탭을 **보기 셋**으로: `/audit`(admin — 지금 그대로) | `/history`(설정 기록 — 누구나, 보이는 리소스만) | `/apply-sets`(선언 묶음). 탭은 보이는 보기가 하나라도 있으면 보인다(`NavView`에 `admin?`을 더하고 탭의 `admin`을 보기로 내림 — `nav.ts`·`nav.test`). 설정 기록: 거르기 줄(종류·사용자·출처·기간·폴더), 시간 줄 목록(날짜 묶음, 출처 칩 — 웹·CLI·apply·Terraform·Task·system, 사유), 줄을 누르면 오른쪽 서랍 `ChangeDrawer`(전·후 칸 표 — 바뀐 경로만, 펼치면 전체 JSON, 감사 줄 링크, "이 revision으로 되돌리기…" → 계획 diff 확인 대화상자). "한 시점으로 되돌리기…" 단추 → 대화상자(시각·범위 고르기 → 서버 계획 표 → 확인, 데이터 잃는 단계는 체크로 따로 허락).
  - 선언 묶음: 목록(이름·객체 수·마지막 적용·누가·드리프트 칩), 상세 `/apply-sets/:name`(객체 표 — 상태 칩 같음·다름·없음, 다른 칸 펼침과 "누가 바꿨나", 고른 것 "실제를 선언으로 받아들이기", "YAML로 내보내기").
  - 리소스 상세(VM·볼륨·네트워크·보안 그룹·폴더·백업 작업·…)의 ⋯ 메뉴에 **"설정 기록…"**(그 리소스로 거른 `ChangeDrawer` 목록 서랍 — 같은 부품)과 **"YAML로 내보내기"**(`GET /manifests/{kind}/{key}` → 웹의 작은 YAML 쓰개 `toYaml` — 읽개는 없다, 복사·내려받기). 목록 화면 고른 줄 일괄 "YAML로 내보내기". Terraform이 관리하는 객체는 머리에 칩.
  - 설정 › 보안 탭 안 보기 `/settings/security` | `/settings/certificates`(ACME 계정·DNS 공급자·인증서 목록, 노드마다 만료·다음 갱신·오류 칩, "지금 갱신", 만들기 대화상자 — 계정(디렉터리 고르기·약관 동의·EAB), DNS 공급자(종류별 칸, 비밀은 저장 뒤 다시 보이지 않음), 인증서(이름·노드마다/공유·도전·키·프로필, HTTP-01이면 `http_listen` 없는 노드 경고)).
  - 화면은 늦게 읽는 조각 `web/src/history/`·`web/src/declarative/`·`web/src/certs/`, 문구는 늦게 받는 묶음, 첫 화면 묶음 증가는 보기 이름·팔레트 별칭(`설정 기록`·`history`·`drift`·`인증서`·`certificate`·`ACME`)·⋯ 메뉴 두 줄뿐(+1.5 KB 밑).

## 14. 시험과 CI

- **단위(opl-types·opl-cluster·opl-api, 이 Mac)**: 새 타입 직렬화·가림(`Debug`·보기 JSON·watch·이력에 비밀 문자열 없음), 지금 타입 바이트 그대로; `Attributed` 왕복·옛 명령 바이트 그대로; 상태 머신 이력(같은 값 쓰기는 줄 없음, 지우기 줄, 보존 셋 — 리소스당·나이·바이트, **두 상태 머신에 같은 로그를 적용하면 이력 표가 바이트로 같음**(결정성), 스냅샷 왕복, 기능 전 스냅샷에 칸 없음), `history_mask`(종류마다 비밀 경로 — 가림 함수 ⊆ 이력 가림), `Kind::history_tracked`가 `ALL` 전부를 분류; 매니페스트 검증(칸 틀림 경로·중복·고리 경로), 정규화(기본값·목록 차례), 세 갈래 비교 표(선언 칸만·빠진 칸 되돌림·목록 통째·비밀), 위상 정렬, 내보내기 → 적용 = 모두 `unchanged`(종류마다 왕복 성질 시험 — proptest), 계획 토큰(문서 해시·revision 바뀜 → stale); ACME JWS(RFC 8555 부록 견본·RFC 7638 견본 지문), EAB, ARI id(x509 견본), problem document 갈래, `due` 순수 함수(ARI 창·1/3·이름 바뀜·물러서기), DNS UPDATE·TSIG 부호화(RFC 8945 견본), SigV4 Route 53 견본, 훅 경로 검사.
- **API 시험(opl-api, FakeStorage — CI `check`에 qemu-img 없음)**: apply 흐름(폴더 → 네트워크 → 볼륨 → VM, 두 번째 apply 모두 unchanged, 칸 바꿈 → update 하나, 가지치기·`--prune-data` 없으면 막힘·주석 건너뜀, 실패 단계에서 멈추고 다시 적용하면 남은 것만), 미리 확인이 할당량·용량·MAC 겹침·권한을 단계에 보이고 **숨은 네트워크·폴더가 새지 않음**(계획 71 리뷰 2 시험 꼴), 409 `cluster_upgrading`(기능 셋), 저장한 계획 stale 412, Terraform 관리 충돌, 이력(출처 — 세션·토큰·`Orbivisor-Client`·사유·Task·system, 감사 id 이음), 하나 되돌리기(비밀 남김 경고), 살리기(같은 id — 다른 리소스 참조가 다시 풀림, 볼륨 막힘, VM 디스크 없음 막힘), 시점 되돌리기(폴더 범위, 그 뒤 만든 것 지우기 확인), 드리프트(웹에서 바꿈 → `Modified` + `by`, adopt), 권한 행렬(`insta` — 이력 보기 가림·restore 단계 권한), 감사에 비밀 없음.
- **ACME(opl-api, 기능 `acme-it` — GitHub `acme` 작업과 Linux에 Pebble이 있을 때)**: Pebble(`-strict`, `PEBBLE_VA_NOSLEEP=1`, nonce 거절 5% 그대로) + `pebble-challtestsrv`: 계정(EAB 요구 설정 둘 다), HTTP-01(우리 `http_listen` 리스너를 Pebble `httpPort`로 — 노드 둘 중 주문하지 않은 노드가 답하도록 이름을 그쪽으로), DNS-01 훅(challtestsrv `set-txt`를 부르는 셸 훅), RFC 2136(작업에 BIND 9 + TSIG 키, Pebble DNS를 그 BIND로), ARI 갱신(Pebble의 짧은 유효 기간 설정 → `replaces`로 다시 받음), 공유 인증서(노드 셋이 같은 사슬·Raft 키), 노드마다(키가 Raft에 없음 — 저장소 바이트 검사), 바꿔 끼우기(손맞춤한 인증서 일련번호가 바뀜, 열린 연결 그대로), 실패 → 물러서기·경보. Cloudflare·PowerDNS·Route 53은 같은 프로세스의 가짜 서버(요청 꼴·SigV4 확인)로.
- **Terraform(`terraform/`, Go)**: `go vet`·`staticcheck`·단위(스키마 ↔ 매니페스트 바꾸기), 스키마 맞춤(OpenAPI), 수용 시험(`TF_ACC=1`, `opl-devserver`를 띄움) — 리소스마다 만들기·가져오기·바꾸기(계획에 한 칸만)·밖에서 바꿈 → plan이 봄·밖에서 지움 → 다시 만듦·지우기, `terraform plan`이 할당량 넘음을 **계획 때** 오류로, 쓰기 전용 비밀이 상태 파일에 없음, terraform(1.x 핀)과 tofu(1.x 핀) 둘로.
- **실제 시험(opld `qemu-it`, 이 Mac HVF·svc01)**: 노드 둘: `apply -f examples/web/`(폴더·SDN 없는 네트워크·볼륨·VM 켜짐 — 진짜 QEMU) → `diff` 0 → VM 메모리 바꾼 파일 → `plan`(restart 표시) → `apply --restart` → 웹 API로 vcpus 바꿈 → `drift` 1 + `by` → `restore vm --revision` → `export` → 다른 이름 접두로 다시 apply(왕복) → `restore --at`(폴더) → `apply --prune --prune-data`로 지움. ACME는 실제 시험에 넣지 않는다(Pebble 작업이 맡음).
- **웹(vitest + msw)**: 설정 기록(거르기·날짜 묶음·서랍 전후 표·되돌리기 확인 대화상자의 계획 diff·비밀 `$secret` 문구), 시점 되돌리기 대화상자(데이터 잃는 단계 체크), 선언 묶음 상세(드리프트 칩·누가 바꿨나·adopt), 인증서 보기(만들기 대화상자 셋·HTTP-01 경고·비밀이 저장 뒤 보이지 않음·지금 갱신), ⋯ 메뉴 "설정 기록…"·"YAML로 내보내기"(`toYaml` — CLI `serde_yaml_ng`가 같은 YAML을 같은 값으로 읽는지 `web_cross` 교차 시험 고정 파일), 권한 없으면 단추 없음, 409 업그레이드 안내, axe·대비 두 테마, 390px 가로 스크롤 없음, `routes.test`·`nav.test`(감사 탭 보기 셋·admin 아닌 사용자에게 탭 보임)·`i18n.test`.
- **브라우저 E2E(Playwright, opld `web-it`)**: `web/e2e/history.spec.ts` — VM 하나 만들고 웹에서 메모리 바꿈 → 설정 기록에 두 줄 → 서랍 diff → 되돌리기 → VM 메모리 원래대로; 인증서 보기 열기(가짜 디렉터리 없이 목록·빈 상태만).
- **잴 것(§18에)**: 이력 켠 상태 머신 적용 비용(1 000번 바꾸기, 켬/끔 비교 — 5% 밑 목표), 64 MiB 이력 스냅샷 보내기 시간, 1 000 문서 계획 시간(목표 2초 밑 — 디버그), 5 000 리소스 내보내기.
- **할 수 없는 것과 대신**: 실제 Let's Encrypt(속도 한도·공개 DNS·포트 80이 필요 — 가이드의 손 확인 절차: 스테이징 디렉터리로 노드 하나 HTTP-01·DNS-01 훅, 결과는 사용자가 해 보면 §18·가이드에), 실제 Cloudflare·Route 53·PowerDNS(가짜 서버로 꼴만), Terraform 레지스트리 게시(서명 키 — 절차만), 수백 노드 이력 부하(scale 시험에 이력 켠 판을 더하는 것은 §16).
- **svc01**: 일반 시험 묶음 전체(새 API 시험·opld `qemu-it` 포함, Go가 있으면 `terraform` 수용 시험도 — 없으면 GitHub). **GitHub**: `check`·`web`·`deny`, 새 작업 **`acme`**(Pebble·challtestsrv 핀한 판·SHA-256, BIND 9 — 우분투 꾸러미), **`terraform`**(setup-go 핀, terraform·tofu 핀한 실행 파일 + SHA-256, `cargo build -p opld --features devserver --bin opl-devserver`, `go test ./... ` + `TF_ACC=1` 행렬 둘, `govulncheck`), `linux-kvm`은 opld 시험 전부를 돌므로 새 실제 시험이 거기서도 돈다(탐침으로 확인), `ldap`·`package`는 바뀐 것이 없다.

## 15. 견준 길

- **계획 자리: 서버(채택) / CLI가 계획(kubectl 클라이언트 쪽 apply 옛 꼴) / Terraform만.** 서버가 하면 CLI·Terraform·웹이 같은 비교와 검증을 쓰고, 미리 확인이 할당량·용량·독점 키 같은 서버 상태를 본다. CLI 계획은 클라이언트마다 갈라진다(kubectl이 서버 쪽 apply로 옮긴 까닭).
- **쓰기 길: 종류 처리기의 함수(채택) / 매니페스트가 Raft 명령을 직접 / HTTP 되부름.** 직접 명령은 검증·할당량·Task 만들기·감사를 비껴간다. HTTP 되부름은 인증·감사를 두 번 거치고 오류 꼴이 흐려진다. 대가: 처리기에서 안 함수를 꺼내는 리팩터(종류마다 몇십 줄 — 스테이지 2·3).
- **원자성: 단계 차례 + 멈춤 + 다시 하면 남은 것(채택) / 여러 명령을 Raft 묶음 하나로.** Raft 묶음은 볼륨 만들기 Task·VM 켜기처럼 바깥 일을 담지 못하고 처리기 규칙을 비껴간다. Terraform·kubectl도 원자가 아니다. 대신 저장한 계획·`If-Match`·idempotent를 약속한다.
- **세 갈래 기억: 적용 묶음 status(채택) / 객체 주석 last-applied(kubectl 옛 꼴) / 칸 단위 관리자(k8s managedFields).** 주석은 객체 spec을 부풀리고 이력에 잡음을 만든다. 칸 단위 관리자는 모든 종류 spec에 소유 표를 들여야 한다 — 우리는 객체 단위 관리자(apply 묶음·Terraform 레이블)로 충분하다. 묶음 status는 드리프트 기준도 되어 한 자리가 두 일을 한다.
- **Terraform 언어: Go 프레임워크(채택) / Rust tfplugin6 / OpenAPI 코드 생성 공급자(tfplugingen-openapi).** §10.1. 코드 생성은 기술 미리 보기이고 우리 종류마다 다른 PATCH 꼴을 다루지 못한다 — 대신 공급자가 매니페스트 API 하나만 써서 리소스가 스키마만 다르다.
- **ACME: 손으로 짠 RFC 8555(채택) / `instant-acme` crate / 바깥 도구(certbot·lego·acme.sh)를 부름.** RFC 8555 + ARI + EAB는 600~900줄이고 ring·rcgen·x509-parser·reqwest가 이미 있다(계획 72의 손 SOAP와 같은 판단, 새 의존성 없음). `instant-acme`는 잘 유지되지만 hyper-rustls·다른 암호 공급자 기능 고르기를 끌고 오고 ARI·프로필 판 따라가기를 남에게 맡긴다. 바깥 도구는 배포물이 늘고 클러스터 조율(한 번 받아 나눔·Raft 도전 값)이 안 된다 — 대신 DNS 공급자 수의 부족은 훅으로 그 도구들의 플러그인을 잇는다. 대가: 꼴 실수 위험 — Pebble `-strict`가 잡는다.
- **HTTP-01 답: Raft의 도전 값 + 모든 노드 리스너(채택) / 주문한 노드가 잠깐 포트 80(Proxmox 꼴).** VIP·라운드 로빈 이름에서 검증자가 어느 노드로 가도 답한다. 대가: 선택 리스너 하나(기본 꺼짐).
- **공유 키: Raft 비밀(채택) / 노드마다 같은 이름 따로 주문 / 키를 노드끼리 peer RPC로.** 따로 주문은 속도 한도(같은 이름 묶음 주 5장)에 노드 수만큼 걸린다. peer 전달은 오프라인 노드가 놓친다. Raft 비밀은 원격 토큰·S3 비밀과 같은 자리·같은 가림. 노드마다 인증서의 키는 노드 밖으로 나가지 않는다.
- **이력 저장: Raft 상태 머신 표(채택) / 감사 파일에 전후 값 / 노드마다 따로 DB.** 감사 파일은 노드마다 다르고 돌림으로 사라지며 되돌리기의 바탕이 될 수 없다. 노드 DB는 새 노드·리더 바뀜에 갈라진다. 상태 머신은 결정적이고 스냅샷으로 따라간다. 대가: 스냅샷 크기(한도 설정 — 기본 64 MiB)와 적용 비용(잰다).
- **이력 꼴: 줄마다 전체 spec + lz4(채택) / 앞 줄과의 차이만.** 차이만 두면 보존 한도로 앞 줄을 지울 때 다시 쓰기가 필요하고 한 revision을 읽는 데 사슬을 따라야 한다. spec은 작고 lz4가 잘 줄인다.
- **비밀 이력: "바뀜/같음"만(채택) / 해시 / 암호화한 평문.** 해시는 낮은 엔트로피 비밀번호를 대입으로 푼다. 암호화한 평문은 키 관리(어디에, 누가)가 새 문제다. 대가: 비밀은 되돌릴 수 없다(경고).
- **YAML: CLI만 `serde_yaml_ng`(채택) / 서버도 YAML / JSON만.** 서버가 YAML을 읽으면 큰 파서가 인증 전 공격면에 들어간다. JSON만이면 사람 파일·git 리뷰에 불편하다. 웹은 쓰기만 하므로 작은 쓰개로 충분하다(교차 시험).
- **화면 자리: 활동 › 감사 탭 안 보기 + 리소스 ⋯ 서랍 + 설정 › 보안 안 보기(채택) / 새 사이드바 "구성 관리".** 계획 61 규칙. 이력·드리프트는 "누가 무엇을 했나"의 감사 확장이고, 리소스 서랍은 그 리소스에서 바로 연다.

## 16. 나중 할 일

- 내부 클러스터 인증서(8007 mTLS 노드 인증서 824일) 자동 갱신과 CA 돌리기, 손 PEM 파일 바뀜 다시 읽기, TLS-ALPN-01.
- 웹 YAML 편집기(붙여 넣어 계획 → 적용), VM 빌더 미리 보기의 "YAML" 판(계획 71 JSON·CLI 옆), 사용자(`User`) 매니페스트(비밀번호 없는 realm 사용자), 노드 설정(풀·네트워크 노드 쪽) 선언.
- GitOps 끌어오기(클러스터가 git 저장소를 주기로 적용 — Flux·Argo 같은 것), 변경 승인 흐름(계획을 남이 승인해야 적용), 바꿈 창(변경 금지 시간).
- Terraform 레지스트리·OpenTofu 레지스트리 게시 자동화(GoReleaser·서명), Pulumi 브리지(Terraform 공급자에서), Ansible 모음.
- 이력: 칸 단위 되돌리기(한 경로만), status 이력(상태 바뀜 시간 줄), 이력 내보내기(SIEM), 수백 노드 scale 시험에 이력 켠 판.
- DNS-01 공급자 더하기(Azure DNS·Google Cloud DNS·Hetzner·OVH — 훅으로 지금도 됨), CAA 미리 확인.

## 17. 스테이지 나누기

각 스테이지는 커밋 하나이고, 혼자 컴파일되고 시험된다(`cargo test`·`clippy`·`fmt`, 웹 스테이지는 `pnpm typecheck`·`lint`·`test`, Terraform 스테이지는 `go vet`·`go test`). 화면은 스테이지 7에서 열린다.

1. **feat(types,cluster): 선언형 운영 타입과 변경 기록 — 매니페스트·적용 묶음·ACME 계정·DNS 공급자·인증서 리소스, 출처를 단 명령과 상태 머신 설정 이력 표·보존 한도, 기능 declarative·config-history·acme** — `crates/opl-types/src/{manifest,apply_set,acme,history}.rs`(+ 직렬화·가림·`history_mask`·바이트 호환 시험), `resource.rs` `Kind` 넷(`ALL` 47, `history_tracked`), `task.rs` `Apply`·`AcmeOrder`, `node.rs` `FEATURES` 셋, `settings.rs` `history`·`acme`, `alert.rs` 조건 셋(평가는 스테이지 3·4), `opl-cluster` `Command::Attributed`·`ChangeContext`·리더가 `at_ms`를 찍음·상태 머신 `history`·`history_seq` 표와 보존·스냅샷 칸(결정성 시험·스냅샷 왕복·기능 전 빈 칸), `opl-api` 미들웨어가 `ChangeContext`를 채움(머리 `Orbivisor-Client`·`Orbivisor-Change-Reason`·감사 id), Task 실행기 출처.
2. **feat(api,cli): 선언형 적용 엔진 — 종류별 어댑터·이름 참조 풀기·의존 차례·세 갈래 비교·서버 미리 확인·저장한 계획·가지치기, 매니페스트 내보내기, orbivisorctl apply·diff·plan·export** — `crates/opl-api/src/declarative/{mod,manifest,plan,apply,export}.rs`, `kinds/{folder,storage_pool,network,router,floating_ip,security_group,volume,vm,placement_rule}.rs`(처리기에서 안 함수 꺼냄 — 동작 그대로), Task `apply`, `/apply`·`/manifests`·`/apply-sets` 경로·경로표·감사·OpenAPI(`Manifest{Kind}`)·`web/src/api/schema.d.ts`, `oplctl` `apply|diff|plan|export|set`(+ `serde_yaml_ng`, `secretFrom`, 크기 문자열, `deny.toml` 확인), 시험(§14 apply 흐름·미리 확인 새지 않음·stale·가지치기·왕복 proptest).
3. **feat(api,cli): 나머지 선언 종류와 설정 이력 API·되돌리기 — 백업·HA·권한·알림·realm·설정 어댑터, 타임라인·비교·하나 되돌리기·지운 것 살리기·시점 되돌리기, 적용 묶음 드리프트(리더 루프)와 경보, orbivisorctl history·restore·drift** — `declarative/kinds/{load_balancer,ha_group,ha_check,backup_job,backup_store,remote,grant,access_role,group,alert_rule,notification_target,realm,settings}.rs`(비밀 칸 `secret_ref`), Terraform 관리 충돌, `history.rs`(API·비교·restore 셋 — declarative 계획으로), `drift.rs`(`DriftChecker` 리더 루프·`by`·adopt), 경보 `ApplySetDrift`, `member.rs` 루프 띄우기, 경로표·감사·OpenAPI·`schema.d.ts`, `oplctl history|restore|drift`, 시험(§14 이력·되돌리기·살리기 막힘·시점·드리프트·권한 행렬).
4. **feat(api,opld,cli,ci): ACME 인증서 — RFC 8555 클라이언트·EAB·ARI·프로필, HTTP-01 리스너와 Raft 도전 값, DNS-01(Cloudflare·Route 53·RFC 2136·PowerDNS·훅), 리더 AcmeRenewer와 acme-order Task, SNI로 고르는 API 인증서 바꿔 끼우기, 만료 경보, GitHub acme 작업(Pebble)** — `crates/opl-api/src/acme/{client,jws,ari,http01,renewer,install,dns/{mod,query,cloudflare,route53,rfc2136,pdns,hook}}.rs`, `opl-store` `sigv4` 공개, `opl-peer/src/tls.rs` SNI 고르기, `api_cert.rs` 차례 셋, `opld/src/config.rs` `[api] http_listen`·`member.rs` 리스너·루프, 경보 `CertificateExpiring`·`CertificateRenewFailed`, `declarative/kinds/{acme_account,acme_dns,certificate}.rs`(선언 어댑터 — 계정 키는 서버가 만듦), `/acme-accounts`·`/acme-dns`·`/certificates` API·경로표·감사·가림·OpenAPI·`schema.d.ts`, `oplctl cert …`, 기능 `acme-it`·`tests/acme.rs`, `.github/workflows/ci.yml` `acme`(Pebble·challtestsrv·BIND 핀).
5. **feat(terraform,opld,ci): Terraform/OpenTofu 공급자 — Go terraform-plugin-framework, 리소스 열다섯·데이터 소스 여섯, 계획 때 서버 미리 확인, 가져오기·쓰기 전용 비밀, 개발 서버 opl-devserver, GitHub terraform 작업(terraform·tofu 수용 시험)** — `terraform/{go.mod,main.go,internal/{client,provider,resources,datasources}/…,examples/,docs/(tfplugindocs),LICENSE}`, `crates/opld/src/bin/opl-devserver.rs`(기능 `devserver`), 스키마 맞춤 시험(OpenAPI), 수용 시험, `.github/workflows/ci.yml` `terraform`(setup-go·terraform·tofu 핀, govulncheck).
6. **feat(web): 감사 탭의 설정 기록·선언 묶음 보기, 리소스 ⋯ 메뉴의 설정 기록 서랍·YAML 내보내기, 되돌리기·시점 되돌리기 대화상자, 설정 보안 탭의 인증서 보기** — `web/src/layout/nav.ts`(보기 `admin`, 감사 탭 보기 셋, 보안 탭 보기 둘), `App.tsx` 늦은 경로, `web/src/history/{HistoryPage,ChangeDrawer,RestoreDialog,PointInTimeDialog,diff.ts,i18n}`, `web/src/declarative/{ApplySetsPage,ApplySetPage,toYaml.ts,ExportMenu,i18n}`, `web/src/certs/{CertificatesPage,AccountDialog,DnsDialog,CertificateDialog,i18n}`, 상세 ⋯ 메뉴 두 줄(VM·볼륨·네트워크·보안 그룹·폴더·백업 작업·라우터·부하 분산기·배치 규칙·HA 묶음), Terraform 칩, 팔레트 별칭, `oplctl` `web_cross` YAML 교차 고정 파일, vitest(§14 웹)·axe·대비·390px, 번들 기록.
7. **test(opld,web),docs: 선언형 운영 실제 시험(QEMU 노드 둘 — apply·drift·restore·export 왕복)과 브라우저 E2E, 가이드·아키텍처·설계 스테이지 보고·계획 73 실행 기록** — `crates/opld/tests/declarative.rs`(`qemu-it`), `web/e2e/history.spec.ts`, `crates/opld/tests/web.rs`, `examples/declarative/web/*.yaml`, 새 가이드 `docs/guide/declarative.md`(매니페스트·apply·diff·plan·export·묶음·가지치기·Terraform 공급자·이력·되돌리기·드리프트·Let's Encrypt 손 확인과 레지스트리 게시 절차), `docs/guide/cluster.md`(ACME — 인증서 차례·HTTP-01 리스너·DNS-01 공급자·훅 예·공유/노드마다), `docs/guide/access.md`(새 권한 없음·단계 권한), `docs/guide/web-ui.md`(감사 탭 보기·⋯ 메뉴·인증서 보기), `docs/guide/development.md`(`opl-devserver`·`acme-it`·`terraform`·`acme` 작업), `docs/architecture.md`(리소스 47종·`Attributed`·이력 표·리더 루프 둘·보안 표·§1.1 견주기 줄·§19 확인 못 한 것), `docs/index.md`, README 한 줄, 계획 문서.

**함께 할 수 있는 것**: 스테이지 1 뒤에 스테이지 4(ACME — 선언 엔진과 닿지 않음)와 스테이지 2가 서로 닿지 않는다(따로 작업 트리에서 하고 차례대로 cherry-pick, `FEATURES`·`Kind`는 스테이지 1이 이미 더함). 스테이지 3은 2 뒤. 스테이지 5(Terraform)는 3 뒤(종류 전부·OpenAPI 매니페스트 스키마)이고 4와 함께 해도 된다(ACME 인증서 리소스 `orbivisor_certificate`만 4의 타입 — 1에 있음). 스테이지 6은 3·4의 `schema.d.ts` 뒤(msw로 먼저 시작할 수 있다), 스테이지 7은 마지막.

## 18. 스테이지 보고 (설계와 다른 것·결정)

### 스테이지 1 — 타입과 변경 기록

- **싸는 쓰기의 범위**: `Cluster::write`는 기록 종류(`Kind::history_tracked`)의 spec 쓰기(`Create`·`UpdateSpec`·`Delete`)만 `Attributed`로 싼다. status 쓰기와 기록하지 않는 종류(Task·세션·노드…)는 옛 명령 그대로라 로그 크기·바이트가 그대로다. 싸는 조건은 "노드 리소스가 하나 이상 있고 모두 `config-history`를 알림" — 노드 리소스가 없는 막 만든 클러스터의 쓰기는 싸지 않는다(빈 목록을 참으로 보면 옛 노드가 합류하기 전 로그에 새 명령이 남는다). 출처 없는 쓰기는 `System`으로 싼다(§7.1 그대로).
- **토큰 주체**: `ActorAuth::Token { name }`의 이름은 인증 추출기가 저장소의 토큰 리소스 이름으로 채운다(못 찾으면 토큰 id 문자열). 세션이고 `Orbivisor-Client`가 없으면 출처는 `web`, 머리가 있으면 머리를 따른다. 사유 머리는 UTF-8 바이트(obs-text)도 받는다(한글 사유). 감사 줄 id는 요청 시작에 정해 이력 줄 `request`와 같다.
- **Task 실행기 출처**: Task 실행 전체를 `{actor: REQUESTED_BY 레이블의 사용자(이름은 저장소에서), auth: system, task: id, source: task}`로 감싼다. 스테이지 2의 Task `apply`는 안에서 다시 범위를 열어 `apply`·`terraform` 출처로 덮는다(`ChangeContext::scope`는 겹쳐 열 수 있다). `tokio::spawn`으로 띄운 작업에는 넘어가지 않는다(그 쓰기는 `System`). 범위는 `perform`을 `Box::pin`한 future를 감싼다 — 감싸지 않으면 큰 Task future(VMware 동기화)가 디버그 빌드에서 스택을 넘친다(브라우저 E2E가 잡음).
- **이력 표 꼴**: `history` (종류, id, revision) → lz4(`compress_prepend_size`) JSON, `history_seq` revision → (종류, id, at_ms, 줄 바이트) — 전역 보존이 줄을 풀지 않고 시각·크기를 본다. 합계는 `meta`의 `history_count`·`history_bytes`. 스냅샷 칸은 `history: [[kind, id, revision, at_ms, base64(lz4)]]`(시각을 실어 설치 때 풀지 않는다), 비면 쓰지 않는다(시험: 기능 전 스냅샷에 칸 없음).
- **보존**: 리소스당 한도는 가장 새 N줄을 남긴다(지운 리소스의 `Delete`는 가장 새 줄이라 남는다). 나이·전체 바이트는 명령마다 `history_seq` 앞에서 최대 256줄. 한도는 묶음마다 Raft의 설정에서 한 번 읽고 설정 쓰기가 있으면 다시 읽는다(범위 밖이면 기본값 — `SettingsSpec::history()`와 같은 규칙).
- **관찰 노드**: Follow는 리소스만 싣으므로 관찰 노드(계획 48)에는 이력 표가 없다(설치 때 비움). 스테이지 3의 이력 API는 Raft 구성원에서 읽거나 그쪽으로 넘겨야 한다.
- **설정 칸**: `SettingsSpec.history`·`acme`는 타입·범위 검증만 있다. 설정 PATCH는 아직 받지 않는다(설정은 `deny_unknown_fields`라 옛 노드가 풀지 못한다 — 스테이지 3·4가 기능 게이트와 함께 연다).
- **권한·watch**: 새 종류 넷의 `kind_privilege`는 기본(읽기 `cluster.read`, 쓰기 `cluster.admin`) 그대로이고 watch에도 아직 없다. 적용 묶음의 `system.read` 보기·ACME 가린 watch는 API 스테이지(2·4)가 더한다(가림 함수 `acme::redact_*`는 이번에 opl-types에 둠).
- **Task·경보 자리**: Task `apply`·`acme-order`는 만들기 게이트(409)와 실행 자리("not available yet")만, `apply`는 시간 제한 없는 Task 목록에, 둘 다 대상 줄에 선다. 경보 조건 셋은 기능 확인 뒤 400 "not available yet"(평가는 스테이지 3·4). 웹은 컴파일 맞춤만(대상 링크 자리 — `certificate` → 설정 보안, `apply_set` → 감사, 폼은 새 조건을 아직 보이지 않음).
- **타입 꼴**: `CertificateScope::PerNode { nodes }`는 빈 목록이 "모든 노드"다(`All | Some` 대신). ACME 시각(`not_after`·`renew_at`·`next_attempt_at`)은 유닉스 초, 이력 시각은 밀리초. `Kind::parse`를 더했다. 매니페스트 머리는 YAML과 같게 `apiVersion`, 비밀 참조는 `secretRef`.
- **잰 값(이 Mac, 릴리스 빌드)**: 상태 머신에 2 KiB VM spec 바꾸기 1 000번(묶음 하나에 명령 하나, 이력은 리소스당 100줄로 다듬으며) — 파일 저장소(fsync, 실제 노드와 같음) 5.10 s → 5.15 s **+1.0%**, 메모리 저장소(CPU만) 43.6 ms → 68.5 ms +57%(spec 쓰기 하나에 약 25 µs — lz4·두 표 넣기·한도 다듬기). 쓰기 길 전체(노드 하나 클러스터, Raft 로그·적용, `Cluster::update_spec` 300번) 4.53 s → 4.59 s **+1.3%**. 목표(5% 밑)는 디스크에 쓰는 실제 길에서 맞는다. 시험 `state_machine::tests::history_overhead`·`tests/history.rs::write_path_overhead`(`--ignored`).
