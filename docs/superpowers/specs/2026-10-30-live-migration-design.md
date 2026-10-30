# opLite 라이브 마이그레이션 설계 (하위 프로젝트 4: 계획 20·21)

작성일: 2026-10-30
상위 문서: `2026-10-20-opl-node-core-design.md`(하위 프로젝트 분해, UEFI 변수 위치, `PoolSpec.shared`), `2026-10-28-shared-storage-nfs-design.md`(공유 풀), `2026-10-29-shared-storage-ceph-rbd-design.md`, `2026-10-30-shared-storage-iscsi-lvm-design.md`(공유 블록 풀의 활성화)

## 1. 목표와 범위

처음 분해에서 하위 프로젝트 4는 라이브 마이그레이션(`qm migrate`·vMotion)이다. 공유 스토리지 설계는 여기에 두 가지를 넘겼다: UEFI 변수를 노드 로컬 파일에서 VM을 따라가게 하는 일과, 풀의 `shared` 설정을 바꾸는 일. 이 설계는 셋을 두 계획으로 나눈다.

| 계획 | 내용 |
|---|---|
| 20 | 공유 풀 위의 라이브 마이그레이션: QEMU 마이그레이션(TLS), 노드 사이 RPC, `VmMigrate` Task, API·oplctl·웹, 2노드 실제 시험 |
| 21 | UEFI 변수가 노드를 옮긴 VM을 따라간다(멈춘 채 옮기기), 디렉터리 풀의 `shared` 바꾸기 |

넣지 않는 것: 디스크까지 복사하는 마이그레이션(로컬 풀, `blockdev-mirror` — 디스크가 모두 공유 풀에 있어야 한다), 마이그레이션 취소(시간 제한으로 끝난다), 사후 복사(post-copy), 다른 QEMU 버전 사이의 마이그레이션, CPU 모델 맞추기(같은 CPU의 노드를 전제), macOS 노드(HVF), HA·자동 재배치(뒤의 하위 프로젝트), 노드 유지보수 때 모두 옮기기, 풀의 `nodes`·백엔드 바꾸기.

### 견준 길

- **UEFI 변수를 공유 풀의 볼륨으로(Proxmox `efidisk`):** 노드가 죽어도 변수가 남지만, 볼륨 수명·크기(pflash는 정확한 크기가 필요해 LV·RBD의 올림과 맞지 않는다)·스냅샷·복제를 모두 바꿔야 하고, AAVMF 변수(64 MiB)는 볼륨으로도 무겁다. **VM을 따라 복사(채택):** 라이브 마이그레이션에서는 QEMU가 pflash 내용을 함께 옮기고(받는 쪽이 재개될 때 파일에 쓴다), 멈춘 채 옮기면 새 노드가 시작할 때 변수를 가진 노드에서 받아 온다. 노드가 죽은 경우만 템플릿으로 돌아간다(지금과 같다, HA에서 다시 본다).
- **마이그레이션 지휘를 API 노드에서 vs 원본 노드의 Task에서(채택):** 기존 Task 실행기(노드별 큐, 재시작 시 "interrupted")를 그대로 쓰고, 원본 QEMU의 QMP가 그 노드에만 있다.
- **QEMU가 직접 TCP+TLS(채택) vs gRPC로 중계:** QEMU의 `tls-creds-x509`가 노드 인증서를 그대로 쓸 수 있고, 메모리를 opld로 거쳐 보낼 이유가 없다.

## 2. 마이그레이션 흐름 (계획 20)

원본 노드 S, 대상 노드 T. Task `VmMigrate { node: T }`는 S에서 돈다(`spec.node = S`, 대상 VM 큐).

1. **API 검사**(§4) 뒤 Task를 만든다. 202.
2. **S: 시작.** reconciler에 이 VM의 마이그레이션을 알려(`ReconcilerHandle::begin_migration(id)`) 워커가 이 VM에 손대지 않게 하고, `status.phase = Migrating`, `status.migration = Some(MigrationStatus { node: T, task })`를 쓴다(revision 검사). 이미 `spec.node != S`이거나 VM이 실행 중이 아니면 실패.
3. **S → T: 받을 준비(RPC `MigrateIncoming`).** 요청: VM id, Task id, S의 QEMU 버전. T는:
   - 자기 QEMU 버전과 다르면 거절(`migration_incompatible`).
   - VM이 `Migrating`이고 `status.migration`이 이 Task·T를 가리키는지 본다.
   - reconciler의 `runtime_spec`으로 디스크를 켜고(공유 LVM은 §3) 탭을 만든 뒤, 같은 cmdline에 `-incoming defer`를 더해 QEMU를 띄운다(`-S`는 늘 있다 — 받은 뒤에도 멈춰 있다). 실행 디렉터리에 받는 중 표시(`incoming`)를 남긴다.
   - QMP로 원본과 같은 마이그레이션 설정(능력·TLS)을 하고 `migrate-incoming`을 포트 범위(`[migration] ports`, 기본 `60000-60099`)에서 빈 포트로 연다(쓰는 중이면 다음 포트).
   - 응답: 포트. 실패하면 띄운 QEMU를 끄고 켠 디스크를 끈다.
4. **S: 보내기.** 능력 `events`·`auto-converge`, 매개변수 `tls-creds`·`tls-hostname`(T의 클러스터 주소의 호스트, 노드 인증서 SAN에 있다)·`max-bandwidth`(`[migration] bandwidth_mib`, 0이면 QEMU 기본)를 정하고 `migrate uri=tcp:{T 호스트}:{포트}`. `MIGRATION` 이벤트·`query-migrate`로 `completed`·`failed`·`cancelled`를 기다린다. 시간 제한은 `[migration] timeout`(기본 30분, Task 공통 제한 대신)이고 넘으면 `migrate_cancel`.
5. **S → T: 받기 확인(RPC `MigrateIncomingWait`).** T의 QEMU가 `completed`가 될 때까지 기다린다(원본의 완료는 대상이 장치 상태를 다 읽었다는 뜻이 아니다).
6. **S: 확정.** Raft에 `spec.node = T`(revision 검사 `update_spec`, 다른 필드는 그대로)와 `status { phase: Running, node: T, migration: None }`를 쓴다. 이 쓰기가 성공한 순간이 마이그레이션의 확정이다.
7. **T: 넘겨받기.** T의 reconciler가 `spec.node == T`인 VM을 맡아 워커를 띄우고, 이미 떠 있는 받은 QEMU를 붙인다(`hv.inspect`). 받는 중 표시가 있고 QEMU가 `paused`이면 `cont`하고 표시를 지운다(기존 "멈춘 채 남은 VM 이어 가기"에 받은 VM을 더한다). 대상 QEMU가 알아서 GARP를 보낸다(`announce-*` 기본).
8. **S: 정리.** 원본 QEMU(`postmigrate`)를 `quit`하고, 탭을 지우고, 디스크를 끈다(`deactivate_volume`). `end_migration(id)`. 워커는 `spec.node != S`이므로 물러난다. Task 성공.

**실패**(3~6단계에서, 확정 전): S는 `MigrateIncomingAbort`로 T의 QEMU를 끄게 하고(받는 중 표시가 있는 것만), 원본이 `postmigrate`·`paused`이면 `cont`, `status.phase = Running`, `migration: None`를 쓰고 `end_migration`. Task는 실패(오류 문구는 QEMU의 `error-desc`).

**두 QEMU가 함께 도는 일은 없다.** 대상은 `-S`로 떠서 확정(6) 뒤 T의 워커만 `cont`한다. 원본은 확정 전에만 `cont`된다.

**중간에 죽으면:**
- S의 opld가 확정 전에 죽음: 다시 뜨면 Task는 "interrupted"로 실패하고 `begin_migration`도 없으므로, 워커가 `Migrating` VM을 보면 `status.migration`을 지우고 `Running`으로 되돌린 뒤 원본이 `postmigrate`면 `cont`한다(기존 이어 가기에 `postmigrate`를 더한다 — `Migrating`이었을 때만). T는 아래 규칙으로 받은 QEMU를 끈다.
- S의 opld가 확정 뒤 죽음: T가 넘겨받는다. S가 다시 뜨면 원본 QEMU는 `spec.node != S`인데 `postmigrate`로 남아 있다 — S의 reconciler가 시작할 때(그리고 `tag_sweep`마다) `spec.node`가 자기가 아니고 `postmigrate`인 QEMU를 끄고 디스크를 끈다.
- T의 받은 QEMU가 남음: T는 받는 중 표시가 있고 `spec.node != T`인 QEMU를, 그 VM의 `status.migration`이 없거나 다른 Task·노드를 가리키면 끈다(opld 시작 때, `MigrateIncomingAbort` 때, `tag_sweep`마다). 확정 전 S가 죽어 Task가 실패하면 S의 워커가 `migration`을 지우므로 T의 다음 정리가 끈다.
- QEMU 자신이 이미 "디스크를 놓았다"(원본 `postmigrate`의 inactivate) — 확정 전 실패에서 `cont`가 다시 잡는다(QEMU 동작).

구현: 원본 Task는 `opl-api/src/tasks/migrate.rs`이고 일반 Task 시간 제한 밖에서 단계마다 따로 제한한다(보내기는 `[migration] timeout`, `MigrateIncoming` 120초, `MigrateIncomingWait` 180초 — 받는 쪽 기다림 120초보다 길게, Abort 60초). 확정 쓰기와 되돌리기 쓰기는 같은 revision을 확인하므로 둘 중 하나만 적용되고, 확정 쓰기가 모호하게 실패하면 60초 동안 다시 해 본 뒤 되돌린다. S의 QEMU 버전은 실행 중인 원본의 QMP `query-version`, T의 버전은 `qemu --version`이다(같은 QEMU 바이너리). 접속 호스트와 `tls-hostname`은 T의 클러스터 멤버 주소(`members().addr`)의 호스트(T의 opld `address`, 노드 인증서에 있다)이고, 받는 QEMU는 T의 `cluster_listen` IP(기본 `0.0.0.0`, IPv6 `[::]`)에서 듣는다(`address`는 그 기계에서 `127.0.1.1` 등으로 풀릴 수 있다). 받는 쪽 처리는 opld가 아니라 opl-api의 `IncomingMigrations`(opld가 피어 서버에 주입)이고, 받는 VM의 기록이 Raft로 올 때까지 10초 기다린다. 포트는 opld가 범위에서 먼저 bind 해 보고 빈 것을 고른다(한 QEMU에 `migrate-incoming`은 한 번만 되므로 실패하면 그 QEMU를 버려야 한다). 모두 쓰는 중이면 "no free migration port in A-B on HOST"로 실패한다. TLS 객체(`opl-mig-tls`)는 QMP `object-add`로 매번 지우고 다시 만들고 양쪽 다 `verify-peer`다. 원본은 `ReconcilerHandle::begin_migration`(워커가 하던 걸음을 마칠 때까지 기다림)·`end_migration`으로 워커를 멈춰 둔다. QEMU 8.2의 받는 QEMU는 받기에 실패하면 스스로 끝나므로 T에는 자기 것이 아닌 VM의 충돌 이벤트가 생길 수 있다(무시한다). 남은 QEMU 정리는 Task 실행기 시작 때와 `tag_sweep`(30분)마다 돈다. 받는 중 표시 파일에는 마이그레이션 Task id를 적고, 정리는 `status.migration`이 그 Task와 이 노드를 가리킬 때만 남긴다(받기는 같은 VM의 다른 Task 받는 QEMU를 먼저 끈다). VM이 지워진 `postmigrate` 원본도 끈다. opld가 끝을 지켜보지 못한 마이그레이션(재시작, 되돌리기)은 `migrate_cancel`로 취소한 뒤 재개 여부를 정하고, 보류가 아니고 표시가 없는 자기 노드 VM의 `postmigrate` QEMU는 그 VM이 이 노드에서 돌던 것일 때만(`status.node`가 이 노드, phase `Running`·`Migrating`) 워커가 이어서 실행한다(확정 쓰기는 표시가 있는 revision에서만 적용되므로 더는 확정될 수 없다). 확정은 늘 `status.node`를 대상으로 바꾸므로, 아니면 다른 노드로 확정된 마이그레이션의 정리되지 않은 원본이 그 뒤 이 노드로 다시 옮겨진 것이다: 낡은 메모리를 이어서 실행하지 않고 워커와 정리가 그 QEMU를 끄고, 워커가 새로 켠다. `Migrating`을 쓴 뒤 그 VM의 스냅샷·복제·크기 늘리기 Task가 끝나지 않았으면 되돌리고, 확정 쓰기는 `spec.power`가 running일 때만 한다(검사와 기록 사이에 들어온 요청). 보류 중의 수명 이벤트(원본 크래시)는 보류가 풀린 뒤 처리한다. CI(QEMU 8.2.2)에서 확인: 완료 뒤 원본은 `postmigrate`, 받는 쪽은 `-S`라 `paused`로 남고 `cont` 뒤 `running`, 실패(연결 거부·TLS 없는 대상)에서 원본은 `running` 그대로.

## 3. 스토리지

- 디스크는 모두 공유 풀이어야 한다(`is_shared`). 각 노드는 자기 경로로 디스크를 연다(NFS 마운트 경로가 달라도 된다) — cmdline의 블록 노드 이름(`disk{i}`)·장치는 같다.
- 파일 디스크(NFS·공유 디렉터리): QEMU가 받는 쪽 이미지를 비활성으로 열고 확정 때 잠금을 넘긴다. 따로 할 일이 없다.
- RBD: 같다(배타 잠금 기능을 쓰지 않는다).
- **공유 LVM:** 배타 활성화(`-aey`)가 두 노드의 동시 사용을 막으므로, 마이그레이션 동안 공유 잠금으로 바꾼다. `Storage`에 `share_volume(pool, id)`(켠 LV를 공유로: `lvchange -asy`)와 `exclusive_volume(pool, id)`(다시 배타로: `lvchange -aey`)를 더한다(기본 Ok, 다른 백엔드는 할 일이 없다). S가 3단계 전에 `share_volume`, T는 `activate_volume` 대신 공유로 켜고, 8단계에서 S가 끈 뒤 T의 워커가 넘겨받을 때 `exclusive_volume`. 실패하면(1단계 되돌리기) S가 다시 배타로. 구현 전 CI에서 lvmlockd가 켜진 LV의 배타↔공유 바꾸기를 하는지 확인한다(계획 20의 환경 탐색). **안 되면 공유 LVM 디스크의 마이그레이션은 409 `migration_unsupported`로 두고 한계에 적는다.**
- 남은 활성화 정리(`deactivate_idle_volumes`)는 받는 중인 VM의 디스크를 쓰는 중으로 본다(로컬 하이퍼바이저에 떠 있다 — 기존 규칙). 원본의 `Migrating` VM도 그렇다.

구현: 공유 LVM 마이그레이션을 구현했다. CI(lvm2 2.03.16, sanlock)에서 쓰는 중인 LV의 `lvchange -asy`(배타→공유)·`-aey`(다른 노드가 쥐지 않았을 때 공유→배타)가 모두 되는 것을 확인했다(`lvs`의 `lv_active_exclusively`는 공유 잠금이어도 참으로 보이므로 잠금 모드 판단에 쓰지 않는다). `Storage::share_volume`·`exclusive_volume`(기본 Ok, 공유 LVM만 `-asy`·`-aey`)에 형식 인자를 더했고, T는 `opl_reconciler::resolve::incoming_spec`으로 디스크를 공유로 켠다. S가 `Migrating`을 쓴 뒤 공유로 바꾸고(실패하면 되돌린다), 실패하면 보류가 풀린 뒤 S의 워커가 다시 배타로, T는 넘겨받아 재개한 뒤 배타로 바꾼다 — 원본이 아직 놓지 않았으면 `retry_interval`마다 다시 해 본다. 배타 여부는 메모리에만 있으므로 워커는 자기가 켜서 시작하지 않은 실행 중 VM(opld 재시작, 넘겨받기)에 붙을 때 한 번 `exclusive_volume`(멱등 `-aey`)을 부른다. 남은 활성화 정리는 `status.migration.node`가 자기인 VM의 디스크도 쓰는 중으로 본다.

## 4. API·Task·클라이언트 (계획 20)

- `POST /vms/{key}/migrate`, 본문 `{ "node": "<노드 이름 또는 id>" }`, `Operate` 권한(운영자 가능), 202와 Task. 검사 순서:
  - 대상 노드가 없으면 404, 지금 노드와 같으면 400 `invalid_request`.
  - VM이 `Running`(`spec.power == Running`이고 `phase == Running`)이 아니면 409 `vm_not_running`.
  - `check_not_busy`·스냅샷·되돌리기·크기 늘리기 Task가 있으면 409 `vm_busy`(크기 늘리기는 계획 19의 실행 중 규칙과 같이 끝나지 않은 것 모두).
  - 공유 풀이 아닌 디스크, 대상 노드가 macOS이거나 원본이 macOS, (계획 20의 결과에 따라) 공유 LVM 디스크: 409 `migration_unsupported`(디스크 이름·이유).
  - 대상 노드가 오프라인이거나 유지보수 중: 409 `node_unavailable`.
  - 스케줄러가 그 노드를 고를 수 있는지(디스크 풀 준비, 네트워크, 아키텍처, 남은 메모리): 안 되면 409 `no_capacity`(이유 문구). 기존 `scheduler`의 노드 판정을 한 노드에 대해 부른다.
- 마이그레이션 중(`phase == Migrating` 또는 끝나지 않은 `VmMigrate` Task)에는 켜기·끄기·강제 끄기·재시작·스냅샷·되돌리기·크기 늘리기·복제·VM 바꾸기(`PUT`)·지우기가 409 `vm_busy`다(`check_not_busy`에 더한다).
- `VmPhase::Migrating`(kebab `migrating`), `VmStatus.migration: Option<MigrationStatus { node: Uuid, task: Uuid }>`(`serde(default)`, 없으면 빼고 쓴다). `TaskOp::VmMigrate { node: Uuid }`(`kind: "vm-migrate"`).
- 혼합 버전 클러스터는 지원하지 않는다(새 RPC). 모든 노드를 올린 뒤 쓴다.
- 설정 `[migration]`: `ports = "60000-60099"`, `bandwidth_mib = 0`, `timeout = "30m"`. 방화벽은 노드 사이 그 범위를 열어야 한다.
- TLS: opld가 시작할 때 노드 인증서·키·클러스터 CA를 `{state_dir}/migration-tls/`(0700, 키 0600)에 `ca-cert.pem`·`server-cert.pem`·`server-key.pem`·`client-cert.pem`·`client-key.pem`으로 쓰고, QEMU에 `tls-creds-x509`(받는 쪽 `endpoint=server,verify-peer=on`, 보내는 쪽 `endpoint=client`)로 준다. 인증서가 바뀌면(갱신) 다시 쓴다.
- 노드 사이 RPC(`opl.peer.v1.Peer`, JSON 코덱, 기존 mTLS 회원 검사): `MigrateIncoming { vm, task, qemu_version } -> { port }`, `MigrateIncomingWait { vm, task } -> {}`, `MigrateIncomingAbort { vm, task } -> {}`. 받는 쪽 처리는 opld가 reconciler 핸들과 하이퍼바이저로 한다.
- 드라이버: `Hypervisor`에 `start_incoming(spec, ports) -> port`, `migrate(id, uri, opts) -> Result<(), DriverError>`(완료까지), `incoming_wait(id)`, `qemu_version()`을 더한다. `MockHypervisor`도 같이(API·reconciler 테스트). cmdline은 `VmRuntimeSpec`이 아니라 시작 방식으로 `-incoming defer`를 더한다.
- oplctl: `vm migrate VM NODE`(Task를 기다리고 "vm X: migrated to NODE"). 웹: VM 상세의 "마이그레이션" 동작(실행 중일 때), 대상 노드 고르기 대화상자(온라인·같은 아키텍처 노드), `Migrating` 상태 표시와 다른 동작 막기, Task 설명(`vm-migrate`). 번역 키 ko·en.

구현: macOS 판정은 노드 상태의 하이퍼바이저 이름(`qemu-hvf`)이다 — TCG로 도는 macOS 노드는 막지 않는다. 메모리 판정은 `scheduler::node_fits`(온라인·유지보수·아키텍처·풀·브리지)와 따로 `Inventory::free_memory_mib`로 하고, 아키텍처가 다르면 400 `machine_arch_mismatch`다. 이미 `Migrating`인 VM은 `vm_not_running`이 아니라 `vm_busy`로 거절한다. 검사와 Task 만들기는 `vm_lock` 안에서 한다. `VmMigrate`는 Task 실행기의 VM 큐로 돈다. `[migration] timeout`은 humantime이 아니라 작은 자체 파서(`30m`, `1h30m`, `90s`)다. TLS 파일은 `{data_dir}/migration-tls/`(드라이버의 `state_dir`가 데이터 디렉터리, 기본 `/var/lib/opl/migration-tls`)이고 opld가 시작할 때마다 쓴다. oplctl `vm list`의 NODE 칸은 마이그레이션 중에 `원본 -> 대상`이다. 웹은 VM 상세에만 "마이그레이션"이 있고(목록 메뉴에는 없다) 대상 목록은 HVF 노드를 거르지 않는다(서버가 409로 이유를 보인다). 마이그레이션 중인 VM은 대시보드에서 원본 노드에 센다.

## 5. UEFI 변수가 VM을 따라간다 (계획 21)

- `VmStatus.firmware_node: Option<Uuid>` — 지금 UEFI 변수 파일을 가진 노드. 워커가 UEFI VM을 시작할 때(그 노드의 변수로 켰으므로) 자기 노드로 쓰고, 마이그레이션 확정(§2의 6)이 T로 쓴다(QEMU가 pflash를 옮겼다).
- 시작할 때 `firmware_node`가 다른 노드면, 그 노드에서 RPC `UefiVars { vm } -> stream<Chunk>`로 변수 파일을 받아 로컬 파일을 바꾼다(임시 파일에 받은 뒤 이름 바꾸기). 1 MiB 조각, 0만 있는 조각은 표시만 보낸다(AAVMF 64 MiB). 그 노드가 오프라인이거나 파일이 없으면 경고를 남기고 로컬 파일(없으면 템플릿)로 켠다 — 시작을 막지 않는다. 받은 노드에서는 VM이 다시 그 노드에 오기 전까지 파일을 지우지 않는다(다시 오면 `firmware_node`가 달라 새로 받는다).
- 스냅샷·복제가 변수를 복사하는 기존 경로(Task 노드의 로컬 파일)는 `firmware_node`가 그 노드가 아니면 같은 RPC로 먼저 받는다.

계획 20 확인: 라이브 마이그레이션에서는 QEMU가 pflash 변수를 옮겨 받는 쪽이 재개될 때 대상 노드의 변수 파일에 쓴다(CI에서 원본 파일 끝 표식이 대상 파일에 생기고 두 파일이 같았다, `migration-it`도 본다). 이 절은 멈춘 채 옮기기만 다룬다.

구현: `firmware_node`는 `serde(default, skip_serializing_if)`인 선택 필드다(OpenAPI에서 필수 아님). RPC는 opl-peer의 이진 코덱(요청은 VM id 16바이트, 조각은 태그 0 데이터 / 태그 1 0 조각 길이 u64 BE, 오류는 gRPC 상태 코드 — 파일 없음은 `NotFound`)이고 기존 회원 검사를 거친다. 받는 쪽(`Peers::uefi_vars`, 60초)은 같은 디렉터리의 `.uefi-vars.fd.{uuid}.tmp`에 받아 flush·fsync 뒤 이름을 바꾸고(디렉터리도 fsync), 실패하면 기존 파일을 그대로 둔다. 받는 크기는 256 MiB까지이고, `UefiVarsFetcher`는 받은 크기가 이 노드의 UEFI 변수 템플릿(하이퍼바이저 설정)과 다르면 실패로 친다(다른 펌웨어 빌드). reconciler는 opl-peer에 의존하지 않고 `FirmwareFetcher` trait(`ReconcilerConfig.firmware`, Task 실행기는 `TaskRunnerConfig.firmware`)으로 받으며, opld가 opl-api의 `UefiVarsFetcher`(오프라인으로 기록된 노드는 연결해 보지 않고, 회원이 아니면 오류)를 넣는다. 워커는 QEMU를 새로 띄울 때(`start()`, 디스크 resolve 뒤)만 받고 다시 붙기·마이그레이션 넘겨받기에서는 받지 않는다. `firmware_node = 자기`는 QEMU가 뜬 뒤 `Running` 기록과 함께, 그리고 이 노드에서 UEFI VM의 QEMU가 도는 것을 볼 때마다 쓴다(넘겨받기가 원본의 마지막 기록보다 먼저 일어나는 경우). 워커의 status 쓰기는 자기가 바꾸지 않은 `firmware_node`를 덮지 않고, 상태를 새로 만드는 곳은 값을 이어받는다. 마이그레이션 확정 뒤의 status 쓰기(`finish()`)가 T를 쓴다. 스냅샷·복제 Task는 변수를 복사하기 전에 받고(BIOS, 이미 이 노드, 이 노드에서 QEMU가 돌면 받지 않는다), 되돌리기는 변수를 쓴 Task 노드를, 복제는 새 VM의 변수 복사본을 둔 Task 노드를 `firmware_node`로 쓴다. 실패는 모두 `tracing::warn` 뒤 이 노드의 파일(없으면 템플릿 또는 복사하지 않음)이다.

## 6. 풀의 `shared` 바꾸기 (계획 21)

- `PATCH /pools/{key}` 본문 `{ "shared": bool }`(지금은 `shared`만), `Write` 권한(관리자). 디렉터리 풀만 바꿀 수 있다 — 다른 백엔드는 400(NFS·RBD·공유 LVM은 늘 공유, LVM-thin·ZFS는 늘 로컬). 같은 값이면 그대로 200.
- 로컬 → 공유: 언제나 된다(관리자가 그 경로가 목록의 모든 노드에서 같은 공유 파일 시스템임을 보증한다).
- 공유 → 로컬: 풀의 볼륨을 쓰는 VM 중 `spec.node`가 그 볼륨의 `status.node`(만든 노드)와 다른 것이 있거나, 그 풀 디스크를 가진 VM의 끝나지 않은 `VmMigrate` Task가 있으면 409 `pool_in_use`(VM 이름). 볼륨은 만든 노드에 있는 것으로 돌아간다.
- revision 검사로 쓴다. oplctl `pool set POOL --shared|--no-shared`, 웹 풀 상세의 공유 스위치(디렉터리 풀만).

구현: 경로는 기존 풀 경로인 `PATCH /api/v1/storage-pools/{key}`이고 본문 `PoolPatch { shared }`는 모르는 필드를 거절한다. 디렉터리가 아닌 풀은 같은 값이어도 400 "only dir pools can change shared; {kind} pools are always shared/node-local"이고, 같은 값이면 쓰지 않고 200이다. `If-Match`(선택, 다르면 412)가 없으면 revision 검사 쓰기를 10번까지 다시 해 보고 409 `busy`다. 검사는 VM 옮기기·마이그레이션 요청과 같은 `vm_lock` 안에서 한다. `vm_lock`은 이 노드의 요청만 막으므로 공유 → 로컬은 쓴 뒤 다시 검사해, 이제 안 되면 revision 검사로 공유로 되돌리고 409 `pool_in_use`다. 노드에 놓이지 않은 VM이나 만든 노드가 기록되지 않은 볼륨은 막지 않는다. oplctl `pool set`은 `--shared`·`--no-shared` 중 정확히 하나를 받고 `storage pool X: shared|node-local`을 찍는다. 웹은 풀 상세가 아니라 풀 목록의 "공유" 칸(쓰기 권한과 디렉터리 풀이면 스위치, 아니면 글자)이고 양쪽 다 확인 대화상자, 409는 `pools.sharingErrors.<code>` 문구에 서버 문구(VM 이름)를 붙인다.

## 7. 테스트

- 단위: cmdline `-incoming defer`, QMP 마이그레이션 상태 해석, 포트 범위 해석, API 검사(각 409·400), `check_not_busy`의 마이그레이션, 워커의 `Migrating` 되돌리기·`postmigrate` 정리, MockHypervisor로 2노드 in-process 마이그레이션(`opl-api/tests/two_nodes.rs`의 실제 gRPC mTLS), RPC 회원 검사, `PATCH /pools`, 변수 조각 전송(0 조각).
- 실제(`migration-it`, Linux root, CI `linux-kvm`): 한 호스트에서 opld 둘(`opld/tests/common`의 `Daemon`)로 클러스터를 만들고, 공유 디렉터리 풀의 qcow2 디스크 VM을 띄워 T로 옮긴다 — 확정 뒤 `spec.node`·T의 QEMU `running`·S의 QEMU 없음, 게스트가 계속 도는지(QMP `query-status`와 마이그레이션 전후 디스크 쓰기), UEFI VM의 변수 파일이 T에 쓰였는지. 확정 전 실패(대상 포트가 막힘)에서 원본이 계속 도는지. NFS·RBD·(되면) 공유 LVM 풀로 한 번씩.

구현: 기능 `migration-it`(opld, `crates/opld/tests/migration.rs`)은 Linux 전용이 아니고 이 Mac에서도 TCG로 돈다(HVF에서는 원본 QEMU가 가끔 `assert_hvf_ok`로 죽는다). 한 호스트의 두 노드는 systemd 스코프 이름과 탭 이름이 겹치므로 스코프를 끄고 NIC 없이 띄운다. 게스트 OS가 없으므로 디스크 내용은 정지한 VM에서 `qemu-io`로 본다. 포트 막힘 실패 뒤 포트를 풀고 되돌아오기까지 본다. NFS·RBD·공유 LVM은 각 풀 기능과 그 시험의 환경 변수가 있을 때(0→1→0 왕복), 공유 LVM은 세 시점에 `lvchange -aey`가 되는지 본다. 2노드 in-process 시험(`opl-api/tests/two_nodes.rs`, MockHypervisor)은 성공·실패·되돌리기를 본다.
- 계획 21: 2노드 실제 시험에서 멈춘 UEFI VM을 옮겨 켰을 때 변수가 같은지(앞 노드에서 `efivar`를 쓰는 대신 변수 파일의 해시로 본다), `PATCH /pools`.

구현(계획 21): `migration-it`의 `uefi_variables_follow_a_stopped_vm`은 로컬로 만든 디렉터리 풀을 `oplctl pool set --shared`로 공유로 바꾸고, node1에서 켠 VM의 `firmware_node`를 확인한 뒤 멈추고 node1 변수 파일 끝에 표식을 쓰고 `PUT`의 `node`로 node2에 옮긴다. 이때 공유 → 로컬 `PATCH`가 409 `pool_in_use`인지 보고, 켜면 node2 파일에 표식이 있고(node1 파일은 남는다) `firmware_node`가 node2인지 본다. 펌웨어가 켤 때 변수를 다시 쓰므로 해시가 아니라 표식으로 본다. 단위·in-process: opl-peer gRPC(조각·0 조각·없음·비회원), reconciler(가짜 fetcher — 받음, 실패해도 켬, 같은 노드면 안 받음, BIOS), `opl-api/tests/two_nodes.rs`(실제 gRPC로 멈춘 VM 옮겨 켜기), `tests/pools.rs`의 `PATCH`.

## 8. 알려진 한계

- 디스크가 모두 공유 풀이어야 한다(로컬 디스크 복사 없음).
- 같은 QEMU 버전·같은 CPU의 노드 사이만 된다. `-cpu host`이므로 CPU가 다르면 마이그레이션이 실패하거나 게스트가 멈출 수 있다(검사하지 않는다).
- 취소가 없다. 수렴하지 않으면 `auto-converge`가 게스트를 늦추고, 시간 제한(30분)이 지나면 실패로 끝나 원본이 계속 돈다.
- 노드가 죽으면 그 노드에만 있던 UEFI 변수는 잃는다(템플릿으로 켠다). 구현: 계획 21대로 — 다른 노드가 켤 때 경고를 남기고 그 노드의 파일이나 템플릿으로 켠다.
- 스냅샷의 변수 복사본(`snapshots/{id}.vars`)은 RPC로 받지 않는다(변수 파일만). 스냅샷에서 복제하는 Task가 스냅샷을 찍은 노드가 아닌 곳에서 돌면 새 VM은 변수 없이(템플릿으로) 켜진다. 되돌리기 Task가 그런 노드에서 돌면 변수는 되돌리지 않고 지금 변수를 둔다(경고).
- 모든 노드가 같은 펌웨어 빌드(같은 크기의 변수 템플릿)를 써야 변수가 따라간다. 크기가 다르면 받은 파일을 버리고 켜는 노드의 파일(없으면 템플릿)로 켠다.
- VM이 거쳐 간 다른 노드의 변수 파일(받은 쪽은 옛 파일을 지우지 않는다)은 VM을 지워도 그 노드에 남는다.
- 마이그레이션 포트 범위를 노드 사이 방화벽에서 열어야 한다.
- macOS 노드는 마이그레이션을 할 수 없다.
- 공유 LVM 디스크: 계획 20의 확인 결과에 따른다(§3). 구현: 지원한다(마이그레이션 동안 LV 공유 잠금).
- 계획 20 전에 클러스터에 들어온 노드의 인증서는 키 용도에 keyEncipherment가 없어 QEMU 8.2 이하의 마이그레이션 TLS가 실패한다. opld가 시작할 때 경고하고, 노드를 다시 합류시켜야 새 인증서를 받는다.
- 받기 준비 RPC가 기한(120초)에 끊기면 대상에 띄우던 받는 QEMU나 공유로 켠 디스크가 남을 수 있다. 다음 정리(`tag_sweep`, 30분)나 같은 VM의 다음 마이그레이션이 치운다.
- 받기 확인 뒤 확정 전에 대상의 받는 QEMU가 죽으면, 확정 뒤 대상 워커가 VM을 새로 켠다(게스트 메모리 상태를 잃는다, 두 QEMU가 함께 돌지는 않는다).
