# Orbivisor 계획 73: 선언형 운영 — 선언형 적용(apply·diff·plan·export), Terraform/OpenTofu 공급자, ACME 인증서, 설정 이력과 되돌리기 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** API 우선 제품을 **선언형으로 운영**하게 한다. **선언형 적용**: `orbivisorctl apply -f 디렉터리`가 YAML 매니페스트(스물다섯 종류)를 서버로 보내고, 서버가 지금 상태와 비교해 계획(만들기·바꾸기·그대로·지우기·막힘)을 세우고 의존 차례로 지금 처리기 길을 거쳐 적용한다 — 몇 번 해도 같고, `diff`·`plan`은 쓰지 않는 서버 미리 확인(권한·기능·참조·할당량·용량·MAC/IP), 저장한 계획은 그사이 바뀌면 거절, 적용 묶음이 마지막 선언을 기억해 가지치기(데이터 종류는 따로 허락)와 드리프트를 맡는다. `export`가 같은 YAML로 내보낸다. **Terraform/OpenTofu 공급자**: `terraform/`의 Go 공급자가 같은 매니페스트 API로 리소스 열다섯·데이터 소스 여섯을 다루고 계획 때 서버 미리 확인을 부른다. **ACME**: 손으로 짠 RFC 8555 클라이언트(EAB·ARI·프로필), HTTP-01(어느 노드든 답하는 선택 포트 80)·DNS-01(Cloudflare·Route 53·RFC 2136·PowerDNS·훅), 리더가 조율하는 주문, 노드마다·VIP 공유 인증서, 다시 시작 없는 바꿔 끼우기, 만료 경보. **설정 이력**: 상태 머신이 spec 바뀜마다 누가·언제·어디서·왜를 적고, 타임라인·비교·하나 되돌리기·지운 것 살리기·한 시점으로 여러 개 되돌리기·보존 한도, 드리프트는 바꾼 사람까지 짚는다.

**Architecture:**
- **새 리소스 넷**(`opl-types/src/{manifest,apply_set,acme,history}.rs`): `apply-set`(마지막 선언·드리프트), `acme-account`(계정 키·EAB — 가림), `acme-dns`(DNS 공급자 비밀 — 가림), `certificate`(노드마다/공유, 도전 값, 공유 키는 Raft 비밀, 노드마다 키는 그 노드 디스크). 기능 `declarative`·`config-history`·`acme`(섞인 클러스터는 409 `cluster_upgrading`). **새 권한 없음** — 단계마다 그 종류의 지금 권한.
- **출처를 단 명령**: `Command::Attributed { meta{at_ms, actor, source, reason, request}, command }` — API 미들웨어가 `ChangeContext`(작업 지역 값)를 채우고 `Cluster::write`가 싸며 **리더가 `at_ms`를 찍어** 적용이 결정적이다. 상태 머신 표 `history`·`history_seq`(lz4 전체 spec, 비밀은 "바뀜/같음"만), 보존(리소스당·나이·전체 바이트 — 명령 시각 기준), 스냅샷에 실림.
- **선언 엔진**(`opl-api/src/declarative/`): 종류마다 `Declarable` 어댑터(열쇠·참조·정규화·내보내기·바꿀 수 없는 칸·처리기에서 꺼낸 만들기/바꾸기/지우기) → 참조 풀기(실제 + 계획 안) → 위상 정렬 → 세 갈래 비교(선언 칸만 + 묶음의 마지막 선언에서 빠진 칸) → 미리 확인(`prepare_with(Planned)` 등, 권한 먼저·숨은 것 새지 않음) → 계획 토큰 → Task `apply`(단계마다 권한 다시·`If-Match`·만든 Task 기다림, 첫 실패에서 멈춤, 다시 하면 남은 것만).
- **이력·되돌리기**(`history.rs`): 하나 되돌리기·살리기(같은 id, 데이터 종류 막음)·시점 되돌리기 모두 **선언 엔진의 계획**으로 — 같은 검증·차례·권한. 리더 `DriftChecker`(5분 + watch 몰아 받기)가 묶음 드리프트와 "누가 바꿨나"를 적는다.
- **ACME**(`opl-api/src/acme/`): ring ES256 JWS·nonce 통·EAB·ARI·`replaces`·프로필, 리더 `AcmeRenewer`(60초) → Task `acme-order`(노드마다는 그 노드, 공유는 하나), 물러서기, HTTP-01 답은 Raft 도전 값을 모든 노드의 `[api] http_listen`이, DNS-01은 권한 서버로 전파 확인. `ApiCertAgent`가 차례(손 파일 > ACME > 내부 CA)로 `ApiCert::replace`, SNI로 노드 인증서·VIP 인증서를 고른다.
- **Terraform**(`terraform/`, Go terraform-plugin-framework): 공통 CRUD 하나(매니페스트 하나 적용·읽기·지우기) + 리소스마다 스키마, `ModifyPlan`의 서버 미리 확인, 쓰기 전용 비밀, 관리자 레이블. 수용 시험은 개발 서버 `opl-devserver`(FakeStorage 노드 하나)에 terraform·tofu 둘로.

**Tech Stack:** Rust(axum·utoipa·serde·openraft·redb·lz4_flex·ring·rcgen·x509-parser·reqwest, `opl-types`·`opl-cluster`·`opl-api`·`opl-peer`·`opl-store`·`oplctl`·`opld`), 새 Rust 의존성은 `serde_yaml_ng`(oplctl만) 하나, Go 1.24+(terraform-plugin-framework·terraform-plugin-go·terraform-plugin-testing·tfplugindocs), React 19·TypeScript·Vite(늦게 읽는 경로), TanStack Query·watch, react-i18next, vitest + msw + axe-core, Playwright(opld `web-it`), GitHub Actions(Pebble·pebble-challtestsrv·BIND 9·terraform·tofu). 새 npm 의존성 없음.

**설계 문서:** `docs/superpowers/specs/2026-11-25-declarative-design.md`(상위: `2026-11-25-dnd-builder-design.md`, `2026-11-25-vmware2-design.md`, `2026-11-18-folders-permissions-quotas-2fa-design.md`, `2026-11-11-availability-scale-design.md`, `2026-11-18-ui-ux-redesign-design.md`)
**선행 계획:** 계획 1~72 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 매니페스트 `orbivisor.io/v1`(스물다섯 종류, 이름·폴더 경로 참조, VM `state`), 서버 계획·미리 확인·의존 차례·세 갈래 비교·저장한 계획·가지치기(데이터 종류 따로 허락)·Task `apply` | 템플릿 언어·변수, 원자 적용, 자동 바꿔 만들기, 서버가 YAML 읽기 |
| `orbivisorctl apply·diff·plan·export·set`, `secretFrom`(환경·파일), 크기 문자열 | 웹 YAML 편집기, 사용자 비밀번호 매니페스트 |
| Go Terraform/OpenTofu 공급자(리소스 15·데이터 소스 6), 계획 때 미리 확인, 가져오기, 쓰기 전용 비밀, `opl-devserver` | 레지스트리 게시 자동화(절차만), Pulumi·Crossplane |
| ACME 계정(EAB)·DNS 공급자 다섯·인증서(노드마다·공유·와일드카드), ARI·프로필, HTTP-01 선택 리스너, 리더 조율·물러서기, SNI 고르기, 만료 경보(내부 노드 인증서 포함) | 내부 8007 인증서 갱신·CA 돌리기, TLS-ALPN-01 |
| 설정 이력(상태 머신, 출처·사유·감사 id), 타임라인·비교·하나 되돌리기·살리기·시점 되돌리기, 보존 설정, 묶음 드리프트·adopt·경보 | status·디스크 데이터 이력, 감사 로그를 Raft로, 칸 단위 되돌리기 |
| 웹: 활동 › 감사 탭 보기 셋(감사·설정 기록·선언 묶음), ⋯ 메뉴 "설정 기록…"·"YAML로 내보내기", 설정 › 보안 탭 "인증서" 보기 | 새 사이드바 항목·탭 |

## 실행 환경

- **이 Mac(aarch64):** Rust 전부(`cargo test -p opl-types -p opl-cluster -p opl-api -p oplctl -p opld`, `clippy`·`fmt`·`cargo deny check`), Go(`/opt/homebrew/bin/go` 1.26 — `go vet`·`go test`, 수용 시험은 terraform·tofu 실행 파일을 스크래치 디렉터리에 받아 `TF_ACC_TERRAFORM_PATH`로), 웹(`pnpm typecheck`·`lint`·`test`·`build`·`gen:api`), opld `qemu-it` 실제 시험(HVF), `web-it` 브라우저 E2E. Pebble은 `go install`로 받아 `acme-it`를 이 Mac에서도 돌 수 있다(포트 80 대신 Pebble `httpPort`).
- **GitHub:** `check`·`web`·`deny` 통과, 새 작업 **`acme`**(Pebble·pebble-challtestsrv·BIND 9)와 **`terraform`**(setup-go·terraform·tofu 행렬·govulncheck)이 통과해야 한다. `linux-kvm`은 opld 시험 전부를 돌므로 새 `declarative` 실제 시험이 KVM에서도 돈다 — 탐침으로 확인. `ldap`·`package`는 바뀐 것이 없다(도는 그대로 통과만).
- **배포 서버 svc01(Rocky 10 aarch64, `qemu-tcg`, QEMU `/opt/qemu`):** 일반 시험 묶음 전체(새 API 시험·opld `qemu-it` 포함). Go가 있으면(`dnf install golang`) Terraform 수용 시험도, 없으면 GitHub `terraform` 작업이 맡는다.
- **테스트 VM(svc01·svc02):** 배포 뒤 `examples/declarative/web/`를 `apply`(폴더·네트워크·볼륨·VM 둘) → 웹에서 한 VM의 메모리 바꿈 → `drift`가 사람·시각을 보임 → 설정 기록 서랍에서 되돌리기 → `export` → 다른 접두로 다시 apply → 폴더 시점 되돌리기 → `apply --prune --prune-data`. Terraform으로 VM 하나(`examples/terraform/`). ACME는 LAN 실험실이라 공개 CA에 닿지 않는다 — svc01에서 Pebble을 띄워 사내 ACME처럼(`ca_bundle`) 공유 인증서 하나를 받아 두 노드가 같은 인증서를 내는지 브라우저로 확인.

## 설계 결정

- **서버가 계획한다:** CLI·Terraform·웹이 같은 비교·검증·차례를 쓰고, 미리 확인이 할당량·용량·독점 키 같은 서버 상태를 본다(계획 71이 미리 확인을 서버에 둔 것과 같은 까닭).
- **쓰기는 지금 처리기 길로만:** 어댑터는 처리기에서 꺼낸 함수를 부른다 — 검증·할당량·Task 만들기·감사를 비껴가지 않는다. 매니페스트 엔진은 Raft 명령을 직접 내지 않는다.
- **원자 대신 차례·멈춤·다시 하면 남은 것:** 볼륨 Task·VM 켜기 같은 바깥 일은 Raft 묶음에 담기지 않는다. 저장한 계획 토큰·`If-Match`·idempotent로 약속한다.
- **세 갈래 기억과 드리프트 기준을 적용 묶음 status 한 자리에:** 객체 주석·칸 단위 관리자 없이 객체 단위 관리자(묶음·Terraform 레이블)로 충돌을 막는다.
- **Terraform은 Go 프레임워크:** 계획 의미·수용 시험·문서·릴리스 도구를 HashiCorp 프레임워크가 진다. Rust tfplugin6는 protobuf·msgpack·go-plugin 손맞춤과 계획 의미를 우리가 소유해야 하고, 서버가 계획하므로 같은 타입을 쓰는 이점이 작다. 대가: 두 번째 언어·따로 도는 CI 작업.
- **ACME는 손으로 짠 RFC 8555:** ring·rcgen·x509-parser·reqwest가 이미 있고 ARI·EAB·프로필을 우리가 따라간다(새 의존성 없음). 꼴 실수는 Pebble `-strict`가 잡는다. DNS 공급자 부족은 훅이 acme.sh·lego 플러그인을 잇는다.
- **HTTP-01 답은 Raft 도전 값 + 모든 노드 리스너, 공유 키는 Raft 비밀:** VIP·라운드 로빈 이름에서 어느 노드로 가도 답하고, 한 번 받아 나눠 속도 한도가 노드 수와 무관하다. 노드마다 인증서 키는 노드 밖으로 나가지 않는다.
- **이력은 Raft 상태 머신 표에, 리더가 찍은 시각으로:** 어느 노드에서 읽어도 같고 새 노드는 스냅샷으로 받는다. 보존도 적용 때 결정적으로. 대가: 스냅샷 크기(기본 64 MiB 한도)와 적용 비용(잰다).
- **비밀 이력은 "바뀜/같음"만:** 해시는 낮은 엔트로피 비밀을 대입으로 풀고 암호화는 키 관리 문제를 새로 만든다. 대가: 비밀은 되돌리지 않는다(경고).
- **되돌리기는 선언 엔진의 계획:** 하나·살리기·시점 모두 같은 검증·권한·차례·저장한 계획 — 따로 길을 만들지 않는다.
- **YAML은 CLI에서만 읽음:** 서버는 JSON만 받아 인증 전 공격면에 큰 파서가 없다. 웹은 쓰기만(작은 쓰개 + CLI와 교차 시험).
- **화면은 감사 탭 안 보기·⋯ 서랍·보안 탭 안 보기:** 계획 61 규칙 — 사이드바·탭을 늘리지 않는다.

## Task

각 Task(스테이지)는 하나의 커밋이며, 앞 Task의 커밋 위에서 순서대로 한다(함께 할 수 있는 것은 아래 "함께"). 스테이지마다 혼자 컴파일되고 시험된다(`cargo test`·`clippy`·`fmt`, 웹은 `pnpm typecheck`·`lint`·`test`, Terraform은 `go vet`·`go test`). 화면은 Task 6에서 열린다. 설계와 다른 것·결정은 설계 문서 §18에 스테이지마다 남긴다.

- [ ] **Task 1: 선언형 운영 타입과 변경 기록 — 매니페스트·적용 묶음·ACME 계정·DNS 공급자·인증서 리소스, 출처를 단 명령과 상태 머신 설정 이력 표·보존 한도, 기능 declarative·config-history·acme(types·cluster)** — `crates/opl-types/src/manifest.rs`(`Manifest`·`ManifestKind` ↔ `Kind` 표·`SecretValue`), `apply_set.rs`(`ApplySet`·`AppliedObject`·`DriftReport`), `acme.rs`(`AcmeAccount`·`AcmeDns`(공급자 다섯)·`Certificate`·`IssuedCert` — 비밀 `Secret`, 가림 함수), `history.rs`(`HistoryRecord`·`ChangeMeta`·`Actor`·`ChangeSource`·`history_mask` 종류별 비밀 경로), `resource.rs` `Kind` 넷(`ALL` 47, `WATCHABLE`, `history_tracked` — `ALL` 전부 분류 시험), `task.rs` `Apply`·`AcmeOrder`(kebab 태그 시험), `node.rs` `DECLARATIVE`·`CONFIG_HISTORY`·`ACME`(`FEATURES` 끝), `settings.rs` `history`·`acme`(범위 검증), `alert.rs` `CertificateExpiring`·`CertificateRenewFailed`·`ApplySetDrift`(평가는 Task 3·4), `crates/opl-cluster/src/types.rs` `Command::Attributed`(옛 명령 바이트 그대로), `change.rs` `ChangeContext`(`task_local!`), `cluster.rs` `write`가 싸고 기능이 없으면 그대로·리더가 `at_ms`를 찍음, `state_machine.rs`·`store.rs` `history`·`history_seq` 표·`history_bytes`·보존 셋(설정에서 읽음)·`SnapshotData.history`(기능 전 빈 칸), `crates/opl-api` 미들웨어(`Orbivisor-Client`·`Orbivisor-Change-Reason`·감사 id)와 Task 실행기 출처, 시험(직렬화·가림·결정성 — 두 상태 머신 같은 로그 → 같은 이력 바이트·같은 값 쓰기 줄 없음·보존 셋·스냅샷 왕복·`history_mask` ⊇ 보기 가림).
- [ ] **Task 2: 선언형 적용 엔진 — 종류별 어댑터·이름 참조 풀기·의존 차례·세 갈래 비교·서버 미리 확인·저장한 계획·가지치기, 매니페스트 내보내기, orbivisorctl apply·diff·plan·export(api·cli)** — `crates/opl-api/src/declarative/{mod,manifest,plan,apply,export}.rs`(검증·정규화·위상 정렬·고리 경로·비교 함수(경로 목록)·권한 먼저 막힘·`prepare_with(Planned)`·독점 키 미리 보기·계획 토큰 30분·stale 412·`plan_blocked` 409·Task `apply` 단계 실행·만든 Task 기다림·묶음 status·레이블), `kinds/{folder,storage_pool,network,router,floating_ip,security_group,volume,vm,placement_rule}.rs`(처리기에서 안 함수 꺼냄 — 처리기 동작·시험 그대로, VM `state`·`restart`), `/apply`·`/manifests`·`/manifests/{kind}/{key}`·`/apply-sets` 경로·`authz_routes.rs`·감사·OpenAPI(`Manifest{Kind}`)·`web/src/api/schema.d.ts`, `crates/oplctl/src/declarative.rs`(`apply|diff|plan|export|set`, `serde_yaml_ng` — 여러 문서·8 MiB 펼침 한도, `secretFrom`, 크기 문자열, 색 계획 표, 끝 코드), `Cargo.toml`·`deny.toml` 확인, 시험(FakeStorage — 폴더 → 네트워크 → 볼륨 → VM, 다시 적용 모두 unchanged, 칸 하나 update, 가지치기·데이터 보호·주석, 실패 뒤 다시 적용, 숨은 네트워크·폴더가 새지 않음, 409 `cluster_upgrading`, 내보내기 → 적용 왕복 proptest, CLI `fake_api`).
- [ ] **Task 3: 나머지 선언 종류와 설정 이력 API·되돌리기 — 백업·HA·권한·알림·realm·설정 어댑터, 타임라인·비교·하나 되돌리기·지운 것 살리기·시점 되돌리기, 적용 묶음 드리프트(리더 루프)와 경보, orbivisorctl history·restore·drift(api·cli)** — `declarative/kinds/{load_balancer,ha_group,ha_check,backup_job,backup_store,remote,grant,access_role,group,alert_rule,notification_target,realm,settings}.rs`(비밀 `secret_ref` — 견주지 않음·평문은 바뀜/같음), Terraform 관리 레이블 충돌(`managed_elsewhere`·`--force-conflicts`), `crates/opl-api/src/history.rs`(`/history` 타임라인 — 보이는 것만·쪽 열쇠, `/history/{kind}/{key}[/{revision}]?compare=`, `restore` 하나·살리기(`create_with_id`·데이터 종류 막힘·이름 겹침), `/history/restore` 시점(범위·그 뒤 만든 것·`allow_data_loss`·`history_expired`)), `drift.rs`(`DriftChecker` 5분 + watch 10초 몰아 받기·`by` 이력 줄·`/apply-sets/{key}/adopt`), 경보 `ApplySetDrift` 평가, `opld/src/member.rs` 루프 띄우기, 경로표·감사·OpenAPI·`schema.d.ts`, `oplctl history|history show|restore|drift`, 시험(출처 다섯 갈래·감사 id, 되돌리기 비밀 남김 경고, 살리기 같은 id·참조 다시 풀림·볼륨/VM 디스크 막힘, 시점 폴더 범위, 드리프트 웹 바꿈 → `Modified`+`by`·adopt, 권한 행렬 `insta`, 감사에 비밀 없음).
- [ ] **Task 4: ACME 인증서 — RFC 8555 클라이언트·EAB·ARI·프로필, HTTP-01 리스너와 Raft 도전 값, DNS-01(Cloudflare·Route 53·RFC 2136·PowerDNS·훅), 리더 AcmeRenewer와 acme-order Task, SNI로 고르는 API 인증서 바꿔 끼우기, 만료 경보, GitHub acme 작업(Pebble)(api·opld·cli·ci)** — `crates/opl-api/src/acme/{client,jws,ari,http01,renewer,install}.rs`(nonce 통·`badNonce` 다시·problem document 갈래·`due` 순수 함수·물러서기·동시 2·진행 중 주문 이어 보기·자기 확인), `acme/dns/{mod,query,cloudflare,route53,rfc2136,pdns,hook}.rs`(권한 서버 전파 확인·CNAME 위임·TSIG·훅 경로 검사·환경 비움), `crates/opl-store` `backend::sigv4` 공개, `crates/opl-peer/src/tls.rs` SNI 고르기(`ResolvesServerCert`), `crates/opl-api/src/api_cert.rs` 차례(손 파일 > ACME > 내부 CA)·노드 키 파일 0600, `crates/opld/src/config.rs` `[api] http_listen`·`member.rs` 리스너·`AcmeRenewer`·노드 status `http01`, 경보 `CertificateExpiring`(공개 인증서 + 내부 노드 인증서)·`CertificateRenewFailed`, `declarative/kinds/{acme_account,acme_dns,certificate}.rs`(선언 어댑터 — 계정 키는 서버가 만듦), `/acme-accounts`·`/acme-dns`·`/certificates`(+`actions/renew`·`actions/check`) API·경로표·감사·가림·watch·OpenAPI·`schema.d.ts`, `oplctl cert account|dns|add|list|get|renew|delete`, 기능 `acme-it`·`tests/acme.rs`(Pebble·challtestsrv·BIND), 가짜 Cloudflare·PowerDNS·Route 53 시험, `.github/workflows/ci.yml` `acme`(Pebble·pebble-challtestsrv 판·SHA-256 핀, BIND 9), 시험(JWS·RFC 7638·EAB·ARI id 견본, TSIG 견본, HTTP-01 다른 노드가 답함, 공유 키 노드 셋 같은 사슬, 노드마다 키가 Raft에 없음, 바꿔 끼우기 일련번호·열린 연결 그대로, 실패 → 물러서기·경보).
- [ ] **Task 5: Terraform/OpenTofu 공급자 — Go terraform-plugin-framework, 리소스 열다섯·데이터 소스 여섯, 계획 때 서버 미리 확인, 가져오기·쓰기 전용 비밀, 개발 서버 opl-devserver, GitHub terraform 작업(terraform·tofu 수용 시험)(terraform·opld·ci)** — `terraform/go.mod`(`toolchain` 고정)·`main.go`, `internal/client`(매니페스트 API·Task 기다리기·`Orbivisor-Client: terraform/{판}`·CA·지문), `internal/provider`(설정 `endpoint`·`api_token`·`ca_pem`·`ca_fingerprint`·`timeout`), `internal/resources`(공통 CRUD + 스키마 열다섯 — `orbivisor_folder`·`network`·`router`·`floating_ip`·`security_group`·`load_balancer`·`volume`·`vm`·`placement_rule`·`ha_group`·`backup_job`·`backup_store`·`grant`·`access_role`·`certificate`, `ModifyPlan` 미리 확인, 쓰기 전용 비밀 `*_wo`·`*_wo_version`, 정체성 `{name}`), `internal/datasources`(여섯), `examples/`·`docs/`(tfplugindocs)·`LICENSE`, `crates/opld/src/bin/opl-devserver.rs`(기능 `devserver` — FakeStorage 노드 하나·admin 토큰·CA, 연결 JSON 한 줄), 스키마 맞춤 시험(OpenAPI `Manifest{Kind}` ↔ 공급자 스키마), 수용 시험(`TF_ACC=1` — 리소스마다 만들기·가져오기·한 칸 바꾸기·밖에서 바꿈/지움·지우기, 계획 때 할당량 오류, 상태 파일에 비밀 없음), `.github/workflows/ci.yml` `terraform`(setup-go·terraform·tofu 핀·SHA-256·`govulncheck`·`go mod verify`).
- [ ] **Task 6: 감사 탭의 설정 기록·선언 묶음 보기, 리소스 ⋯ 메뉴의 설정 기록 서랍·YAML 내보내기, 되돌리기·시점 되돌리기 대화상자, 설정 보안 탭의 인증서 보기(web)** — `web/src/layout/nav.ts`(`NavView.admin`, 감사 탭 보기 셋 `/audit`·`/history`·`/apply-sets`, 보안 탭 보기 둘 `/settings/security`·`/settings/certificates`), `App.tsx` 늦은 경로, `web/src/history/{HistoryPage,ChangeDrawer,RestoreDialog,PointInTimeDialog,diff.ts,i18n/{en,ko}.json}`, `web/src/declarative/{ApplySetsPage,ApplySetPage,toYaml.ts,ExportMenu,i18n}`, `web/src/certs/{CertificatesPage,AccountDialog,DnsDialog,CertificateDialog,i18n}`, 상세 ⋯ 메뉴 "설정 기록…"·"YAML로 내보내기"(VM·볼륨·네트워크·보안 그룹·폴더·백업 작업·라우터·부하 분산기·배치 규칙·HA 묶음)·목록 일괄 내보내기·Terraform 칩, 팔레트 별칭, `crates/oplctl/src/web_cross.rs` YAML 교차 고정 파일, vitest(거르기·서랍 전후 표·되돌리기 계획 diff·`$secret` 문구·시점 대화상자 데이터 잃는 단계 체크·드리프트 칩과 누가 바꿨나·adopt·인증서 대화상자 셋·HTTP-01 경고·비밀이 저장 뒤 안 보임·권한 없으면 단추 없음·409 안내·axe·대비·390px), `routes.test`·`nav.test`(admin 아닌 사용자에게 감사 탭 보임)·`i18n.test`, 번들 기록.
- [ ] **Task 7: 선언형 운영 실제 시험(QEMU 노드 둘 — apply·drift·restore·export 왕복)과 브라우저 E2E, 가이드·아키텍처·설계 스테이지 보고·계획 73 실행 기록(opld·web·docs)** — `crates/opld/tests/declarative.rs`(`qemu-it` — 노드 둘, `apply -f examples/declarative/web/` 진짜 VM 켜짐 → `diff` 0 → 메모리 바꾼 파일 `plan` restart → `apply --restart` → API로 vcpus 바꿈 → `drift` + `by` → `restore --revision` → `export` → 다른 접두로 다시 apply → `restore --at`(폴더) → `apply --prune --prune-data`), `web/e2e/history.spec.ts`, `crates/opld/tests/web.rs`, `examples/declarative/web/*.yaml`·`examples/terraform/`, 새 가이드 `docs/guide/declarative.md`(매니페스트·apply·diff·plan·export·묶음·가지치기·Terraform·이력·되돌리기·드리프트·레지스트리 게시 절차), `docs/guide/cluster.md`(ACME 절 — 인증서 차례·HTTP-01·DNS-01·훅 예·Let's Encrypt 스테이징 손 확인 절차), `docs/guide/access.md`, `docs/guide/web-ui.md`, `docs/guide/development.md`(`opl-devserver`·`acme-it`·`acme`·`terraform` 작업·Go), `docs/architecture.md`(리소스 47종·`Attributed`·이력 표·리더 루프 둘·SNI·보안 표·§1.1 견주기·§19 확인 못 한 것), `docs/index.md`(가이드 표·계획 목록), README 한 줄, 이 문서.

**함께:** Task 1 뒤 Task 2와 Task 4(ACME)는 서로 닿지 않는다(따로 작업 트리에서 하고 차례대로 cherry-pick — `Kind`·`FEATURES`·타입은 Task 1이 이미 더함, 겹치는 파일은 `authz_routes.rs`·`lib.rs`·`member.rs`·`ci.yml`의 다른 줄뿐). Task 3은 2 뒤. Task 5(Terraform)는 3 뒤(종류 전부·OpenAPI 매니페스트 스키마)이고 4와 함께 해도 된다. Task 6은 3·4 뒤(`schema.d.ts` — msw로 먼저 시작할 수 있다), Task 7은 마지막.

## 완료 기준 (계획 73)

- 새 리소스 넷·Task 둘·기능 셋·`Command::Attributed` 하나만 더하고 새 권한은 없다. 지금 타입·지금 Raft 명령의 직렬화가 그대로다(바이트 호환 시험). 기능이 모두 켜지기 전에는 `Attributed`를 쓰지 않는다.
- 설정 이력이 결정적이다: 같은 로그를 적용한 두 상태 머신의 이력 표가 바이트로 같고, 스냅샷으로 새 노드가 같은 이력을 받는다. 같은 값 쓰기·status 쓰기는 줄을 남기지 않고, 보존 한도(리소스당·나이·전체 바이트)를 지킨다. 이력·API·watch·감사·로그 어디에도 비밀 평문·해시가 없다.
- `apply -f`가 스물다섯 종류를 의존 차례로 만들고, 다시 적용하면 모든 단계가 `unchanged`다. `export` 결과를 다시 적용해도 `unchanged`(왕복). `diff`·`plan`은 아무것도 쓰지 않고(Raft revision 그대로) 할당량·용량·MAC/IP·권한·기능 문제를 단계에 보이며 볼 수 없는 리소스의 존재를 새지 않는다. 저장한 계획은 그사이 바뀌면 412다. 가지치기는 묶음 레이블이 있는 것만, VM·볼륨·백업 저장소는 `--prune-data` 없이 지우지 않는다.
- 하나 되돌리기·살리기(같은 id)·시점 되돌리기가 선언 엔진 계획으로 돌고, 데이터 종류 살리기와 보존 밖 시점은 막힌다. 비밀은 되돌리지 않고 경고한다. 드리프트가 다른 칸과 그 칸을 바꾼 사람·시각·출처를 보인다.
- Terraform 공급자가 리소스 열다섯·데이터 소스 여섯을 terraform과 tofu 둘로 수용 시험을 통과하고(만들기·가져오기·바꾸기·밖에서 바꿈·지우기), `terraform plan`이 할당량 넘음을 계획 때 보이며, 쓰기 전용 비밀이 상태 파일에 없다. 스키마 맞춤 시험이 서버 매니페스트와 공급자 스키마의 어긋남을 잡는다.
- ACME가 Pebble(`-strict`, nonce 거절)에 대해 HTTP-01(주문하지 않은 노드가 답함)·DNS-01(훅·RFC 2136 BIND)로 노드마다·공유 인증서를 받고, ARI로 `replaces` 갱신하고, 노드가 다시 시작 없이 새 인증서를 내며(열린 연결 그대로), 노드마다 인증서의 키가 Raft에 없다. 실패하면 물러서고 3번이면 경보, 만료 14일 안이면 경보(내부 노드 인증서 포함).
- 감사 탭 보기 셋·⋯ 서랍·인증서 보기가 키보드만으로 되고 axe 위반이 없으며 두 테마 대비를 지키고 390px에서 가로 스크롤이 없다. 사이드바·탭은 그대로다. 첫 화면 묶음 증가 1.5 KB 밑, 새 조각 gzip 각 25 KB 밑.
- `cargo test`·`clippy`·`fmt`·`cargo deny`, `go vet`·`go test`, `pnpm typecheck`·`lint`·`test`·`build`, 로컬 `qemu-it`·브라우저 E2E가 통과하고 svc01 일반 시험과 GitHub `check`·`web`·`deny`·`acme`·`terraform`·`linux-kvm`을 포함한 모든 작업이 통과한다.

## 알고 받아들인 제한

- **원자 적용이 아니다:** 첫 실패에서 멈추고 성공한 단계는 남는다 — 다시 적용하면 남은 것만 한다(가이드).
- **이력은 업그레이드를 마친 때부터, spec만:** 기능이 켜지기 전 바뀜과 status·디스크 데이터는 이력에 없다. 데이터는 스냅샷·백업이 맡는다. 비밀은 되돌리지 않는다.
- **이력 크기는 스냅샷에 든다:** 기본 64 MiB(설정 8~512). 크게 잡으면 스냅샷 보내기·새 노드 합류가 느려진다(잰 값은 §18).
- **실제 공개 CA·DNS 공급자로 시험하지 않았다(계획 단계 가정):** Pebble·challtestsrv·BIND와 가짜 Cloudflare·PowerDNS·Route 53뿐이다. Let's Encrypt 스테이징 손 확인 절차는 가이드에 둔다. HTTP-01은 포트 80이 밖에서 닿아야 한다.
- **Terraform 공급자 게시:** 레지스트리 게시(서명 키·저장소 이름)는 사용자 몫이다 — 개발은 `dev_overrides`·파일 시스템 미러. OpenTofu의 쓰기 전용 칸 지원은 판에 따라 `sensitive`로 내린다.
- **내부 클러스터 인증서는 그대로:** 8007 mTLS 노드 인증서(824일)는 갱신하지 않고 만료 경보만 낸다.

## 실행 후 기록

- 스테이지: TODO(설계·계획 커밋, 스테이지마다 SHA)
- Mac 시험: TODO
- 리뷰: TODO
- svc01 일반 시험: TODO
- GitHub 탐침: TODO(`acme`·`terraform` 작업, `linux-kvm`의 `declarative` 실제 시험·브라우저 E2E)
- 웹 번들: TODO
- 잰 값: TODO(이력 적용 비용·64 MiB 스냅샷 보내기·1 000 문서 계획·5 000 리소스 내보내기)
- 배포: TODO(svc02 → svc01, `examples/declarative/web/` 흐름·Terraform VM 하나·Pebble 공유 인증서)

## 다음 계획으로 넘길 사항

- 내부 클러스터 인증서 갱신·CA 돌리기, 손 PEM 다시 읽기, TLS-ALPN-01, DNS-01 공급자 더하기·CAA 미리 확인.
- 웹 YAML 편집기·VM 빌더 YAML 판, 사용자·노드 설정 매니페스트, GitOps 끌어오기·변경 승인·바꿈 창.
- Terraform·OpenTofu 레지스트리 게시 자동화, Pulumi 브리지, Ansible 모음.
- 칸 단위 되돌리기, status 이력, 이력 내보내기(SIEM), 이력 켠 수백 노드 scale 시험.
