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
| 18 | **iSCSI**, **Ceph RBD** | QEMU 네트워크 블록 드라이버(`iscsi`, `rbd`) |

하위 프로젝트 4(라이브 마이그레이션)는 그 뒤다. UEFI 변수를 공유 스토리지로 옮기는 일도 거기서 다룬다.

## 2. 계획 15 목표와 범위

| 항목 | 내용 |
|---|---|
| 백엔드 구조 | 저장소 코드가 `PoolBackend`의 종류를 가정하지 않게, 풀의 루트 디렉터리를 백엔드별로 정한다 |
| NFS 풀 | `PoolBackend::Nfs { server, export, options }`. opld가 풀을 쓸 수 있는 노드마다 마운트한다 |
| 노드별 풀 상태 | 노드가 자기에게서 풀을 쓸 수 있는지·용량을 `NodeStatus.pools`로 알린다 |
| `shared` 바로잡기 | 스케줄러·Task 노드 선택·복제·가져오기가 `shared`와 볼륨이 있는 노드를 본다 |
| 클라이언트 | oplctl `pool create --nfs`, 웹의 풀 만들기·풀 목록(종류, 노드별 상태) |

넣지 않는 것: macOS 노드의 NFS 마운트(macOS는 `dir`만), NFS 서버 관리, 마운트를 opld 밖(fstab)에 맡기는 방식의 자동 감지(그런 경우는 지금처럼 `dir` + `shared: true`), UEFI 변수 공유(하위 프로젝트 4), 블록 백엔드(계획 16 이후).

## 3. 데이터 모델

- `PoolBackend`에 `Nfs { server: String, export: String, options: Option<String> }`를 더한다(serde `{"kind":"nfs", ...}`, `deny_unknown_fields`).
  - `server`: 호스트 이름 또는 IP(공백·`:`·`,` 없음, IPv6는 `[...]` 없이 적고 마운트할 때 감싼다).
  - `export`: `/`로 시작하는 절대 경로.
  - `options`: `mount -o`에 그대로 넘길 쉼표 목록(예: `vers=4.2,hard`). 공백·제어 문자 금지. 없으면 `vers=4.2`가 아니라 커널 기본값을 쓴다(옵션 없음).
- NFS 풀은 항상 공유다. API는 NFS 풀을 만들 때 `shared`가 없으면 `true`로 두고 `false`면 400 `invalid`로 거절한다.
- `fn is_shared(pool) -> bool`(`opl-types`): `spec.shared || backend가 Nfs`.
- `NodeStatus.pools: BTreeMap<String, PoolState>`(풀 이름 → 상태, `#[serde(default)]`):
  - `PoolState { ready: bool, error: Option<String>, total_bytes: u64, available_bytes: u64 }`.
  - 노드는 쓸 수 있는 풀(`nodes`가 비었거나 자기를 포함)만 적는다. 없는 풀 이름은 지운다.
- `StoragePoolStatus`의 `total_bytes`·`available_bytes`는 지금처럼 비워 둔다(노드별 값이 기준). 웹·oplctl은 노드별 값을 보인다.

## 4. 풀 루트와 저장소 드라이버

- 저장소 함수들은 풀 스펙 대신 풀 리소스(`&StoragePool`, id·이름 포함)를 받는다. 기존 `&StoragePoolSpec` 인자를 모두 바꾼다(기계적 변경).
- `opl-driver::storage::pool_root(pool, mount_root) -> PathBuf`:
  - `Dir { path }` → `path`
  - `Nfs { .. }` → `{mount_root}/{pool.id}`
- `DirStorage::new(qemu_img, mount_root)`. `mount_root`는 opld의 `{data_dir}/pools`. 파일 배치(`{root}/{id}.qcow2|raw`), 하드 링크로 넣기, qemu-img 사용, qcow2 내부 스냅샷은 그대로다(NFSv3·v4 모두 하드 링크를 지원한다).
- reconciler의 디스크 경로(`resolve.rs`)도 `pool_root`를 쓴다. 디스크는 여전히 파일 경로다.
- 리소스 API의 `PoolBackend::Dir` 분해는 `match`로 바꾸고 백엔드별로 검증한다(`Dir`: 절대 경로, `Nfs`: 위 규칙).

## 5. NFS 마운트 (opld)

`crates/opld/src/pools.rs`의 `PoolMounter`. 노드마다 반복 작업 하나다(브리지 새로 고침처럼 `tick`, 기본 15초, `[pools] refresh_secs`).

- tick마다 풀 목록을 읽고 이 노드가 쓸 수 있는 풀마다:
  - `Dir`: 디렉터리가 있고 쓸 수 있으면 `ready`, 용량은 statvfs.
  - `Nfs`: 마운트 지점 `{data_dir}/pools/{id}`를 만들고, `/proc/self/mountinfo`에서 그 지점에 `nfs`/`nfs4` 마운트가 있는지 본다. 없으면 `mount -t nfs [-o options] server:export <dir>`(IPv6는 `[server]:export`)를 시간 제한(30초)을 두고 부른다. 실패하면 `ready: false`, `error`에 stderr 첫 줄. 마운트돼 있으면 쓰기 확인(작은 파일 만들고 지우기) 뒤 `ready`, 용량은 statvfs.
- 이 노드가 쓸 수 없게 된 NFS 풀(지워졌거나 `nodes`에서 빠짐)의 마운트 지점은 `umount`하고 디렉터리를 지운다. 그 풀의 디스크를 쓰는 QEMU가 이 노드에서 돌고 있으면 언마운트하지 않고 다음 tick에 다시 본다.
- 결과를 `NodeStatus.pools`에 쓴다(바뀐 경우만, 자기 노드 리소스라 쓰는 쪽은 하나다. 리비전 충돌이면 다시 읽고 다시 쓴다).
- opld가 내려갈 때 마운트는 그대로 둔다(돌던 VM이 없고, 다시 올라오면 그대로 쓴다).
- **macOS:** NFS 풀은 마운트하지 않고 `ready: false`, `error: "nfs pools are not supported on macOS"`.
- 마운트는 root 권한이 필요하다. opld가 root가 아니면 `mount`가 실패하고 그 메시지가 `error`에 남는다.

## 6. 배치와 Task 노드 (`shared` 바로잡기)

공통 도우미 `fn pool_usable_on(pool, node) -> bool`: `nodes`가 비었거나 노드를 포함하고, 노드의 `status.pools[pool.name].ready`가 참.

- **스케줄러 `reaches_disks`:** VM의 디스크 볼륨마다 `pool_usable_on(pool, node)`이고, 풀이 공유가 아니며 `volume.status.node`가 있으면 그 노드여야 한다. (지금은 `nodes`가 빈 로컬 풀에서 파일이 없는 노드를 고를 수 있었다.)
- **볼륨 Task 노드(`pool_node`, `volume_node`):** 볼륨 만들기는 이 노드에서 풀을 쓸 수 있으면 이 노드, 아니면 쓸 수 있는 노드 가운데 첫째(id 순). 쓸 수 있는 노드가 없으면 409 `pool_unavailable`. 기존 볼륨의 작업(삭제·크기)은 공유가 아니면 `volume.status.node`, 공유면 위와 같은 규칙.
- **가져오기 확인(`check_import`)·복제 대상 풀:** `pool_not_on_node`의 조건을 `pool_usable_on`으로 바꾼다. 풀이 노드 목록에는 있지만 준비되지 않았으면 409 `pool_unavailable`(메시지에 노드 상태의 `error`).
- **스냅샷의 `check_local`:** `is_shared`를 쓴다(NFS도 통과).
- **VM 시작(reconciler):** 디스크 풀을 이 노드에서 쓸 수 없으면 지금의 `PoolNotOnNode`처럼 시작 실패로 남긴다(메시지에 풀 상태 오류).
- 옛 노드는 `pools`가 비어 있다. 혼합 버전 클러스터는 지원하지 않는다(계획 13과 같다). 다만 `Dir` 풀은 새 opld가 첫 tick에 채우므로, 올린 직후 몇 초 동안은 배치 후보가 없을 수 있다.

## 7. API·클라이언트

- 풀 API는 그대로이고 `PoolBackend`의 새 종류와 `NodeStatus.pools`가 OpenAPI에 나온다. 새 오류 code `pool_unavailable`(409).
- **oplctl:** `pool create NAME --nfs SERVER:/EXPORT [--nfs-options OPTS]`(기존 `--path`와 함께 쓸 수 없다), `pool list`에 종류와 "준비된 노드 수/쓸 수 있는 노드 수", `pool show`(또는 기존 상세)에 노드별 상태·용량·오류.
- **웹:** 풀 만들기 대화상자에 종류(디렉터리/NFS) 선택과 NFS 필드(서버, 내보내기 경로, 옵션), 풀 목록에 종류 열과 노드별 상태(준비/오류 이유), 용량은 노드별 값(공유 풀은 첫 준비된 노드의 값). 노드 상세에 풀 상태. 문구는 en·ko.

## 8. 오류 처리

- 마운트 실패·시간 초과는 노드 상태의 `error`로만 남기고 다음 tick에 다시 시도한다(로그는 상태가 바뀔 때 한 번).
- NFS 서버가 멈추면 statvfs·쓰기 확인이 멈출 수 있다. 확인은 별도 blocking 작업에 시간 제한(10초)을 두고, 넘으면 `ready: false`, `error: "nfs server not responding"`. 멈춘 blocking 작업은 버린다(다음 tick은 앞 작업이 끝나지 않았으면 그 풀을 건너뛴다).
- 풀 삭제는 지금처럼 볼륨이 남아 있으면 409다. 지운 NFS 풀은 각 노드의 다음 tick에서 언마운트된다.

## 9. 테스트

- **단위:** `PoolBackend::Nfs` 직렬화·검증, `is_shared`, `pool_root`, 마운트 인자 만들기(IPv6 포함), mountinfo 해석, `pool_usable_on`, 스케줄러(로컬 풀은 볼륨 노드로, 공유 풀은 준비된 노드로, 준비 안 된 노드 제외), `pool_node`/`volume_node`, API(NFS 풀 만들기·`shared: false` 거절·`pool_unavailable`).
- **opld:** `PoolMounter`를 가짜 마운트 명령(테스트용 trait)으로 돌려 상태 쓰기·언마운트·macOS 메시지를 본다.
- **실제 NFS(`nfs-it` 기능, Linux만):** CI `linux-kvm` 작업에 `nfs-kernel-server`를 깔고 임시 디렉터리를 `localhost`로 내보낸 뒤(sudo), opld 통합 테스트가 NFS 풀을 마운트하고 볼륨을 만들고 qcow2 스냅샷을 찍고 지운 뒤 언마운트되는지 본다. macOS 개발 환경에서는 건너뛴다.
- 기존 시험(워크스페이스, `storage-it`, `qemu-it`, `web-it`, 웹)이 그대로 통과한다.

## 10. 알려진 한계

- NFS 마운트는 Linux 노드만, opld가 root일 때만 된다.
- UEFI 변수는 노드 로컬이다. 공유 풀의 VM이 다른 노드에서 시작되면 새 UEFI 변수로 부팅한다(지금과 같다, 하위 프로젝트 4에서 다룬다).
- NFS의 qcow2 잠금은 QEMU의 OFD 잠금에 기대며 NFSv4에서만 노드 사이에 동작한다. 같은 볼륨을 두 노드에서 동시에 쓰는 일은 배치 규칙이 막는다.
- 노드별 풀 상태는 tick 간격(기본 15초)만큼 늦다.
