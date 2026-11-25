# 선언형 운영

YAML 매니페스트로 클러스터를 적용·비교·내보내기, 적용 묶음과 가지치기, 설정 이력과 되돌리기, 드리프트, Terraform/OpenTofu
공급자(계획 73, [설계 문서](../superpowers/specs/2026-11-25-declarative-design.md)). API 인증서를 ACME로 받는 것은
[클러스터 운영](cluster.md#api-인증서acme)에, 웹 화면은 [웹 화면 둘러보기](web-ui.md#설정-기록선언-묶음인증서-화면)에 있다.

- [매니페스트](#매니페스트)
- [적용·비교·계획·내보내기](#적용비교계획내보내기)
- [적용 묶음과 가지치기](#적용-묶음과-가지치기)
- [권한](#권한)
- [설정 기록](#설정-기록)
- [되돌리기](#되돌리기)
- [드리프트](#드리프트)
- [Terraform/OpenTofu 공급자](#terraformopentofu-공급자)
- [한계](#한계)

서버가 계획한다. `orbivisorctl`·Terraform·웹은 문서를 보내고 서버가 지금 상태와 견준 계획(만들기·바꾸기·그대로·지우기·막힘)을
돌려준다. 쓰기는 종류마다 지금 API 처리기를 그대로 거치므로(검증·권한·할당량·감사·Task) 폼이나 `vm create`로 만든 것과 같다.
모든 노드가 기능 `declarative`(설정 이력은 `config-history`)를 알아야 쓸 수 있다(아니면 409 `cluster_upgrading`).

## 매니페스트

```yaml
# examples/declarative/web/80-vms.yaml
apiVersion: orbivisor.io/v1
kind: Vm
metadata: { name: web01, folder: /demo/web, labels: { tier: web } }
state: running
spec:
  cpu: { cores: 1 }
  memory_mib: 256Mi
  disks: [{ volume: web01-root, bus: virtio-blk }]
  nics: [{ network: web-net, model: virtio }]
```

문서는 `apiVersion: orbivisor.io/v1`, `kind`, `metadata`(`name`, `folder`(폴더 경로), `labels`, `annotations`), `spec`,
VM만 `state`(`running`·`stopped`)다. 한 파일에 여러 문서(`---`)를 두고, 파일은 `*.yaml`·`*.yml`·`*.json`이다.
저장소의 `examples/declarative/web/`이 폴더 `/demo/web`, 브리지 `vmbr0`에 잇는 네트워크 `web-net`, 볼륨 둘, 켜진 VM 둘이다
(풀 이름 `local`과 브리지 이름은 클러스터에 맞게 고친다).

- **spec은 저장 꼴 그대로다.** 칸 이름은 API 만들기 본문의 spec과 같고(`memory_mib`·`size_bytes` — `memory: 8Gi`가 아니다), id
  대신 이름으로 가리킨다(디스크의 `volume`, NIC의 `network`·`security_groups`, 풀의 노드, 유동 IP의 VM 등). 폴더는 `spec.folder`가
  아니라 `metadata.folder`에 경로로 둔다. 칸 이름이 틀리면 400 `manifest_invalid`(문서 번호·칸), 같은 리소스가 두 번이면
  `manifest_duplicate`, 참조 고리는 `manifest_cycle`이다. 한 번에 문서 1 000개·8 MiB까지.
- **폴더 옮기기는 따로.** 있는 리소스의 `metadata.folder`를 바꾼 선언은 `blocked: immutable`이다 — `orbivisorctl folder members
  move`로 옮긴 뒤(옮기기 권한을 따로 보는 지금 규칙) 파일을 맞춘다.
- **크기 문자열.** `_bytes`로 끝나는 칸은 `40Gi`·`512Mi`·`2G`(정수와 `K`·`Ki`·`M`·`Mi`·`G`·`Gi`·`T`·`Ti`)를, `_mib` 칸은 단위 없는
  MiB 수나 MiB로 나누어떨어지는 크기 문자열을 받는다. CLI가 보내기 전에 숫자로 바꾼다(서버는 숫자만 받는다, `1.5Gi`는 거절).
- **비밀.** 비밀 칸(cloud-init 비밀번호, 백업 저장소 키·S3 비밀, 원격 토큰, realm 비밀, 알림 비밀, ACME EAB·DNS 공급자 비밀)과
  webhook URL·헤더 값에 `{secretFrom: {env: WEB_PW}}`·`{secretFrom: {file: pw.txt}}`(상대 경로는 매니페스트 파일 옆, 끝 줄바꿈은 뺌)를
  쓰면 CLI가 평문으로 풀어 보낸다. 다른 칸(설명·레이블·`user_data`)의 `secretFrom`은 오류다 — 그 칸은 평문으로 저장되어 보인다. 내보낸 파일의 비밀은 `{secretRef: …}`이고, 다시 적용하면 서버가 저장한 값을 그대로 둔다(파일에 비밀이 없어도
  왕복이 같다). 저장한 값이 없는데 참조만 오면 막힘 `secret_missing`.
- **순서는 서버가 정한다.** 참조를 따라 같은 계획 안에서 만들 것까지 풀고, 아래 표의 차례로 적용한다(지우기는 거꾸로).

| 종류(`kind`) | 내보내기 파일 | 메모 |
|---|---|---|
| `Folder` | `10-folders.yaml` | 열쇠는 `트리:경로`(`vm:/demo/web`), spec `{kind, quota?}`. 바꿀 수 있는 것은 할당량뿐 — 이름·부모를 바꾼 문서는 다른 폴더다(만들기, 옛 것은 가지치기). 옮기려면 `orbivisorctl folder move` |
| `StoragePool` | `20-storage-pools.yaml` | 노드는 이름 |
| `Network` | `30-networks.yaml` | simple·SDN 서브넷·DHCP 칸 |
| `SecurityGroup` | `40-security-groups.yaml` | 규칙 목록 통째 |
| `Router` | `50-routers.yaml` | 지우기는 인터페이스를 먼저 뗀다 |
| `HaGroup` | `60-ha-groups.yaml` | VM의 `ha.group`이 가리킨다 |
| `Volume` | `70-volumes.yaml` | 풀·형식 바꾸기와 줄이기는 막힘, 늘리기는 크기 늘리기 길 |
| `Vm` | `80-vms.yaml` | `state`, 내보내기에 `node`(스케줄러 몫)가 없다, 서버가 만든 MAC은 내보낸다 |
| `FloatingIp`·`LoadBalancer`·`PlacementRule` | `90`·`100`·`110` | 부하 분산기 `monitors`는 매니페스트 밖 |
| `HaCheck` | `120-ha-checks.yaml` | 클러스터가 만든 `auto-` 검사는 다루지 않는다 |
| `BackupStore`·`Remote`·`BackupJob` | `130`·`140`·`150` | 백업 저장소는 만들 때 `encryption.key`를 쓰고 init Task를 기다린다. 뒤에 바꿀 수 있는 것은 gc·verify·max_streams·S3 자격·`ca_pem`뿐. 원격 `ca_pem`은 매니페스트 밖. 백업 작업 `vms`는 통째 |
| `Realm`·`AccessRole`·`Group`·`Grant` | `160`~`190` | 그룹 멤버는 사용자 이름. grant 열쇠는 아래 |
| `NotificationTarget`·`AlertRule` | `200`·`210` | |
| `Settings` | `220-settings.yaml` | 하나뿐(`metadata.name: cluster`), 지우지 않는다(가지치기도 건너뜀). 구독 키·이미지 카탈로그는 매니페스트 밖 |
| `AcmeAccount`·`AcmeDns`·`Certificate` | `230`~`250` | 계정 키는 서버가 만든다(매니페스트·diff에 없음), 계정은 `email`·`ca_bundle`만 바뀐다. 계정 만들기는 CA에 곧 등록한다 |

사용자·노드·Task·스냅샷·백업·가져오기·이전·내보내기·SDN 설치·Ceph·토큰·2단계 인증은 매니페스트 종류가 아니다.

**grant 열쇠.** grant는 바꾸는 길이 없어(주거나 뺄 뿐) 이름이 아니라 **내용 전체가 열쇠**다: `user:alice operator vm:/prod`
(내려가지 않으면 끝에 ` no-propagate`). 대상은 `/`(루트), `vm:/prod`(트리:경로 — 폴더 문서 열쇠와 같다), `vm/web01`(종류/이름 —
`vm`·`network`·`storage-pool`·`router`·`floating-ip`·`load-balancer`…)이다. `metadata.name`은 사람이 붙이는 이름이고 열쇠에
들지 않는다(내보내기는 `grant-` + 열쇠 SHA-256 앞 12자). 내용을 바꾼 문서는 다른 grant다 — 새로 주고, 적용 묶음의 가지치기가
옛 것을 뺀다.

```yaml
apiVersion: orbivisor.io/v1
kind: Grant
metadata: { name: web-team }
spec: { group: web-team, role: operator, target: "vm:/prod/web" }
```

## 적용·비교·계획·내보내기

```bash
orbivisorctl apply -f examples/declarative/web/ --set web   # 계획을 보이고 묻고 그 계획을 적용한다
# + Folder demo
# + Folder web (/demo)
# + Network web-net
# + Volume web01-root
# + Vm web01 (/demo/web)
# ...
# Plan: 7 to create, 0 to update, 0 to delete, 0 unchanged.
# Apply these changes? [y/N] y
# Applied: 7 created, 0 updated, 0 deleted.
orbivisorctl apply -f web/ --set web --reason "INC-1234 more memory" --yes   # 묻지 않음(단말이 아니면 --yes가 있어야 한다)
orbivisorctl diff -f web/ --set web; echo $?                 # 0 같음, 1 다름, 2 오류·막힘(kubectl diff와 같은 약속 — CI에서)
orbivisorctl plan -f web/ --set web
# ~ Vm web01 (/demo/web)
#     ~ memory_mib: 256 -> 512
#     note: the VM is running; this hardware change applies by hotplug or at its next restart
# Plan: 0 to create, 1 to update, 0 to delete, 6 unchanged.
orbivisorctl plan -f web/ --set web --out plan.json          # 계획 id를 저장한다(0600, 문서·비밀 없음 — 계획은 서버에 30분)
orbivisorctl apply --plan plan.json --yes                    # 저장한 계획 그대로. 그사이 바뀌었으면 거절
orbivisorctl export --dir ./cluster                          # 종류마다 파일(10-folders.yaml … 적용 차례 번호)
orbivisorctl export --kind vm --folder /demo/web --minimal   # 표준 출력에 YAML, --set NAME은 그 묶음이 기억하는 것만
```

- **파일 인자.** `-f PATH`(여러 번, `-`는 표준 입력), 디렉터리는 이름 차례로 읽고 `-R`이면 아래로 내려간다. 세 명령 모두
  `--set`·`--prune`·`--prune-data`·`--force-conflicts`를 받고, `apply`는 `--plan FILE`·`--reason`·`--yes`·`--no-wait`, `plan`은
  `--out`을 더 받는다. `-o json`은 계획 JSON을 그대로 쓴다.
- **선언한 칸만 견준다.** 선언하지 않은 칸은 서버 값 그대로다(웹에서 고친 칸을 apply가 덮지 않는다). 적용 묶음(`--set`)이 있으면
  앞 선언에 있었는데 이번 파일에서 뺀 칸은 기본값으로 되돌린다(세 갈래 비교 — kubectl의 last-applied와 같은 뜻). 목록(디스크·NIC·규칙)은
  통째로 견준다. 계획의 비밀은 `(changed)`·`(secret)`로만 보인다.
- **켜진 VM의 하드웨어 바꿈.** 핫플러그할 수 있는 것은 곧, 아니면 다음 재시작 때 적용된다 — 계획에 `note: … next restart`(`restart`)가
  붙을 뿐 apply가 VM을 껐다 켜지 않는다. `state`가 다르면 켜거나 끈다(켜짐을 기다리지 않는다). 지우는 VM은 먼저 끈다(600초).
- **막힘.** 권한 없음(`forbidden`), 없는 참조(`missing_reference`), 바꿀 수 없는 칸(`immutable` — 손으로 할 일 문구), 가지치기 보호
  (`protected`), 다른 관리자(`managed_elsewhere`), 기능 없음(`cluster_upgrading`) 등은 그 단계를 `!`로 보인다. 막힌 단계가 하나라도
  있으면 `apply`는 **하나도 하지 않는다**(서버는 409 `plan_blocked`, `diff`는 끝 코드 2).
- **저장한 계획.** `POST /apply?dry_run`이 돌려준 계획 id는 받은 노드 메모리에 30분(노드마다 256개) 있다. 같은 사용자의 같은
  자격(세션 또는 같은 API 토큰)만 적용하고(단계는 계획을 세운 자격의 권한으로 돈다), 다른 자격·다른 노드·30분 지남·노드 재시작이면
  404 `plan_not_found`, 문서가 다르거나 계획이 본 리소스의 설정(이름·레이블·spec — 상태만 바뀐 것은 아니다)이 바뀌었으면 412
  `plan_stale`(다시 계획한다 — Terraform 저장 계획과 같은 약속)이다. `apply --plan`은 같은 `--server`일 때만 보낸다. 로드밸런서 뒤에서는
  요청이 다른 노드로 갈 수 있으니 `--plan` 대신 바로 `apply`를 쓴다(그것도 보인 계획 id를 그대로 적용한다).
- **원자 적용이 아니다.** 적용 Task `apply`는 단계를 차례로 하고(단계마다 권한과 계획 때 설정을 다시 보고 지금 revision으로
  `If-Match`, 그 쓰기가 만든 Task를 기다림) **첫 실패에서 멈춘다.** 끝난 단계는 되돌리지 않는다. 적용 묶음 status는 단계마다 쓴다(도중에
  노드가 다시 시작해도 만든 객체를 묶음이 안다). 같은 파일을 다시 적용하면 남은 것만 한다(몇 번 해도 같다).
  Task 진행에 "n/m, 실패 단계, 오류"가 보이고 묶음 status의 `last_error`에 남는다. 같은 적용에서 만든 풀에 볼륨을 만들면 노드가
  풀을 알리기 전이라 `pool_unavailable`로 멈출 수 있다 — 다시 적용한다.
- **내보내기.** `GET /manifests`가 읽을 수 있는 것만 적용 차례로, id 대신 이름·폴더 경로, 비밀은 `secretRef`, status 없이 준다.
  내보낸 것을 그대로 같은 클러스터에 적용하면 모든 단계가 `unchanged`다(왕복). `--minimal`은 `null`·`false`·빈 칸과 서버가 만든
  MAC·주소·유동 IP 주소를 뺀다(다른 클러스터에 새 MAC으로 만들 때, 사람이 읽을 파일). `--kind`(`vm`·`Volume`…, 여러 번)·`--folder
  /p`(그 아래 모두)·`--set NAME`으로 거른다. `export`에 하위 명령을 주면 VM 내보내기(OVA, 계획 72)다.
- **API.** `POST /apply[?dry_run=true] {documents, apply_set?, prune?, prune_data?, force_conflicts?, plan?}` → 200 계획 또는 202
  `{plan, task}`, `GET /manifests?kinds=&folder=&apply_set=&minimal=`, `GET|DELETE /manifests/{kind}/{key}`(하나 — Terraform이 쓴다).
  서버는 JSON만 읽는다(YAML은 CLI가 바꾼다). 매니페스트 스키마는 OpenAPI의 `Manifest{Kind}`다.

## 적용 묶음과 가지치기

적용 묶음(apply set, `--set NAME`)은 그 이름으로 마지막에 선언한 것을 기억한다(리소스 `apply-set`의 status — 객체 1 000개·선언
4 MiB까지, 넘으면 400 `apply_set_too_large` — 묶음을 나눈다). 처음 쓰면 만들어지고, 묶음에 든 리소스에는 레이블
`orbivisor.io/apply-set={이름}`이 붙는다.

```bash
orbivisorctl set list                       # NAME OBJECTS APPLIED BY ERROR
orbivisorctl set get web                    # 묶음 JSON(선언·객체·드리프트)
orbivisorctl apply -f web/ --set web --prune                # 묶음이 기억하는데 파일에 없는 것을 지운다
orbivisorctl apply -f web/ --set web --prune --prune-data   # VM·볼륨·백업 저장소도 지운다
orbivisorctl set adopt web [--object Vm/web01]              # 드리프트를 받아들인다(아래)
orbivisorctl set delete web                 # 묶음만 지운다 — 리소스는 그대로, 레이블만 뗀다
```

- **가지치기**(`--prune`, `--set`이 있어야 한다)는 묶음의 객체 목록에 있고 묶음 레이블이 남아 있는데 파일에 없는 것을 거꾸로 지운다.
  데이터가 있는 VM·볼륨·백업 저장소는 `--prune-data` 없이는 막힘(`protected`)이다. 주석 `orbivisor.io/prune: "false"`인 리소스는
  건너뛴다. 묶음 status는 성공·그대로인 단계까지만 갱신한다(실패한 단계의 선언은 다음 적용이 다시 본다).
- **관리자 충돌.** 관리자는 객체 통째 단위다. 레이블 `orbivisor.io/managed-by=terraform`인 객체를 다른 손님(묶음 적용·묶음 없는 적용·
  되돌리기)이 바꾸려 하거나, 어느 묶음의 객체를 Terraform·다른 묶음이 바꾸려 하면 그 단계가 `blocked: managed_elsewhere`다.
  `--force-conflicts`(되돌리기·Terraform 공급자에도 있다)는 넘겨받고 다른 관리자의 레이블을 뗀다. 묶음 없는 적용이 묶음 객체를
  바꾸는 것과 그대로인 단계는 충돌이 아니다.

## 권한

새 권한은 없다. 적용·되돌리기의 각 단계는 **그 종류·대상의 지금 권한**으로 한다(VM이면 그 폴더의 `vm.create`·`vm.config`·…,
grant면 `permission.modify`와 위임 규칙, ACME 셋은 루트 `cluster.admin`).

- `POST /apply`(계획·적용)와 되돌리기는 **어딘가의** `system.read`가 있어야 한다(폴더 사용자도 자기 폴더에 적용한다). 바꿀 것이 있는
  단계만 쓰기 권한을 보고, 그대로인 단계는 읽기만 본다. 권한이 없으면 그 단계는 `blocked: forbidden`이고 그 단계의 나머지 검사를
  하지 않는다. 볼 수 없는 폴더·참조는 없는 것과 같은 문구다(`folder "/x" not found`·`Network "x" not found`).
- 적용 묶음 보기(`set list|get`, `GET /apply-sets*`)는 루트 `system.read`, 묶음 지우기·`adopt`는 그 객체 모두의 바꾸기 권한이다
  (`adopt`에 객체를 주지 않으면 볼 수 있고 바꿀 수 있는 드리프트만 받아들인다).
- 내보내기와 설정 기록은 지금 읽을 수 있는 리소스만 보인다(지운 리소스는 마지막 폴더 기준). 설정 기록의 줄은 그때의 폴더도 읽을 수
  있어야 보인다(읽을 수 없는 폴더에서 옮겨 온 객체의 그때 줄은 숨는다). 비밀은 기록·감사·계획 어디에도 평문으로 남지 않는다 —
  webhook URL은 `https://host/…`, 헤더 값은 `$secret` 표시, 이미지 카탈로그 URL은 질의·사용자 정보를 가린 글로 남는다(이 셋은 숨긴
  부분만 바뀐 것을 기록이 보이지 않는다)([접근과 보안](access.md#선언형-적용과-설정-기록의-권한)).

## 설정 기록

모든 노드가 기능 `config-history`를 알게 된 뒤부터 설정 리소스의 spec·레이블 바뀜이 Raft 상태 머신에 **누가(사용자·토큰)·언제·어디서
(출처)·왜(사유)** 와 함께 남는다(업그레이드를 마치기 전의 바뀜은 기록이 없다). 같은 값 쓰기와 status 바뀜은 줄을 만들지 않는다.
기록 종류는 매니페스트 스물다섯에 사용자·노드 펜스·VMware 원본·적용 묶음을 더한 스물아홉이다(Task·세션·노드·토큰·2단계 인증·
스냅샷·백업 같은 작업 기록과 SDN·Ceph 설치 상태는 빼).

```bash
orbivisorctl history                                  # 클러스터 전체 타임라인(새 것부터 100줄)
# REVISION  TIME                  USER   SOURCE               OP      KIND  NAME   CHANGED
# 81234     2026-11-25T15:02:11Z  admin  web                  update  vm    web01  cpu.cores
# 81190     2026-11-25T14:40:03Z  admin  apply                update  vm    web01  memory_mib
orbivisorctl history vm web01                         # 리소스 하나(지운 것도 id나 마지막 이름으로)
orbivisorctl history --user alice --source terraform --since 24h --folder /prod --set web --limit 500
orbivisorctl history show vm web01 81234              # 그 줄의 전체 spec과 바로 앞 줄 비교
orbivisorctl history show vm web01 81234 --compare live   # 지금 리소스와(또는 --compare 81190)
```

- **출처**(`--source`): `web`(로그인 세션이고 `Orbivisor-Client` 머리가 없음), `cli`, `api`, `apply`(선언형 적용), `terraform`,
  `task`(Task가 쓴 spec — 사용자는 Task를 만든 사람), `system`(리더 루프·reconciler). 손님은 머리 `Orbivisor-Client: cli/0.1.0`
  (64자, 영숫자·`/._-`만 — 앞머리 `cli`·`terraform`·`tofu`·`web`, 그 밖은 `api`)로, 사유는 `Orbivisor-Change-Reason`(200자, 제어
  문자를 뺌, 한글 됨)으로 알린다. CLI는 `apply`·`restore`의 `--reason`이 이 머리다.
- **감사 줄과 이음.** 줄마다 `request`가 그 요청의 감사 로그 id다(`orbivisorctl audit -o json`의 `id`). 되돌린 줄은 `restored_from`
  (되돌린 revision)을 단다 — 표의 `(restore rN)`. 되돌리기는 적용 Task가 쓰지만 출처는 요청한 쪽 그대로다(웹에서 되돌리면
  `web`, CLI면 `cli`).
- **비밀.** 비밀 칸은 기록에 평문·해시·지문 없이 `{"$secret": "set|same|changed|cleared"}`로만 남는다.
- **보존.** 리소스마다 가장 새 100줄(10~1000), 365일(7~3650), 전체 64 MiB(압축 뒤, 8~512). 쓰기마다 Raft 적용 안에서
  결정적으로 다듬는다. 지운 리소스의 `Delete` 줄은 가장 새 줄이라 나이·전체 한도까지 남는다. 기록은 Raft 스냅샷에 실리므로 전체
  한도를 크게 잡으면 새 노드 합류·스냅샷 보내기가 그만큼 느려진다. `orbivisorctl settings set`에는 아직 키가 없어 설정 매니페스트나
  `PATCH /api/v1/settings {"history": {...}}`로 바꾼다:

  ```yaml
  apiVersion: orbivisor.io/v1
  kind: Settings
  metadata: { name: cluster }
  spec:
    history: { max_per_resource: 200, max_age_days: 90, max_total_mib: 128 }
  ```

- **관찰 노드.** 기록은 Raft 구성원에만 있다. 관찰 노드(계획 48)의 API는 구성원(리더 먼저)에게 노드 RPC로 읽고, 닿지 않으면 503
  `history_unavailable`이다.
- **API.** `GET /history?kind=&key=&folder=&user=&source=&apply_set=&since=&until=&before=&limit=`(최대 1 000, 쪽 열쇠 `next_before`, 줄마다
  `changed` 경로 20개), `GET /history/{kind}/{key}`, `GET /history/{kind}/{key}/{revision}?compare=prev|live|REV`.

## 되돌리기

되돌리기는 모두 **선언형 계획**이다: 기록의 spec을 매니페스트로 바꿔 서버가 계획을 세우고, CLI·웹이 그 계획을 보인 뒤 확인하면 그
계획 id를 적용한다(그사이 바뀌었으면 412 `plan_stale`). 의존 차례·권한·미리 확인·막힘은 `apply`와 같다. 되돌릴 수 있는 것은 매니페스트
스물다섯 종류다.

```bash
orbivisorctl restore vm web01 --revision 81190 --dry-run   # 계획만
orbivisorctl restore vm web01 --revision 81190 --reason "undo INC-1234" --yes
orbivisorctl restore vm web02 --revision 81300             # 지운 VM: 같은 id로 다시 만든다
orbivisorctl restore vm web02 --revision 81300 --name web02-old   # 이름이 그새 쓰이면 새 이름(새 id)으로
orbivisorctl restore --at 2026-11-24T17:00:00Z --folder /prod/web   # 한 시점으로(시각·`2h` 전·revision)
orbivisorctl restore --at 81000 --set web --kind vm --key Network/web-net --delete-created --allow-data-loss
```

- **하나 되돌리기**(`KIND NAME --revision REV`): 그 리소스만 그 줄의 spec으로. 지금 객체의 revision으로 `If-Match`한다.
- **지운 리소스 살리기.** 마지막 줄이 지우기인 리소스는 지우기 직전 spec으로 **같은 id**로 다시 만든다(다른 리소스가 id로 가리키던
  참조가 다시 산다). 폴더·grant(이름이 곧 id)·원격·백업 저장소는 새 id다. 이름이 쓰이면 409 `name_taken` — `--name NEW`(새 id).
  **데이터 종류는 막는다**(`data_lost`): 볼륨(디스크 내용이 없다 — 백업에서 복원한다), 백업 저장소, 디스크 볼륨이 하나라도 없는 VM.
- **한 시점으로**(`--at`): 범위(`--folder /p`(그 아래 모두)·`--set`·`--kind`(여러 번)·`--key KIND/NAME`(여러 번) 가운데 하나 이상 —
  클러스터 전체는 400) 안 리소스마다 그 시점의 spec으로 문서 묶음을 만든다. 그 뒤 바뀐 것은 되돌리고, 그 뒤 지운 것은 살리고
  (`--no-recreate`면 살리지 않음), 그 뒤 만든 것은 `--delete-created`일 때만 지운다(VM·볼륨·백업 저장소는 `--allow-data-loss`도).
  보존 한도로 그 시점을 모르는 리소스는 `blocked: history_expired`다.
- **되돌리지 않는 것.** 비밀(기록에 평문이 없다 — 지금 비밀을 그대로 두고 경고 `secret_not_restored`, 지운 리소스면 그 칸을 빼고
  경고), 이름(`name_not_restored`), 전원(`state`)은 되돌리지 않는다. 바꿀 수 없는 칸이 다르면(예: 볼륨 풀) 막힘이다.
- 모든 되돌리기에 `--dry-run`·`--yes`·`--reason`·`--force-conflicts`·`--no-wait`가 있다. API는 `POST /history/{kind}/{key}/restore
  {revision, name?, dry_run?, reason?}`, `POST /history/restore {at: {revision}|{time}, scope, recreate_deleted, delete_created,
  allow_data_loss, dry_run?}`이고 답은 `POST /apply`와 같다.

## 드리프트

리더의 `DriftChecker`가 적용 묶음마다 선언과 실제를 계획과 같은 비교 함수로 견준다 — 10초마다 바뀐 것이 있으면(바뀜을 몰아 받음),
아니어도 5분마다. 결과가 바뀔 때만 묶음 status `drift`를 쓴다. 상태는 `modified`(다른 칸 경로), `missing`(밖에서 지움), `unmanaged`
(묶음 레이블이 떨어짐)이고, 칸마다 `by`가 적용 뒤 그 칸을 처음 바꾼 이력 줄(누가·언제·출처·사유)이다.

```bash
orbivisorctl drift            # 모든 묶음. 끝 코드 0 없음, 1 있음
# SET  KIND  NAME   STATE     PATHS      CHANGED BY
# web  Vm    web01  modified  cpu.cores  admin via web at 2026-11-25T15:02:11Z
# Apply the files again to undo, or `orbivisorctl set adopt SET` to accept the live values.
orbivisorctl drift web
orbivisorctl apply -f web/ --set web --yes        # 선언대로 되돌린다
orbivisorctl set adopt web --object Vm/web01      # 실제를 선언으로 받아들인다(묶음의 선언만 바뀜)
orbivisorctl export --set web --dir web/          # 그 뒤 git의 파일을 맞춘다
```

- 경보 조건 `apply_set_drift`(리더 평가, 값은 다른 객체 수)가 있다. `orbivisorctl alert rule create`와 웹 규칙 폼에는 아직 없어 API
  (`POST /alert-rules {"name":"drift","spec":{"condition":{"kind":"apply_set_drift"},"targets":["ops-mail"]}}`)나 `AlertRule` 매니페스트로
  만든다.
- 묶음 없는 리소스는 드리프트가 없다(설정 기록은 있다). Terraform이 관리하는 객체의 드리프트는 `terraform plan`이 본다.

## Terraform/OpenTofu 공급자

`terraform/`의 Go 공급자 `terraform-provider-orbivisor`(terraform-plugin-framework)가 같은 매니페스트 API를 쓴다. 리소스 하나의
만들기·바꾸기는 문서 하나짜리 `POST /apply`(`manager: terraform`)이고, `terraform plan`이 서버 미리 확인을 불러 할당량·용량·권한·
참조·MAC/IP 오류를 **plan에서** 보인다. CI가 terraform 1.17.2·OpenTofu 1.13.9로 수용 시험을 돌린다. 쓰기 전용 칸 때문에 Terraform
1.11 이상이 필요하다.

### 설치(레지스트리 게시 전)

아직 레지스트리에 올리지 않았다. 직접 빌드해 `dev_overrides`로 쓴다(Go 1.26):

```bash
cd terraform && go build -o ~/.local/bin/terraform-provider-orbivisor .
cat > ~/.terraformrc <<'EOF'     # OpenTofu는 ~/.tofurc
provider_installation {
  dev_overrides { "gaiger777/orbivisor" = "/home/me/.local/bin" }
  direct {}
}
EOF
```

`dev_overrides`를 쓰면 `terraform init` 없이 `plan`·`apply`가 그 실행 파일을 쓴다(경고가 한 줄 나온다). 시험해 볼 서버는
`cargo run -p opld --features devserver --bin opl-devserver`(가짜 하이퍼바이저·메모리 스토리지 노드 하나, 연결 정보 JSON 한 줄 —
[개발](development.md))이고, 예는 `terraform/examples/web/main.tf`, 리소스 문서는 `terraform/docs/`다.

```hcl
terraform {
  required_providers {
    orbivisor = { source = "gaiger777/orbivisor" }
  }
}

provider "orbivisor" {
  endpoint       = "https://node1.example.net:8006"   # 또는 OPL_SERVER
  api_token      = var.orbivisor_token                # 또는 OPL_TOKEN (orbivisorctl token create)
  ca_fingerprint = "sha256:3f1c…"                     # 또는 ca_pem = file("cluster-ca.pem") / OPL_CA_PEM / OPL_CA_FINGERPRINT
}
```

- **공급자 칸.** `endpoint`(`OPL_SERVER`), `api_token`(`OPL_TOKEN`), `ca_pem`(`OPL_CA_PEM` — 클러스터 CA, 비우면 시스템 루트: ACME
  인증서일 때), `ca_fingerprint`(`OPL_CA_FINGERPRINT` — `ca_pem` 없이 서버의 CA를 받아 이 지문일 때만 믿는다), `timeout`(적용·지우기
  하나의 Task를 기다리는 한도, Go 기간, 기본 `20m`), `force_conflicts`(적용 묶음이 관리하는 객체를 넘겨받음 — 아니면
  `managed_elsewhere` 오류). 머리 `Orbivisor-Client: terraform/{판}`이라 설정 기록의 출처가 `terraform`이다.
- **리소스 열다섯.** `orbivisor_folder`, `orbivisor_network`, `orbivisor_router`, `orbivisor_floating_ip`, `orbivisor_security_group`,
  `orbivisor_load_balancer`, `orbivisor_volume`, `orbivisor_vm`(`state` 칸 포함), `orbivisor_placement_rule`, `orbivisor_ha_group`,
  `orbivisor_backup_job`, `orbivisor_backup_store`, `orbivisor_grant`, `orbivisor_access_role`, `orbivisor_certificate`.
- **데이터 소스 여섯.** `orbivisor_nodes`(목록 하나), `orbivisor_storage_pool`, `orbivisor_image`(카탈로그 id + `arch`, 받은 볼륨),
  `orbivisor_template`(템플릿이 아니면 오류), `orbivisor_network`, `orbivisor_folder`(경로 + 트리 → id·열쇠·할당량).

### 칸 모양

```hcl
resource "orbivisor_vm" "web01" {
  name       = "web01"
  folder     = "/prod/web"
  state      = "running"
  cpu        = { cores = 4 }
  memory_mib = 8192
  disks      = [{ volume = orbivisor_volume.web01_root.name, bus = "virtio-blk" }]
  nics       = [{ network = orbivisor_network.app.name, model = "virtio" }]
  cloud_init = { user = "rocky", ssh_keys = [file("~/.ssh/id_ed25519.pub")] }

  cloud_init_password_wo         = var.web_password   # 쓰기 전용: 상태에 남지 않는다
  cloud_init_password_wo_version = 1                  # 올리면 다시 보낸다
}
```

- 맨 위 spec 칸만 타입이 있는 속성(문자열·정수·참거짓·문자열 목록)이고, 안이 객체·목록인 칸(`cpu`·`disks`·`nics`·`cloud_init`…)은
  **Dynamic** 속성이라 **서버 JSON 꼴 그대로** 쓴다(칸 이름·값은 plan 때 서버가 본다 — 틀리면 `manifest_invalid`). 칸 이름은
  매니페스트 spec과 같다.
- 설정에 없는 칸은 서버 값 그대로다(상태에도 없다). 설정에서 **뺀** 칸·안쪽 칸은 서버 기본값으로 되돌린다. 밖에서 바꾼 선언 칸은 다음
  plan이 다름으로 보인다(서버가 채운 MAC·기본값은 다름이 아니다).
- **레이블은 Terraform이 맡는다.** 설정에 없는 레이블은 지운다(서버 레이블 `orbivisor.io/…`만 남긴다). 공급자가 만든 객체에는
  `orbivisor.io/managed-by=terraform`이 붙어 웹 머리에 "Terraform이 관리" 칩이 보이고, `orbivisorctl apply`·되돌리기가 바꾸려 하면
  `managed_elsewhere`로 막힌다.
- **id와 가져오기.** 리소스 id는 UUID가 아니라 매니페스트 열쇠다: 이름, 폴더는 `트리:경로`, grant는 내용. 가져오기는 열쇠나 UUID다.
  가져오면 서버의 칸 전부(빈 값·서버 MAC·유동 IP 주소·비밀 참조를 뺀 — `--minimal`과 같은 규칙)를 상태로 받는다.

  ```bash
  terraform import orbivisor_vm.web01 web01
  terraform import orbivisor_folder.web vm:/prod/web
  terraform import orbivisor_grant.web_team "group:web-team operator vm:/prod/web"
  ```

- **쓰기 전용 비밀.** `cloud_init_password_wo`(VM), `encryption_key_wo`·`s3_secret_access_key_wo`(백업 저장소)와 각 `…_version`.
  만들 때와 판 숫자를 올릴 때만 보내고 아니면 서버가 저장한 값을 둔다. Dynamic 칸 안에 평문 비밀을 쓰면 설정 검사 오류다(상태에
  남으므로).
- **plan 때 확인.** 바뀐 리소스마다 `POST /apply?dry_run`(문서 하나)을 부른다. 막힘은 진단 오류(맨 위 칸이면 그 속성에), 경고와 켜진
  VM의 `restart`는 경고다. 같은 실행에서 만들 객체를 이름으로 가리키면 서버에는 아직 없으므로 `missing_reference`는 **plan에서
  경고**로 내리고 apply 때 서버가 다시 본다. 값이 아직 unknown이면 확인을 건너뛴다.
- **바꿔 만들기.** 이름·폴더와, 데이터가 없는 종류의 바꿀 수 없는 칸(폴더 `kind`, 네트워크 `vlan`, 유동 IP·부하 분산기 `network`,
  grant 전부)은 `RequiresReplace`다. **VM·볼륨·백업 저장소는 바꿀 수 없는 칸이 달라도 바꿔 만들지 않는다** — 서버의
  `blocked: immutable`이 plan 오류로 손으로 할 일을 알린다(데이터를 지우는 plan이 슬쩍 생기지 않게).
- **지우기.** `DELETE /manifests/{kind}/{key}`로 지금 처리기의 지우기 규칙을 따른다(VM은 끄고 지움, 붙은 볼륨은 409). 템플릿 VM은
  공급자가 먼저 템플릿을 되돌리는 Task를 기다린 뒤 지운다. 밖에서 지운 객체는 Read가 상태에서 빼고 다음 apply가 다시 만든다.
- **OpenTofu.** 같은 실행 파일·같은 설정이다(`~/.tofurc`, 주소 `gaiger777/orbivisor`는 `registry.opentofu.org` 아래로 풀린다).
  OpenTofu 1.13.9에서도 쓰기 전용 칸이 그대로 된다(수용 시험이 상태 파일에 비밀이 없음을 본다).

### 레지스트리 게시(사용자가 할 일)

게시는 하지 않았다. 순서만 적는다(공식 문서 기준 — 이 저장소에서 해 보지 않았다).

1. 공개 GitHub 저장소 `terraform-provider-orbivisor`를 만들고 `terraform/`을 그 뿌리로 옮긴다(레지스트리는 저장소 이름
   `terraform-provider-{이름}`과 저장소 뿌리의 Go 모듈을 본다). 모듈 경로를 새 저장소로 바꾼다.
2. GPG 서명 키(RSA 또는 DSA)를 만들고 공개 키를 Terraform Registry(GitHub로 로그인 → 설정 › Signing Keys)에 올린다.
3. GoReleaser 설정(`.goreleaser.yml` — 레지스트리 꼴 zip·`SHA256SUMS`·`SHA256SUMS.sig`, `terraform-registry-manifest.json`의
   `protocol_versions: ["6.0"]`)과 태그 `v*`에서 도는 릴리스 작업(서명 키는 GitHub 비밀)을 둔다. HashiCorp의 공급자 틀
   `terraform-provider-scaffolding-framework`의 파일이 그대로 쓸 수 있는 예다.
4. `v0.1.0` 태그를 밀어 릴리스를 만들고 registry.terraform.io의 "Publish › Provider"에서 저장소를 고른다.
5. OpenTofu는 `opentofu/registry` 저장소에 공급자 등록 요청(GitHub 이슈 양식)과 서명 키를 낸다 — 같은 GitHub 릴리스를 가리킨다.
6. `terraform/docs/`는 `tfplugindocs`로 다시 만든다(명령은 `terraform/main.go` 머리).

## 한계

- 여러 리소스를 한 Raft 거래로 적용하지 않는다(첫 실패에서 멈추고 다시 적용하면 남은 것만). 자동 바꿔 만들기(지우고 다시 만들기)도
  없다 — VM·볼륨·백업 저장소의 바꿀 수 없는 칸은 손으로 한다.
- 켜진 VM을 apply가 껐다 켜지 않는다(`--restart` 없음). 핫플러그가 안 되는 바꿈은 다음 재시작 때다.
- 템플릿 언어(변수·덮어쓰기)와 사용자 정의 종류는 없다(Helm·Kustomize 같은 도구가 매니페스트를 만들면 된다). 사용자(`User`)는
  매니페스트 종류가 아니다.
- 저장한 계획은 계획을 세운 노드 메모리에만 있다(노드가 다시 뜨거나 다른 노드로 가면 다시 계획).
- 관리자 충돌은 객체 통째 단위다(칸 단위 관리자는 없다).
- 되돌리기는 비밀·이름·전원을 되돌리지 않고, 데이터(디스크 내용·백업)는 되돌리지 않는다(스냅샷·백업이 맡는다). 업그레이드를 마치기
  전과 보존 한도 밖의 바뀜은 기록이 없다. status는 기록하지 않는다.
- `apply`·`plan`·`diff`·`restore` 밖의 `orbivisorctl` 명령(`vm set`·`vm stop`·`cert` 등)은 아직 `Orbivisor-Client`·`--reason`을
  보내지 않는다 — `orbivisorctl login`은 API 토큰을 만들므로 그 바뀜은 출처가 `api`(토큰 이름 `oplctl-…`)로 남는다.
- `apply --plan FILE`의 `--reason`은 기록되지 않는다: 사유는 계획을 세울 때 담기는데 `plan`에는 `--reason`이 없다.
- `apply_set_drift`·인증서 경보 조건은 CLI 경보 규칙 명령과 웹 폼에 아직 없다(API·매니페스트로).
- 웹에는 YAML 편집기(붙여 넣어 적용)와 목록에서 여럿 골라 내보내기가 없다(선언 묶음 상세의 "YAML로 내보내기"가 묶음 전체를 낸다).
- 수백 노드에서 기록 읽기·스냅샷 크기의 부하는 재지 않았다. 관찰 노드 끝까지(opld 여러 노드)의 기록 읽기는 RPC 시험만 있다.
- Terraform 공급자는 레지스트리에 없다(`dev_overrides`). 리소스 열다섯 밖의 종류(realm·원격·알림·설정·ACME 계정·DNS 공급자 등)는
  공급자에 없다 — `orbivisorctl apply`로 다룬다.
