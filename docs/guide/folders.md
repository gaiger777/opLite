# 폴더와 권한

인벤토리 폴더, 폴더·객체 권한(아래로 내려감), 사용자 정의 역할과 그룹, 폴더 할당량, 2단계 인증(TOTP·복구 코드·패스키).
로그인·LDAP·OIDC·감사 로그는 [접근과 보안](access.md)에, 화면 구성은 [웹 화면 둘러보기](web-ui.md)에 있다.

- [예: 팀에 폴더 맡기기](#예-팀에-폴더-맡기기)
- [폴더](#폴더)
- [권한](#권한)
- [할당량](#할당량)
- [2단계 인증](#2단계-인증)
- [웹 화면](#웹-화면)
- [API](#api)
- [버전이 섞인 클러스터](#버전이-섞인-클러스터)
- [한계](#한계)

## 예: 팀에 폴더 맡기기

A팀이 자기 VM만 보고 다루게 하고, vCPU 32개·VM 20대까지로 묶는다. admin으로:

```bash
orbivisorctl folder create /teamA --vcpus 32 --memory 128G --vms 20     # VM 트리에 폴더와 할당량
orbivisorctl folder create /teamA --kind network                        # 네트워크 트리에도 같은 이름
orbivisorctl folder members move /teamA vm/web-1 vm/db-1                # 있는 VM 옮기기(항목마다 결과)
orbivisorctl user create alice --role none --password-file alice.pw     # 전역 역할 없음 — 권한은 grant로만
orbivisorctl group create team-a --member alice --external corp:cn=team-a,ou=groups,dc=example,dc=com
orbivisorctl grant add --group team-a --role operator --on /teamA       # /teamA와 그 아래 VM에 operator
orbivisorctl grant add --group team-a --role operator --on network:/teamA
orbivisorctl perm check vm/web-1 --user alice
# alice (role none) on vm/web-1: system.read, vm.create, vm.delete, vm.config, vm.power, …
```

alice는 VM 목록·`/watch`·작업·경보·팔레트에서 `/teamA`의 것만 보고, 다른 팀 VM은 **404**다. `vm create`는 `--folder /teamA`로
만든다(루트에 만들 권한이 없으므로 `--folder` 없이는 **400** `folder_required`). 21번째 VM은 **409** `quota_exceeded`다.
`operator`에는 하위 폴더 만들기·권한 주기가 없다. 팀 관리자에게 맡기려면 그것을 담은 역할을 만들어 준다([위임](#위임과-상승-막기)).

## 폴더

- **종류별 트리 셋.** VM(템플릿 포함), 네트워크, 스토리지 풀이 따로 트리를 가진다. 한 트리에 종류를 섞지 않으므로 "A팀 VM과
  A팀 네트워크"는 폴더 둘, grant 둘이다(같은 그룹에 두 줄). 볼륨·백업·보안 그룹·노드에는 폴더가 없다.
- **루트.** 트리마다 보이지 않는 루트가 있다. 폴더를 정하지 않은 리소스는 루트에 있고, 업그레이드 전의 리소스도 그렇다.
  CLI는 `/`, 웹은 "맨 위"(모든 VM·모든 네트워크·모든 스토리지 풀)다. 루트 권한(`grant --on root`, 전역 역할)은 세 트리와
  클러스터 객체(노드·설정·HA 등) 모두의 조상이다.
- **중첩.** 깊이는 16단계까지(넘으면 **409** `folder_too_deep`), 폴더는 클러스터 전체(세 트리 합)에 10,000개까지(**409**
  `too_many_folders`). 이름은 1~64자이고 `/`·제어 문자·앞뒤 공백이 없어야 한다. 같은
  부모 아래 이름은 대소문자를 무시하고 하나다(**409** `name_taken`, 두 노드가 동시에 만들어도 하나만). 다른 부모 아래에는 같은
  이름(`/teamA/prod`, `/teamB/prod`)을 쓴다. 폴더 리소스의 이름은 폴더 id이고 화면 이름은 스펙에 있다.
- **옮기기.** 폴더는 부모를 바꿔 옮긴다(`folder move /teamA/prod --to /teamB`, `--to /`는 맨 위). 자기 아래로 옮기면 **409**
  `folder_cycle`. 리소스는 `folder members move DEST vm/web network/lan storage-pool/fast …`(API `POST /folders/{id}/members`,
  맨 위로는 `/folders/root/members`)로만 옮긴다. 항목마다 성공·실패를 돌려주고 CLI는 하나라도 실패하면 실패로 끝난다. 항목은
  모두 대상 폴더와 같은 트리여야 한다. `PUT /vms/{key}`로 `folder`를 바꾸면 **400**("use POST /folders/{id}/members to move")이다.
  VM 이동은 실행 중에도 되고 VM에는 아무 일도 일어나지 않는다(권한·할당량만 바뀐다).
- **지우기.** 빈 폴더만 지운다. 하위 폴더나 리소스가 있으면 **409** `folder_not_empty`. 지우기와 넣기가 엇갈려도 Raft 순서로
  하나만 이긴다.
- **만들 때 폴더 고르기.** `vm|network|pool create --folder /teamA`(API는 `spec.folder`에 폴더 id). 복원은 백업한 VM의 폴더가
  아직 있으면 그곳, 없으면 맨 위이고, 복제는 원본 VM의 폴더다. 복원·복제·가져오기 실행의 다른 폴더는 API의 `folder`로 고른다.
- **목록 거르기.** `vm|network|pool list --folder /teamA`는 그 폴더에 바로 든 것, `--recursive`를 더하면 하위 폴더까지다(웹은
  "하위 포함" 칩). `--folder /`는 맨 위에 있는 것이다.
- **보이는 폴더.** 폴더 목록(`GET /folders?kind=vm`)은 읽을 수 있는 폴더만이다. 부모를 볼 수 없는 폴더는 그 사용자에게 맨 위
  폴더로 보인다(CLI 경로 `/teamA/prod`도 그 사람이 보는 트리 기준이다).

```bash
orbivisorctl folder tree                         # VM 트리(--kind network|storage)
# vm:/
#   teamA
#     prod
#   teamB
orbivisorctl folder list --kind network          # 경로·트리·id·할당량
orbivisorctl folder create /teamA/prod           # 부모의 inventory.folder
orbivisorctl folder rename /teamA/prod production
orbivisorctl folder move /teamA/production --to /teamB
orbivisorctl folder delete /teamB/production     # 빈 폴더만
orbivisorctl vm list --folder /teamA --recursive
orbivisorctl network create lan-a --bridge br0 --folder /teamA     # 네트워크 트리의 /teamA
```

폴더 인자는 `[종류:]경로`(`/teamA/prod`, `network:/lan`, `storage:/fast`) 또는 폴더 id다. `folder` 명령은 `--kind`(기본
`vm`)로 트리를 정한다.

## 권한

### grant와 평가

- **grant = 주체 + 역할 + 대상.** 주체는 사용자나 로컬 그룹, 역할은 내장이나 사용자 정의, 대상은 루트·폴더·객체 하나(VM·
  네트워크·스토리지 풀)다. 폴더 grant는 기본으로 **아래로 내려간다**(하위 폴더와 그 안의 리소스). `--no-propagate`면 그 폴더
  자체(이름 바꾸기·지우기·그 폴더의 권한 보기)에만 닿는다.
- **유효 권한 = 합집합.** 전역 역할(루트에 건 내장 역할로 읽는다) ∪ 자기와 자기 그룹의 grant 가운데 그 객체에 직접 건 것 ∪
  조상 폴더·루트에 내려가게 건 것. **거부 규칙은 없다.** grant를 더하면 권한이 늘기만 하고, "왜 할 수 있나"는 근거 grant로
  설명된다(`perm check`). "prod 전체에서 payroll VM만 빼기"는 payroll을 형제 폴더로 옮겨 푼다.
- 어떤 권한이든 그 객체의 `system.read`를 품는다(풀에 `storage.allocate`만 받아도 그 풀이 보인다).
- 권한은 요청마다 다시 본다. grant·그룹·역할을 바꾸면 다음 요청부터 반영되고 세션은 끊기지 않는다. 열린 `/watch`는 권한이
  바뀐 주체만 `{"type":"error","code":"permissions"}`로 닫혀 웹이 목록을 다시 읽는다.

### 권한 목록

| 권한 | 뜻 |
|---|---|
| `system.read` | 보기: 목록·상세·watch·지표·작업·경보 |
| `vm.create` · `vm.delete` | 그 폴더에 VM 만들기(새로·복원·복제·가져오기·배포 받기) · 지우기 |
| `vm.config` | 스펙 바꾸기, CD, cloud-init, 펌웨어 초기화, 핫플러그, 템플릿으로 바꾸기·되돌리기, 게스트 비밀번호 |
| `vm.power` · `vm.console` | 켜기·끄기·재시작·에이전트 종료 · 콘솔 |
| `vm.snapshot` · `vm.migrate` · `vm.clone` | 스냅샷 만들기·되돌리기·지우기 · 라이브·저장소 옮기기, 로컬 디스크 복제, 추천 적용 · 원본(템플릿) 쪽 복제·배포 |
| `backup.create` · `backup.restore` · `backup.delete` · `backup.job` | 지금 백업·원격 사본 · 복원 · 지우기 · 백업 작업(루트) |
| `storage.allocate` · `storage.config` | 풀에 볼륨 만들기·늘리기·지우기, 이미지 받기 · 풀 만들기·바꾸기 |
| `network.assign` · `network.config` | NIC를 그 네트워크에 붙이기 · 네트워크·보안 그룹 쓰기 |
| `inventory.folder` · `inventory.move` · `inventory.quota` | 하위 폴더 만들기·이름 바꾸기·지우기 · 옮기기(원본·대상 둘 다) · 자식 폴더의 할당량 |
| `permission.modify` | 이 대상 아래 grant 주기·빼기(자기 권한 안에서), 누가 접근하는지 보기 |
| `alert.read` · `alert.manage` | 경보 보기 · 경보 규칙 쓰기(루트) |
| `cluster.read` | 노드·클러스터·설정·HA·배치·Ceph·원격 읽기, 노드·클러스터 지표, Prometheus |
| `cluster.admin` | 계정·영역·남의 토큰·알림 대상·감사 로그·설정·노드·HA·배치·Ceph·원격 쓰기, 사용자 정의 역할·그룹·루트 grant |

`orbivisorctl role privileges`가 이름 목록을 보인다.

### 내장 역할과 전역 역할

| 내장 역할 | 권한 |
|---|---|
| `admin` | 전부 |
| `operator` | `system.read`·`cluster.read`·`alert.read`, `vm.*` 8개, `backup.*` 4개, `storage.allocate`, `network.assign` |
| `viewer` | `system.read`·`cluster.read`·`alert.read` |

내장 역할은 예전 역할과 뜻이 같다. 사용자의 전역 역할(`user create --role`)은 "루트에 그 내장 역할 grant"로 읽으므로 grant가
하나도 없는 클러스터는 예전과 똑같이 동작한다. 새 전역 역할 **`none`**은 권한이 없다 — 폴더 grant로만 일하는 사용자에게 준다.
`operator`에는 `inventory.*`·`permission.modify`·`network.config`·`storage.config`가 없다.

### 사용자 정의 역할

```bash
orbivisorctl role create team-admin \
  --privileges vm.create,vm.delete,vm.config,vm.power,vm.console,vm.snapshot,inventory.folder,inventory.move,inventory.quota,permission.modify \
  --description "팀 폴더 관리자"
orbivisorctl role list                         # 내장 셋 + 사용자 정의
orbivisorctl role set team-admin --privileges vm.power,vm.console    # 이 역할을 건 모든 grant가 바로 바뀐다
orbivisorctl role delete team-admin            # grant가 쓰고 있으면 409 role_in_use
```

역할 만들기·바꾸기·지우기는 `cluster.admin`, 읽기는 로그인한 누구나다(위임자가 고른다). 이름 `admin`·`operator`·`viewer`·
`none`은 예약이고 설명은 256자까지다. 모르는 권한 이름은 **400**이다.

### 그룹과 외부 그룹

- **로컬 그룹**(`cluster.admin`)은 멤버(로컬·외부 사용자)와 **연결한 외부 그룹**(`영역:그룹` — LDAP 그룹 DN이나 OIDC `groups`
  클레임 값)을 가진다. grant는 외부 그룹 이름을 바로 가리키지 않고 늘 로컬 그룹을 가리킨다. 디렉터리 그룹 이름이 바뀌면 그룹
  한 곳만 고친다.
- 외부 사용자는 로그인 때 받은 그룹 목록이 저장되고(`external_groups`, 바뀌었을 때만 쓴다), 그 영역의 연결한 외부 그룹과 같은
  값(앞뒤 공백을 떼고 ASCII 대소문자 무시 — [역할 매핑](access.md#외부-인증ldap)과 같은 비교)이 있으면 그 그룹에 든다. 디렉터리에서
  그룹을 빼면 **다음 로그인**부터 반영된다.
- 영역의 그룹 → 역할 매핑(전역 역할)은 그대로 쓴다. 폴더 grant로만 일할 외부 사용자는 매핑의 기본 역할을 `none`으로 두어
  로그인하게 한다(`realm set-ldap corp --default-role none`). 매핑도 기본 역할도 없으면 예전처럼 **403** `no_role`이다.

```bash
orbivisorctl group create team-a --member alice --external corp:cn=team-a,ou=groups,dc=example,dc=com --description "A팀"
orbivisorctl group set team-a --add-member bob --remove-external corp:cn=old,ou=groups,dc=example,dc=com
orbivisorctl group get team-a
orbivisorctl group delete team-a               # grant가 쓰고 있으면 409 group_in_use
```

### grant 주기와 보기

```bash
orbivisorctl grant add --group team-a --role operator --on /teamA          # 폴더(VM 트리)
orbivisorctl grant add --user bob --role viewer --on network:/teamA
orbivisorctl grant add --user carol --role team-admin --on vm/db-1          # 객체 하나
orbivisorctl grant add --user dave --role viewer --on /teamA --no-propagate # 폴더 자체만
orbivisorctl grant list --on /teamA            # --user, --group으로도 거른다
orbivisorctl grant remove <grant id>
orbivisorctl perm mine                         # 내 권한: 루트, 폴더마다, 객체마다
orbivisorctl perm check vm/web-1 --user alice  # 권한과 근거 grant(perm effective의 별칭)
orbivisorctl perm who /teamA                   # 이 대상에 닿는 grant와 전역 역할 사용자
```

대상 인자는 `root`, 폴더(접두사 없는 경로는 VM 트리, `network:/x`·`storage:/x`, `folder/<id>`), `vm/NAME`, `network/NAME`,
`storage-pool/NAME`(`pool/NAME`)이다. 그룹으로 닿는 사용자는 `perm who`에 그룹 grant의 주체로 보인다.

### 위임과 상승 막기

- grant를 주거나 빼려면 **grant가 닿는 모든 곳**에서 역할의 권한과 `permission.modify`가 자기 유효 권한 안이어야 한다. 닿는
  곳은 대상 자체이고, 아래로 내려가는 grant면 그 아래 모두다. 아래 모두에 닿는 자기 권한은 그 폴더나 조상에 **아래로 내려가는
  grant**로 받은 것뿐이라서, `--no-propagate`로 받은 권한은 그 폴더 자체에만 다시 줄 수 있다. 아니면 **403** `forbidden`
  "cannot grant privileges you do not hold: vm.delete"(아래 모두가 모자라면 "… do not hold everywhere inside: …"). 빼기도 같은
  규칙이다(자기가 가진 것보다 넓은 grant는 빼지 못한다).
- 루트 grant, 사용자 정의 역할·그룹 쓰기는 `cluster.admin`이다(역할 하나를 고치면 모든 grant가 바뀌므로).
- 할당량은 그 폴더의 **부모**에서 `inventory.quota`가 필요하다(맨 위 폴더는 루트). 팀 관리자는 자기 폴더의 상한을 올리지 못하고
  자식 폴더의 상한만 나눈다.
- 폴더를 옮기려면 그 폴더와 함께 **옛 부모와 새 부모 안 모두**에서 `inventory.move`가 필요하다. 안 모두에 닿는 권한은 그
  부모나 조상에 아래로 내려가는 grant로 받은 것뿐이라서, `--no-propagate`로 그 폴더에만 받은 `inventory.move`로는 폴더를 자기가
  다스리는 폴더 아래로 끌어와 하위 트리를 넘겨받지 못한다(**403** "moving a folder needs inventory.move everywhere inside …").
  리소스를 옮기려면 그 리소스와 대상 폴더 모두에 `inventory.move`가 필요하다(vSphere처럼, 객체 하나에 받은 `inventory.move`로
  그 객체를 자기 폴더로 옮기면 그 폴더의 관리를 받는다 — 객체 grant에 이 권한을 넣을 때 주의한다).
- grant 보기: `cluster.admin`은 전부, 그 밖에는 자기·자기 그룹의 grant와 `permission.modify`가 있는 대상의 grant다.
- **지운 사용자·대상.** 사용자를 지우면 그 사용자의 grant와 그룹 멤버 자리를, 폴더·VM·네트워크·풀을 지우면 그것에 건 grant를 함께
  지운다(지운 뒤 곧바로, 놓친 것은 리더의 정리 작업이 1분마다). 그사이 남은 것은 아무에게도 닿지 않는다. 그룹 고치기는 이미
  들어 있던 지운 사용자를 오류 없이 뺀다.
- grant는 클러스터 전체에 10,000개(**409** `too_many_grants`), API 토큰은 사용자마다 100개(**409** `too_many_tokens`)까지다.
- 사용자 목록(`GET /users`)은 `cluster.admin`만 읽는다. 위임자는 사용자 이름 대신 **id**로 준다(웹은 이미 접근하는 주체를 고르기
  목록으로 준다). `perm who`의 전역 역할 사용자 표도 `cluster.admin`에게만 채운다. 남의 유효 권한(`perm effective --user`)은 그
  대상의 `permission.modify`가 있어야 하고, 없으면 사용자가 있든 없든 **403**이다.
- **노드 자원.** 스토리지 풀 만들기(dir 경로·LVM VG·ZFS 데이터셋·Ceph·NFS), dir 풀의 `shared` 바꾸기(모든 노드가 같은 파일을
  본다는 주장 — 마이그레이션·HA가 믿는다)와 브리지 네트워크 만들기(노드의 브리지·NIC·VLAN)는 루트 `cluster.admin`이다. 폴더의
  `storage.config`·`network.config`로는 VXLAN 네트워크 만들기와 그 폴더의 풀 `backups` 바꾸기, 풀·네트워크 지우기만 한다.

### API 토큰의 범위

```bash
orbivisorctl token create ci --scope vm.power,vm.console@/teamA --scope vm.power,vm.console@/teamB
orbivisorctl token list                        # SCOPE 열
```

토큰의 권한은 **주인의 유효 권한 ∩ 범위**다(범위는 `대상`에 `권한`을 내려가게 건 천장으로 읽는다). 범위 없는 토큰은 예전처럼
주인과 같다. 범위는 권한 목록 하나와 대상 여럿이라 `--scope`마다 권한이 같아야 한다. 범위 토큰으로는 토큰을 만들지 못하고(넓히기
막기) 2단계 인증도 바꾸지 못한다(**403**). 토큰은 2단계 인증을 거치지 않으므로 자동화에는 범위 토큰을 쓴다.

### 404와 403, 거르는 곳

- **읽을 수 없는 객체는 404**, 읽을 수 있지만 그 동작 권한이 없으면 **403** `forbidden`이다. 이름이나 id로 남의 객체가 있는지
  알 수 없다. 만들기에서 폴더를 주지 않았는데 루트에는 권한이 없고 다른 폴더에는 있으면 **400** `folder_required`, 어디에도
  없으면 **403**이다.
- **목록**(VM·볼륨·스냅샷·백업·작업·가져오기·네트워크·풀·폴더 등)은 볼 수 있는 것만 돌려준다.
- **`/watch`**는 이벤트마다 거른다. 보이던 객체가 옮겨져 안 보이게 되면 이름·id만 담은 `deleted`가 온다.
- **지표**: VM 지표·기록은 그 VM의 `system.read`, 풀 기록은 풀의 `system.read`. 노드 지표·`/metrics/cluster`는 루트
  `cluster.read`다(노드 합계가 다른 팀의 부하를 드러내므로). 대시보드 요약의 상위 VM·풀은 거른다. Prometheus `/metrics`는 모든
  VM의 값을 담으므로 루트 `cluster.read`에 더해 루트 아래 모두를 읽어야 한다(전역 `viewer` 이상이나 아래로 내려가는 루트 grant —
  예전 viewer 토큰은 그대로 된다).
- **경보**: 활성 경보·기록은 대상의 `alert.read`로 거르고(노드·백업 작업 경보는 루트), 규칙 쓰기는 루트 `alert.manage`, 알림
  대상은 `cluster.admin`이다.
- **작업(Task)**: 목록·상세는 대상을 읽을 수 있어야 하고, 취소는 그 작업의 권한이다. 대상이 없는 작업(노드 비우기·Ceph·HA 등)은
  루트 `cluster.read`다.
- **파생 객체는 주인을 따른다**: 볼륨은 디스크로 붙은 VM(아니면 풀, CD의 ISO는 주인이 아니다), 스냅샷은 VM, 백업은 VM(지웠으면
  백업에 담긴 VM의 폴더), 작업은 대상이다. 가져오기(VM import)는 루트 객체다(올리기·ESXi 연결은 루트 `vm.create`).
- **만들기·배포·복원**은 대상 폴더 `vm.create`, 템플릿 `vm.clone`, 풀 `storage.allocate`, NIC마다 `network.assign`(보안 그룹은 읽기)과
  할당량을 본다. CD-ROM에 넣는 ISO는 그 풀을 읽을 수 있어야 하고(못 보면 **404**), 마이그레이션의 `pool`과 새 복제
  대상(`replication`)의 풀(없으면 디스크마다 같은 이름의 풀)은 그 풀의 `storage.allocate`가 필요하다. 복제·배포는 확인한 폴더에 들고(작업이 기다리는 사이 원본을 옮겨도), 스냅샷 되돌리기는 VM의 지금
  폴더를 둔다. 원격 클러스터로 백업 보내기(`backup replicate`, 백업 작업의 원격 대상)는 루트 `cluster.admin`이다(받는 쪽은 폴더를
  버리고 루트에 두므로 그쪽의 모든 읽기 사용자에게 보인다). 감사 로그는 루트 `cluster.admin`, 콘솔은 `vm.console`이다(열린 콘솔은
  권한을 잃으면 — grant 빼기, 다른 폴더로 옮기기 — 2초 안에 닫힌다).
- **작업의 요청자.** API 요청이 만든 작업(Task)은 레이블 `requested-by`에 요청한 사용자 id를 갖는다(스케줄러·HA·정리 작업이 만든
  작업에는 없다).
- 루트 `cluster.read`가 없는 폴더 사용자는 노드 목록을 못 본다. VM은 자동 배치이고 머신 종류는 서버가 클러스터 아키텍처로 고른다
  (웹은 노드 칸을 숨긴다). CLI `vm create`는 그때 `--machine virt|q35`를 달라고 한다.

## 할당량

- **VM 폴더만**(네트워크·풀 폴더에 주면 **400**). 차원은 vCPU, 메모리(MiB), 디스크(바이트), VM 수이고 비운 차원은 상한이 없다.
- **세는 것 = 할당량, 전원과 무관.** 하위 트리의 VM마다 vCPU 수, 메모리, 디스크로 붙은 볼륨 크기의 합(링크드 클론도 가상 크기),
  템플릿이 아닌 VM 수. 템플릿은 디스크만 센다. 붙지 않은 볼륨과 CD의 ISO는 세지 않는다.
- **중첩.** 사용량은 하위 트리 합이고, 쓰기는 경로 위의 **할당량 있는 모든 조상**을 지켜야 한다(가장 빡빡한 것이 묶는다). 자식
  상한 합이 부모를 넘어도 된다(미리 떼어 두지 않는다). 자식 상한이 부모 때문에 의미가 없으면 사용량에 "부모가 묶음"(`bound by
  /teamA`)으로 보인다.
- **줄이기.** 사용량보다 낮게 둘 수 있다. 그때는 "상한 넘음"으로 보이고 **늘리는 쓰기만** 막는다. HA 복구·마이그레이션·비우기·
  균형 조정(노드만 바꿈), 이름 바꾸기, 끄기는 막히지 않는다.
- **노드 사이 경쟁에도 지킨다.** 확인은 Raft 상태 머신에서 커밋 순서대로 한다. 두 노드에서 동시에 만들어도 뒤에 커밋된 쪽만
  거절된다. 그 전에 API가 요청 때 같은 계산으로 먼저 거절한다: **409** `quota_exceeded` "folder /teamA would use 34 of 32 vCPUs",
  `details`에 `folder`·`dimension`(`vcpus`·`memory_mib`·`disk_bytes`·`vms`)·`limit`·`used`. 미리 확인하는 곳: VM 만들기, 스펙
  바꾸기(핫플러그·CD·템플릿 바꾸기 포함), 복제·배포, 복원, 가져오기 실행, 볼륨 늘리기, 폴더로 옮기기, 폴더 옮기기.
- 복제·복원·가져오기 작업은 디스크를 만든 뒤 VM을 쓰므로, 경쟁에서 진 작업은 마지막 쓰기에서 실패하고 만든 볼륨을 지운다.
- **사용량**: `GET /api/v1/folders/{id}/usage` → `{folder, path, usage, limits[{folder, path, quota, usage}], binding{차원: 폴더}}`
  (그 노드의 상태로 바로 셈, 캐시 없음). 읽을 수 없는 조상 폴더의 상한은 `limits`·`binding`에 없다(그 상한에 걸린 쓰기는 409가
  알린다).
- **볼륨 늘리기**는 저장소를 늘리기 전에 요청한 크기를 먼저 기록해 Raft에서 몫을 잡는다(그동안 같은 폴더의 다른 늘리기는 409).
  저장소가 크기를 올려(LVM extent·ZFS 단위) 상한을 조금 넘더라도 실제 크기를 기록한다(사용량을 적게 세지 않는다). 늘리지 못하면
  디스크의 실제 크기로 되돌린다.

```bash
orbivisorctl folder quota set /teamA --vcpus 32 --memory 128G --disk 2T --vms 20
orbivisorctl folder quota set /teamA --disk none     # 준 차원만 바꾼다. none은 그 차원을 지운다
orbivisorctl folder quota clear /teamA
orbivisorctl folder usage /teamA
# folder /teamA
# vCPUs: 2 of 32 (bound by /teamA)
# memory (MiB): 512
# disk: 0B
# VMs: 2 of 20 (bound by /teamA)
```

`--memory`는 단위가 없으면 MiB, `--disk`는 단위가 없으면 바이트다. `folder create`도 같은 선택을 받는다.

## 2단계 인증

비밀번호(로컬 또는 LDAP) 다음에 둘째 단계를 묻는다. 요소는 인증 앱(TOTP), 패스키(WebAuthn), 복구 코드다.

- **TOTP.** RFC 6238, SHA-1·6자리·30초, 앞뒤 한 칸까지 받는다(일반 인증 앱). 한 번 쓴 칸의 코드는 다시 받지 않는다(등록 확인에 쓴
  코드도). 두 노드에 같은 코드를 동시에 보내도 하나만 통과한다.
- **복구 코드.** 첫 요소(TOTP 확인이나 첫 패스키)를 더할 때 10개를 **한 번만** 보인다(`xxxx-xxxx-xxxx-xxxx`, 공백·`-`·대소문자는
  무시). 하나로 한 번 로그인한다. 다시 만들면 남은 코드는 버린다. 마지막 요소를 빼면 복구 코드도 지운다.
- **패스키.** 웹에서만 등록·사용한다(CLI 없음). 사용자당 16개, 이름 1~64자. ES256·EdDSA·RS256 키를 받고 증명(attestation)은 보지
  않는다. RP ID는 정책의 `rp_id` 또는 브라우저가 연 호스트 이름이다. **웹 화면을 IP 주소(`https://10.0.0.11:8006`)로 열면
  브라우저가 패스키를 막는다** — 화면은 패스키를 숨기고 이유를 보인다(API **400** `webauthn_unavailable`). 패스키를 쓰려면 DNS
  이름으로 연다.
- **로그인.** 비밀번호가 맞고 요소가 있거나 정책이 요구하면, 쿠키는 5분짜리 **대기 세션**이고 응답에 `second_factor {methods,
  webauthn?, enroll, expires_at}`가 있다. `POST /api/v1/auth/login/second-factor {"totp"|"recovery"|"webauthn": …}`가 맞으면 같은
  쿠키가 정식 세션이 된다. 대기 세션으로 다른 경로를 부르면 **401** `second_factor_required`, 틀린 코드는 **401**
  `invalid_second_factor`이다. 틀린 횟수는 사용자마다 **모든 노드를 합쳐** Raft에 세고(확인 전에 올린다), 5번 틀리면 대기
  세션이 끝나 비밀번호부터 다시 하고 30초부터 두 배로(15분까지) 막힌다(**429**, 맞는 코드도). 통과하면 지우고, 막혀 있지 않은 채
  15분 동안 시도가 없어도 잊는다(드문 오타가 쌓여 막지 않게). 요소를 빼거나 더할 때의 본인 확인 코드도 같은 수로 센다. 노드·IP마다의
  메모리 제한도 그대로 있다.
- **정책.** 설정의 `two_factor {roles, realms, rp_id}`: 전역 역할이 `roles`에 있거나 영역(`local` 포함)이 `realms`에 있는 사용자에게
  요구한다. OIDC 영역은 넣을 수 없다(**400**, IdP가 MFA를 맡는다). `roles`에 `admin`이 있으면 grant로 관리 권한을 받은 사용자 —
  루트 `cluster.admin`이나 어디서든 `permission.modify`(남에게 권한을 줄 수 있다) — 도 요구한다. 그 밖의 사용자 정의 역할
  grant로는 정하지 않는다. 정책이 요구하는데
  요소가 없으면 로그인이 **강제 등록**으로 간다(웹: 인증 앱 QR이나 패스키, 그다음 복구 코드. CLI: 터미널에서 TOTP 등록). 등록을
  마치면 로그인이 끝난다. 요구받는 사용자는 마지막 요소를 뺄 수 없다(**409** `two_factor_required`).
- **본인 확인.** TOTP 끄기, 패스키 빼기, 복구 코드 다시 만들기는 코드·복구 코드 또는 로컬 사용자의 비밀번호가 필요하다(외부
  사용자는 코드만). 이미 요소가 있으면 **요소 더하기**(`/auth/2fa/totp/confirm`·`/auth/2fa/webauthn/register/finish`의 `proof
  {code|password}`)도 같다(없으면 **400** `proof_required`) — 훔친 세션·토큰이 오래 가는 요소를 더하지 못하게. 인증 앱 더하기는
  새 앱의 코드를 먼저 보고 맞을 때만 본인 확인을 쓴다(틀린 앱 코드에 복구 코드가 없어지지 않게). 강제 등록 중인 대기
  세션은 그사이 본인이 등록했으면 **409** `already_enrolled`(다시 로그인)이다.
- **관리자 초기화.** `orbivisorctl user 2fa-reset alice`(`DELETE /api/v1/users/{key}/two-factor`, `cluster.admin`)는 요소를 모두
  지우고 그 사용자의 세션을 끝낸다. API 토큰은 남는다. 자기 자신은 안 된다(**409** `cannot_reset_self`).
- **다른 인증과.** LDAP 사용자는 디렉터리 바인드 뒤 로컬 요소를 묻는다(요소는 `alice@corp`에 붙는다). OIDC 사용자에게는 묻지 않고
  정책도 요구하지 않는다(2FA 쓰기는 **400**).
- **API 토큰.** 토큰은 2단계 인증을 거치지 않는다. 대신 토큰은 둘째 단계를 마친 세션이나 다른 토큰으로만 만들 수 있으므로
  비밀번호만 훔쳐서는 토큰을 만들 수 없다. 정책을 켜기 전에 만든 토큰은 그대로 남으니 `token list`로 보고 정리한다. 자동화에는
  [범위 토큰](#api-토큰의-범위)을 쓴다.
- **저장.** TOTP 비밀은 서버가 다시 계산해야 하므로 Raft에 평문으로 있다(LDAP 서비스 계정 비밀번호와 같이 API 응답·watch·감사
  로그에는 나오지 않는다). 따로 암호화하지 않는다: 쓸 수 있는 클러스터 키(CA 개인키)도 같은 Raft 상태에 있어 스냅샷이 새면
  함께 새기 때문이다. 확인하지 않은 등록 중 비밀은 다음 등록이나 TOTP 끄기 때까지 남는다. 복구 코드는 SHA-256, 패스키는 공개 키만
  둔다.
- **감사 로그.** `2fa totp on|off`, `2fa recovery regenerate`, `2fa passkey add|remove NAME`, `second factor totp|recovery|webauthn`,
  `user 2fa-reset NAME`, `settings two-factor roles=[…] realms=[…]`(비밀·코드는 적지 않는다).

```bash
orbivisorctl 2fa totp enable                   # 비밀과 otpauth:// URI를 보이고 앱의 코드를 물어 확인, 첫 요소면 복구 코드 10개
orbivisorctl 2fa status                        # TOTP, 남은 복구 코드, 패스키, 정책이 요구하는지
orbivisorctl 2fa recovery regen --code 123456
orbivisorctl 2fa totp disable --password-file pw.txt
orbivisorctl login --server https://10.0.0.11:8006 --user alice --password-file pw.txt --code 123456
# --code가 없으면 터미널에서 묻는다(터미널이 아니면 실패). 6~8자리 숫자는 TOTP, 그 밖은 복구 코드다.
orbivisorctl settings set two_factor.roles=admin two_factor.realms=local,corp two_factor.rp_id=orbivisor.example.com
orbivisorctl settings set two_factor=off       # 정책 지우기
orbivisorctl user list                         # 2FA 열
```

`2fa totp enable --no-confirm`은 비밀만 보이고 `2fa totp confirm CODE`로 끝낸다(스크립트용). 패스키만 있는 사용자는 웹에서
토큰을 만들어 `orbivisorctl login --token-file`로 들어온다.

## 웹 화면

- **폴더 보기.** 가상 머신 › VM, 인프라 › 스토리지 풀, 인프라 › 네트워크 탭의 보기 전환 `목록 | 폴더`(`/vm-folders`·
  `/storage-folders`·`/network-folders`). 왼쪽은 트리(화살표 키로 움직이고 펼친다), 오른쪽은 그 폴더의 표와 **하위 포함** 칩이다.
  폴더 보기 위의 "새 폴더"로 만들고, 이름 바꾸기·지우기·할당량은 폴더 상세 머리의 ⋯ 메뉴에 있다. VM 목록 보기에는 폴더 열과 폴더 거름(하위 포함)이 있고, 템플릿은
  VM 폴더 보기와 목록 거름에 함께 보인다.
- **폴더 상세**(`/vm-folders/:id` 등): 머리와 요약 카드(VM 수, 사용량/상한 막대 넷, "부모가 묶음"), 탭 `내용 | 할당량 | 권한`.
- **옮기기.** 표의 행(여럿 고르면 묶음)을 트리 노드로 **끌어 놓으면** 바로 옮기고 결과를 알림 하나로 묶는다. 놓기 전에 대상
  폴더가 강조되고 놓을 수 없으면 이유가 보인다. 키보드·휴대폰은 일괄 막대의 **"폴더로 옮기기…"**가 확인 창(대상, 가능·불가 수와
  이유)을 연다.
- **권한 탭과 방패 단추.** VM 상세와 폴더 상세에 "권한" 탭이 있다. 네트워크·풀은 상세 화면이 없어 행의 **방패 단추**가 오른쪽
  서랍으로 같은 내용을 연다. 누구나 "여기서 내 권한"을 보고, `permission.modify`가 있으면 접근할 수 있는 사람(grant와 근거)과
  권한 더하기·빼기가 보인다. 사용자 목록을 읽지 못하는 위임자는 이미 접근하는 주체에서 고르거나 id를 넣는다.
- **설정 탭.** 권한(`/settings/permissions`, 모든 grant 표, 대상으로 루트·모든 폴더를 고르는 더하기)·역할(`/settings/roles`,
  권한 체크 표)·보안(`/settings/security`, 2단계 인증 정책)은 admin, 그룹(`/settings/groups`)은 그룹을 읽을 수 있는 사용자(=
  `cluster.admin`)에게 보인다. **2단계 인증**(`/settings/account`)은 모두에게 보이는 내 계정 화면이다(인증 앱 QR, 패스키, 복구
  코드). 사용자 표에 2FA 열과 "2단계 인증 초기화"(자기 자신 없음)가 있고 역할 고르기에 `none`이 있다.
- **로그인.** 둘째 단계 화면(코드 칸, "패스키 사용", "복구 코드"), 정책이 요구하면 강제 등록 화면. 대기 세션이 만료되거나 다른
  화면에서 `second_factor_required`를 받으면 둘째 단계로 돌아간다.
- **보이는 것.** 화면은 `/permissions/mine`으로 객체마다 할 수 있는 동작만 보인다(VM 상세 머리·⋯ 메뉴·전원 단추·콘솔·스냅샷·
  일괄 막대·팔레트 동작, 풀·네트워크 행). 폴더 사용자는 자기 폴더만 보고, 루트 읽기 권한이 필요한 탭(노드·Ceph·보안 그룹·백업
  작업·HA 그룹·HA 검사·배치·설정 › 일반·구독)은 숨는다. **+ 만들기**와 만들기 단추는 어느 폴더에서든 권한이 있으면 보이고, VM
  만들기 화면은 권한 있는 폴더를 골라 두며(`?folder=`), 노드 칸을 숨기고 머신 종류는 서버가 고른다.

## API

모든 경로는 `/api/v1` 아래다(OpenAPI 태그 `folders`·`permissions`·`auth`).

| 경로 | 뜻 |
|---|---|
| `GET`·`POST /folders`(`?kind=vm\|network\|storage`), `GET`·`PATCH`·`DELETE /folders/{id}` | 폴더. `PATCH {name?, parent?(null = 맨 위), quota?(null = 지움)}`, `If-Match` |
| `POST /folders/{id\|root}/members {items:[{kind, key}]}` | 옮기기(항목마다 `ok`·`error`) |
| `GET /folders/{id}/usage` | 하위 트리 사용량·상한·묶는 폴더 |
| `GET`·`POST /access-roles`, `GET`·`PUT`·`DELETE /access-roles/{key}` | 역할(내장은 읽기만) |
| `GET`·`POST /groups`, `GET`·`PUT`·`DELETE /groups/{key}` | 그룹(`cluster.admin`) |
| `GET /grants`(`?target=root\|folder/ID\|vm/ID…&principal=user/ID\|group/ID`), `POST /grants`, `GET`·`DELETE /grants/{id}` | grant |
| `GET /permissions/mine` | 내 권한 `{all, root, root_contents, folders, contents, objects}`(웹이 쓴다) |
| `GET /permissions/effective?user=&object=` | 권한과 근거 grant |
| `GET /permissions/who?object=` | 대상에 닿는 grant와 사용자(`permission.modify`) |
| `POST /tokens {name, scope?: {privileges, targets}}` | 범위 토큰 |
| `GET /auth/2fa`, `POST /auth/2fa/totp` → `/totp/confirm {code}`, `DELETE /auth/2fa/totp {code\|password}` | TOTP |
| `POST /auth/2fa/recovery {code\|password}` | 복구 코드 다시 만들기 |
| `POST /auth/2fa/webauthn/register` → `/register/finish`, `DELETE /auth/2fa/webauthn/{id}` | 패스키 |
| `POST /auth/login/second-factor`, `DELETE /users/{key}/two-factor`, `PATCH /settings {two_factor}` | 둘째 단계, 초기화, 정책 |

`/watch`의 종류에 `folder`·`access-role`·`grant`·`group`이 더해졌다. 오류: 404, 403 `forbidden`, 400 `folder_required`, 409
`quota_exceeded`·`folder_not_empty`·`folder_cycle`·`folder_too_deep`·`name_taken`·`role_in_use`·`group_in_use`·`cluster_upgrading`,
401 `second_factor_required`·`invalid_second_factor`, 403 `enrollment_required`. 감사 설명 예: `folder create /teamA/prod`,
`folder move vm web -> /teamA`, `grant add group:team-a operator on /teamA`, `role create team-admin`.

## 버전이 섞인 클러스터

새 기능은 두 노드 기능으로 가린다. **`folders`**는 폴더·grant·역할·그룹, 리소스의 `folder` 칸, 전역 역할 `none`, 외부 그룹 저장,
토큰 범위, 할당량이고, **`two-factor`**는 2단계 인증 요소·대기 세션·정책이다. 모든 노드가 그 기능을 알릴 때만 쓰기를 받고, 아니면
**409** `cluster_upgrading`이다. 옛 노드는 grant를 모르므로 하나라도 있으면 권한을 잘못 볼 수 있어서다. 새 것이 하나도 없는 동안
새 노드의 판단은 옛 노드와 같다. 역할 `none`으로 매핑하는 영역도 `folders` 뒤다. 2단계 인증은 **닫힌 실패**다: 어느 노드가
`two-factor`를 아직 알리지 않았어도(합류 중인 노드 등) 요소가 있는 사용자는 둘째 단계를 거치고, 그 대기 세션은 로그인한 노드의
메모리에만 둔다. 정책이 요구하는데 요소가 없는 사용자는 등록할 수 없으므로 로그인이 **409** `cluster_upgrading`이다. 2FA 쓰기와
정책 바꾸기도 **409**이고, 웹의 2단계 인증 화면은 "모든 노드를 올리기 전에는 쓸 수 없습니다"를 보인다. 받은 백업의 폴더 칸은
버린다(보낸 클러스터의 폴더 id다).

## 한계

- **이름이 종류 전체에서 하나다.** 남의 폴더에 같은 이름의 VM이 있으면 만들기가 **409** `name_taken`이라 보이지 않는 객체의 이름이
  드러난다. 이름 앞에 팀 접두사(`a-web`)를 붙인다.
- **거부 규칙이 없다.** 위 폴더에서 받은 권한을 아래에서 빼지 못한다. 뺄 것은 형제 폴더로 옮긴다.
- 외부 사용자의 그룹은 로그인할 때만 읽는다. 디렉터리에서 그룹을 빼도 다음 로그인 전까지(토큰만 쓰면 마지막 로그인의 그룹으로)
  그룹 grant가 남는다. 바로 막으려면 그 사용자를 지우거나 grant를 뺀다.
- **볼륨 늘리기의 올림.** 저장소가 요청보다 크게 올리면(LVM extent·ZFS 단위) 폴더가 그만큼 상한을 넘을 수 있다(기록은 실제 크기).
- **Raft 적용 비용.** 할당량이 걸린 폴더의 VM 쓰기는 적용할 때 모든 VM을 한 번 훑는다. VM 1만 개에서 VM 쓰기 하나의 적용이 약
  17.8 ms(할당량 없음) → 27.5 ms(할당량 폴더)였다(릴리스 빌드). 할당량이 없는 클러스터는 더하는 비용이 없다.
- `/watch`는 목록을 읽은 뒤 구독하기 전(밀리초) 사이에 옮겨진 객체를 놓칠 수 있다(웹은 다시 연결할 때 목록을 다시 읽는다).
- 웹에 토큰 범위 화면이 없다(CLI `token create --scope`나 API로 만든다). 복제·복원·가져오기의 폴더 고르기도 CLI·웹에 없고 API
  `folder`로만 한다.
- 웹 트리는 가상 스크롤이 없다(펼친 노드만 그린다 — 폴더 수백 개까지). 템플릿 탭에는 트리가 없다.
- 볼륨·ISO·이미지·가져오기·백업 복원·템플릿 배포 단추는 그 권한이 **어디에든** 있으면 보이고, 그 객체에서 되는지는 서버가 정한다
  (주인 VM·풀을 화면에서 정확히 셈하기 어려워서).
- 끌어 놓기는 확인 없이 바로 옮긴다("폴더로 옮기기…"는 확인 창을 거친다).
- 패스키는 증명(attestation)·MDS를 보지 않고, CLI에서 쓸 수 없고, IP 주소로 연 웹에서 쓸 수 없다. SMS·이메일 코드, OIDC의 `amr`
  요구는 없다. 패스키만 있는 LDAP 사용자는 패스키를 빼거나 복구 코드를 다시 만들 때 복구 코드로 확인한다(다 쓰면 관리자 초기화).
- TOTP 비밀은 Raft에 평문으로 있다(보호는 0700 데이터 디렉터리, 따로 둔 암호화 키가 없다). 정책을 켜기 전 토큰을 따로 표시하지
  않는다. 둘째 단계를 틀린 횟수는 사용자마다 세므로 비밀번호를 아는 공격자는 그 사용자의 둘째 단계를 되풀이해 잠시(한 번에
  15분까지) 막을 수 있다. 그때는 비밀번호를 바꾸고 관리자 초기화(`user 2fa-reset`)로 센 것을 지운다.
- 합류에 반쯤 실패해 `two-factor` 기능을 알리지 않은 노드 기록이 있으면 대기 세션이 받은 노드의 메모리에만 있어, 고정(sticky)이
  아닌 부하 분산기 뒤에서는 둘째 단계가 다른 노드로 가 **401**이 된다. 정책이 요구하는데 요소가 없는 사용자는 로그인이 **409**
  `cluster_upgrading`이다. `node list`로 그 노드를 찾아 고치거나 뺀다.
- 기다리는 작업(복제·배포·복원·가져오기)은 요청할 때 권한을 본다. 그 뒤 grant를 빼도 이미 줄에 선 작업은 그대로 돈다(vSphere와
  같다, 레이블 `requested-by`로 누가 했는지는 남는다).
- 폴더 사용자는 볼 수 있는 풀의 ISO만 CD에 넣는다. 루트 ISO 풀을 함께 쓰게 하려면 그 풀에 `viewer` grant를 준다.
- 볼륨·백업·보안 그룹·노드 폴더, 태그로 주는 권한, 시간 제한 grant, 폴더별 감사 로그 읽기·Prometheus, 실행 중만 세는 할당량·풀별
  디스크 할당량은 없다.
