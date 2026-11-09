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

## 7. 구현(계획 39)

- 타입: `TaskOp::NodeDrain`(`{"kind":"node-drain"}`, 필드 없음, 대상과 노드는 비울 노드). 실행기는 VM·백업 줄처럼 대상(노드) id로 줄을 세워 한 노드를 한 번에 하나만 비우고, 일반 600초 제한을 두지 않는다(VM마다 기다림만 `migration_timeout`).
- API(`opl_api::drain`): `POST /nodes/{key}/drain`(노드 쓰기 — admin, tag nodes) → 202 Task 리소스(업그레이드와 같은 모양). 순서는 §3.1대로 403 → 404 → 위트니스 409 `node_unavailable`("node "X" is a witness node and runs no VMs") → 오프라인 409 `node_unavailable`("node "X" is offline; HA recovers the HA VMs of an offline node") → 끝나지 않은 `node-drain` Task 409 `node_busy`("node "X" is already being drained") → `cluster::require_feature(cluster, "node-drain")`(§3.1의 `require_current` 대신: 위트니스 포함 모든 노드가 `NodeStatus.features`에 그 기능을 적었어야, 아니면 409 `cluster_upgrading` "some nodes do not support node drain yet (a, b); upgrade them first") → `maintenance = true`(이미면 쓰지 않음, revision 확인 쓰기를 10번까지 다시 하고 넘으면 409 `busy`) → Task. 같은 프로세스에 함께 온 요청은 정적 뮤텍스로 차례로 검사한다(다른 노드로 온 요청과 겹쳐도 실행기의 노드 줄이 차례로 한다).
- 기능 표시: `NodeStatus.features: Vec<String>`(serde default, 비면 쓰지 않음 — `NodeStatus`는 모르는 필드를 무시하므로 옛 노드도 읽는다), 이 빌드의 목록은 `opl_types::node::FEATURES`(`["node-drain"]`), opld 등록(`record_node`·`register_witness`)이 적는다. 옛 버전 리더가 노드 status를 다시 쓰면 모르는 `features`가 빠지므로, 노드와 위트니스가 주기 갱신(`BRIDGE_REFRESH` 60초, `bootstrap::refresh_features` — 이 빌드의 목록과 다를 때만 revision 확인 쓰기)으로 되살린다. 그래서 새 버전 노드가 `cluster_upgrading`에 나와도 다음 주기 뒤(또는 그 노드의 opld를 다시 시작하면) 풀린다. `require_current`는 롤링 업그레이드 중(한 노드만 새 버전) 비우기를 늘 막아 "노드 하나씩 비우고 올림" 절차가 깨지므로 버전 대신 기능을 본다: 비우기가 처음 들어간 버전으로 올리는 동안은 못 쓰고, 그 뒤의 업그레이드에서는 버전이 섞여도 된다.
- `vms::start_migration(state, vm, target, source: Option<Uuid>) -> Result<Task, ApiError>`(pub(crate)): `migrate_vm`의 확인과 Task 만들기를 뽑은 함수로, 스스로 `state.vm_lock`을 잡고 VM·노드를 id로 다시 읽는다. `source`가 있으면(비우기) 그 아래에서 VM이 아직 그 노드에 있는지 보고 아니면 409 `vm_moved`다. `migrate_vm`은 `None`으로 부르고 동작은 그대로다. 실행기의 `Ctx`에는 `AppState`가 없어 `TaskRunnerConfig.api: Option<Arc<AppState>>`를 더했다(opld `member.rs`와 시험 도우미가 넣음, 없으면 비우기 Task가 "this node cannot create migration tasks"로 실패). `vm_lock`은 노드마다라 API 노드와 공유되지 않는다(기존 `migrate_vm`과 같은 한계 — revision 확인 쓰기가 막음).
- 비우기 Task(`tasks/drain.rs`): §3.2와 같다. 다른 점과 구체: 옮기지 못한 VM은 다시 보지 않는다. 옮길 것이 없어진(꺼짐·지워짐·다른 노드로 감) VM은 다시 나타나면 다시 보되 세 번까지(그 뒤 `busy`), 옮긴 VM이 다시 들어오면 한 번 더 옮기지 않고 `moved back during the drain`(끝이 있게). 성공은 노드가 빈 것이다: 옮길 VM이 없으면 이 노드로 들어오는 마이그레이션(`status.migration`의 대상이 이 노드인 VM, 끝나지 않은 `VmMigrate { node: 이 노드 }` Task)이 끝나기를 최대 `migration_timeout` 기다려(넘으면 그 VM은 `busy`) 다시 반복하므로 들어온 VM도 옮긴다. 기다리기(settle)는 VM이 이 노드에서 실행 중이고 phase `running`이며 `status.migration`이 없고 그 VM을 대상으로 한 끝나지 않은 Task가 없을 때까지(`poll_interval`마다 클러스터에서 다시 읽음, `migration_timeout`을 넘기면 `busy`). 기다린 뒤(마이그레이션을 시작하기 직전)와 들어오는 마이그레이션을 기다리는 동안에도 노드를 다시 읽어 유지보수가 끝났으면 멈춘다. 대상은 `node_fits`를 지나고 `qemu-hvf`가 아니며 `free_memory_mib`가 VM 메모리 이상인 노드 가운데 남은 메모리가 가장 많은 노드(같으면 id가 작은 것, §3.2의 "`schedule`의 첫 결과"가 HVF 노드면 다른 노드를 보지 못하므로). `start_migration`은 원본(이 노드)을 넘겨 부르고, 오류는 `vm_not_running`·`not_found`·`vm_moved`면 넘어가고, `vm_busy`면 한 번 더 기다려 다시 하고 그래도면 `busy`, 나머지(`no_capacity` 포함)는 오류 메시지 그대로. 마이그레이션 Task는 `poll_interval`마다 다시 읽어 끝나기를 기다리고, 읽기가 계속 실패하거나 끝나지 않으면 `migration_timeout`의 두 배 + 1분에서 `migration task did not finish`. 실패 문구는 이름 순 `"N VM(s) were not moved: a (disks on a local pool), b (busy)"`.
- VM 켜기·고치기(`vms.rs`): `start`는 유지보수 노드에 고정된 VM이 **완전히 꺼져 있을 때만**(`is_stopped`, 같은 revision 확인 쓰기 안에서) `schedule`로 다른 노드를 골라 `spec.node`를 바꾼다. 아직 그 노드에서 돌거나 멈추는 중이면 그대로 둔다. `prepare`(만들기·고치기·스냅샷 되돌리기)는 요청한 노드가 지금 노드와 다를 때만 유지보수를 확인한다(409 `node_unavailable` "node "X" is in maintenance and cannot take VMs; end maintenance first" — §3.3의 "직접 고름"을 "새로 고름"으로 좁힘: 이미 그 노드에 있는 VM의 다른 고치기와 다시 켜기는 막지 않는다).
- oplctl: `node maintenance <노드> on|off`(GET → 라벨과 spec은 그대로 두고 `spec.maintenance`만 바꿔 읽은 revision으로 If-Match PUT, `revision_mismatch`면 다시 읽어 세 번까지, 출력 `node X: maintenance on`; 끄면 그 노드의 끝나지 않은 `node-drain` Task를 찾아 stderr에 `warning: the running drain of X stops (task T)`, 목록을 읽지 못하면 `warning: cannot check for a running drain: …`만 쓰고 성공), `node drain <노드> [--wait]`(기다리지 않으면 ``drain of X started (task T); follow it with `oplctl task get T` ``, `--wait`이면 `node X drained` 또는 Task 오류와 종료 코드 1). 기다리는 한도는 §3.6의 "`migration_timeout` × VM 수" 대신 고정 `client::DRAIN_WAIT`(24시간)이다 — oplctl은 서버의 `migration_timeout`과 옮길 VM 수를 모른다.
- 웹(`web/src/nodes/NodesPage.tsx`): 행 메뉴 "노드 비우기…"(노드 쓰기 권한 메뉴, 위트니스는 메뉴 없음, 오프라인·하이퍼바이저 `qemu-hvf`("macOS 노드의 VM은 옮길 수 없습니다")·끝나지 않은 `node-drain` Task가 있으면 이유와 함께 막힘), "유지보수 끝내기"는 끝나지 않은 `node-drain` Task가 있으면 확인 대화상자("끝내면 진행 중인 비우기가 멈춥니다"), 확인 대화상자(`/vms`에서 `spec.node`가 이 노드이고 `spec.power`가 running인 VM 수 — Task와 같은 기준, 로컬 디스크 VM은 옮기지 못하고 비우기가 실패한다는 고정 안내 — VM마다 풀을 따지지 않음), POST 뒤 Task "X 비우기"를 따라감. i18n `nodes.drain.*`·`errors.node_busy`·`tasks.op.node-drain`, ko·en. 일반 문구 `errors.node_unavailable`에 서버 설명을 붙여(`{{message}}`) VM 켜기의 409가 이유를 보인다.
- 시험: opl-types `TaskOp` JSON; opl-api `tests/vms.rs`(유지보수 노드에 고정된 꺼진 VM이 다른 노드에서 켜짐, 그 노드밖에 없으면 409, 만들기·고치기에서 유지보수 노드를 고르면 409, 실행 중 VM 고치기·다시 켜기는 그대로), `tests/drain.rs`(권한·404·유지보수로 바뀜·Task, 위트니스·오프라인 409, `node_busy`가 `cluster_upgrading`보다 먼저, 기능 없는 노드(위트니스) 409와 옛 버전이어도 기능이 있으면 202, 빈 노드 성공, 옮기지 못한 이유 — 로컬 풀·메모리가 모자란 노드뿐(자리 없음)·HVF, HVF 노드를 건너뛰고 다른 노드로 `VmMigrate`, settle의 `busy` 시간 초과), `tests/two_nodes.rs`(두 노드 실제 mTLS gRPC, 가짜 하이퍼바이저: 공유 풀 VM 둘이 이름 순으로 옮겨지고 성공, 로컬 디스크 VM은 남고 실패 메시지, 비우기를 시작할 때 들어오던 VM도 들어온 뒤 옮김, 옮기는 중 유지보수를 끄면 멈춤); opld 등록이 기능을 적음, 지운 기능 표시를 주기 갱신이 되살림; oplctl 파싱·fake_api(GET → If-Match PUT 본문, `revision_mismatch` 다시 하기, 끄기의 비우기 경고와 Task 목록을 읽지 못할 때의 경고·성공, drain 출력·`--wait` 성공과 실패); opld E2E `tests/migration.rs` `drain_moves_a_running_vm_to_the_other_node`(기능 `migration-it`, 실제 QEMU — macOS는 TCG: 노드 1의 실행 중 VM을 `node drain --wait`로 노드 2에, 원본 QEMU 정리, `maintenance off` 뒤에도 노드 2에 남음); 웹 Vitest(옮길 VM 수와 안내·요청·Task 이름, `node_busy`·`node_unavailable` 오류, 오프라인·macOS·비우는 중 막힘, 위트니스에 메뉴 없음, 비우는 중 유지보수 끝내기 확인, VM 켜기 `node_unavailable` 문구).
- §5와 다른 점: 고정 VM 켜기 시험은 `two_nodes.rs`가 아니라 `tests/vms.rs`(가짜 노드 기록)에 있다. §4의 `cluster_upgrading`은 "옛 버전 노드"가 아니라 "비우기 기능을 적지 않은 노드"다.
