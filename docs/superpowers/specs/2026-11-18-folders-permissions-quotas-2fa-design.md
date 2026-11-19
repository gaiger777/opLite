# 폴더·객체 권한·폴더 할당량·2단계 인증 설계 (계획 62)

작성일: 2026-11-18
상위 문서: `2026-11-06-enterprise-design.md`(설계 4절 `Authorizer` 훅, 감사 로그), 계획 34~38 인증 영역(`realm.rs`·`ldap.rs`·`oidc_login.rs`), 계획 46 공유 세션(`SHARED_SESSIONS`), `2026-11-17-templates-linked-clones-design.md`(계획 58 "Raft에 쓰고 다시 보기"), `2026-11-18-dashboard-metrics-notifications-design.md`(계획 60), `2026-11-18-ui-ux-redesign-design.md`(계획 61, 다른 가지에서 구현 중)
기준 트리: 계획 60 끝(`26ae1b4`). 이 문서는 구현 전 설계다. 스테이지 보고는 구현하면서 덧붙인다.

## 1. 목적

지금 권한은 사용자마다 전역 역할 하나다(`opl_types::auth::Role` = `Admin|Operator|Viewer`, `UserSpec.role`). 확인은 `RoleAuthorizer::check(principal, Action, Kind)`(`opl-api/src/auth.rs`) 하나이고, 핸들러 148곳이 `state.authorize(&principal, Action::…, XSpec::KIND)`를 부른다. 웹은 같은 규칙을 `web/src/auth/permissions.ts`의 `can(role, action, kind)`로 흉내 낸다. 그래서 "A팀은 A팀 VM만 다룬다"를 할 수 없고, VM이 수천 개면 목록을 나눌 길도 없다. 팀별 자원 상한과 2단계 인증도 없다. 이 계획은 vSphere식 **인벤토리 폴더 + 객체 권한(아래로 전파)**, 폴더 **할당량**, **TOTP·패스키 2단계 인증**을 넣는다.

| | Proxmox VE 8 | vSphere 8 | Orbivisor 계획 62 |
|---|---|---|---|
| 묶음 | 리소스 풀(8.x부터 중첩), 경로 ACL `/vms/100`·`/pool/x` | VM·호스트·데이터스토어·네트워크 폴더 | 종류별 폴더 트리 셋(VM·네트워크·스토리지), 중첩(§5) |
| 권한 | (사용자·그룹·토큰, 역할, 경로, propagate), 더 구체적인 경로가 이김, `NoAccess` | (주체, 역할, 객체, propagate), 객체 쪽이 이김, "No Access" | (주체, 역할, 대상, propagate)의 **합집합**, 거부 없음(§6) |
| 역할 | 내장 + 사용자 정의(권한 목록) | 내장 + 사용자 정의(권한 목록) | 내장 셋(지금 역할 그대로) + 사용자 정의(§6.2) |
| 할당량 | 없음 | Cloud Director에서만 | 폴더마다 vCPU·메모리·디스크·VM 수, 하위 트리 합, Raft에서 확인(§9) |
| 2FA | TOTP·WebAuthn·Yubico·복구 코드, 영역마다 강제 | vCenter SSO: RSA SecurID·스마트카드·IdP | TOTP·복구 코드·WebAuthn, 역할·영역마다 강제(§10) |

## 2. 범위

| 넣는 것 | 넣지 않는 것(§16) |
|---|---|
| 종류별 중첩 폴더(VM·템플릿 / 네트워크 / 스토리지 풀), 옮기기·이름 바꾸기·빈 폴더 지우기 | 볼륨·백업·노드 폴더, 태그 기반 권한 |
| 권한(사용자·그룹 → 역할 → 루트·폴더·객체, propagate), 사용자 정의 역할, 로컬 그룹과 외부 그룹 연결 | 거부 규칙·"No Access" 역할, 시간 제한 권한 |
| 모든 API 경로·목록·`/watch`·지표·경보·Task·콘솔·백업 복원의 확인, 유효 권한 조회 | 폴더 단위 감사 로그 읽기, 폴더별 Prometheus |
| 폴더 할당량(vCPU·메모리·디스크 바이트·VM 수), 사용량 보기, 노드 사이 경쟁에도 지킴 | 실행 중만 세는 할당량, CPU·IO 속도 제한, 풀별 디스크 할당량 |
| TOTP(RFC 6238)·복구 코드·WebAuthn(패스키), 정책(역할·영역), 관리자 초기화, 토큰 범위 | SMS·이메일 코드, Yubico OTP, CLI 패스키, OIDC `amr` 강제 |
| 웹: VM·인프라 영역의 폴더 보기, 끌어 놓기·일괄 옮기기, 설정의 권한·역할·그룹·보안, "권한" 탭 | 폴더별 대시보드 |

**기능 둘**(`opl_types::node`, `FEATURES` 끝). **`folders`**(`FOLDERS`)는 새 종류 `Folder`·`Grant`·`AccessRole`·`Group`, 스펙의 `folder` 칸, `Role::None`, `UserSpec.external_groups`, `ApiTokenSpec.scope`, 할당량을 가린다. **`two-factor`**(`TWO_FACTOR`)는 새 종류 `TwoFactor`, `SessionSpec.pending`, `SettingsSpec.two_factor`를 가린다. 그 쓰기는 모든 노드가 기능을 알릴 때만 받고, 아니면 409 `cluster_upgrading`이다(`require_feature`). 계획 58~60처럼 기능 검사는 검증보다 먼저 한다(§11).

## 3. 지금 있는 것 (계획 60 끝)

- **인증**: `AppState::authenticate`가 `Authorization: Bearer opl_…`(토큰 → `ApiTokenSpec.user`)나 세션 쿠키(`find_session` → 메모리 `Sessions` 또는 Raft `Session`, `session_epoch` 비교, `realm_exists`)로 `Principal { user, name, role }`을 만든다. 요청마다 `UserSpec`을 다시 읽으므로 역할 변경이 곧바로 반영된다. `FromRequestParts for Principal`이 모든 핸들러의 입구다.
- **로그인**: `users::login` → `login_local`(argon2, `attempts` 제한) 또는 `login_realm`(LDAP 바인드 → `RoleMapping::role_for(groups)` → `upsert_external`). OIDC는 `oidc_login.rs` 콜백이 같은 `role_for`·`upsert_external`·`new_session`을 쓴다. 외부 그룹은 로그인 때 역할을 고르는 데만 쓰고 저장하지 않는다.
- **권한**: `Authorizer` 트레이트(“엔터프라이즈 플러그인이 세분화된 RBAC로 바꾼다”)와 `RoleAuthorizer`. `account` 종류(`User`·`ApiToken`·`JoinToken`·`Realm`·`Session`·`NodeFence`·`NotificationTarget`)는 admin만 읽는다. Operator는 `Vm`·`Volume`·`Backup`·`BackupJob`·`VmImport`만 쓴다. 감사 로그(`audit.rs`, `ReadAudit`)는 admin만 읽는다.
- **목록·watch**: `resources.rs`의 `list<S>`/`get<S>` 매크로가 종류 단위로 `Read`를 본 뒤 전부 돌려준다. `/watch`(`watch.rs`)는 `WATCHABLE` 15종을 종류 단위로 확인한 뒤 모든 이벤트를 보낸다.
- **상태 머신**: `apply_command`(`opl-cluster/src/state_machine.rs`)는 JSON 스펙을 보고 결정적으로 거부한다. `check_exclusive`(`exclusive.rs`)가 VM 쓰기마다 모든 VM을 훑어 볼륨·MAC 중복을 막는다(`Rejection::InUse`). 이것이 할당량·폴더 검사의 본이다.
- **비밀 저장의 본**: 비밀번호는 argon2 PHC, 토큰·세션은 SHA-256, LDAP bind 비밀번호·CA 키(`ClusterSpec.ca_key_pem`)는 평문으로 Raft에 두고 API·`Debug`에서 가린다. 저장 시 암호화는 어디에도 없다.

## 4. 타입 (opl-types)

- **`folder.rs`**(새): `FolderSpec { name: String, kind: FolderKind (Vm|Network|Storage), parent: Option<Uuid>, quota: Option<FolderQuota> }`, `deny_unknown_fields`. 리소스 이름은 폴더 id 문자열로 둔다(이름 표는 종류 전체에서 하나라서 형제마다 같은 이름 `prod`를 쓰려면 화면 이름을 스펙에 둔다). `FolderQuota { vcpus, memory_mib, disk_bytes, vms: Option<u64> }`. `FolderUsage`도 같은 네 칸이다.
- **스펙의 `folder` 칸**: `VmSpec`·`NetworkSpec`·`StoragePoolSpec`에 `#[serde(default, skip_serializing_if = "Option::is_none")] folder: Option<Uuid>`. 없으면 그 종류의 루트이므로 옮기기 작업이 없고, 루트에 둔 리소스는 바이트까지 지금과 같다. 템플릿은 `VmSpec`이라 VM 트리에 산다.
- **`authz.rs`**(새):
  - `Privilege`(표 §6.2, kebab 문자열 `"vm.power"`)와 `PrivSet(u64)` 비트 집합. 저장은 문자열 목록이고 모르는 이름은 읽을 때 버린다(경고). 새 권한을 더해도 앞 버전이 역할을 푼다. 쓰기 검증은 모르는 이름을 400으로 막는다.
  - `builtin_privileges(Role) -> PrivSet`: 지금 역할의 뜻 그대로(§6.3).
  - `AccessRoleSpec { privileges: Vec<String>, description }`(`Kind::AccessRole`, "access-role"). 내장 역할 `admin`·`operator`·`viewer`는 저장하지 않고 이름을 예약한다.
  - `GrantSpec { principal: PrincipalRef (User{id}|Group{id}), role: RoleRef (Builtin(Role)|Custom(Uuid)), target: GrantTarget (Root|Folder{id}|Object{kind,id}), propagate: bool = true }`(`Kind::Grant`).
  - `GroupSpec { members: Vec<Uuid>, external: Vec<ExternalGroup { realm, group }> }`(`Kind::Group`).
- **`auth.rs`**: `Role::None`("none", 전역 역할 없음 — 권한은 grant로만), `UserSpec.external_groups: Vec<String>`(로그인 때 본 외부 그룹, 비면 쓰지 않음), `ApiTokenSpec.scope: Option<TokenScope { privileges: Vec<String>, targets: Vec<GrantTarget> }>`. `RoleMapping`은 그대로이고 `default: none`을 받는다.
- **`two_factor.rs`**(새): `TwoFactorSpec { user: Uuid, totp: Option<Totp { secret: String /*base32*/, confirmed: bool, created_at }>, recovery: Vec<String /*SHA-256*/>, webauthn: Vec<Passkey { id, name, rp_id, cose_key: Vec<u8>, sign_count: u32, created_at }> }`, 상태 `{ last_totp_step: u64, last_used: u64 }`(`Kind::TwoFactor`, `account` 목록, watch 불가). `Debug`는 `secret`을 가린다. `SessionSpec.pending: Option<PendingLogin { expires_at, enroll: bool, challenge: Option<String> }>`, `SettingsSpec.two_factor: Option<TwoFactorPolicy { roles: Vec<Role>, realms: Vec<String>, rp_id: Option<String> }>`.
- **순수 함수**(`authz_eval.rs`, IO 없음): `effective(&Snapshot, principal, object) -> PrivSet`의 참조 구현(조상을 걸어 올라가며 합집합). §7 색인의 시험 기준이다. `totp(secret, step, digits) -> u32`(ring `HMAC_SHA1_FOR_LEGACY_USE_ONLY`, base32는 이미 트리에 있는 `data-encoding`).

## 5. 폴더 트리 (opl-cluster 상태 머신·opl-api)

- **종류별 트리 셋**(vSphere의 VM/Network/Datastore 폴더). 한 트리로 모든 종류를 섞지 않는 이유: 계획 61에서 VM은 "가상 머신" 영역, 네트워크·풀은 "인프라" 영역에 산다. 할당량은 VM 트리에만 뜻이 있다. 종류가 다른 것을 같은 폴더에 두면 옮기기·할당량·화면 규칙마다 갈래가 생긴다. 대가로 "A팀 VM + A팀 네트워크"는 grant 둘이다(그룹 하나에 두 줄, §6.4).
- **루트**: 종류마다 있는 보이지 않는 루트(`parent: None`). 루트는 리소스가 아니다. 루트 권한은 `GrantTarget::Root` 하나이고 세 트리와 클러스터 객체 모두의 조상이다.
- **상태 머신 검사**(새 `opl-cluster/src/folders.rs`, `apply_command`에서 `check_exclusive` 옆): 모두 결정적이고 `Folder` 리소스가 없으면 아무 일도 하지 않는다(옛 노드와 결과가 같다, §11).
  - `Folder` Create/UpdateSpec: `parent`가 있고 종류가 같다, 걸어 올라가 자기를 만나지 않는다(`Rejection::FolderCycle`), 깊이 ≤ 16.
  - 형제 이름 중복: `exclusive_values`에 `Folder → ("name", "{parent}/{소문자 name}")`를 더해 `InUse`로 막는다(노드 둘이 같은 이름을 동시에 만들어도 하나만).
  - `Folder` Delete: 자식 폴더나 그 폴더를 가리키는 VM·네트워크·풀이 있으면 `Rejection::FolderNotEmpty`.
  - VM·네트워크·풀 Create/UpdateSpec: `folder`가 있으면 그 폴더가 있고 종류가 맞아야 한다(`NotFound`). 지우기와 넣기가 엇갈려도 Raft 순서로 하나만 이긴다.
- **API**: 폴더 PATCH(이름·`parent`·할당량), 리소스 옮기기는 `POST /folders/{id}/members`(§13)로만 한다. `PUT /vms/{key}`가 `folder`를 바꾸면 400 `"use POST /folders/{id}/members to move"`(옮기기 권한을 따로 보기 위해). 리소스 만들기 요청은 `folder`(id 또는 경로 `/teamA/prod`)를 받는다. 없으면 루트이지만, 루트에 `vm.create`가 없는 사용자는 400 `folder_required`.

## 6. 권한 모델

### 6.1 grant와 평가

- grant = (주체: 사용자 또는 그룹, 역할, 대상: 루트·폴더·단일 객체, propagate). 유효 권한(사용자 U, 객체 O) = `builtin(U.role)`(루트 암묵 grant) ∪ U와 U가 든 그룹의 grant 가운데 (O에 직접 건 것) ∪ (O의 조상 폴더·루트에 `propagate`로 건 것). propagate가 거짓인 폴더 grant는 그 폴더 객체 자체(이름 바꾸기·지우기·그 폴더의 권한 보기)에만 닿는다(vSphere와 같다).
- **거부 규칙은 두지 않는다.** vSphere·Proxmox는 "더 구체적인 쪽이 이김"과 No Access로 가린다. 그러면 평가가 순서에 따라 달라지고(사용자 grant가 그룹 grant를 덮는가, 자식 grant가 부모를 덮는가), "왜 못 하나"를 설명하기 어렵고, 비트 OR 한 번으로 캐시할 수 없다. 합집합은 단조라서 grant를 더하면 권한이 늘기만 하고, 색인이 단순하고, 성질 시험(§14)이 쉽다. 대가: "prod 전체에서 payroll VM만 빼기"는 payroll을 형제 폴더로 옮겨 푼다(가이드에 적는다).
- 모든 권한은 그 객체의 `system.read`를 품는다(풀에 `storage.allocate`만 받아도 풀이 보인다).
- **읽을 수 없는 객체는 404**, 읽을 수 있으나 권한이 없으면 403 `forbidden`. 이름·id로 남의 객체 존재를 알 수 없게 한다. 다만 이름이 종류 전체에서 하나라서 만들기의 409 `name_taken`은 남는다(§12 한계).

### 6.2 권한 목록

| 권한 | 대상 | 지금 확인(Action, Kind) |
|---|---|---|
| `system.read` | 보기: 목록·상세·watch·지표·Task·경보 | Read 그 종류 |
| `vm.create` · `vm.delete` | 폴더에 만들기(새로·가져오기·복원·배포 받기) · 지우기 | Write Vm·VmImport |
| `vm.config` | `PUT` 스펙, CD, cloud-init, 펌웨어 재설정, 핫플러그, 템플릿 바꾸기·되돌리기 | Write Vm |
| `vm.power` · `vm.console` | 켜기·끄기·재시작·에이전트 종료 · 콘솔 | Operate Vm |
| `vm.snapshot` · `vm.migrate` · `vm.clone` | 스냅샷 · 라이브·저장소 옮기기 · 원본(템플릿) 쪽 복제·배포 | Write Vm |
| `backup.create` · `backup.restore` · `backup.delete` · `backup.job` | 지금 백업 · 복원 · 지우기 · 백업 작업(루트) | Write Backup·BackupJob |
| `storage.allocate` · `storage.config` | 풀에 볼륨 만들기·늘리기·지우기 · 풀 만들기·바꾸기 | Write Volume · Write StoragePool |
| `network.assign` · `network.config` | NIC를 그 네트워크에 붙이기 · 네트워크·보안 그룹 쓰기 | (Write Vm) · Write Network·SecurityGroup |
| `inventory.folder` · `inventory.move` · `inventory.quota` | 하위 폴더 만들기·이름·지우기 · 옮기기(원본·대상 둘 다) · 자식 폴더 할당량 | 새 것 |
| `permission.modify` | 이 대상 아래 grant 주기·빼기(자기 권한 안에서) | 새 것 |
| `alert.read` · `alert.manage` | 경보 보기 · 규칙 쓰기(루트) | Read · Write AlertRule |
| `cluster.read` | 노드·클러스터·설정·HA·배치·Ceph·원격 읽기, 노드·클러스터 지표, Prometheus | Read 그 밖(계정 아님) |
| `cluster.admin` | 계정·영역·남의 토큰·알림 대상·감사 로그·설정·노드·HA·배치·Ceph·원격 쓰기, 역할·그룹·루트 grant | 그 밖 전부 |

### 6.3 내장 역할 = 지금 역할

`admin` = 전부. `operator` = `system.read`·`cluster.read`·`alert.read`·`vm.*`·`backup.*`·`storage.allocate`·`network.assign`. `viewer` = `system.read`·`cluster.read`·`alert.read`. `UserSpec.role`은 그대로 남고 "루트에 그 내장 역할 grant"로 읽는다. 그래서 grant가 하나도 없는 클러스터에서는 모든 (Role, Action, Kind)가 지금 `RoleAuthorizer::check`와 같은 답을 낸다(§14 동치 시험, 옛 구현을 시험 안 참조로 남긴다). 새 권한(`inventory.*`·`permission.modify`·`network.config`·`storage.config`)은 operator에 넣지 않는다.

### 6.4 그룹과 외부 그룹

- 로컬 그룹 `Group`: `members`(로컬·외부 사용자 id)와 `external`(`{realm, group}` — LDAP 그룹 DN 또는 OIDC `groups` 클레임 값). 외부 사용자는 그 영역의 `external_groups`에 같은 값(앞뒤 공백 떼고 ASCII 대소문자 무시, `role_for`와 같은 비교)이 있으면 그룹에 든다.
- **grant는 외부 그룹 이름을 바로 가리키지 않는다.** DN 문자열이 grant마다 흩어지면 오타를 잡을 곳이 없고, 디렉터리 그룹 이름이 바뀌면 grant를 다 고쳐야 한다. 연결은 `Group` 한 곳에서 하고, 화면은 "이 그룹에 든 외부 그룹"을 보인다.
- `login_realm`·OIDC 콜백이 `upsert_external`에 그룹 목록을 넘긴다. 바뀌었을 때만 `external_groups`를 쓴다(세션은 끊지 않는다 — 요청마다 색인을 보므로 곧바로 반영). 토큰만 쓰는 외부 사용자는 마지막 로그인의 그룹을 쓴다(README 한계).
- `RoleMapping`(그룹 → 전역 역할)은 그대로 둔다. 폴더 grant만 받을 외부 사용자는 매핑 `default: none`으로 로그인시킨다. 둘 다 없으면 지금처럼 403 `no_role`.

### 6.5 위임과 상승 막기

grant를 만들려면 대상에 `permission.modify`가 있어야 하고, 역할의 권한이 그 대상에서 자기 유효 권한의 부분집합이어야 한다(아니면 403 `"cannot grant privileges you do not hold: vm.delete"`). 루트 grant, 사용자 정의 역할·그룹 쓰기는 `cluster.admin`이다(역할 하나를 고치면 모든 grant가 바뀐다). 할당량은 그 폴더의 **부모**에서 `inventory.quota`를 요구한다(팀 관리자가 자기 상한을 올리지 못하게).

### 6.6 API 토큰의 범위

토큰 유효 권한 = 주인 유효 권한 ∩ `scope`(있으면 `targets`에 `privileges`를 건 propagate grant로 읽은 천장). 범위 없는 토큰은 지금처럼 주인과 같다. 토큰은 2FA를 거치지 않으므로 자동화에는 범위 토큰을 권한다(§10.4).

## 7. 평가 색인 (opl-api `authz/`)

- **노드마다, 리더 없이.** 모든 API 노드는 Raft 상태를 로컬에 가진다. 색인은 그 노드의 저장소에서 만들고 리더에 아무것도 묻지 않는다. 수백 노드에서 리더 짐이 늘지 않는다.
- **세대**: `AuthzIndex`가 `cluster.follow`로 커밋 흐름을 받아 `Folder`·`Grant`·`AccessRole`·`Group`·`User`·`ApiToken` 이벤트에 `generation: AtomicU64`를 올린다. VM 이동은 세대를 올리지 않는다(객체의 폴더는 확인할 때 읽는다).
- **주체별 색인** `EffectiveIndex { generation, root: PrivSet, folders: HashMap<Uuid, PrivSet>, objects: HashMap<(Kind, Uuid), PrivSet> }`: 처음 요청에서 만들고 세대가 바뀌면 다시 만든다. 그 주체의 grant를 대상별로 모은 뒤 폴더 트리를 위에서 아래로 한 번 훑어 propagate를 내린다(O(폴더 + grant)). 확인은 `folders[obj.folder] | objects[obj]` 두 번 찾기다. 캐시는 주체 1024개 LRU(`HashMap` + 시계 바늘, `Mutex` 하나, 만들기는 잠금 밖).
- **빠른 길**: 루트에 `admin`(전역 역할 포함)이면 색인 없이 전부 참. 목록 거르기는 VM 1만 개 × 확인 2회라 1 ms 안이다(§14 벤치 기준값).
- **API**: `Authz::require(principal, Privilege, &impl AuthzObject) -> Result<(), ApiError>`(못 읽으면 404, 읽으면 403), `Authz::filter(principal, iter)`, `Authz::require_root(principal, Privilege)`. `AuthzObject`는 `Vm`·`Network`·`StoragePool`·`Folder`에 구현하고, 파생 객체는 주인을 따른다: `Volume`(붙은 VM 또는 풀), `Snapshot`(VM), `Backup`(VM, 지워졌으면 `vm_spec.folder`), `Task`(`TaskOp` 대상 종류 + `target`, 대상이 없으면 루트), `VmImport`(대상 폴더). `Principal`은 그대로 두고 색인이 `user`로 그룹·토큰 범위를 찾는다(토큰 요청은 `Principal.token: Option<Uuid>`를 더한다).
- `Authorizer` 트레이트와 `state.authorize(action, kind)`는 **루트 확인 전용**으로 남긴다(Node·Settings·Ceph·HA·배치·원격·영역·사용자 등 루트 종류의 호출은 바뀌지 않는다). 폴더를 따르는 종류(Vm·Volume·Snapshot·Backup·VmImport·Network·StoragePool)에 `authorize`를 부르면 디버그 빌드의 `debug_assert!`와 경로 시험(§14)이 막는다. 보안 그룹·백업 작업은 루트 종류다.

## 8. 시행 범위

| 면 | 규칙 |
|---|---|
| 모든 경로 | 위 표의 권한. 경로표 `authz_routes.rs`가 (메서드, 경로) → 권한·대상 해석을 적고, OpenAPI 경로가 표에 없으면 시험이 실패한다 |
| 목록 | `list<S>` 매크로와 손으로 짠 목록(`vms`·`volumes`·`backups`·`snapshots`·`tasks`·`vm_imports`)이 `Authz::filter`를 거친다. 볼 수 없는 것은 빠지고 `revision`은 그대로다 |
| `/watch` | 종류 확인 대신 이벤트마다 거른다. 연결마다 보낸 id 집합을 두고, 보이던 객체가 안 보이게 되면(폴더 이동) 가짜 `Deleted`를 보낸다. 세대가 바뀌면 `{"type":"error","code":"permissions"}`로 닫아 목록을 다시 읽게 한다(웹은 `expired`와 같이 다룬다) |
| 검색·⌘K | 팔레트는 `useList` 캐시만 쓴다(계획 61 §5.2). 목록 거르기로 충분하고 새 API가 없다 |
| 지표 | `/vms/{key}/metrics[/history]`는 VM `system.read`, 풀 기록은 풀 `system.read`. 노드 지표·`/metrics/cluster`·Prometheus `/metrics`는 루트 `cluster.read`(노드 합계가 다른 팀의 부하를 드러내므로 VM별 거르기 대신 클러스터 읽기를 요구한다). `Digest`의 상위 VM 목록은 거른다 |
| 경보 | 규칙 쓰기는 루트 `alert.manage`. `/alerts`·`/alerts/history`는 subject로 거른다(VM → VM `system.read`, 노드·풀 → `cluster.read`·풀 읽기). 알림 대상은 `cluster.admin` |
| Task | 목록·상세는 대상 읽기로 거르고, 취소는 그 op의 권한이다. 대상 없는 Task(노드 비우기 등)는 `cluster.read` |
| 감사 로그 | 루트 `cluster.admin`(지금 admin과 같다). 폴더별 읽기는 §16 |
| 콘솔 | `vm.console`(WebSocket 업그레이드 전에) |
| 백업·복원 | 지금 백업 `backup.create`(VM), 복원 `backup.restore`(백업) + 대상 폴더 `vm.create` + 풀 `storage.allocate` + 네트워크 `network.assign` + 할당량. 원격 대상은 `cluster.admin` |
| 만들기·배포 | 대상 폴더 `vm.create`, 템플릿 `vm.clone`, 풀 `storage.allocate`, NIC마다 `network.assign`, 보안 그룹은 읽기 |

## 9. 폴더 할당량

- **세는 것(할당 기준, 전원과 무관)**: 하위 트리의 VM에 대해 `vcpus = CpuSpec::vcpus()`, `memory_mib`, `disk_bytes` = `disks[].volume`이 가리키는 볼륨의 `size_bytes` 합(연결 복제도 가상 크기), `vms` = 템플릿 아닌 VM 수. 템플릿은 디스크만 센다. 붙지 않은 볼륨은 세지 않는다(볼륨에는 폴더가 없다, §16).
- **중첩**: 사용량은 하위 트리 합이고, 쓰기는 경로 위의 **할당량 있는 모든 조상**을 지켜야 한다(가장 빡빡한 것이 묶는다). 자식 상한을 부모에서 미리 떼어 두지 않는다 — 자식 상한 합이 부모를 넘어도 된다(과약정). 미리 떼면 쓰지 않는 몫이 묶이고, 부모 상한을 줄일 때 자식을 먼저 줄여야 하는 순서 문제가 생긴다. 자식 상한이 부모보다 크면 화면이 "부모가 묶음"을 보인다.
- **줄이기**: 사용량보다 낮게 둘 수 있다. 그때는 "초과"로 보이고 늘리는 쓰기만 막는다.
- **경쟁 안전 = Raft 안 확인.** `folders.rs`의 `check_quota`가 `apply_command`에서 VM Create/UpdateSpec, 볼륨 UpdateSpec(크기가 늘 때, 붙은 VM을 찾아), 폴더 UpdateSpec(`parent`가 바뀔 때)마다 돈다. 경로에 할당량 있는 조상이 없으면 바로 끝난다. 있으면 VM을 한 번 훑어(`check_exclusive`와 같은 훑기에 붙인다) 그 조상들의 사용량 전·후를 셈하고, **어느 차원이든 후 > 상한이고 후 > 전**이면 `Rejection::QuotaExceeded { folder, dimension, limit, used }`. "늘릴 때만"이라서 HA 복구·마이그레이션·비우기·균형 조정(노드만 바꿈), 이름 바꾸기, 초과 폴더의 끄기는 막히지 않는다. 커밋 순서로 결정되므로 노드 둘이 동시에 만들어도 뒤에 커밋된 쪽만 거절된다. 예약·CAS 재시도·리더 단일 지점이 없다.
- **API 사전 확인**: 만들기·복제·템플릿 배포(개수 n이면 n배)·크기 바꾸기·핫플러그·복원·가져오기는 요청 때 같은 계산(`quota::precheck`, 로컬 상태)으로 409 `quota_exceeded` `"folder /teamA would use 34 of 32 vCPUs"`를 먼저 낸다. 복제·복원·가져오기 Task는 디스크를 만든 뒤 VM을 쓰므로, 경쟁에서 진 Task는 마지막 쓰기에서 `QuotaExceeded`로 실패하고 지금 실패 정리가 볼륨을 지운다.
- **사용량 보기**: `GET /folders/{id}/usage`는 하위 트리 사용량·상한·묶는 조상을 로컬 상태로 셈한다(10초 캐시).
- **비용**: 할당량이 걸린 VM 쓰기의 apply가 VM 수만큼 JSON을 푼다. 이미 `check_exclusive`가 같은 훑기를 하므로 한 번에 합친다. `scale.rs`에 VM 1만 개 apply 시간 기준을 더하고, 넘으면 파생 표(`folder_usage`, 스냅샷 설치 때 다시 셈)로 바꾼다(§16).

## 10. 2단계 인증

### 10.1 요소

- **TOTP**: SHA-1, 6자리, 30초, ±1 칸 허용(RFC 6238 권장, 앱 호환). 재사용 막기: 받은 칸이 `last_totp_step` 이하이면 거절하고, 성공하면 `UpdateStatusIf`(revision CAS)로 올린다. 노드 둘에 같은 코드를 동시에 내도 하나만 통과한다.
- **복구 코드**: 10개, 80비트 무작위(`xxxx-xxxx-xxxx-xxxx`), SHA-256만 저장(엔트로피가 커서 argon2가 필요 없다, 토큰과 같다). 쓰면 `update_spec(expected_revision)`으로 지운다.
- **WebAuthn(패스키)**: 직접 짠 최소 검증기 `opl-api/src/webauthn.rs`. `webauthn-rs`는 MPL-2.0(`deny.toml` 허용 목록 밖)이고 openssl을 끌어와 aarch64 정적 빌드를 흔든다. 범위를 좁혀 짠다: 등록은 `attestation: "none"`(증명문은 보지 않음), 알고리즘은 ES256(ring `ECDSA_P256_SHA256_ASN1`)·EdDSA(ring `ED25519`)·RS256(ring `RsaPublicKeyComponents`), CBOR는 `ciborium`(Apache-2.0). 확인: `clientDataJSON`의 `type`·`challenge`·`origin`(요청 호스트 또는 `web_origins`), `rpIdHash`, UP 플래그, `signCount`(0이 아니고 줄면 거절·경고). RP ID는 `two_factor.rp_id` 또는 요청 호스트이며, **IP 주소 접속에서는 브라우저가 WebAuthn을 막는다**. 화면은 그때 패스키를 숨기고 이유를 보인다.

### 10.2 로그인 흐름

`/auth/login`이 비밀번호(또는 LDAP)를 통과하고 그 사용자에게 요소가 있거나 정책이 요구하면, 쿠키는 **대기 세션**(`SessionSpec.pending`, 5분)이고 본문은 `{ "second_factor": { "methods": ["totp","recovery","webauthn"], "webauthn": {요청 옵션}, "enroll": false } }`다. `POST /auth/login/second-factor {totp|recovery|webauthn}`이 맞으면 같은 세션의 `pending`을 지운다(`update_spec` CAS). `find_session`이 `pending`을 돌려주고, `Principal` 추출기는 대기 세션을 401 `second_factor_required`로 막는다. 새 추출기 `PendingPrincipal`만 받으며, 받는 곳은 second-factor·등록 경로뿐이다. 실패는 `attempts`(사용자·IP)로 센다. 정책이 요구하는데 요소가 없으면 `enroll: true` 대기 세션이 등록 경로만 쓴다.

### 10.3 정책·초기화·저장

- 정책 `SettingsSpec.two_factor { roles, realms }`: 전역 역할이 `roles`에 있거나 영역(`local` 포함)이 `realms`에 있으면 요구한다. OIDC 영역 이름은 400(IdP가 MFA를 맡는다). 사용자 정의 역할 grant로는 정하지 않는다(로그인 때 grant 전체를 볼 이유가 없고, 폴더 위임자가 남의 2FA 요구를 바꾸게 된다).
- 관리자 초기화 `DELETE /users/{key}/two-factor`(`cluster.admin`, 자기 자신은 안 됨): `TwoFactor`를 지우고 `session_epoch`을 올려 세션을 끊는다. 감사 `user 2fa-reset alice`.
- **비밀 저장**: TOTP 비밀은 해시할 수 없다(서버가 HMAC을 다시 셈). 암호화도 하지 않는다. 쓸 수 있는 키가 같은 Raft DB 안(CA 키)에 있으니 겉치레다. LDAP bind 비밀번호·CA 키와 같이 `account` 종류에 두고 API 응답·watch·`Debug`·감사에서 빼며, 실제 보호는 0700 데이터 디렉터리다. 복구 코드는 해시, 패스키는 공개 키만 둔다.

### 10.4 다른 인증과

- **API 토큰**: 2FA를 거치지 않는다(자동화). 토큰 만들기는 2FA를 거친 세션이나 다른 토큰으로만 할 수 있으므로 비밀번호만 훔쳐서는 토큰을 만들 수 없다. 정책이 켜질 때 남은 토큰은 그대로이고, 화면이 "2FA 이전 토큰 n개"를 보인다. 범위(§6.6)를 권한다.
- **OIDC**: 로컬 2FA를 묻지 않는다. **LDAP**: 디렉터리 바인드 뒤 로컬 요소를 묻는다(요소는 외부 사용자 `alice@corp`에 붙는다).
- **CLI**: `oplctl login`이 `second_factor`를 받으면 TOTP·복구 코드를 묻는다. 패스키는 웹 전용이다.

## 11. 버전이 섞인 클러스터

- 옛 노드는 grant를 모르고 전역 역할만 본다. 그래서 grant·폴더 지정·`Role::None`이 **하나라도 있으면** 옛 노드가 더 주거나(폴더로 좁힌 operator가 전부 봄) 덜 준다(`none` 사용자를 못 풂). 그래서 §2의 쓰기는 모두 `require_feature(FOLDERS)` 뒤에만 받는다. 새 종류·새 칸이 없는 동안 새 노드의 판단은 옛 노드와 같다(§6.3 동치).
- 상태 머신의 새 검사(§5·§9)는 `Folder`가 없으면 아무 일도 하지 않으므로, 업그레이드 중 같은 로그를 적용한 옛·새 노드의 결과가 같다. `Rejection`의 새 변형은 모든 노드가 새 버전인 뒤에만 나온다.
- 뒤에 옛 노드가 합류하는 것은 지금 합류 검사(`version_too_old`)가 막는다. 2FA도 같다. `two-factor` 전에는 대기 세션을 만들지 않으므로 옛 노드가 대기 세션을 정식 세션으로 받는 일이 없다.

## 12. 다른 기능과의 관계

- **HA·마이그레이션·비우기·균형 조정**: 시스템 쓰기라 주체가 없고, 노드만 바꾸므로 할당량에 걸리지 않는다(§9).
- **템플릿(계획 58)**: 템플릿은 VM 트리의 폴더에 산다. 배포는 원본 `vm.clone`, 대상 폴더 `vm.create`와 할당량이다.
- **백업·복원**: `BackupSpec.vm_spec.folder`가 지운 VM 백업의 주인이다. 복원은 원래 폴더가 있으면 그곳, 없으면 루트이고 `--folder`로 바꾼다.
- **원격 복제·가져오기**: 받는 클러스터에 없는 폴더 id는 버리고(루트) 받는 쪽 요청의 `folder`를 쓴다.
- **보안 그룹**: 루트 객체(`network.config`)로 둔다. 폴더에 넣는 것은 §16.
- **노드 목록**: 루트 `cluster.read`가 없는 폴더 사용자는 노드 목록을 못 본다. VM 만들기는 자동 배치이고 화면은 노드 칸을 숨긴다.
- **이름 존재 노출**: 이름이 종류 전체에서 하나라서 만들기의 `name_taken`이 안 보이는 객체 이름을 드러낸다(README 한계, 이름 앞에 팀 접두사를 권함).
- **규모(수백 노드)**: 리더 일은 늘지 않는다. 색인·거르기·사전 확인은 요청 받은 API 노드에서, 상태 머신 검사는 모든 노드의 apply에서 돈다.

## 13. API·CLI·웹

- **API**(태그 `folders`·`permissions`·`auth`):
  - 폴더 `GET|POST /folders`(`?kind=vm`, 트리 평면 목록), `GET|PATCH|DELETE /folders/{id}`, `POST /folders/{id}/members {items: [{kind, key}]}`(일괄, 항목마다 결과), `GET /folders/{id}/usage`.
  - 역할 `/access-roles[/{key}]`(내장은 읽기만), 그룹 `/groups[/{key}]`, grant `/grants[/{id}]`(`?target=folder/ID&principal=group/ID`).
  - 유효 권한: `GET /permissions/mine`(색인 그대로 `{root, folders, objects}` — 웹이 쓴다), `GET /permissions/effective?user=alice&object=vm/web`(권한과 근거 grant), `GET /permissions/who?object=vm/web`(대상 `permission.modify` 필요).
  - 2FA: `GET /auth/2fa`, `POST /auth/2fa/totp`(비밀·`otpauth://` URI) → `POST /auth/2fa/totp/confirm {code}`(복구 코드 10개, 한 번만), `DELETE /auth/2fa/totp {code|password}`, `POST /auth/2fa/recovery`, `POST /auth/2fa/webauthn/register` → `/finish`, `DELETE /auth/2fa/webauthn/{id}`, `POST /auth/login/second-factor`, `DELETE /users/{key}/two-factor`, `PATCH /settings {two_factor}`.
  - 오류 404·403 `forbidden`, 409 `quota_exceeded`·`folder_not_empty`·`folder_cycle`·`name_taken`·`cluster_upgrading`, 400 `folder_required`, 401 `second_factor_required`·`invalid_second_factor`. 감사 설명 `folder create /teamA/prod`, `folder move vm web -> /teamA`, `grant add group:team-a operator on /teamA`, `user 2fa-reset alice`.
- **CLI**: `folder tree|list|create|rename|move|delete [--kind vm|network|storage]`, `folder quota set /teamA --vcpus 32 --memory 128G --disk 2T --vms 20`, `folder usage /teamA`, `vm|network|pool move NAME… --folder /teamA`, `--folder` on `vm create|clone|import`·`backup restore`, `role list|get|create|set|delete`, `group …`, `grant list|add|remove`, `perm check --user alice vm/web`, `perm who vm/web`, `2fa status|totp enable|totp disable|recovery regen`, `user 2fa-reset NAME`, `token create --scope vm.power,vm.console@/teamA`.
- **웹**(계획 61을 따른다, 사이드바는 그대로 최상위 영역만):
  - **가상 머신 > VM** 탭에 보기 전환 `목록 | 폴더`(`/vms` | `/vm-folders`). 폴더 보기는 왼쪽 트리(가상화된 `role="tree"`, 화살표 키) + 오른쪽 계획 61 `DataTable`(그 폴더, "하위 포함" 칩)이다. 목록 보기에 폴더 열과 거름 칩. 템플릿 탭도 같은 트리를 쓴다.
  - **인프라 > 네트워크·스토리지 풀** 탭에 같은 보기 전환(`/network-folders`·`/storage-folders`).
  - 폴더 상세 `/vm-folders/:id`: `ResourceHeader`, 요약 카드(VM 수·사용량/상한 막대 넷), 탭 `내용 | 할당량 | 권한`. VM·네트워크·풀 상세에 **"권한" 탭**(유효 권한·근거·이 객체 grant, `permission.modify`면 더하기).
  - 옮기기: 행(여러 개 고르면 묶음)을 트리 노드로 끌어 놓기(HTML5 DnD, 새 의존성 없음, 놓기 전 대상 폴더 강조와 불가 이유). 키보드·휴대폰 대안은 `BulkBar`의 "폴더로 옮기기…" 대화상자. 결과는 계획 61 묶음 알림.
  - **설정**에 탭 `권한`(`/settings/permissions`, 모든 grant 표)·`역할`(`/settings/roles`, 권한 체크 표)·`그룹`(`/settings/groups`)·`보안`(`/settings/security`, 2FA 정책). 사용자 메뉴의 "2단계 인증"은 `/settings/account`(모두에게 보임, 토큰 탭처럼). 사용자 표에 2FA 열과 "초기화".
  - 로그인 화면에 둘째 단계(코드 칸·"패스키 사용"·"복구 코드"), 강제 등록 화면(QR: npm `qrcode-generator` MIT, 의존성 없음).
  - **숨기기**: `can(role, action, kind)`를 `usePermissions()`(`/permissions/mine`, `useQuery`, `folder`·`grant`·`access-role`·`group` watch에 무효화)의 `can(privilege, object?)`로 바꾼다. 43곳은 루트 확인 shim으로 먼저 옮긴 뒤 객체가 있는 곳을 객체 확인으로 바꾼다. 팔레트 동작·`BulkBar` "가능 n / 불가 m"·+ 만들기 메뉴도 같은 함수를 쓴다. i18n en·ko(`folders.*`, `permissions.*`, `twoFactor.*`).

## 14. 시험과 CI

- **순수(opl-types)**: serde 하위 호환(옛 `VmSpec`·`UserSpec`·`ApiTokenSpec`·`SessionSpec`·`SettingsSpec` JSON이 바이트 그대로, `folder` 없음 = 루트), 모르는 권한 이름 버리기, TOTP RFC 6238 부록 B의 SHA-1 벡터(59·1111111109·1111111111·1234567890·2000000000·20000000000, 8자리) + 6자리 자르기, base32 왕복.
- **동치**: grant 없는 색인 × 모든 (`Role`, `Action`, `Kind`)가 시험 안에 남긴 옛 `RoleAuthorizer::check`와 같다.
- **권한 행렬**(`insta`): 주체(루트 admin·operator·viewer, `none` + 그룹 grant(LDAP 외부 그룹), 폴더 A operator, 객체 하나 grant, propagate 거짓, 범위 토큰) × 객체(A의 VM, A/B의 VM, C의 VM, 루트 VM, 풀, 네트워크, 템플릿, 백업, Task) × 권한.
- **성질 시험**(`proptest`, 새 dev 의존성 MIT/Apache): 무작위 트리(폴더 ≤ 40)·grant·객체에서 색인 = 참조 `effective`, 단조(grant 더하기는 권한을 줄이지 않음), propagate 거짓은 자손에 닿지 않음, 객체 이동 뒤 = 새 조상 기준, 세대 무효화 뒤 = 처음부터 만든 색인.
- **상태 머신**(opl-cluster): 순환(A→B, B→A 동시)·빈 폴더 지우기와 넣기의 엇갈림·형제 이름 중복이 커밋 순서로 하나만 이김, `Folder` 없는 로그가 옛 구현과 같은 결과.
- **경로 시험**(opl-api, `MockHypervisor`): `authz_routes.rs`가 OpenAPI 모든 경로를 덮는지, 폴더 A 사용자로 **모든 목록 경로**가 A만 돌려주는지, B 객체 경로는 404·A의 권한 밖 동작은 403인지, 대기 세션 쿠키로 모든 경로가 401인지 본다. `/watch`는 거르기·이동 뒤 가짜 `Deleted`·세대 바뀜 닫기를, 지표·경보·Task·감사는 거르기와 루트 요구를 본다. 위임 상승 막기, 할당량 부모 권한도 본다.
- **할당량 경쟁**: opl-cluster `memnet` 노드 셋에서 두 팔로워가 같은 폴더에 동시에 만들기 100회 → 합이 상한을 넘지 않고 진 쪽이 `QuotaExceeded`. 늘리기만 막기(초과 폴더에서 노드 바꾸기·끄기는 통과), 볼륨 늘리기, 폴더 이동. opld E2E로 한 번 더(아래).
- **2FA**: 재사용 거절과 두 노드 동시 같은 코드(CAS), 복구 코드 한 번만, 정책 강제 등록, 관리자 초기화가 세션을 끊음. WebAuthn은 시험 안 **소프트웨어 인증기**(ring 키로 `attestationObject`·`authenticatorData`·서명을 만듦)로 ES256·EdDSA·RS256 등록·로그인, 틀린 origin·challenge·rpId·signCount 되감기 거절을 본다.
- **opld E2E**(새 `tests/permissions.rs`, QEMU 없음, 이 Mac·svc01): 노드 하나 — admin이 `/teamA`·`/teamB`와 꺼진 VM(디스크 없음)을 만들고, 그룹 `team-a`(LDAP 없이 로컬 멤버)에 `/teamA` operator를 준다. alice 토큰의 `vm list`·`watch`·`task list`에 teamA만 있고 teamB VM은 404. 할당량 `--vms 2`에서 셋째 409. alice가 TOTP를 켜고(시험이 코드 계산) 로그인에 둘째 단계가 필요하다. 노드 둘 — 두 노드 API로 동시에 만들기 → 상한을 지킴(`cluster.rs` 본).
- **웹**: vitest(`usePermissions`·트리 키보드·끌어 놓기·숨기기, msw), Playwright `web/e2e/permissions.spec.ts`: **admin이 아닌 사용자로 로그인하면 VM 목록·폴더 트리·팔레트에 자기 폴더만 보이고 + 만들기가 그 폴더로 채워진다**. 2FA 로그인 단계 하나.
- **svc01(aarch64)**: 일반 묶음 전체와 위 E2E, `scale.rs` VM 1만 개 + 할당량 apply 시간 기록. **GitHub**: 새 작업 없음. `deny`가 `ciborium`·`proptest`·`qrcode-generator` 라이선스를 본다(셋 다 허용 목록 안).

## 15. 견준 길

- **Proxmox 풀 + 경로 ACL vs vSphere 폴더 + 객체 권한(채택, 사용자 결정).** 리소스가 폴더 하나에만 속해 "어디에 있나"가 하나이고 할당량을 그 트리에 걸 수 있다.
- **한 트리 vs 종류별 트리(채택).** 영역 나눔(계획 61)·할당량과 맞는다. 대가는 팀마다 grant 둘.
- **더 구체적인 쪽이 이김 + 거부 vs 합집합(채택).** §6.1.
- **grant에 외부 그룹 이름 vs `Group` 연결(채택).** §6.4.
- **할당량: API 확인 / 예약 객체 CAS / Raft 안 확인(채택).** API만 확인하면 두 노드가 함께 넘는다. 예약은 실패 때 되돌리기와 표류 정리가 필요하고 폴더 객체 하나에 쓰기가 몰린다. 상태 머신 확인은 이미 있는 `check_exclusive` 틀이고 커밋 순서가 답을 정한다. 계획 58의 "쓰고 다시 보기"는 늘리기가 여럿(크기·핫플러그·이동)이라 물러설 쪽을 고르는 규칙이 복잡해서 쓰지 않는다.
- **할당량 중첩: 부모에서 떼기 vs 조상마다 하위 트리 합(채택).** §9.
- **TOTP 비밀: 암호화 vs Raft 평문 + API에서 가림(채택).** §10.3.
- **WebAuthn: `webauthn-rs` vs 최소 직접 구현(채택).** 라이선스·openssl. 대가는 증명(attestation) 검증·MDS가 없다(§16).
- **목록 거르기: SQL식 질의 vs 메모리 비트 집합(채택).** 상태가 이미 메모리에 가깝고 확인이 두 번 찾기다.

## 16. 나중 할 일

볼륨·백업·보안 그룹·노드 폴더, 태그(라벨) 기반 grant, 폴더별 감사 로그 읽기와 Prometheus, 실행 중만 세는 할당량·풀별 디스크 할당량·붙지 않은 볼륨 세기, `folder_usage` 파생 표, WebAuthn 증명·MDS·CLI 패스키, OIDC `amr`/`acr` 요구, 시간 제한 grant, 외부 그룹 주기적 다시 읽기(LDAP 동기화), 이름을 폴더 범위로 만들기.

## 17. 스테이지 나누기

각 스테이지는 커밋 하나이고, 혼자 컴파일되고 시험된다. 새 API는 그 스테이지에서 연다.

1. **feat(types): 폴더·grant·역할·그룹·2FA 타입, 권한 목록과 내장 역할, 기능 `folders`·`two-factor`(opl-types)** — `folder.rs`·`authz.rs`·`two_factor.rs`, 스펙의 `folder`·`Role::None`·`external_groups`·`scope`·`pending`·`two_factor`, `Kind` 다섯, 참조 `effective`, `totp`(RFC 벡터), serde 하위 호환.
2. **feat(cluster,api): 폴더 상태 머신 검사와 권한 색인(opl-cluster·opl-api `authz/`)** — `folders.rs`(부모·순환·빈 폴더·형제 이름), `Rejection` 변형, `AuthzIndex`·세대·LRU, `Authz::require|filter`, `RoleAuthorizer` 동치·행렬·`proptest`. 핸들러는 아직 루트 shim이라 동작이 같다.
3. **feat(api): 모든 경로·목록·watch·지표·경보·Task 시행, 폴더·역할·그룹·grant·유효 권한 API(opl-api)** — `authz_routes.rs`와 덮기 시험, 404/403, 위임, 외부 그룹 저장, 토큰 범위, OpenAPI·`schema.d.ts`.
4. **feat(cluster,api): 폴더 할당량** — `check_quota`, `QuotaExceeded` → 409, 일곱 경로 사전 확인, 사용량 API, memnet 경쟁 시험, `scale.rs` 기준.
5. **feat(api): 2단계 인증 — TOTP·복구 코드·대기 세션·정책·초기화, WebAuthn 최소 검증기** — `PendingPrincipal`, CAS 재사용 막기, `webauthn.rs`(`ciborium`)와 소프트웨어 인증기 시험, `deny.toml` 확인.
6. **feat(cli): folder·role·group·grant·perm·2fa 명령, `--folder`, 로그인 둘째 단계, opld E2E와 CI(oplctl·opld)** — `tests/permissions.rs`(한 노드·두 노드).
7. **feat(web): 폴더 보기·끌어 놓기·일괄 옮기기, 권한 탭, 설정의 권한·역할·그룹·보안, 2FA 등록·로그인, `usePermissions` 숨기기(web)** — 계획 61 `AREAS`·`ViewSwitch`·`DataTable`·`BulkBar` 위에, Playwright `permissions.spec.ts`.
8. **docs: README·가이드 "폴더와 권한"(위임 예, 거부 없는 설계로 빼기, 할당량, 2FA·패스키·IP 접속 한계, 이름 노출), 설계 문서 스테이지 보고, 계획 62 실행 기록**.

## 18. 스테이지 보고 (설계와 다른 것·결정)

**스테이지 1(opl-types)**

- `Action`(Read·Write·Operate)을 opl-api에서 `opl_types::authz::Action`으로 옮기고 opl-api는 `pub use`로 그대로 쓴다. 종류 단위 확인이 요구하는 권한 `kind_privilege(Action, Kind)`를 opl-types에 두어 루트 shim과 동치 시험이 같은 표를 쓰게 했다. 동치 시험(모든 `Role`×`Action`×`Kind::ALL`)은 opl-types(옛 규칙 사본)와 opl-api(진짜 `RoleAuthorizer`) 둘 다에 넣었다.
- 새 종류의 루트 확인 규칙: `TwoFactor`·`Grant`·`Group`은 계정 종류(`is_account_kind`, 읽기도 admin — 비밀이나 누가 무엇을 받았는지를 담는다), `Folder`·`AccessRole`은 누구나 읽고 쓰기는 admin(`inventory.folder`·`cluster.admin`)이다. `RoleAuthorizer`에 그 셋과 `Role::None => false`를 더했다(새 종류가 아직 없으므로 동작은 같다). 폴더 사용자가 자기 grant를 보는 길은 `/permissions/mine`(스테이지 3)이다.
- 직렬화 모양: `PrincipalRef`·`RoleRef`·`GrantTarget`은 `{"type": …}` 태그(`{"type":"object","kind":"vm","id":…}` — `kind`가 객체 종류라 태그 이름을 `type`으로 했다), `RoleRef`는 `{"type":"builtin","role":"operator"}`·`{"type":"custom","id":…}`. `GrantSpec.propagate`는 기본 참이고 참이면 쓰지 않는다. `PrivSet`은 JSON에서 이름 목록이고 모르는 이름은 버린다. `Passkey.cose_key`는 base64url 문자열이다.
- 설계에 없던 작은 것: `GroupSpec.description`, `AccessRoleSpec.description`(256자), `FolderSpec::validate`(이름 1~64자, `/`·제어 문자·앞뒤 공백 없음, 할당량은 VM 폴더만), `sibling_key`(`"{부모 id|root-<종류>}/{소문자 이름}"`), `parse_folder_path`, `Role::ALL`·`as_str`, `Kind::ALL`, 사용자 정의 역할 이름은 `none`도 예약, grant 객체 대상은 VM·네트워크·풀만, `TokenScope`는 권한·대상이 하나 이상, `TwoFactorPolicy.validate`(영역 이름 또는 `local`, RP ID는 IP가 아닌 소문자 도메인)·`requires`, `verify_totp`(±1칸, `last_step` 초과만), `realm::same_group`(역할 매핑과 그룹 연결의 같은 비교).
- `authz_eval`의 참조 `effective(&AuthzSnapshot, user, &AuthzTarget)`와 토큰용 `effective_scoped`: 대상은 `Root`·`Folder(id)`·`Object{kind,id,folder}`, 없는 폴더를 가리키는 객체는 루트에 있는 것으로, 조상 걷기는 순환·깊이 16에서 멈춘다. 루트 grant가 propagate 거짓이면 루트 대상(클러스터 객체)에만 닿는다.
- 스펙의 새 칸(`folder` 등)은 아직 API가 받을 때 막지 않는다. 기능 게이트·`PUT`의 `folder` 바꾸기 거절은 스테이지 3이 한다.

**스테이지 2(opl-cluster `folders.rs`·opl-api `authz/`)**
- 상태 머신 검사는 `check_exclusive` 뒤에 `folders::check_write`(만들기·스펙 바꾸기)와 `folders::check_delete`(지우기)로 붙였다. 거부는 새 변형 `FolderCycle { id, parent }`·`FolderTooDeep { id, depth }`·`FolderNotEmpty { id }`(API 409 `folder_cycle`·`folder_too_deep`·`folder_not_empty`)와 기존 `NotFound { kind: folder }`(부모·리소스 폴더가 없거나 다른 트리)다. 깊이 초과는 설계에 거부 이름이 없어 `FolderTooDeep`를 더했다. 옮길 때는 하위 트리 높이까지 센다.
- "폴더가 없으면 아무 일도 하지 않는다"는 리소스 쓰기(VM·네트워크·풀의 `folder`)에만 걸었다: 가리킨 폴더가 없고 폴더가 하나도 없으면 받는다. 그래서 마지막 폴더를 지운 직후 그 id를 가리키는 쓰기는 받아지고(평가는 루트로 읽는다), 폴더 종류 쓰기는 옛 노드가 풀지도 못하므로(기능 게이트) 늘 검사한다. 리소스 스펙 바꾸기는 `folder`가 바뀔 때만 본다(폴더가 없던 때 들어간 값이 다른 칸 바꾸기를 막지 않게).
- 설계에 없던 것: 자식 폴더나 리소스가 있는 폴더의 `kind` 바꾸기는 `FolderNotEmpty`(PATCH는 종류를 받지 않지만 상태 머신이 트리 규칙을 지킨다). 형제 이름 키는 `exclusive_values`가 `FolderSpec::sibling_key`를 써서 `("name", …)`로 내고, API는 기존대로 `name_taken`으로 바꾼다.
- 색인의 세대는 백그라운드 작업 대신 확인 때마다 이 노드의 변경 구독을 기다리지 않고(`now_or_never`) 비워 올린다. 이 노드에 적용된 커밋은 다음 확인부터 반영되고(시험이 결정적), 구독이 밀리거나 끊기면 세대를 올리고 다시 구독한다. 상태 스냅샷(`AuthzSnapshot` + 토큰)은 세대마다 한 번 뜨고 주체 색인은 그 위에 만든다.
- 색인은 설계의 "위에서 아래로 한 번 훑기" 대신 폴더마다 `authz_eval::ancestors`로 조상을 걸어(폴더 수 × 깊이 ≤ 16) 참조 구현과 똑같이 끝나게 했다(없는 부모·순환 데이터도 같은 답). 객체 확인은 `contents[folder] | objects[(kind,id)]` 두 번 찾기, 폴더 grant가 없는 주체는 맵이 비어 루트 값 하나다. 디버그 빌드에서 VM 1만 개·grant 500개·폴더 200개의 색인 만들기 약 1.5 ms, 거르기 약 6 ms(시험 상한 100 ms).
- `Principal`은 아직 바꾸지 않았다. 평가기는 `Subject { user, token }`을 받고 `From<&Principal>`은 `token: None`이다. 스테이지 3이 `Principal.token`을 더하고 `From`을 고친다. `AuthzObject`는 `Vm`·`Network`·`StoragePool`·`Folder`·`AuthzTarget`에만 구현했다. 파생 객체(볼륨·스냅샷·백업·Task·가져오기)는 주인을 찾아야 하므로 스테이지 3이 한다. `Authorizer`의 `debug_assert!`(폴더 종류에 루트 확인 금지)도 시행과 함께 스테이지 3이다.
- `proptest` 1.12(`default-features = false, features = ["std"]`, fork·timeout 없음)를 opl-api dev 의존성으로 더했다. 새로 들어온 상자(chacha20·core_detect·rand 0.10·rand_core 0.10·rand_xorshift·unarray)는 모두 MIT/Apache-2.0이라 `deny.toml`은 그대로다.

**스테이지 3(opl-api 시행·폴더·권한 API)**
- 경로표 `authz_routes.rs`는 문서만이 아니라 **시행**이다: `route_layer` 미들웨어가 (메서드, 경로 템플릿)으로 규칙(`Public`·`Handler`·`List`·`Root(권한)`·`Read(객체)`·`Object(객체, 권한)`)을 찾아 경로의 첫 인자로 객체를 찾고(`authz_routes::resolve`) 핸들러보다 먼저 404/403을 낸다. 표에 없는 경로는 403(기본 거부)이고, 시험이 OpenAPI의 모든 (메서드, 경로)가 표에 있고 표에 낡은 경로가 없음을 본다. 미들웨어가 인증한 `Principal`을 요청 확장에 넣어 핸들러가 다시 인증하지 않는다. 감사 미들웨어가 바깥이라 권한 거절도 감사에 남는다. 본문에 대상이 있는 경로(만들기·복원·복제·가져오기 실행·grant)는 `Handler`/`Object`로 적고 핸들러가 폴더·풀·네트워크를 더 본다.
- `Authorizer` 플러그인은 루트 확인만 정한다(`builtin()`이 참인 `RoleAuthorizer`면 루트 확인도 색인이 한다). 폴더를 따르는 객체는 플러그인과 관계없이 색인이다. 그래서 `DenyAll` 시험 둘(지표)은 루트 경로만 본다.
- 파생 객체의 주인(`authz::Owners`): 볼륨은 **디스크로 붙은 VM**, 아니면 풀(CD의 ISO는 주인이 아니다), 스냅샷은 VM, 백업은 VM(지웠으면 `vm_spec.folder`), Task는 op 대상(복원·가져오기 Task는 새 VM이 생기기 전에는 백업·가져오기), 노드·Ceph·HA 기록 Task는 루트 `cluster.read`(`Owned::read`). 가져오기(`VmImport`)는 폴더가 없어 루트 객체다(올리기·ESXi는 루트 `vm.create`). 실행 때 `ImportRun.folder`로 만들 VM의 폴더를 정한다.
- 경로별 권한에서 설계 표에 없던 것: VM 복제 실행(`/replicate`)은 `vm.migrate`, 게스트 비밀번호는 `vm.config`, 스냅샷 만들기·되돌리기·지우기는 `vm.snapshot`, 백업 원격 사본은 `backup.create`, 가져오기 지우기는 `vm.create`, 볼륨 늘리기는 볼륨 주인과 풀 둘 다 `storage.allocate`, 이미지 카탈로그 보기는 루트 `system.read`(받기는 풀 `storage.allocate`, 템플릿이면 루트 `vm.create`), 추천 적용은 그 VM의 `vm.migrate`. 모두 내장 역할의 답은 그대로다(기존 시험 통과).
- 폴더 칸을 받는 만들기: `POST /vms`·`/networks`·`/storage-pools`의 `spec.folder`, 복원 `RestoreRequest.folder`(없으면 백업한 VM의 폴더가 아직 있으면 그곳 — API가 정해 `TaskOp::BackupRestore.folder`에 담는다), 복제 `CloneRequest.folder`(`CloneOverrides.folder`), 가져오기 `ImportRun.folder`. 모두 `folders` 기능 뒤다. 폴더를 주지 않았는데 루트에는 권한이 없고 다른 폴더에는 있으면 400 `folder_required`, 어디에도 없으면 403(옛 viewer 시험의 403 그대로).
- `/watch`: 루트 종류는 구독 때 루트 확인, 폴더를 따르는 종류와 grant는 이벤트마다 거른다. 연결마다 주인 표를 두고 이벤트 순서대로 고쳐 이벤트 앞뒤의 보임을 견준다(보이던 것이 안 보이면 이름·id만 담은 `deleted`, 스펙은 null). 세대가 바뀌면 그 주체의 색인을 다시 만들어 **권한이 같으면 계속**(남의 토큰·grant), 다르면 `permissions`로 닫는다 — 세대마다 닫으면 누가 로그인해(토큰) 모든 구독이 끊긴다. 파생 종류(볼륨·스냅샷·백업·Task)를 구독하는데 VM·풀이 옮겨져 보임이 바뀌면 그것들의 이벤트가 없으므로 `permissions`로 닫는다. 목록과 구독 사이(밀리초)에 옮겨진 객체는 놓칠 수 있다(웹은 다시 연결할 때 목록을 다시 읽는다). `folder`·`access-role`·`grant`·`group`도 구독할 수 있다.
- 경보·알림 기록은 대상의 `alert.read`로 거른다(노드·백업 작업·시험 보내기는 루트). 알림 기록에는 대상 종류·id가 없어 규칙의 조건 종류와 대상 이름으로 찾는다. 클러스터 지표 요약은 루트 `cluster.read`이고 상위 VM·풀은 거른다(캐시는 주체와 관계없이 하나). 감사 로그는 루트 `cluster.admin`. cloud-init user-data 가리기는 그 VM의 `vm.config`로 정한다(목록·watch·백업·스냅샷·Task).
- 위임: 역할은 로그인한 누구나 읽고(위임자가 고른다), 그룹 읽기·쓰기는 `cluster.admin`, 쓰이는 역할·그룹 지우기는 409 `role_in_use`·`group_in_use`(설계에 없던 오류). grant 보기는 `cluster.admin`, 자기(그룹) grant, 대상의 `permission.modify`. 대상이 지워진 grant는 루트 관리자만 보고 뺀다. 폴더 PATCH는 경로표가 읽기만 보고 핸들러가 칸마다(이름 `inventory.folder`, 부모 양쪽 `inventory.move`, 할당량 부모 `inventory.quota`) 본다. 옮기기 `POST /folders/root/members`는 트리의 루트로 옮긴다.
- 토큰 범위는 `POST /tokens {scope}`(`folders` 기능 뒤)로 만든다. 범위 토큰으로는 토큰을 만들지 못한다(범위 없는 토큰으로 넓히기 막기). 전역 역할 `none`인 사용자 만들기·바꾸기도 `folders` 뒤다. LDAP·OIDC 로그인은 디렉터리 그룹을 다듬어(앞뒤 공백·중복 제거·정렬) 바뀌었을 때만 `external_groups`에 쓰고, 모든 노드가 `folders`를 알 때만 쓴다.
- `/permissions/mine`은 `{all, root, root_contents, folders, contents, objects}`다(객체의 유효 권한 = 폴더의 `contents` 또는 `root_contents` ∪ `objects`의 그 객체). `/permissions/effective`는 자기 것은 토큰 범위까지, 남의 것은 참조 구현(`authz_eval::effective`)으로 셈하고 근거 grant를 준다.

**스테이지 4(폴더 할당량 — opl-cluster `check_quota`·opl-api 사전 확인·사용량 API)**
- 계산 규칙은 `opl_types::quota` 하나다(VM 몫 `FolderUsage::of_vm`·JSON 판 `of_vm_value`, 조상 걷기, 하위 트리 합, `quota_exceeded`). 상태 머신은 스펙을 JSON으로 보고 모르는 칸을 무시해 버전이 다른 노드도 같은 답을 낸다. 판정은 쓰기 전·후의 트리와 폴더별 직접 사용량을 견줘 **할당량 있는 어느 폴더든 (후 > 상한 그리고 후 > 전)**이면 거절한다. 그래서 VM 만들기·바꾸기·옮기기, 볼륨 늘리기, 폴더 옮기기가 한 규칙이고, 여럿이 넘으면 폴더 id 차례의 첫 것을 낸다(모든 노드가 같다).
- 설계의 이름 `check_quota`는 `folders::quota_check`(준비) + `QuotaCheck::add`(VM 하나 세기) + `finish`(판정)다. VM 쓰기는 `check_exclusive`의 VM 훑기에 붙여 한 번만 풀고, 볼륨·폴더 쓰기는 따로 VM을 훑는다. 훑지 않는 빠른 길: 할당량이 하나도 없음, 새 위치의 조상에 할당량이 없음, VM 바꾸기가 어느 차원도 늘리지 않고 할당량 조상이 그대로(HA·마이그레이션·비우기·끄기·이름), 볼륨이 커지지 않음, 폴더 부모가 그대로(할당량 바꾸기·줄이기는 막지 않는다). 할당량 조상이 없는 폴더의 VM은 훑기에서 볼륨 크기를 읽지 않는다.
- `Rejection::QuotaExceeded { folder, dimension, limit, used }`(`dimension`은 `vcpus`·`memory_mib`·`disk_bytes`·`vms`) → 409 `quota_exceeded`, `details`에 네 칸. 사전 확인은 메시지에 경로를 쓰고(`folder /teamA would use 34 of 32 vCPUs`), Raft 거부는 폴더 id를 쓴다(오류 변환에 클러스터가 없다).
- 사전 확인 자리: `POST /vms`, `write_spec`(PUT·핫플러그·CD·템플릿 바꾸기가 모두 지나는 한 곳), 복제(새 VM은 템플릿 아님, 디스크는 원본 크기와 배포가 준 크기 중 큰 것), 복원(백업 디스크의 가상 크기), 가져오기 실행(원본 디스크 용량), 볼륨 늘리기(API와 Task가 저장소를 늘리기 직전에 한 번 더), `POST /folders/{id}/members`의 VM 항목, `PATCH /folders/{id}`의 부모 바꾸기. 템플릿 배포에 개수 칸이 없어 n배는 없다.
- 남는 틈: 볼륨 늘리기 Task가 저장소를 늘린 뒤 크기 기록이 다른 노드와의 경쟁(또는 LVM·ZFS의 올림)으로 할당량에 걸리면 디스크만 커진 채 기록은 옛 크기로 남고 Task가 실패한다. Task가 저장소 전에 다시 확인해 틈을 좁혔다. 스냅샷 되돌리기 뒤 크기 기록도 같은 거절을 경고로 넘긴다.
- `GET /folders/{id}/usage` → `{folder, path, usage, limits[{folder, path, quota, usage}], binding{차원: 폴더}}`. 10초 캐시는 두지 않았다(로컬 목록 셈이 싸고, 쓰기 바로 뒤 화면이 옛 값을 보이지 않게).
- 규모 기준은 `scale.rs` 대신 `opl-cluster` `folders.rs`의 `quota_apply_cost_with_10k_vms`(`--features scale-it`, `apply_command`가 crate 안 함수라서). VM 1만 개(디스크·NIC 하나씩, 폴더 20개 중 10개에 할당량)에서 VM 쓰기 하나의 apply: 릴리스 빌드(이 Mac) 할당량 없는 폴더에 만들기 17.8 ms(기존 겹침 훑기), 할당량 폴더에 만들기 27.5 ms, 할당량 폴더의 노드 바꾸기 17.1 ms. 디버그 145·221·146 ms. 더하는 값이 기존 훑기의 반 남짓이라 파생 표(`folder_usage`)는 만들지 않았다(§16 그대로).
- 경쟁 시험은 `three_nodes.rs`의 `concurrent_creates_on_two_nodes_keep_the_folder_quota`(memnet 세 노드, 리더 전달, 두 팔로워가 50개씩 동시에 → 상한 20개만, 나머지 80개 `QuotaExceeded`, 모든 노드의 목록 20, 줄인 상한 아래서 노드 바꾸기·끄기 통과).

**스테이지 5(2단계 인증 — opl-api `two_factor.rs`·`webauthn.rs`)**
- 경로 이름: 패스키 등록 끝내기는 `POST /auth/2fa/webauthn/register/finish`다(설계의 "`/register` → `/finish`"). 등록 챌린지는 Raft에 두지 않고 클러스터 CA 키에서 HKDF로 뽑은 HMAC 키로 서명한 토큰(사용자·챌린지·RP ID·5분)을 클라이언트에 맡긴다(OIDC 상태 쿠키와 같은 틀, info만 다르다). 그래서 등록을 다른 노드에서 끝내도 된다. 로그인 챌린지는 설계대로 `PendingLogin.challenge`이고 대기 세션과 함께 한 번만 쓴다.
- 경로표: 대기 세션이 쓰는 경로(`/auth/login/second-factor`, `GET /auth/2fa`, `POST /auth/2fa/totp`·`/totp/confirm`·`/webauthn/register`·`/register/finish`)는 `Public`으로 적고 핸들러의 추출기가 인증한다. 둘째 단계는 `PendingPrincipal`(쿠키 대기 세션만, 토큰은 401, 이미 끝난 세션은 409 `already_signed_in`), 상태·등록은 `Enroller`(정식 세션·API 토큰, 또는 `enroll` 대기 세션 — 등록을 마치면 같은 쿠키로 로그인이 끝난다). 요소 빼기·복구 코드 다시 만들기는 `Handler`(정식 세션·토큰)다. 모든 다른 경로는 대기 세션이면 401 `second_factor_required`(시험이 OpenAPI의 모든 경로를 돈다).
- 본인 확인을 설계(`DELETE /auth/2fa/totp {code|password}`)보다 넓혔다: 패스키 지우기와 복구 코드 다시 만들기도 `{code|password}`(TOTP·복구 코드, 또는 로컬 사용자의 비밀번호)가 필요하다. 훔친 세션으로 오래 가는 둘째 단계를 손에 넣지 못하게. 대가로 패스키만 있는 LDAP 사용자는 복구 코드로 확인한다(다 쓰면 관리자 초기화). 요소 더하기(TOTP 켜기·패스키 더하기)는 확인 없이 받는다(정식 세션은 이미 둘째 단계를 거쳤다). 이미 켠 TOTP 위에 새로 등록하면 409 `totp_enabled`.
- 복구 코드는 첫 요소(TOTP 확인 또는 첫 패스키)를 더할 때 비어 있으면 만들어 그 응답에서 한 번 준다. 마지막 요소를 빼면 복구 코드도 지우고(쓸모가 없다) 빈 `TwoFactor`는 지운다. 정책이 요구하는 사용자의 마지막 요소 빼기는 409 `two_factor_required`(설계에 없던 오류). 형식은 base32 16자(80비트) `xxxx-xxxx-xxxx-xxxx`, 비교는 공백·`-`·대소문자 무시, 모든 해시와 같은 시간 비교.
- TOTP 확인 코드의 칸도 `last_totp_step`에 적어 그 코드로 로그인하지 못하게 했다. TOTP 쓰기는 스펙·상태 revision CAS를 엇갈림마다 다시 읽어 다섯 번까지 한다(다른 노드가 같은 칸을 먼저 썼으면 다시 읽은 `last_totp_step`이 거절한다). 노드 둘 동시 같은 코드 시험은 `two_nodes.rs`의 `the_same_totp_code_on_two_nodes_passes_once`(실제 gRPC 두 노드).
- 시도 제한은 로그인의 `AttemptLimiter`를 키 `2fa:<사용자 id>`(+IP)로 쓴다: 둘째 단계, TOTP 확인, 본인 확인 모두. 다섯 번 틀리면 30초부터 두 배로 막힌다(429). 틀린 둘째 단계는 401 `invalid_second_factor`, 정식 세션의 확인 실패는 403 `invalid_second_factor`(웹이 로그아웃으로 읽지 않게).
- 메모리 세션(옛 노드가 남았거나 정족수가 없을 때)도 `pending`을 담는다(`Sessions::create_pending`·`complete`). Raft 대기 세션은 `expires_at`을 대기 끝(5분)으로 두어 버려진 것을 정리 작업이 곧 지우고, 둘째 단계를 마치면 `pending`을 지우며 `expires_at`을 지금 + 12시간으로 늘린다(revision CAS).
- API 토큰은 2FA를 거치지 않고 `/auth/2fa`를 쓸 수 있다(CLI `2fa status|totp enable|…`). 범위 토큰은 2FA를 바꾸지 못한다(403, 범위 토큰이 토큰을 만들지 못하는 것과 같다). OIDC 사용자는 2FA 쓰기가 400이고 정책도 그들에게 요구하지 않는다(`roles`에 맞아도).
- 정책 PATCH: `two_factor`는 정책 전체이고 `{}`는 지운다. `local`이 아닌 영역은 있는 LDAP 영역이어야 한다(없으면·OIDC면 400). 감사 `settings two-factor roles=[…] realms=[…]`. 그밖의 감사: `2fa totp on|off`, `2fa recovery regenerate`, `2fa passkey add|remove NAME`, `second factor totp|recovery|webauthn`, `user 2fa-reset NAME`(비밀·코드는 없다).
- 설계에 없던 작은 것: `UserView.two_factor`(요소가 있으면 참, 거짓이면 쓰지 않음 — 사용자 표의 2FA 열), 로그인 응답 `LoginResponse`(= `UserView` + `second_factor {methods, webauthn?, enroll, expires_at}`), `GET /auth/2fa`의 `rp_id`(IP 주소 접속이면 없음 — 화면이 패스키를 숨긴다)·`available`(모든 노드가 `two-factor`를 안다)·`enrolling`, 사용자 지우기가 그 `TwoFactor`도 지운다, 패스키 이름 1~64자·사용자당 16개.
- WebAuthn 검증기(`opl_api::webauthn`, 공개 모듈 — 통합 시험이 쓴다): 등록은 `fmt`과 무관하게 증명문을 보지 않는다(`attestation: "none"`으로 요청). 확인: clientData `type`·challenge(같은 시간 비교)·origin(허용 목록)·`crossOrigin` 거짓, RP ID 해시, UP, 요구하면 UV(지금 경로는 `preferred`라 요구하지 않는다), COSE 키 모양(EC2 P-256·OKP Ed25519·RSA 2048~8192비트), authData 끝에 남는 바이트 없음(ED면 확장 맵 하나), 서명 횟수는 어느 쪽이든 0이 아니면 늘어야 한다(되감기는 경고 로그). origin 허용 목록은 요청 호스트(scheme을 모르면 http·https 둘 다)와 `web_origins` 가운데 호스트가 RP ID이거나 그 하위 도메인인 것이다. 시험 안 소프트웨어 인증기는 `tests/common/authenticator.rs`(ring 키, RS256은 저장소의 OIDC 시험 키).
- `ciborium`은 `=0.2.2`로 못 박았다(Apache-2.0, 딸려 온 `half` MIT/Apache-2.0·`crunchy` MIT). `cargo deny check`: advisories·bans·licenses·sources 모두 ok.

**스테이지 6(oplctl 명령·로그인 둘째 단계·opld E2E)**
- 명령은 새 모듈 둘에 두었다: `oplctl/src/access.rs`(`folder`·`role`·`group`·`grant`·`perm`, `--folder` 거르기, 토큰 범위)와 `two_factor.rs`(`2fa`, 로그인 둘째 단계). API가 id만 받으므로 CLI가 경로·이름을 id로 바꾼다. 폴더 인자는 `[KIND:]PATH`(`/teamA/prod`, `network:/lan`, `/`는 트리의 맨 위) 또는 폴더 id이고, 부모를 볼 수 없는 폴더는 보는 사람에게 맨 위 폴더다(서버 목록과 같은 규칙). `folder` 명령은 `--kind`(기본 `vm`)로 트리를 정하고, grant·`perm`·토큰 범위의 대상에서 접두사 없는 경로는 VM 트리다(팀마다 VM·네트워크 폴더 이름이 같아도 모호하지 않게).
- 설계의 `vm|network|pool move NAME… --folder`는 `folder members move DEST vm/web network/lan …`(항목마다 결과, 하나라도 실패하면 명령이 실패)로 했다. `--folder`는 `vm`·`network`·`pool`의 `create`와 `list`(`--recursive`로 하위 포함)에 넣었다. `vm clone`·`import run`·`backup restore`의 `--folder`는 넣지 않았다(API는 받는다, 웹·다음 계획).
- `perm check`는 `perm effective`의 별칭이고 `perm mine`(`/permissions/mine`을 표로)을 더했다. `perm who`의 사용자 표는 서버대로 전역 역할 사용자다(그룹 grant로 닿는 사용자는 grant 표의 주체로 보인다). `role privileges`는 권한 이름 목록을 보인다.
- 할당량: `folder quota set FOLDER --vcpus --memory --disk --vms`는 지금 할당량에 준 차원만 덮고 `none`은 그 차원을 지운다(모두 지우면 할당량을 지운다). `--memory`는 단위가 없으면 MiB, `--disk`는 바이트. `folder quota clear`, `folder usage`(묶는 폴더를 `bound by /teamA`로).
- 토큰 범위 `token create --scope PRIVILEGES@TARGET`(여러 번): 범위는 권한 목록 하나와 대상 여럿이라, `--scope`마다 권한이 다르면 거절한다(합쳐서 넓히지 않는다). `token list`에 `SCOPE`, `user list`에 `2FA` 열, `user create --role none`, 영역 그룹 매핑도 `none`을 받는다.
- 로그인: 응답에 `second_factor`가 있으면 `--code`(숫자 6~8자리는 TOTP, 그 밖은 복구 코드) 또는 터미널에서 물어(틀리면 세 번까지) 같은 쿠키로 `/auth/login/second-factor`를 부른 뒤 토큰을 만든다. 터미널이 아니면 묻지 않고 실패한다(스크립트가 멈추지 않게). 패스키만 있는 사용자는 웹에서 토큰을 만들라고 안내한다. 강제 등록(`enroll`)은 터미널에서 TOTP를 등록해 끝낸다(비밀·URI를 보이고 코드를 물은 뒤 복구 코드를 한 번 보임). `--code`로는 등록할 수 없다(비밀을 아직 모른다).
- `2fa totp enable`은 비밀·URI를 보이고 코드를 물어 확인한다. `--no-confirm`은 비밀만 보이고 `2fa totp confirm CODE`로 끝낸다(스크립트·E2E). `2fa totp disable`·`2fa recovery regen`은 `--code` 또는 `--password-file`(없으면 코드를 묻는다). 정책은 `settings set two_factor.roles=admin two_factor.realms=local,corp two_factor.rp_id=…`(정책을 통째로 보내므로 CLI가 지금 정책에 준 칸만 덮는다)와 `two_factor=off`.
- 폴더 권한만 있는 사용자는 노드 목록을 읽지 못해 `vm create`가 기계 종류를 고르지 못한다: 그때 `--machine`을 달라는 오류를 낸다(설계 §12 "화면은 노드 칸을 숨긴다"의 CLI 쪽).
- opld E2E `tests/permissions.rs`(QEMU 없음, 기능 없이 돌아 CI `check`의 `cargo test --workspace`에 든다, 새 작업 없음): 노드 하나 — 폴더 둘·VM 셋, 그룹 grant로 alice(`none`)가 teamA만 보고(목록·`get` 404·`folder list`·`perm`), 폴더 없이 만들기 400 `folder_required`, 자기 할당량 바꾸기 403, `--vms 2`에서 셋째 409 `quota_exceeded`와 옮겨 넣기 거절, 범위 토큰의 지우기·토큰 만들기 403, TOTP 켜기(시험이 `opl_types::two_factor::totp`로 코드 셈)·둘째 단계 로그인(틀린 코드·다음 칸 코드·복구 코드 한 번)·관리자 초기화·정책 설정과 강제 등록 안내. `/watch` 거르기는 opl-api 시험(스테이지 3)이 본다. 노드 둘 — 두 노드 API로 넷씩 동시에 만들어 `--vms 3`을 지키고 나머지는 모두 `quota_exceeded`.
- 시험 하네스의 oplctl future가 디버그 빌드에서 시험 스레드 스택을 넘어(명령이 늘어 `run`이 커졌다) E2E의 `ctl_as`는 `Box::pin`으로 힙에 둔다.

**스테이지 7(웹)**
- 서버 작은 바꿈(따로 커밋 `feat(api): 머신 종류를 빼면…`): `POST /vms`의 `spec.machine`을 빼면 지정 노드, 아니면 위트니스가 아닌 노드들의 아키텍처(모두 같을 때) 머신을 채운다. 아는 노드가 없으면 이 노드의 아키텍처, 섞였으면 400. 요청 스키마(OpenAPI)는 그대로 `VmSpec`이고 설명에만 적었다(옛 클라이언트·schema.d.ts의 필수 칸 유지). 폴더 사용자의 웹 만들기 화면은 노드 목록을 읽지 못하면 노드 칸을 숨기고 머신을 "자동"으로 보내지 않는다.
- `usePermissions()`는 `/permissions/mine`(받기 전·실패 때는 전역 역할의 내장 권한)의 `Permissions`다: `has(priv, target)`·`anywhere(priv)`·`reads(kind)`·`can(action, kind)`(루트 shim). 표에 없는 폴더(읽은 뒤 생긴 폴더)는 `root_contents`로 본다(루트 grant는 모든 폴더의 `contents`에 들어 있으므로 아래 경계다). 43곳은 루트 shim으로 옮기고, 객체가 있는 곳(VM 상세 머리·케밥·전원 단추·미리 보기·콘솔·스냅샷·백업·펌웨어·게스트·CD·일괄 막대·팔레트 동작·다시 하기, 풀·네트워크 행)은 그 객체의 폴더·객체 grant로 본다. "+ 만들기"·만들기 단추는 어디서든(루트·폴더) 권한이 있으면 보인다(`anywhere`), 만들기 화면이 권한 있는 폴더를 골라 둔다. 볼륨·ISO·이미지·가져오기·백업 복원·템플릿 배포는 주인(VM·풀)을 따라 화면에서 정확히 셈하기 어려워 `anywhere`로 두고 서버가 막는다.
- 무효화: 설계는 `folder`·`grant`·`access-role`·`group` watch에 무효화였지만, 권한이 바뀌면 서버가 watch를 `permissions`로 닫으므로 다시 연결할 때마다 `/permissions/mine`을 먼저 다시 읽는다. watch에는 `folder`만 더했고(폴더 트리 목록), 폴더 이벤트에도 권한 표를 다시 읽는다. grant·역할·그룹은 목록을 두지 않고 그 화면이 열 때 읽는다.
- 실시간 연결은 루트에서 읽어야 하는 종류(노드·보안 그룹·백업 작업·HA 그룹·배치 규칙·설정)를 루트 권한이 있을 때만 읽고 구독한다. 폴더 사용자는 그 종류가 403이라 예전에는 연결이 끝없이 다시 시도됐다. `useList`도 읽을 수 없는 종류는 묻지 않는다. 영역 탭에 `kind`(노드·Ceph·보안 그룹·백업 작업·HA 둘·배치·설정 일반·구독)를 달아 폴더 사용자에게 숨긴다(내장 역할에는 그대로 보인다).
- 폴더 보기: VM·스토리지 풀·네트워크 탭의 보기 전환 `목록 | 폴더`(`/vm-folders`·`/storage-folders`·`/network-folders`, 상세 `…/:id`). 트리는 펼친 노드만 그린다(가상 스크롤은 하지 않았다 — 폴더 수백 개까지는 충분, 계획 63 이후). 템플릿 탭의 트리는 넣지 않았다(템플릿은 VM 폴더 보기와 목록의 폴더 거름에 함께 보인다). 끌어 놓기는 놓으면 바로 옮기고 묶음 알림, "폴더로 옮기기…"는 확인 창(대상·가능/불가와 까닭)이다. VM 목록에는 폴더 열과 폴더 거름(하위 포함)이 있다.
- 권한 탭: VM 상세의 탭, 폴더 상세의 탭. 네트워크·풀은 상세 화면이 없어 행의 방패 단추가 오른쪽 서랍으로 같은 패널을 연다. 내 유효 권한(`/permissions/effective`)은 누구나 보고, `permission.modify`가 있으면 `/permissions/who`와 grant 더하기·빼기다. 사용자 목록을 읽지 못하는 위임자는 그룹·사용자 id를 넣는다(이미 접근하는 주체를 고르기 목록으로 준다). 설정 › 권한의 더하기는 대상(루트·모든 폴더)을 고른다.
- 설정 탭: 권한·역할·보안은 admin, 그룹은 그룹 읽기(= cluster.admin), 2단계 인증(`/settings/account`)은 모두. 사용자 표에 2FA 열과 초기화(자기 자신은 없음), 역할 고르기에 `none`. `group_in_use`는 HA 그룹과 같은 코드라 그룹 화면이 문구를 고른다.
- 로그인: 응답의 `second_factor`로 둘째 단계(코드·복구 코드·패스키), `enroll`이면 강제 등록(인증 앱 QR, 도메인 접속이면 패스키, 복구 코드 한 번). 로그인 밖의 401 `second_factor_required`(새로 고친 대기 세션 포함)는 둘째 단계로 돌아간다(방법을 모르면 코드·복구 코드). 403 `enrollment_required`는 등록 화면으로 바꾼다. QR은 npm `qrcode-generator` `2.0.4`(MIT, 의존성 없음, 2025-08 배포)로 SVG 칸을 직접 그린다. 등록 화면은 lazy라 첫 화면에 들지 않는다.
- Playwright `web/e2e/permissions.spec.ts`는 `web.rs`의 둘째 시험 `browser_shows_a_folder_user_only_their_folder`(QEMU 없음)가 돌린다. 두 시험은 spec 파일 하나씩만 돌리고(각 spec이 서로 다른 환경 변수를 요구한다) 뮤텍스로 하나씩 돈다. CI의 `opld/web-it` 실행에 그대로 든다.
- 첫 화면 묶음(`eager.sh`): JS gzip 377,048 → 389,876 B(+12.8 KB, 그중 en·ko 문구 +6.6 KB), CSS 14,219 → 14,461 B.

**스테이지 8(문서)**
- 새 가이드 `docs/guide/folders.md`("폴더와 권한")에 폴더·권한·할당량·2단계 인증·웹·API·섞인 클러스터·한계를 모았다. "웹 화면 둘러보기"는 영역·탭 표(보기 전환 `목록 | 폴더`, 설정 탭 권한·역할·그룹·보안·2단계 인증)와 권한 항목(`/permissions/mine`으로 객체마다)을, "접근과 보안"은 전역 역할 `none`·그룹 연결·LDAP 사용자의 로컬 2FA·OIDC는 묻지 않음·토큰 범위 링크를 고쳤다. CLI 예는 `oplctl` 도움말과 opld E2E가 쓰는 명령 그대로다(`vm|network|pool move` 대신 `folder members move`).

**리뷰 1에서 고친 것 (서버)**
- **위임 상승 막기(C1).** grant 주기·빼기는 grant가 닿는 모든 곳에서 역할의 권한과 `permission.modify`(루트 grant는 `cluster.admin`)가 자기 유효 권한 안이어야 한다. 닿는 곳은 대상 자체와, propagate면 그 아래 모두다. 아래 모두에 닿는 자기 권한은 그 폴더나 조상의 propagate grant뿐이라 폴더 안 객체 층(`Object{kind, nil, Some(F)}`, 루트면 루트 객체 층)으로 본다. 그래서 propagate가 아닌 grant로 받은 권한을 propagate grant로 넓히지 못한다(403 "… do not hold everywhere inside: …"). 빼기도 같은 규칙이다(지워진 사용자 정의 역할의 grant는 빈 권한으로 본다). grant 바꾸기 경로는 없다.
- **옮기기 풀(I1).** `POST /vms/{key}/migrate`의 `pool`은 그 풀의 `storage.allocate`(볼 수 없으면 404 — 풀 이름 알아내기 없음).
- **되돌리기 폴더(I2).** 스냅샷 되돌리기는 노드·예약·HA·고정처럼 VM의 지금 `folder`를 둔다.
- **2FA 닫힌 실패(I3).** 대기 세션을 `two-factor`를 모두 알 때만 만들던 것을 바꿨다: 요소가 있는 사용자는 늘 둘째 단계를 묻고, 그 기능을 알리지 않은 노드가 있으면 대기 세션은 이 노드의 메모리에만 둔다(Raft 세션을 그 노드가 읽지 않게). 정책이 요구하는데 요소가 없는 사용자는 등록 쓰기를 받을 수 없으므로 로그인을 409 `cluster_upgrading`으로 거절한다. 2FA 쓰기·정책은 그대로 기능 뒤다.
- **둘째 단계 시도 제한(I4).** `TwoFactorStatus`에 `failures`·`blocked_until`(0이면 쓰지 않음 — 옛 JSON 그대로)을 더해 사용자마다 모든 노드를 합쳐 센다. 시도마다 확인 **전에** CAS로 올리고(동시 추측도 다섯 번까지만), 다섯 번째부터 30초에서 두 배씩 15분까지 막는다(429). 틀린 뒤 합이 다섯 이상이면 대기 세션을 끝낸다(비밀번호부터, 그 단계도 제한을 받는다). 통과하면 지운다(TOTP 상태 쓰기가 함께 지우고, 복구 코드·패스키는 따로 지운다). 노드·IP별 메모리 제한은 그대로 둔다. 세션별 세기는 따로 두지 않았다: 사용자 합이 다섯을 넘으면 어느 세션이든 다음 실패에 끝나므로 세션 하나가 다섯 번을 넘지 못한다.
- **정책의 admin(I5).** `roles`에 `admin`이 있으면 grant로 루트 `cluster.admin`이나 어디서든 `permission.modify`를 가진 사용자도 요구한다(색인 한 번). `user.modify`는 권한 목록에 없어 `cluster.admin`이 덮는다.
- **크기 키우기 할당량 먼저(I6).** 늘리기 Task는 할당량을 미리 본 뒤 요청한 크기를 스펙에 먼저 써서 Raft가 몫을 잡고, 그다음 저장소를 늘린다. 늘린 실제 크기는 새 상태 칸 `VolumeStatus.disk_bytes`(Task만 쓴다)에 적은 뒤 스펙에 기록하고 지운다(남겨 두면 스냅샷 되돌리기로 작아진 뒤 그 크기까지 다시 늘리는 것이 할당량을 건너뛴다). Raft `quota_check`는 볼륨 크기를 `status.disk_bytes` 이하로 올리는 기록을 막지 않는다(디스크는 이미 컸다 — 사용량을 적게 세지 않는다). 늘리지 못하면 디스크의 실제 크기로 되돌린다. 남는 틈: 잡은 뒤 저장소를 늘리기 전에 노드가 죽으면 스펙이 디스크보다 크게 남는다(크게 세므로 할당량에는 안전하고, 더 큰 크기로 다시 늘리면 맞춰진다).
- **호스트 경로·브리지는 관리자만(I7).** 풀 만들기(모든 백엔드 — dir 경로뿐 아니라 VG·데이터셋·Ceph conf·NFS 서버도 노드 자원이다)와 브리지 네트워크 만들기는 루트 `cluster.admin`이다. 폴더의 `storage.config`·`network.config`는 VXLAN 네트워크 만들기와 그 폴더의 풀 PATCH(`shared`·`backups`)·지우기, 네트워크 지우기만 덮는다. 내장 역할로는 바뀌는 것이 없다(전에도 admin만 가졌다).
- **작은 것(M1~M7).** CD-ROM ISO는 그 주인(풀)의 `system.read`(만들기·PUT·`/cdrom`). 복제 Task는 확인한 폴더를 덮어쓰기 `folder`로 갖고 Task는 덮어쓰기에 폴더가 없으면 루트에 둔다(스냅샷 복제가 스냅샷 때 폴더로 가던 것도 막는다). 상태 머신은 폴더가 하나도 없어도 없는 폴더를 가리키는 쓰기를 거절한다(스테이지 2의 "폴더가 없으면 보지 않음"을 뺐다 — `folder` 칸은 기능 뒤라 옛 노드는 쓰지 않는다). 역할 `none` 매핑 영역은 `folders` 뒤, 받은 백업의 `vm_spec.folder`는 버린다. 요소가 있으면 요소 더하기(`totp/confirm`·`register/finish`)에 `proof {code|password}`(없으면 400 `proof_required`), 강제 등록 세션은 쓰기 안에서 요소가 없음을 다시 본다(409 `already_enrolled`). 관리자 초기화가 세션 세대를 못 올리면 409 `busy`, TOTP 확인 코드의 칸 기록은 다시 읽어 다시 하고 못 하면 409 `busy`. TOTP 비밀 암호화는 하지 않았다: 쓸 수 있는 클러스터 키(CA 개인키)가 같은 Raft 상태에 있어 스냅샷 유출을 막지 못한다 — 한계로 적었다. `/permissions/effective`는 남의 것이면 사용자를 찾기 전에 `permission.modify`를 본다. `/permissions/who`의 전역 역할 사용자 표는 루트 `cluster.admin`에게만 채운다. `/folders/{id}/usage`는 읽을 수 없는 조상을 `limits`·`binding`에서 뺀다. Prometheus는 `metrics.read`가 없고 `cluster.admin`을 요구하면 viewer 토큰이 깨지므로 루트 `cluster.read`에 더해 루트 아래 모든 VM 읽기(propagate 루트 grant·전역 역할)를 요구한다(viewer 토큰 그대로).
- **고치지 않은 것.** 노드 지표(`/nodes/{key}/metrics`·`/metrics/nodes`)는 노드 점만 돌려주고 VM 목록이 없어 그대로 둔다.

**리뷰 1에서 고친 것 (웹)**
- **폴더 사용자의 홈(I1).** 루트 `cluster.read`가 없으면 건강 띠의 노드·리더, 용량·풀·클러스터 차트·노드 부하·상위 VM 위젯을 그리지 않고(묻지도 않는다) 한 줄로 알린다. 머리줄의 클러스터 전환(이름은 `/cluster`)도 두지 않는다. 숨긴 탭의 주소(`/nodes` 등)를 바로 열면 권한을 받은 뒤 셸이 본문 대신 "볼 권한이 없습니다" 빈 화면을 보인다(`usePermissionsReady` — 그 전에는 전역 역할 값이라 깜박이지 않게 기다린다). `useSettings`·`useSubscription`도 `cluster.read`가 있을 때만 묻는다.
- **기본 폴더는 권한 뒤에(I2).** 새 VM 화면은 `/permissions/mine`을 받을(또는 받지 못할) 때까지 폼을 열지 않는다(폼이 첫 폴더 값을 굳힌다).
- **휴대폰 카드 제목(I3).** VM 목록의 폴더 열을 이름·상태 뒤로 옮겼다(카드 제목은 첫 열).
- **서버 오류 문구·줄 수 있는 역할만(I4).** grant 더하기는 서버와 같은 셈(`grantable`: 대상에서 역할 권한 + `permission.modify`, 루트면 `cluster.admin`, propagate면 그 아래 층도)으로 줄 수 있는 역할만 보이고, 기본은 그중 운영자(없으면 첫 역할)다. 더하기·빼기의 403 `forbidden`은 서버 설명을 붙여 보인다.
- **작은 것(M1~M7).** 새로 고침 뒤 둘째 단계는 방법을 모른다고 알리고 "로그인으로 돌아가기"를 주 단추로, 409 `already_signed_in`은 로그인을 끝내고, 인증 앱·패스키 없이 복구 코드만 물으면 패스키는 도메인 이름에서만 된다고 알린다(M1). 다시 연결할 때 읽을 수 없게 된 종류의 목록과 설정·구독 캐시를 버린다(M2). 폴더 트리는 나중에 고른 폴더의 조상도 펼친다(새 폴더가 아직 없으면 들어온 뒤 한 번, M3). 2FA 정책 폼은 정책 내용으로 키를 잡아 다른 설정 변경에 고치던 값을 잃지 않는다(M4). 나(내가 든 그룹·이 대상의 내 권한 근거)에게 건 grant를 빼면 이름을 넣어 확인한다(M5). 할당량의 vCPU·VM 수는 0 이상의 정수만(칸 오류), GiB 칸은 소수를 받아 반올림한다(M6). 패스키 로그인은 브라우저 창이 열린 동안 바쁨이고, 패스키를 쓸 수 없는 브라우저(안전하지 않은 주소)면 단추 대신 까닭을 보인다(M7).
- **서버 리뷰 1에 맞춤.** 요소가 있으면 인증 앱·패스키 더하기가 본인 확인 창(`ProofDialog`)을 거쳐 `proof`를 보낸다(패스키는 창의 단추가 등록 전체를 한다). 새 문구: `proof_required`·`already_enrolled`, 둘째 단계의 429(`login.secondFactor.too_many_attempts`, 기다리라고), 로그인의 `cluster_upgrading`(업그레이드 중이라 등록을 받을 수 없음). 풀 만들기(+ 만들기·첫 걸음 포함)는 루트 `cluster.admin`만, 네트워크 만들기는 `cluster.admin`이면 브리지·VXLAN, 폴더 `network.config`뿐이면 VXLAN만 고른다. 권한 탭은 관리자가 아니면 전역 역할 사용자가 숨겨졌다고 알리고, 할당량 탭은 읽을 수 없는 조상이 있으면 그 할당량은 보이지 않는다고 알린다.
- **고치지 않은 것.** 올리기·OVA 가져오기·템플릿 배포 창은 접속한 노드를 `/cluster`·노드 목록으로 찾아 폴더 사용자에게는 "노드를 모른다"고 보인다(리뷰 범위 밖, 서버가 노드를 알려 줄 길이 필요하다).
