# opLite 노드 유지보수 모드(노드 비우기) 설계

하위 프로젝트 11. 처음 분해한 하위 프로젝트 1~10이 끝난 뒤, 여러 설계 문서가 "넣지 않은 것"으로 남긴 "유지보수 때 모두 옮기기"(HA, `2026-11-02-ha-fencing-witness-design.md`)와 "노드 비우기(유지보수 모드)"(업데이트, `2026-11-05-install-update-subscription-design.md`)를 채운다. 계획 하나(계획 39)로 한다.

## 1. 목표와 범위

노드의 커널·펌웨어를 올리거나 하드웨어를 손보려면 그 노드의 VM을 먼저 다른 노드로 옮겨야 한다. 지금은 `NodeSpec.maintenance`(새 VM을 놓지 않음)만 있고, 실행 중인 VM은 운영자가 하나씩 `oplctl vm migrate <vm> <노드>`로 옮겨야 한다.

| 포함 | 제외 |
|---|---|
| 노드 비우기: 유지보수로 바꾸고 실행 중인 VM을 하나씩 다른 노드로 라이브 마이그레이션 | 디스크까지 복사하는 마이그레이션(로컬 디스크 VM은 옮기지 못함) |
| 대상 노드 자동 고르기(기존 스케줄러) | 유지보수가 끝난 뒤 VM을 원래 노드로 되돌리기 |
| 유지보수 노드에 고정된 꺼진 VM을 켜면 다른 노드에 놓기 | 옮기지 못한 VM을 끄는 선택 |
| `oplctl node maintenance on/off`, `oplctl node drain`, 웹 "노드 비우기" | 롤링 업그레이드 자동화, 노드 재부팅 |

## 2. 지금 있는 것 (계획 38 끝)

- `NodeSpec.maintenance: bool`: 스케줄러 `node_fits`가 유지보수 노드를 고르지 않는다(VM 만들기·켜기의 자동 배치, HA 복구 대상, 마이그레이션 대상). `PUT /nodes/{key}`(admin)로 바꾸고, 웹 노드 화면에 "유지보수 시작/끝" 메뉴가 있다.
- 꺼진 VM도 `spec.node`를 그대로 둔다. `start`는 `spec.node`가 없을 때만 스케줄한다 — 그래서 **유지보수 노드에 고정된 꺼진 VM을 켜면 그 노드에서 켜진다**.
- 라이브 마이그레이션 `POST /vms/{key}/migrate {node}`(operator 이상): 공유 풀 디스크만, 실행 중, HVF 아님, 대상은 사용자가 고른다. `VmMigrate` Task는 원본 노드에서 돈다. 동시 마이그레이션 제한은 없다.
- HA: 깨끗한 opld 종료 때 HA VM이 돌고 있으면 watchdog을 켠 채로 두어 노드가 재부팅될 수 있다. HA VM이 없으면 watchdog을 끈다.

## 3. 설계

### 3.1 노드 비우기 API

`POST /api/v1/nodes/{key}/drain` (admin — 노드 쓰기). 본문 없음. 202 `{ task }`.

1. 노드가 있어야(404), 위트니스가 아니고(409 `node_unavailable`), 온라인이어야 한다(409 `node_unavailable` — 꺼진 노드의 HA VM은 HA 복구가 맡는다).
2. 그 노드에 끝나지 않은 비우기 Task가 있으면 409 `node_busy`.
3. `require_current`(새 Task 종류).
4. `spec.maintenance`가 거짓이면 참으로 쓴다(revision 확인, 다시 하기). Task가 실패해도 노드는 유지보수로 남는다.
5. `TaskOp::NodeDrain` Task를 그 노드에 만든다.

### 3.2 비우기 Task (`node-drain`)

그 노드의 TaskRunner가 실행한다. 일반 Task 시간 제한(600초)을 쓰지 않는다(마이그레이션 여러 개).

반복:
1. 노드의 `spec.maintenance`가 거짓이 되었으면 멈추고 실패 "drain stopped: maintenance ended".
2. 이 노드에 놓인(`spec.node`) 실행 중(`power=running`) VM을 이름 순으로 본다. 이미 옮기지 못한다고 정한 VM은 건너뛴다.
3. 그 VM에 끝나지 않은 Task가 있으면(다른 마이그레이션·전원·스냅샷) 끝나기를 기다린다(최대 `migration_timeout`, 넘으면 옮기지 못함 "busy").
4. 옮길 수 있는지: 모든 디스크가 공유 풀, 이 노드가 `qemu-hvf`가 아님. 아니면 옮기지 못함(이유).
5. 대상: `scheduler::schedule`(유지보수 노드는 이미 빠짐)에 마이그레이션의 메모리 확인(남은 메모리 ≥ VM 메모리)을 더한다. 없으면 옮기지 못함 "no node with capacity".
6. 마이그레이션 API와 같은 확인·Task 만들기를 crate 함수 `vms::start_migration(state, vm, target)`으로 뽑아 쓰고, 그 `VmMigrate` Task가 끝나기를 기다린다. 실패하면 옮기지 못함(Task 오류).
7. 옮길 VM이 없으면 끝: 옮기지 못한 VM이 없으면 성공, 있으면 실패 "N VM(s) were not moved: a (disks on a local pool), b (no node with capacity)".

한 번에 하나씩 옮긴다(마이그레이션 대역폭 `[migration] bandwidth_mib`이 노드마다라서 동시에 해도 빨라지지 않고, 대상 메모리 계산이 단순해진다). 비우기 중에 새로 켜진 VM도 다음 반복에서 보인다(유지보수 노드에는 새로 놓이지 않지만 고정된 VM을 사용자가 켜는 경우는 3.3이 막는다).

opld가 비우기 중에 다시 시작하면 Task는 기존 규칙대로 "interrupted"로 실패한다. 노드는 유지보수로 남고, 다시 `drain`하면 남은 VM부터 한다(멱등).

### 3.3 유지보수 노드에 고정된 VM 켜기

`start`에서 `spec.node`가 유지보수 노드면 `schedule`로 다른 노드를 고른다(디스크가 그 노드에만 있으면 고를 수 없다). 고를 노드가 없으면 409 `node_unavailable` "node X is in maintenance and no other node can run this VM; end maintenance first". VM 만들기·고치기(`prepare`)의 고정 노드 확인도 같은 규칙(유지보수 노드를 직접 고르면 409 `node_unavailable`).

### 3.4 유지보수 끝

기존 `PUT /nodes/{key}`로 `maintenance: false`. 옮긴 VM은 돌아가지 않는다. 비우기 Task가 돌고 있으면 다음 반복에서 멈춘다(3.2의 1).

### 3.5 HA

HA VM도 같은 방법으로 옮긴다(스케줄러가 HA 가능 노드만 고름). 비우기가 성공하면 그 노드에 HA VM이 없으므로 opld를 멈춰도 watchdog이 꺼진다 — 재부팅 전에 비우기를 권한다.

### 3.6 oplctl과 웹

- `oplctl node maintenance <노드> on|off` (`PUT /nodes/{key}`, If-Match).
- `oplctl node drain <노드> [--wait]`: `--wait`이면 비우기 Task를 끝까지 따라가고(`migration_timeout` × VM 수를 넘지 않게 넉넉히), 실패하면 Task 오류를 보이고 0이 아닌 값으로 끝낸다.
- 웹 노드 화면 행 메뉴 "노드 비우기": 확인 대화상자(그 노드의 실행 중 VM 수, 로컬 디스크 VM은 옮기지 못한다는 안내), Task를 따라감. 노드 이름 옆 유지보수 배지는 그대로.

## 4. 오류

| 코드 | 언제 |
|---|---|
| 404 `not_found` | 노드 없음 |
| 409 `node_unavailable` | 위트니스·꺼진 노드 비우기, 유지보수 노드에 고정된 VM을 다른 데서 켤 수 없음, 유지보수 노드를 직접 고름 |
| 409 `node_busy` | 그 노드의 비우기 Task가 이미 있음 |
| 409 `cluster_upgrading` | 옛 버전 노드가 있음(새 Task 종류) |

## 5. 시험

- opl-api: 비우기 API(권한, 위트니스·꺼진 노드, 이미 있음, 유지보수로 바뀜), 두 노드 `two_nodes.rs`(가짜 하이퍼바이저, 실제 gRPC 마이그레이션): 공유 풀 VM 둘이 다른 노드로 가고 성공, 로컬 디스크 VM은 남고 실패 메시지, 유지보수 끝으로 멈춤, 고정 VM 켜기가 다른 노드로·못 하면 409.
- opld E2E(`migration-it`, 실제 QEMU): 두 노드에서 실행 중 VM을 비우기로 옮김.
- oplctl 파싱·fake_api, 웹 대화상자.

## 6. 알려진 한계

- 로컬 디스크 VM은 옮기지 못한다(직접 끄거나 유지보수를 끝냄).
- 한 번에 하나씩 옮긴다. 진행률은 각 `VmMigrate` Task로 본다(Task에 진행률 필드는 없다).
- 유지보수가 끝나도 VM을 되돌리지 않는다.
- 볼륨 만들기·백업 같은 Task의 노드 고르기는 유지보수를 보지 않는다(VM을 돌리지 않는 짧은 작업).
