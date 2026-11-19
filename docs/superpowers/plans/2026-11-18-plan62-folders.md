# Orbivisor 계획 62: 폴더·객체 권한·폴더 할당량·2단계 인증 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 사용자마다 전역 역할 하나뿐이던 권한을 vSphere식 **인벤토리 폴더 + 아래로 내려가는 객체 권한**으로 넓히고(사용자 정의 역할, 로컬 그룹과 LDAP·OIDC 외부 그룹 연결, 위임, 토큰 범위), 팀 폴더마다 vCPU·메모리·디스크·VM 수 **할당량**을 노드 사이 경쟁에도 지키며, **TOTP·복구 코드·패스키 2단계 인증**과 역할·영역별 강제를 넣는다.

**Architecture:**
- **종류별 폴더 트리 셋**(VM·템플릿 / 네트워크 / 스토리지 풀)과 보이지 않는 루트. 리소스 스펙의 `folder: Option<Uuid>`가 없으면 루트라서 옛 JSON이 바이트 그대로다. 상태 머신(`opl-cluster/src/folders.rs`)이 부모·순환·깊이 16·빈 폴더·형제 이름을 결정적으로 막는다.
- **권한 = 합집합, 거부 없음.** grant(사용자·그룹, 역할, 루트·폴더·객체, propagate)를 노드마다 리더 없이 만든 주체별 색인(`opl-api/src/authz/`, 세대·LRU 1024)으로 두 번 찾기로 확인한다. 전역 역할은 "루트에 내장 역할 grant"라서 grant가 없는 클러스터는 옛 `RoleAuthorizer`와 같은 답을 낸다(동치 시험).
- **경로표가 시행한다.** `authz_routes.rs`의 (메서드, 경로) → 규칙을 `route_layer` 미들웨어가 핸들러보다 먼저 적용한다(404/403). OpenAPI의 모든 경로가 표에 있어야 시험이 지난다. 목록·`/watch`(이벤트마다, 가짜 `deleted`, `permissions` 닫기)·지표·경보·Task를 거른다.
- **할당량 = Raft 안 확인.** `check_exclusive`의 VM 훑기에 붙인 `quota_check`가 "후 > 상한 그리고 후 > 전"이면 `QuotaExceeded`. API는 같은 계산(`opl_types::quota`)으로 미리 409를 낸다.
- **2FA:** 대기 세션(`SessionSpec.pending`, 5분)과 `PendingPrincipal`, TOTP 재사용은 revision CAS, WebAuthn은 직접 짠 최소 검증기(`ciborium`·ring).
- **기능 둘** `folders`·`two-factor`가 모든 새 쓰기를 가린다(섞인 클러스터는 409 `cluster_upgrading`).

**Tech Stack:** Rust(opl-types·opl-cluster·opl-api·oplctl·opld), ring(HMAC-SHA1·ECDSA·Ed25519·RSA), `ciborium` 0.2.2, `data-encoding`, dev `proptest` 1.12·`insta`, React 19·TanStack Query, npm `qrcode-generator` 2.0.4, Vitest·msw, Playwright(opld `web-it`).

**설계 문서:** `docs/superpowers/specs/2026-11-18-folders-permissions-quotas-2fa-design.md`
**선행 계획:** 계획 1~61 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 종류별 중첩 폴더(VM·템플릿 / 네트워크 / 스토리지 풀), 옮기기·이름 바꾸기·빈 폴더 지우기 | 볼륨·백업·보안 그룹·노드 폴더, 태그 기반 권한 |
| grant(사용자·그룹 → 역할 → 루트·폴더·객체, propagate), 권한 25개, 사용자 정의 역할, 로컬 그룹과 외부 그룹 연결, 위임과 상승 막기, 토큰 범위 | 거부 규칙·"No Access" 역할, 시간 제한 grant |
| 모든 API 경로·목록·`/watch`·지표·경보·Task·콘솔·백업 복원의 확인, 유효 권한 조회 | 폴더 단위 감사 로그 읽기, 폴더별 Prometheus |
| 폴더 할당량(vCPU·메모리·디스크·VM 수), 사용량 API, 노드 사이 경쟁에도 지킴 | 실행 중만 세는 할당량, 풀별 디스크 할당량, CPU·IO 속도 제한 |
| TOTP·복구 코드·WebAuthn(패스키), 정책(역할·영역), 강제 등록, 관리자 초기화 | SMS·이메일 코드, Yubico OTP, CLI 패스키, OIDC `amr` 강제 |
| CLI `folder`·`role`·`group`·`grant`·`perm`·`2fa`, `--folder`, 로그인 둘째 단계 | `vm clone`·`import run`·`backup restore`의 `--folder` |
| 웹: 폴더 보기·끌어 놓기·일괄 옮기기, 권한 탭, 설정의 권한·역할·그룹·보안·2단계 인증, 로그인 둘째 단계, `usePermissions` 숨기기 | 토큰 범위 화면, 폴더별 대시보드, 가상 스크롤 트리 |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 Rust·웹 변경과 `cargo test --workspace`, opld E2E `cargo test -p opld --test permissions`(QEMU 없음, 노드 하나·둘), `pnpm -C web typecheck`·`lint`·`test`, 로컬 브라우저 E2E `pnpm -C web build && cargo test -p opld --features web-it --test web`, 할당량 apply 비용 `cargo test --release -p opl-cluster --features scale-it --lib quota_apply_cost`.
- **배포 서버 svc01(Linux aarch64):** 일반 시험 묶음 전체(fmt·clippy·`cargo test --workspace`·starve·규모 100·저장소)와 위 E2E.
- **GitHub(Ubuntu 24.04 x86_64):** 새 작업 없음. `linux-kvm`의 "Web UI and browser"가 두 spec을 탐침으로 돌리고, `deny`가 `ciborium`·`proptest`·`qrcode-generator` 라이선스를 본다.
- **테스트 VM:** 두 노드(svc01·svc02)에 배포한 뒤 폴더·grant·2FA가 없는 클러스터에서 동작이 그대로인지, 새 웹 묶음이 나오는지 확인한다.

## 설계 결정

- **Proxmox 풀 + 경로 ACL 대신 vSphere식 폴더 + 객체 권한(사용자 결정):** 리소스가 폴더 하나에만 속해 "어디에 있나"가 하나이고 할당량을 그 트리에 건다.
- **한 트리 대신 종류별 트리 셋:** 계획 61의 영역 나눔(가상 머신 / 인프라)과 할당량(VM 트리만)에 맞는다. 대가는 팀마다 grant 둘.
- **"더 구체적인 쪽이 이김 + 거부" 대신 합집합:** 평가가 순서와 무관하고, 비트 OR 한 번으로 캐시하며, "왜 되나"를 근거 grant로 설명한다. 빼기는 형제 폴더로 옮겨 푼다.
- **grant에 외부 그룹 이름 대신 로컬 `Group` 연결:** 오타·디렉터리 이름 바뀜을 한 곳에서 고친다.
- **할당량: API 확인·예약 CAS 대신 Raft 안 확인:** 커밋 순서가 답을 정하고 예약·되돌리기·리더 단일 지점이 없다. 자식 상한을 부모에서 떼지 않는다(과약정 허용).
- **TOTP 비밀은 Raft 평문 + API에서 가림:** 같은 DB에 키가 있는 암호화는 겉치레다.
- **`webauthn-rs` 대신 최소 직접 구현:** MPL-2.0과 openssl(aarch64 정적 빌드)을 피한다. 대가는 증명·MDS 없음.

## Task

각 Task(스테이지)는 하나의 커밋이며, 앞 Task의 커밋 위에서 순서대로 한다. 스테이지마다 혼자 컴파일되고 시험된다. 새 API는 그 스테이지에서 연다. 설계와 다른 것·결정은 설계 문서 §18에 스테이지마다 남긴다.

- [x] **Task 1: 폴더·grant·역할·그룹·2FA 타입, 권한 목록과 내장 역할, 기능 `folders`·`two-factor`(opl-types)** — `folder.rs`(`FolderSpec`·`FolderQuota`·`sibling_key`·`parse_folder_path`), `authz.rs`(`Privilege` 25개·`PrivSet`·`builtin_privileges`·`kind_privilege`·`AccessRoleSpec`·`GrantSpec`·`GroupSpec`·`TokenScope`, `Action`을 opl-types로), `two_factor.rs`(`TwoFactorSpec`·`PendingLogin`·`TwoFactorPolicy`·`totp`·`verify_totp`), 스펙의 `folder`·`Role::None`·`external_groups`·`scope`·`pending`·`two_factor`, `Kind` 다섯, 참조 `authz_eval::effective`, RFC 6238 벡터, serde 하위 호환, 역할 동치 시험. (`b0fc154`)
- [x] **Task 2: 폴더 상태 머신 검사와 권한 색인(opl-cluster·opl-api `authz/`)** — `folders.rs`의 `check_write`·`check_delete`(부모·순환·깊이·빈 폴더·형제 이름), `Rejection::FolderCycle|FolderTooDeep|FolderNotEmpty`, `AuthzIndex`(세대·LRU)·`EffectiveIndex`·`Authz::require|filter`, `RoleAuthorizer` 동치·`insta` 권한 행렬·`proptest` 넷, VM 1만 개 벤치. 핸들러는 아직 루트 shim이라 동작이 같다. (`1972302`)
- [x] **Task 3: 모든 경로·목록·watch·지표·경보·Task 시행, 폴더·역할·그룹·grant·유효 권한 API(opl-api)** — 시행하는 경로표 `authz_routes.rs`와 덮기 시험, 404/403, 파생 객체 주인(`authz::Owners`), `/watch` 이벤트별 거르기·가짜 `deleted`·`permissions` 닫기, 위임과 상승 막기, 외부 그룹 저장, 토큰 범위, `folder_required`, 만들기·복원·복제·가져오기의 `folder`, `/folders`·`/access-roles`·`/groups`·`/grants`·`/permissions/*`, OpenAPI·`schema.d.ts`. (`1c065b3`)
- [x] **Task 4: 폴더 할당량(opl-cluster·opl-api)** — `opl_types::quota`, `quota_check`(+`check_exclusive` 훑기에 붙임)·`Rejection::QuotaExceeded` → 409 `quota_exceeded`, 여덟 곳 사전 확인, `GET /folders/{id}/usage`, memnet 세 노드 경쟁 시험, `quota_apply_cost_with_10k_vms`. (`ab39736`)
- [x] **Task 5: 2단계 인증 — TOTP·복구 코드·대기 세션·정책·초기화, WebAuthn 최소 검증기(opl-api)** — `two_factor.rs`(대기 세션·`PendingPrincipal`·`Enroller`, CAS 재사용 막기, 시도 제한, 본인 확인, 정책 PATCH, 관리자 초기화), `webauthn.rs`(ES256·EdDSA·RS256, `ciborium`), 시험 안 소프트웨어 인증기, 두 노드 같은 코드 한 번, `deny` 확인. (`f16a239`)
- [x] **Task 6: folder·role·group·grant·perm·2fa 명령, `--folder`, 로그인 둘째 단계, opld E2E(oplctl·opld)** — `oplctl/src/access.rs`·`two_factor.rs`, 경로 → id 풀기(`[KIND:]PATH`), `folder quota set|clear`·`folder usage`, `token create --scope`, `user 2fa-reset`, `user create --role none`, `settings set two_factor.*`, `login --code`와 터미널 강제 등록, `crates/opld/tests/permissions.rs`(노드 하나·둘). (`90c978d`)
- [x] **Task 7: 폴더 보기·끌어 놓기·일괄 옮기기, 권한 탭, 설정의 권한·역할·그룹·보안, 2FA 등록·로그인, `usePermissions` 숨기기(web)** — 서버 작은 바꿈 `POST /vms`의 머신 종류 기본값(`134e87a`), `usePermissions()`(`/permissions/mine`)와 43곳 옮기기, `LiveProvider`가 읽을 수 있는 루트 종류만 구독, `folders/`(트리·DnD·옮기기 창·사용량 막대), `permissions/`(권한 패널·방패 서랍·설정 화면), `accounts/TwoFactor*`(QR·패스키·복구 코드), 로그인 둘째 단계와 강제 등록, Playwright `permissions.spec.ts`. (`9d65c63`)
- [x] **Task 8: 가이드·README·계획 문서** — 새 가이드 "폴더와 권한"(위임 예, 거부 없는 설계로 빼기, 권한 표, 그룹·외부 그룹, 토큰 범위, 404/403과 거르는 곳, 할당량, 2FA·패스키·IP 접속 한계, 웹, API, 섞인 클러스터, 이름 노출 등 한계), "웹 화면 둘러보기"의 영역·탭 표와 권한 항목, "접근과 보안"의 역할·LDAP·OIDC·토큰 고치기, README 상태·기능 표·안내 표, `docs/index.md`, 개발 문서의 새 시험 명령, 이 계획 문서와 실행 기록.

## 완료 기준 (계획 62)

- grant·폴더가 하나도 없는 클러스터에서 모든 (`Role`, `Action`, `Kind`)의 답이 옛 `RoleAuthorizer`와 같고, 옛 JSON(`VmSpec`·`UserSpec`·`ApiTokenSpec`·`SessionSpec`·`SettingsSpec`)이 바이트 그대로다.
- 색인이 참조 구현과 같고(proptest), grant를 더하면 권한이 줄지 않으며, propagate 거짓은 자손에 닿지 않는다.
- OpenAPI의 모든 경로가 경로표에 있고, 폴더 A 사용자로 모든 목록 경로가 A만 돌려주며, B 객체는 404·A의 권한 밖 동작은 403이다. `/watch`는 거르고 옮겨진 객체에 `deleted`를 보낸다. 대기 세션 쿠키로는 모든 경로가 401이다.
- 위임이 자기 권한 밖을 주지 못하고(403), 자기 폴더 할당량을 올리지 못한다(부모의 `inventory.quota`).
- 두 노드에서 같은 폴더에 동시에 만들어도 할당량을 넘지 않고(memnet·opld 둘 다), 줄인 상한 아래서 노드 바꾸기·끄기는 통과한다.
- TOTP 코드는 두 노드에서 동시에 내도 한 번만, 복구 코드는 한 번만 통과하고, 정책이 등록을 강제하며, 관리자 초기화가 세션을 끊는다. WebAuthn ES256·EdDSA·RS256 등록·로그인이 되고 틀린 origin·challenge·rpId·서명 횟수 되감기는 거절된다.
- 브라우저 E2E: 전역 역할이 없는 폴더 사용자가 VM 목록·폴더 트리·팔레트에 자기 폴더만 보고, + 만들기가 그 폴더를 골라 노드 칸 없이 VM을 만들며, 인증 앱을 켠 뒤 둘째 단계로 다시 로그인한다.
- 모든 새 쓰기는 `folders`·`two-factor` 기능 뒤에 있다(섞인 클러스터 409 `cluster_upgrading`).
- `cargo fmt`·`clippy -D warnings`·`cargo test --workspace`·`cargo deny check`, `pnpm typecheck`·`lint`·`test`, 로컬 브라우저 E2E가 통과하고 svc01 일반 시험이 통과한다.

## 알고 받아들인 제한

- 이름이 종류 전체에서 하나라 만들기의 409 `name_taken`이 볼 수 없는 객체의 이름을 드러낸다(팀 접두사 권함).
- 거부 규칙이 없다. 위에서 받은 권한을 아래에서 빼려면 형제 폴더로 옮긴다.
- 외부 그룹은 로그인 때만 읽는다. 토큰만 쓰는 외부 사용자는 마지막 로그인의 그룹을 쓴다.
- 볼륨 늘리기 작업이 저장소를 늘린 뒤 크기 기록이 다른 노드와의 경쟁(또는 LVM·ZFS 올림)으로 할당량에 걸리면 디스크만 커진 채 기록은 옛 크기로 남고 작업이 실패한다(저장소 직전 다시 확인으로 틈을 좁힘).
- 할당량 폴더의 VM 쓰기는 apply마다 모든 VM을 훑는다: VM 1만 개에서 17.8 ms → 27.5 ms(릴리스). 파생 표 `folder_usage`는 만들지 않았다.
- `/watch`는 목록과 구독 사이(밀리초)에 옮겨진 객체를 놓칠 수 있다.
- 토큰 범위는 CLI·API에만 있고 웹 화면이 없다. 복제·복원·가져오기의 폴더 고르기는 API에만 있다.
- 웹 트리는 가상 스크롤이 없고 템플릿 탭에 트리가 없다. 끌어 놓기는 확인 없이 옮긴다.
- 볼륨·ISO·이미지·가져오기·백업 복원·템플릿 배포 단추는 그 권한이 어디에든 있으면 보이고 객체별 판단은 서버가 한다.
- 위임자는 사용자 목록을 읽지 못해 grant 주체를 id로 넣는다(이미 접근하는 주체는 고르기 목록).
- 패스키는 증명·MDS를 보지 않고, CLI에서 쓸 수 없고, IP 주소로 연 웹에서 쓸 수 없다. 정책을 켜기 전에 만든 토큰을 따로 표시하지 않는다.
- TOTP 비밀은 Raft에 평문이다(API·watch·감사에서 가리고 데이터 디렉터리 0700이 보호).

## 실행 후 기록 (2026-11-19)

- 8개 스테이지를 단계 커밋으로 구현했다(`b0fc154` 타입 … `9d65c63` 웹, 이 문서 커밋). 스테이지 7 앞에 서버 작은 바꿈 `134e87a`(머신 종류를 빼면 클러스터 아키텍처의 기본값)가 따로 있다. 설계와 다른 것·결정은 설계 문서 §18에 스테이지마다 남겼다.
- 워크스페이스 시험 3335개(`cargo test --workspace`). 새 시험: opl-types 동치·serde·RFC 6238, opl-api `authz` 동치·`insta` 행렬·proptest 넷·색인 벤치, `tests/permissions.rs`·`quotas.rs`·`two_factor.rs`·`webauthn.rs`, `two_nodes.rs`의 같은 TOTP 한 번, opl-cluster 상태 머신·`three_nodes.rs` 할당량 경쟁, opld `tests/permissions.rs`(노드 하나·둘, 각 약 7초).
- 웹 시험 1268개(파일 128개). 브라우저 E2E는 spec 2개(`vm.spec.ts`, 새 `permissions.spec.ts` — QEMU 없음)이고 `web.rs`의 두 시험이 하나씩 차례로 돌린다.
- 묶음 크기(`index.html`이 받는 JS gzip 합): 377,048 → 389,876 B(**+12.8 KB**, 그중 en·ko 문구 +6.6 KB). 2FA 등록 화면·권한 화면은 lazy다. CSS 14,219 → 14,461 B.
- 할당량 apply 비용(VM 1만 개, 릴리스, 이 Mac): 할당량 없는 폴더 17.8 ms, 할당량 폴더 27.5 ms, 할당량 폴더의 노드 바꾸기 17.1 ms. 권한 색인(디버그, VM 1만·grant 500·폴더 200): 만들기 약 1.5 ms, 거르기 약 6 ms.
- 새 의존성: `ciborium` 0.2.2(Apache-2.0), dev `proptest` 1.12, npm `qrcode-generator` 2.0.4(MIT). `cargo deny check` 통과.
- 리뷰 1(서버·웹 나눠서): 서버 중대 1·중요 7·경미 7 — 위임 상승(전파 안 되는 grant로 전파 grant 만들기)을 막고 grant가 닿는 범위 전체를 검사, 옮기기 대상 풀·스냅샷 되돌리기의 폴더 유지, 2FA 닫힌 실패·Raft에 둔 사용자별 실패 횟수(5번이면 대기 세션 끝, 30초~15분 막음)·폴더 관리자도 정책 대상, 크기 키우기는 할당량을 먼저 잡고, 저장소 풀·브리지 만들기는 루트 `cluster.admin`, CD-ROM ISO 읽기 확인, 정보 새는 곳 막기(`6249e64`). 웹 중요 4·경미 7 — 폴더 사용자의 홈 위젯, 기본 폴더는 권한 뒤에, 휴대폰 카드 제목, 서버 오류 문구와 줄 수 있는 역할만, 둘째 요소 추가의 확인, 풀·브리지 만들기 숨기기(`96f3003`).
- 리뷰 2(리뷰 1 수정은 옳다고 확인): 서버 중요 3·경미 8 — 폴더 옮기기 상승(옛·새 부모 양쪽의 내용 `inventory.move`), 복제 대상 풀 확인, 원격 백업 보내기는 `cluster.admin`, 지운 사용자·대상의 grant 정리(즉시 + 리더 1분 청소), 개수 상한(폴더·grant 1만, 토큰 사용자당 100), Task 요청자 라벨, 권한 잃으면 콘솔 닫기, 실패 횟수 15분 감쇠, whoami에 연결 노드(`04c1054`). 웹 중요 5·경미 6 — 폴더 사용자의 풀 선택과 "이 노드"는 whoami에서, 루트 권한 가져오기 숨기기, 재시도에 복구 코드 아끼기, 모르는 권한 보존, 지운 영역 정리, 팔레트에 폴더(`a5257a0`).
- **탐침이 찾은 스택 넘침:** `templates` 시험이 디버그 빌드에서 시험 스레드(2 MiB)를 넘쳤다. opld가 아니라 시험이 부르는 `oplctl::run`에서 clap 명령 만들기가 큰 poll 프레임 위에 올라간 것으로, main에서도 이미 약 100 KB만 남아 있었고 계획 62의 새 하위 명령이 넘겼다. `run`을 인자 해석과 `Box::pin(run_cli(..))`로 나눠 이 경로가 약 2.06 MB → 0.95 MB가 됐다(`f4468f9`). 시험의 스택 크기는 늘리지 않았다.
- svc01 일반 시험(리뷰 2 수정 `a5257a0`에서): fmt·clippy(Rust 1.99)·시험 3439개·starve·규모 100·저장소 시험 모두 통과. 스택 넘침 수정 뒤 이 Mac에서 워크스페이스 시험 3354개 통과.
- GitHub 탐침: `a5257a0`에서 install-media·linux-kvm이 위 스택 넘침으로 실패, 나머지(ceph·ldap·package·iso-e2e bios·uefi·catalog-links) 통과 → `f4468f9`에서 모두 통과(linux-kvm은 처음에 Ceph apt 받기가 미러가 느려 15분 제한에 걸려 다시 돌림). linux-kvm 시험 1131개, 브라우저 E2E 둘(`browser_shows_a_folder_user_only_their_folder`, `browser_uploads_image_runs_vm_and_cleans_up`) 통과.
- 배포: (배포 후 채움)

## 다음 계획으로 넘길 사항

- **계획 63~65: SDN** — 로드맵 순서대로 다음은 SDN이다(존·VNet·서브넷·IPAM 등은 그 설계에서 정한다). 네트워크 폴더·`network.assign`·`network.config` 권한을 그대로 쓴다.
- **토큰 범위 웹 화면**: API 토큰 화면에서 권한·대상을 골라 범위 토큰을 만들고 목록에 범위를 보이기. 정책을 켜기 전에 만든 토큰 표시.
- 복제·복원·가져오기의 폴더 고르기(CLI `vm clone`·`import run`·`backup restore`의 `--folder`, 웹 대화상자).
- 위임자가 grant 주체를 고를 수 있는 좁은 사용자 찾기 API(지금은 id 넣기).
- 웹 트리의 가상 스크롤(폴더 수천 개), 템플릿 탭의 트리.
- 볼륨·ISO·이미지·가져오기·백업 복원·템플릿 배포 단추를 객체(주인 VM·풀)별로 정확히 가리기.
- 볼륨 늘리기 할당량 틈 닫기(저장소 전 예약 또는 기록 실패 때 되돌리기), 할당량 apply 비용이 커지면 파생 표 `folder_usage`.
- 설계 §16의 나중 일: 볼륨·백업·보안 그룹·노드 폴더, 태그(라벨) 기반 grant, 폴더별 감사 로그 읽기와 Prometheus, 실행 중만 세는 할당량·풀별 디스크 할당량·붙지 않은 볼륨 세기, WebAuthn 증명·MDS·CLI 패스키, OIDC `amr`/`acr` 요구, 시간 제한 grant, 외부 그룹 주기적 다시 읽기(LDAP 동기화), 이름을 폴더 범위로 만들기(이름 노출 없애기).
