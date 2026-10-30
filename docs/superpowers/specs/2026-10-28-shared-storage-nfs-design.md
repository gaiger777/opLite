# opLite 공유 스토리지 설계 1: 백엔드 구조와 NFS (계획 15)

작성일: 2026-10-28
상위 문서: `2026-10-20-opl-node-core-design.md`(§6 스토리지), `2026-10-26-snapshots-design.md`

## 1. 하위 프로젝트 3 나머지의 분해

공유 스토리지(NFS, LVM-thin, ZFS, iSCSI, Ceph)는 한 계획으로 하기에 크다. 지금 코드는 `dir` 백엔드 하나, 파일 경로로만 넘기는 디스크, qcow2 내부 스냅샷을 전제로 한다. 아래 순서로 나눈다. 각 계획은 혼자서 동작하는 소프트웨어를 낸다.

| 계획 | 범위 | 새로 필요한 바탕 |
|---|---|---|
| **15 (이 문서)** | 백엔드 구조, `shared`의 뜻 바로잡기, 노드별 풀 상태·용량, **NFS** 풀 | 백엔드별 풀 경로, 노드가 알리는 풀 상태 |
| 16 | 블록 백엔드 바탕과 **LVM-thin** | 디스크 소스 추상화(`host_device`), 백엔드 스냅샷, 메모리 스냅샷의 vmstate 위치 |
| 17 | **ZFS**(zvol) | 16의 블록 바탕 |
| 18 | **Ceph RBD** | QEMU 네트워크 블록 드라이버(`rbd`), 네트워크 디스크 소스(`RuntimeDisk.rbd`) |
| 19 | **iSCSI**(공유 LVM) | 공유 LUN 위의 공유(lvmlockd) VG, 16의 블록 디스크 소스, LV 배타 활성화 |

구현: 처음에는 계획 18이 iSCSI와 Ceph RBD를 함께 했으나, 성격이 달라(RBD는 이미지·스냅샷을 API로 다루고 iSCSI는 관리자가 만든 LUN을 쓴다) 계획 18은 Ceph RBD만 하고 iSCSI는 계획 19로 미뤘다(`2026-10-29-shared-storage-ceph-rbd-design.md` §1).

구현: 계획 19는 두 길 가운데 LUN을 그대로 디스크로 쓰는 QEMU `iscsi` 드라이버가 아니라 공유 LUN 위의 공유 LVM(lvmlockd·sanlock, 볼륨마다 두꺼운 LV)을 택했다. LUN을 opLite가 만들거나 늘릴 수 없어 볼륨 모델과 맞지 않기 때문이다. 같은 풀을 iSCSI 로그인 없이 FC·멀티패스 SAN에도 쓴다(`2026-10-30-shared-storage-iscsi-lvm-design.md` §1).

하위 프로젝트 4(라이브 마이그레이션)는 그 뒤다. UEFI 변수를 공유 스토리지로 옮기는 일도 거기서 다룬다.

## 2. 계획 15 목표와 범위

| 항목 | 내용 |
|---|---|
| 백엔드 구조 | 저장소 코드가 `PoolBackend`의 종류를 가정하지 않게, 풀의 루트 디렉터리를 백엔드별로 정한다 |
| NFS 풀 | `PoolBackend::Nfs { server, export, options }`. opld가 풀을 쓸 수 있는 노드마다 마운트한다 |
| 노드별 풀 상태 | 노드가 자기에게서 풀을 쓸 수 있는지·용량을 `NodeStatus.pools`로 알린다 |
| `shared` 바로잡기 | 스케줄러·Task 노드 선택·복제·가져오기가 `shared`와 볼륨이 있는 노드를 본다 |
| 클라이언트 | oplctl `pool create --nfs`, 웹의 풀 만들기·풀 목록(종류, 노드별 상태) |

넣지 않는 것: macOS 노드의 NFS 마운트(macOS는 `dir`만), NFS 서버 관리, 마운트를 opld 밖(fstab)에 맡기는 방식의 자동 감지(그런 경우는 `dir` + `shared: true`, oplctl `--shared`·웹 "공유 스토리지" 체크), UEFI 변수 공유(하위 프로젝트 4), 블록 백엔드(계획 16 이후).

## 3. 데이터 모델

- `PoolBackend`에 `Nfs { server: String, export: String, options: Option<String> }`를 더한다(serde `{"kind":"nfs", ...}`, `deny_unknown_fields`).
  - `server`: 호스트 이름 또는 IP(공백·`:`·`,` 없음, IPv6는 `[...]` 없이 적고 마운트할 때 감싼다). 구현: IPv6 주소로 읽히면 통과, 아니면 비어 있지 않고 `-`로 시작하지 않으며(`mount`가 옵션으로 읽지 않게) 공백·`:`·`,`·`/`·`[`·`]` 없는 ASCII 출력 문자.
  - `export`: `/`로 시작하는 절대 경로(제어 문자 금지).
  - `options`: `mount -o`에 넘길 쉼표 목록(예: `vers=4.2,hard`). 빈 문자열·공백·제어 문자 금지. 마운트의 성격을 바꾸는 옵션도 받지 않는다: `=` 앞 이름(대소문자 무시)이 `bind`·`rbind`·`remount`·`move`·`suid`·`dev`·`exec`·`helper`·`bg`이거나 `x-`로 시작하는 것(`X-mount.*` 포함). `bg`는 마운트하지 못해도 뒤에서 다시 시도하며 성공으로 돌아오므로 받지 않는다. opld는 늘 `nosuid,nodev,noexec`를 앞에 붙이고 사용자 옵션을 그 뒤에 붙인다. 옵션이 없으면 `vers=4.2`가 아니라 커널 기본값을 쓴다(`nosuid,nodev,noexec`만).
  - 검사는 `StoragePoolSpec::validate() -> Result<(), PoolError>`(`opl-types`, `RelativePath`·`BadServer`·`BadExport`·`BadOptions`)이고, API가 풀을 만들 때 부른다. 거절은 400 `invalid_request`다.
- NFS 풀은 항상 공유다. API는 NFS 풀을 만들 때 `shared`가 없으면 `true`로 두고 `false`면 400으로 거절한다. 오류 code는 따로 만들지 않고 기존 `invalid_request`다(이 문서 처음의 `invalid`는 없는 code였다). `shared`를 적었는지 알아야 하므로 `create_pool`은 spec을 JSON 값으로 받아 키를 본 뒤 `StoragePoolSpec`으로 읽는다.
- `fn is_shared(pool) -> bool`(`opl-types`): `spec.shared || backend가 Nfs`.
- `NodeStatus.pools: BTreeMap<String, PoolState>`(풀 이름 → 상태, `#[serde(default)]`):
  - `PoolState { pool_id: Uuid, ready: bool, error: Option<String>, total_bytes: u64, available_bytes: u64 }`. `pool_id`는 상태를 적을 때의 풀 id다(serde default, 이 필드 전의 상태는 nil). 같은 이름으로 지우고 다시 만든 풀에 옛 상태를 쓰지 않게, 읽는 쪽(`pool_state_on`과 그것을 쓰는 `pool_usable_on`·`pool_error_on`, reconciler, oplctl, 웹)은 `pool_id`가 풀 id와 같을 때만 그 상태를 믿고, 다르면 "알리지 않음"으로 본다.
  - 노드는 쓸 수 있는 풀(`nodes`가 비었거나 자기를 포함)만 적는다. 없는 풀 이름은 지운다.
  - opld는 Node 리소스를 등록할 때 이미 적힌 `pools`를 그대로 두고(다시 시작해도 지우지 않는다, 확인된 쓰기 `update_node_status`), `PoolMounter`가 채운다.
- `StoragePoolStatus`의 `total_bytes`·`available_bytes`는 지금처럼 비워 둔다(노드별 값이 기준). 웹·oplctl은 노드별 값을 보인다.

## 4. 풀 루트와 저장소 드라이버

- 저장소 함수들은 풀 스펙 대신 풀 리소스(`&StoragePool`, id·이름 포함)를 받는다. 기존 `&StoragePoolSpec` 인자를 모두 바꾼다(기계적 변경).
- `opl-driver::storage::pool_root(pool, mount_root) -> PathBuf`:
  - `Dir { path }` → `path`
  - `Nfs { .. }` → `{mount_root}/{pool.id}`
- `DirStorage::new(qemu_img, mount_root)`. `mount_root`는 opld의 `{data_dir}/pools`. 파일 배치(`{root}/{id}.qcow2|raw`), 하드 링크로 넣기, qemu-img 사용, qcow2 내부 스냅샷은 그대로다(NFSv3·v4 모두 하드 링크를 지원한다).
- 볼륨 파일 경로는 `DirStorage::volume_path(pool, mount_root, id, format)`(정적 함수)로 만든다.
- **마운트 확인.** `check_pool_root(pool, mount_root) -> Result<PathBuf, DriverError>`(공개)는 NFS 풀이면 `{canonicalize한 mount_root}/{id}`가 `/proc/self/mountinfo`의 `nfs`/`nfs4` 마운트 지점인지 보고, 아니면(mountinfo를 읽을 수 없거나 Linux가 아니어도) `DriverError::PoolNotMounted(path)`다. 마운트 지점 자체는 stat하지 않는다(처음엔 `st_dev`를 부모와 비교했지만 NFS 서버가 멈추면 stat이 끝나지 않는다). mountinfo 해석은 `opl_driver::mounts::nfs_mount_points`이고 opld의 마운터도 이것을 쓴다. 볼륨 파일이 없다는 결과(`VolumeNotFound`)는 다시 확인해 그사이 마운트가 풀렸으면 `PoolNotMounted`로 돌려준다(빈 마운트 지점을 보고 "없음"으로 판단하지 않게). 이 오류는 일시 오류(`is_transient`)다. `DirStorage`의 모든 진입점(만들기·가져오기·지우기·복사·스냅샷·크기·정보·용량)이 이 확인을 거쳐, 마운트되지 않은 빈 마운트 지점(로컬 디스크)에 볼륨을 만들거나 그 자리를 디스크로 쓰지 않는다. 리뷰에서는 `StorageError`라는 이름이 나왔지만 드라이버의 오류 타입은 `DriverError` 하나라 그 변형으로 두었다.
- **멈춘 마운트.** hard 마운트한 NFS의 서버가 멈추면 풀 경로의 파일시스템 호출(있는지 보기, 하드 링크, 지우기, 용량, 임시 파일 정리)이 끝나지 않는다. `DirStorage`는 이런 호출을 `spawn_blocking`으로 돌리고 `FS_TIMEOUT`(30초)이 지나면 기다리지 않고 `DriverError::PoolNotResponding(풀 루트)`(일시 오류)를 돌려준다(tokio 워커를 붙잡지 않는다. 끝나지 않은 호출은 blocking 스레드에 남는다). 임시 파일은 결과와 상관없이 기다려서 지우고, 취소돼 drop되면 blocking 스레드에서 지운다. qemu-img는 원래 비동기 프로세스라 그대로다. 디렉터리 풀도 같은 길을 지나며 동작은 같다. 시작할 때의 고아 볼륨 정리는 `PoolNotMounted`처럼 다시 시도한다.
- reconciler의 디스크 경로(`resolve.rs`)도 `pool_root`를 쓴다. 디스크는 여전히 파일 경로다. `mount_root`는 `ReconcilerConfig::new(node, mount_root)`의 `mount_root` 필드로 받아 `resolve::runtime_spec(cluster, vm, node, mount_root)`에 넘긴다.
- 리소스 API의 `PoolBackend::Dir` 분해는 `match`로 바꾸고 백엔드별로 검증한다(`Dir`: 절대 경로, `Nfs`: 위 규칙).

## 5. NFS 마운트 (opld)

`crates/opld/src/pools.rs`의 `PoolMounter`. 노드마다 반복 작업 하나다(브리지 새로 고침처럼 `tick`, 기본 15초, `[pools] refresh_secs`).

- 명령과 파일시스템은 `MountOps` trait(`mountinfo`, `mount`, `umount`, `check`) 뒤에 둔다. 실제 구현은 `SystemMount`이고 테스트는 가짜를 쓴다. 마운트 인자는 `mount_args`, mountinfo 해석은 드라이버의 `opl_driver::mounts::nfs_mount_points`(마운트 지점의 `\040` 같은 8진수 이스케이프를 되돌린다)다. 마운트 지점은 실제 경로(`canonicalize`한 `mount_root` 아래)로 비교한다.
- 풀 리소스를 구독해 풀이 만들어지거나 지워지면 tick을 기다리지 않고 곧바로 새로 고친다(배치와 볼륨 Task가 준비된 노드만 고르므로 만든 풀을 바로 쓸 수 있게). 구독이 끊기면 다음 새로 고침 때 다시 구독한다.
- tick마다 풀 목록을 읽고 이 노드가 쓸 수 있는 풀마다:
  - `Dir`: 디렉터리가 있고 쓸 수 있으면 `ready`, 용량은 statvfs. 쓰기 확인은 NFS와 같은 `check`(`.opl-check-{노드 id}` 파일을 만들고 지운다)이고 같은 10초 제한이다(넘으면 `"pool directory not responding"`). 디렉터리가 없으면 준비되지 않은 것이다(opld가 만들지 않는다).
  - `Nfs`: 마운트 지점 `{data_dir}/pools/{id}`를 만들고, `/proc/self/mountinfo`에서 그 지점에 `nfs`/`nfs4` 마운트가 있는지 본다. 없으면 `mount -t nfs -o nosuid,nodev,noexec,retry=0[,options] server:export <dir>`(IPv6는 `[server]:export`)를 시간 제한(30초)을 두고 부른다. `retry=0`은 시간 제한으로 `mount`를 끝낸 뒤 mount.nfs가 앞에서 계속 다시 시도하지 않게 한다(다시 시도는 백오프로 마운터가 한다). 사용자 옵션 앞에 붙어 사용자가 바꿀 수 있다. 실패하면 `ready: false`, `error`에 stderr 첫 줄. 마운트돼 있으면 쓰기 확인(작은 파일 만들고 지우기) 뒤 `ready`, 용량은 statvfs. 마운트 지점 디렉터리는 마운트되지 않았을 때만 만든다(멈춘 NFS 마운트 지점은 stat도 멈춘다).
- **풀마다 따로.** 마운트·언마운트는 풀마다 별도 작업으로 돌고, tick은 기다리지 않는다. 진행 중인 마운트가 있는 풀은 끝나기 전에 새로 마운트하지 않고 `error: "mount in progress"`로 알린다(진행 중인 언마운트도 다시 시작하지 않고 로그만 남긴다). 언마운트 중인 풀을 다시 쓰게 되면 mountinfo에 아직 보여도 준비되지 않은 것(`"mount in progress"`)으로 알리고, 언마운트가 끝난 뒤 다시 마운트한다. 작업이 끝나면 곧바로 새로 고쳐 결과를 적는다. 쓰기 확인도 풀마다 동시에 시작해 같은 10초 안에서 기다린다. 그래서 멈춘 NFS 서버 하나가 다른 풀의 상태 기록(즉시 새로 고침 포함)을 늦추지 않는다(처음 멈춘 확인이 최대 10초, 그 뒤로는 건너뛴다).
- **백오프.** 마운트가 실패하면 그 풀의 다음 시도를 15초부터 실패할 때마다 두 배(최대 5분) 미룬다. 그동안 상태는 마지막 오류다. 마운트되면 백오프를 잊는다.
- 이 노드가 쓸 수 없게 된 NFS 풀(지워졌거나 `nodes`에서 빠짐)의 마운트 지점은 `umount`하고 디렉터리를 지운다. 그 풀의 디스크를 쓰는 QEMU가 이 노드에서 돌고 있으면 언마운트하지 않고 다음 tick에 다시 본다. 판단은 클러스터 상태로 한다: `status.node`가 이 노드이고 phase가 `starting`·`running`·`stopping`인 VM의 디스크 볼륨이 있는 풀. `mount_root` 아래 풀 id 이름의 마운트 지점만 건드리고, 마운트되지 않은 채 남은 빈 마운트 지점도 지운다. 언마운트 실패는 같은 메시지면 한 번만 기록한다. `umount -f`는 쓰지 않는다(쓰던 데이터를 잃을 수 있다). (풀 API에 수정이 없으므로 `nodes`에서 빠지는 경우는 지금은 없다.)
- 결과를 `NodeStatus.pools`에 쓴다(바뀐 경우만, 리비전 충돌이면 다시 읽고 다시 쓴다. `update_status_checked`). 다른 것은 그대로이고 용량만 앞서 적은 값과 1 GiB 넘게 바뀌지 않았으면 앞 값을 둔다(`keep_capacity`, 쓰기·지우기로 조금씩 바뀌는 용량 때문에 tick마다 다시 쓰지 않게). 준비 여부나 오류가 바뀔 때만 로그를 남긴다.
- Node status를 쓰는 다른 곳도 리비전을 확인하며 써서 `pools`를 지우지 않는다: 브리지 기록과 opld 종료 때의 오프라인 표시는 `bootstrap::update_node_status`(다시 읽고 고쳐 확인된 쓰기, 충돌이면 다시), 리더의 생존 감시(`opl-peer` monitor)는 충돌이면 쓰지 않고 다음 핑에서 다시 본다. 처음 설계의 "쓰는 쪽은 하나"는 맞지 않았다.
- 설정 `[pools] refresh_secs`(기본 15)는 0이면 opld가 시작하지 않는다. 시간 제한은 상수 `MOUNT_TIMEOUT`(30초, `umount`에도 쓴다)·`CHECK_TIMEOUT`(10초)이다.
- opld가 내려갈 때 마운트는 그대로 둔다(돌던 VM이 없고, 다시 올라오면 그대로 쓴다).
- **macOS:** NFS 풀은 마운트하지 않고 `ready: false`, `error: "nfs pools are not supported on macOS"`.
- 마운트는 root 권한이 필요하다. opld가 root가 아니면 `mount`가 실패하고 그 메시지가 `error`에 남는다.

## 6. 배치와 Task 노드 (`shared` 바로잡기)

공통 도우미 `fn pool_usable_on(pool, node) -> bool`: `nodes`가 비었거나 노드를 포함하고, 노드가 알린 그 풀의 상태(`pool_id`가 같은 것)가 `ready`. 디렉터리 풀은 노드가 상태를 아직 알리지 않았으면(항목 없음, 또는 `pool_id`가 다름) 쓸 수 있는 것으로 보고 `ready: false`를 알렸을 때만 막는다. NFS 풀은 `ready: true`를 알려야 한다.

- **스케줄러 `reaches_disks`:** VM의 디스크 볼륨마다 `pool_usable_on(pool, node)`이고, 풀이 공유가 아니며 `volume.status.node`가 있으면 그 노드여야 한다. (지금은 `nodes`가 빈 로컬 풀에서 파일이 없는 노드를 고를 수 있었다.)
- 도우미는 `opl-types::storage`의 `pool_listed_on(pool, node_id)`(노드 목록만), `pool_state_on(pool, &Node)`(`pool_id`가 같은 상태), `pool_usable_on(pool, &Node)`, `pool_error_on(pool, &Node)`(노드가 알린 오류, 상태가 없으면 `"no status reported yet"`)이다. 상태를 알리지 않은 노드는 NFS 풀에는 준비되지 않은 것이고 디렉터리 풀에는 준비된 것이다. 스케줄러·`pool_node`·`check_pool_on`이 모두 이 규칙을 따른다.
- **볼륨 Task 노드(`pool_node`, `volume_node`):** 볼륨 만들기는 이 노드에서 풀을 쓸 수 있으면 이 노드, 아니면 쓸 수 있는 **온라인** 노드 가운데 첫째(id 순). 쓸 수 있는 노드가 없으면 409 `pool_unavailable`. 기존 볼륨의 작업(삭제·크기)은 공유가 아니면 `volume.status.node`, 공유면 위와 같은 규칙. 그 노드가 Node 리소스로 없으면(클러스터에서 빠짐) 풀이 없어졌을 때처럼 이 노드다(파일이 없으니 지우기는 `VolumeNotFound`로 리소스만 지운다).
- **VM의 디스크 작업 노드(`vm_placement`):** `spec.node`, `status.node`, 배치된 적이 없으면 첫 디스크 볼륨의 `status.node`. 첫 디스크가 공유 풀이면 볼륨을 만든 노드 대신 `pool_node`와 같은 규칙(이 노드가 풀을 쓸 수 있으면 이 노드, 아니면 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드)이다(`volumes::usable_node`). Task 실행기의 끝나지 않은 스냅샷 정리는 노드마다 같은 답이 나오도록 이 노드를 앞세우지 않는다.
- **가져오기 확인(`check_import`)·복제 대상 풀:** 노드 목록에 없으면 지금처럼 409 `pool_not_on_node`, 노드 목록에는 있지만 준비되지 않았으면 409 `pool_unavailable`(메시지에 노드 상태의 `error`). 공통 확인은 `volumes::check_pool_on`이다. 복제 Task도 실행할 때 이 노드에서 대상 풀이 준비됐는지 다시 본다(마운트되지 않은 NFS 풀의 빈 마운트 지점에 복사본을 만들지 않게).
- **스냅샷의 `check_local`:** `is_shared`를 쓴다(NFS도 통과).
- **VM 시작(reconciler):** 디스크 풀을 이 노드에서 쓸 수 없으면 `ResolveError::PoolUnavailable`(메시지에 풀 상태 오류)이고, 이것은 **일시 오류**다: VM을 끄지 않고(`power`는 그대로) 실패로 세지도 않으며, `retry_interval`(기본 2초) 뒤 다시 시도한다(phase `starting`, `last_error`에 이유). 풀이 돌아오면 시작한다. 노드가 그 풀을 `ready: false`로 알렸으면 거절하고, 아직 알리지 않았으면(opld가 막 시작해 첫 새로 고침 전) 클러스터 상태로는 막지 않는다. NFS 디스크는 그와 별도로 `check_pool_root`로 이 노드에 실제로 마운트돼 있는지 확인한다(마운트되지 않았으면 `PoolUnavailable`). 노드 목록에 없는 풀(`PoolNotOnNode`)·없는 볼륨 같은 참조 오류는 지금처럼 끄고 실패로 남긴다. NFS를 지원하지 않는 노드(macOS)에 놓인 NFS 풀 디스크는 기다려도 풀리지 않으므로 `ResolveError::NfsUnsupported`(참조 오류, `cfg!(target_os = "linux")`로 가린다)로 끄고 이유를 남긴다(노드가 알린 `"nfs pools are not supported on macOS"` 문자열은 보지 않는다).
- **시작할 때의 고아 볼륨 정리(Task 실행기):** 풀이 아직 마운트되지 않았거나 응답하지 않아(`PoolNotMounted`·`PoolNotResponding`) 지우지 못한 볼륨은 남겨 두고 `TaskRunnerConfig::orphan_retry`(기본 1분)마다 그 볼륨만 다시 지워 본다. 지웠거나 볼륨이 없어지면 목록에서 뺀다.
- 스케줄러에서 볼륨에 `status.node`가 없으면(아직 만들어지지 않음) 노드를 제한하지 않는다.
- 옛 노드는 `pools`가 비어 있다. 혼합 버전 클러스터는 지원하지 않는다(계획 13과 같다). 디렉터리 풀은 알리지 않은 노드도 쓸 수 있다고 보므로 올린 직후에도 배치 후보가 있다. NFS 풀은 첫 마운트가 끝날 때까지 후보가 없다.

## 7. API·클라이언트

- 풀 API는 그대로이고 `PoolBackend`의 새 종류와 `NodeStatus.pools`가 OpenAPI에 나온다. 새 오류 code `pool_unavailable`(409).
- **oplctl:** `pool create NAME --nfs SERVER:/EXPORT [--nfs-options OPTS]`(기존 `--path`와 함께 쓸 수 없다, IPv6는 `[ADDR]:/EXPORT`), `pool create NAME --path DIR --shared`(밖에서 마운트한 공유 디렉터리, `"shared": true`를 보낸다. NFS 풀은 늘 공유라 `--nfs`와 함께 쓸 수 없다), `pool list`에 종류와 "준비된 노드 수/쓸 수 있는 노드 수"(열 `NAME KIND LOCATION NODES READY`), 기존 상세 `pool get`의 기본 출력을 풀 정보와 노드별 표(`NODE READY SIZE AVAILABLE ERROR`)로 바꾼다. `-o json`은 풀 리소스 그대로다. 노드는 풀의 `nodes`를 그 순서대로 따르고(비었으면 모든 노드), 목록에 있지만 없는 노드는 그 id와 `node not found`로 보이며 분모에 센다. 오프라인 노드는 `node offline`이고 준비로 세지 않는다. `pool_id`가 다른 상태는 알리지 않은 것으로 본다.
- **웹:** 풀 만들기 대화상자에 종류(디렉터리/NFS) 선택과 NFS 필드(서버, 내보내기 경로, 옵션), 디렉터리 풀의 "공유 스토리지" 체크(고르면 `shared: true`), 풀 목록에 종류 열과 노드별 상태(준비/오류 이유), 용량은 노드별 값(공유 풀은 첫 준비된 노드의 값, 로컬 풀은 준비된 노드마다). 웹에는 노드 상세 화면이 없으므로 노드 목록에 "스토리지 풀" 열을 둔다. 오류 문구 `errors.pool_unavailable`. 문구는 en·ko. 도우미는 `web/src/storage/pools.ts`(`poolLocation`, `isShared`, `poolNodes`, `poolCapacity`, `poolsUsableOn`, `poolUsableSomewhere`, `nfsExportError`, `nfsOptionsError`)와 `PoolStatus.tsx`다. `poolsUsableOn(pools, nodes, nodeId)`는 서버의 `pool_usable_on`과 같은 규칙(노드 목록, `pool_id`가 같은 상태의 `ready`, 디렉터리 풀은 알리지 않았어도 됨)에 노드가 온라인일 것을 더한다. 이미지 업로드 대화상자는 접속한 노드에서, 복제 대화상자는 VM이 있는 노드에서 쓸 수 있는 풀만 보이고(복제가 `pool_unavailable`로 거절되면 풀 입력란의 오류로 보인다), 볼륨 만들기 대화상자는 온라인 노드 가운데 어디서든 쓸 수 있는 풀(`poolUsableSomewhere`)만 보인다. 오프라인 노드의 풀 상태는 "오프라인"이고, 용량은 온라인이고 준비된 노드의 값이다. 풀 만들기의 내보내기·옵션 검사는 서버와 같다(제어 문자, 옵션 거부 목록). `errors.pool_unavailable`은 서버 메시지(`{{message}}`) 그대로다.

## 8. 오류 처리

- 마운트 실패·시간 초과는 노드 상태의 `error`로만 남기고, 백오프(15초부터 두 배, 최대 5분) 뒤 다시 시도한다(로그는 상태가 바뀔 때 한 번).
- NFS 서버가 멈추면 statvfs·쓰기 확인이 멈출 수 있다. 확인은 별도 blocking 작업에 시간 제한(10초)을 두고, 넘으면 `ready: false`, `error: "nfs server not responding"`. 멈춘 blocking 작업은 버린다(다음 tick은 앞 작업이 끝나지 않았으면 그 풀을 건너뛴다).
- 풀 삭제는 지금처럼 볼륨이 남아 있으면 409다. 지운 NFS 풀은 각 노드의 다음 tick에서 언마운트된다.

## 9. 테스트

- **단위:** `PoolBackend::Nfs` 직렬화·검증, `is_shared`, `pool_root`, 마운트 인자 만들기(IPv6 포함), mountinfo 해석, `pool_usable_on`, 스케줄러(로컬 풀은 볼륨 노드로, 공유 풀은 준비된 노드로, 준비 안 된 노드 제외), `pool_node`/`volume_node`, API(NFS 풀 만들기·`shared: false` 거절·`pool_unavailable`).
- **opld:** `PoolMounter`를 가짜 마운트 명령(테스트용 trait)으로 돌려 상태 쓰기·언마운트·macOS 메시지를 본다.
- **실제 NFS(`nfs-it` 기능, Linux만):** CI `linux-kvm` 작업에 `nfs-kernel-server`를 깔고 임시 디렉터리를 `localhost`로 내보낸 뒤(sudo), opld 통합 테스트가 NFS 풀을 마운트하고 볼륨을 만들고 qcow2 스냅샷을 찍고 지운 뒤 언마운트되는지 본다. macOS 개발 환경에서는 건너뛴다. 구현: `crates/opld/tests/nfs.rs`(opld의 `nfs-it` 기능), 내보낸 디렉터리는 환경 변수 `OPL_TEST_NFS_EXPORT`로 받고(서버 쪽 경로로 파일을 확인한다), 풀은 oplctl로 만든다. CI는 `/srv/opl-nfs`를 `127.0.0.1(rw,sync,no_subtree_check,no_root_squash)`로 내보낸다. NFS 서버를 막 시작했으면 잠금 유예(약 90초) 때문에 첫 실행이 느릴 수 있다.
- 테스트 도우미: opl-api `common::mark_pool_ready`·`set_pool_state`(풀의 `pool_id`를 채운다), `Env::pool_ready`, opld `Daemon::wait_pool_ready`. 디렉터리 풀은 알리지 않아도 쓸 수 있게 되어 이 도우미가 꼭 필요하지 않은 곳도 그대로 두었다. 디렉터리 풀을 쓰는 opld 통합 테스트는 그 디렉터리를 만들어 둔다.
- nfs-it 시험은 실패해도 임시 디렉터리를 지우기 전에 opld를 멈추고 `{data_dir}/pools` 아래 마운트를 언마운트한다(풀지 못하면 임시 디렉터리를 남긴다). 내보내기는 버려도 되는 디렉터리여야 한다.
- 기존 시험(워크스페이스, `storage-it`, `qemu-it`, `web-it`, 웹)이 그대로 통과한다.

## 10. 알려진 한계

- NFS 마운트는 Linux 노드만, opld가 root일 때만 된다.
- UEFI 변수는 노드 로컬이다. 공유 풀의 VM이 다른 노드에서 시작되면 새 UEFI 변수로 부팅한다(지금과 같다, 하위 프로젝트 4에서 다룬다).
- NFS의 qcow2 잠금은 QEMU의 OFD 잠금에 기대며 NFSv4에서만 노드 사이에 동작한다. 같은 볼륨을 두 노드에서 동시에 쓰는 일은 배치 규칙이 막는다.
- 노드별 풀 상태는 tick 간격(기본 15초)만큼 늦다.
- 마운트된 NFS 풀이 있는 노드에서 `{data_dir}`를 통째로 지우면(`rm -rf /var/lib/opl`) 마운트를 따라 들어가 NFS 내보내기의 파일까지 지운다. 노드를 클러스터에서 빼거나 opLite를 지워도 이 마운트는 남는다. 먼저 opld를 멈추고 `{data_dir}/pools` 아래를 언마운트한다.
- 올리기: 공유가 아닌 풀의 VM은 볼륨이 있는 노드에만 배치된다. 공유 스토리지 위에 `shared` 없이 만든 디렉터리 풀은 풀을 바꾸는 API가 없으므로 볼륨을 다른 풀로 옮기고(복제) `--shared`로 다시 만들어야 한다(README).
