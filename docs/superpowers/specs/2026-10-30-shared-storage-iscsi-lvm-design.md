# opLite 공유 스토리지 설계 5: iSCSI와 공유 LVM (계획 19)

작성일: 2026-10-30
상위 문서: `2026-10-29-shared-storage-lvm-thin-design.md`(블록 바탕, LVM 명령), `2026-10-29-shared-storage-ceph-rbd-design.md`(공유 블록 풀의 Task 노드), `2026-10-28-shared-storage-nfs-design.md`(§1 분해, 마운터)

## 1. 목표와 범위

처음 분해(§1, NFS 설계)는 iSCSI를 "18의 네트워크 디스크 소스(`iscsi`) 또는 공유 LUN 위의 블록 바탕"으로 남겼다. 두 길을 견주었다:

- **LUN을 그대로 디스크로(QEMU `driver=iscsi`):** opLite가 LUN을 만들거나 크기를 바꿀 수 없으므로 볼륨 모델(크기로 만들기, 크기 늘리기, 가져오기, 복제)과 맞지 않는다. 볼륨마다 관리자가 LUN을 미리 만들어야 한다.
- **LUN 위의 공유 LVM(채택):** 관리자는 큰 LUN 하나에 공유 VG를 한 번 만들고, opLite가 그 안에 볼륨마다 LV를 만든다. 볼륨 모델이 그대로 맞고, 계획 16의 블록 디스크 소스(`/dev/{vg}/opl-{id}`)를 다시 쓴다. FC·멀티패스 SAN의 공유 LUN에도 같은 풀을 쓸 수 있다(iSCSI 로그인 없이).

여러 노드가 같은 VG의 메타데이터를 바꾸면 VG가 깨진다. 노드 사이 잠금은 LVM이 공식 지원하는 **lvmlockd + sanlock**(공유 VG, `vgcreate --shared`)에 맡긴다. 잠금은 공유 LUN 위의 sanlock 임대로 하므로 opLite 클러스터와 따로 동작하고, LV를 한 노드에서만 켜는 배타 활성화(`-aey`)가 두 노드가 같은 디스크로 VM을 켜는 것도 막는다.

| 항목 | 내용 |
|---|---|
| 풀 | `PoolBackend::LvmShared { vg, iscsi: Option<IscsiTarget { portal, target }> }`(serde `lvm-shared`), **공유**, raw만 |
| 볼륨 | 두꺼운(linear) LV `{vg}/opl-{id}`, 만들 때 0으로 채운다 |
| 디스크 소스 | 계획 16의 블록 장치 `/dev/{vg}/opl-{id}`(`DiskKind::Block`) |
| 활성화 | 쓰는 노드에서만 배타 활성화(`lvchange -aey`), 다 쓰면 끈다(`-an`) — VM이 멈추면 끈다 |
| 마운터 | iSCSI 로그인(`iscsiadm`), 잠금 시작(`vgchange --lockstart`), VG 확인(잠금 종류 `sanlock`/`dlm`) |
| 스냅샷 | 없음(409) |
| 클라이언트 | oplctl `pool create --lvm-shared VG [--iscsi-portal P --iscsi-target IQN]`, 웹의 공유 LVM 풀 |

넣지 않는 것: iSCSI 타깃·LUN·공유 VG 만들기와 lvmlockd·sanlock 설치(관리자), CHAP 인증(타깃의 이니시에이터 ACL로 막는다 — 비밀번호를 풀 리소스에 두지 않는다), LUN을 그대로 디스크로 쓰는 풀, 공유 LVM의 스냅샷과 thin(lvmlockd의 thin은 배타 활성화만 되어 여러 노드에서 쓸 수 없다), 풀을 지울 때 iSCSI 로그아웃, macOS, 라이브 마이그레이션(하위 프로젝트 4).

## 2. 데이터 모델

- `PoolBackend::LvmShared { vg: String, iscsi: Option<IscsiTarget> }`, `IscsiTarget { portal: String, target: String }`(`deny_unknown_fields`).
  - `vg`: LVM-thin과 같은 VG 이름 규칙. 구현: 틀리면 LVM-thin과 같은 `PoolError::BadLvmName`(400 `invalid_request`)이고 모르는 필드는 거절한다(`iscsi` 안의 `chap` 등도).
  - `portal`: `호스트[:포트]` 또는 `[IPv6][:포트]`. 호스트는 NFS `server`와 같은 규칙, 포트는 1–65535. 구현: `PoolError::BadIscsiPortal(portal)`(400). IPv6 주소는 대괄호가 있어야 한다(`fd00::5`처럼 대괄호 없는 주소는 포트와 가릴 수 없어 거절). 대괄호 안은 `Ipv6Addr`로 읽고 포트는 숫자만이다(`+80` 거절). 웹 `iscsiPortalError`(`pools.errors.portal`)도 같되 대괄호 안은 대강만 본다.
  - `target`: `iqn.`·`eui.`·`naa.`로 시작하고 소문자·숫자·`.`·`:`·`-`만, 223바이트 이하. 구현: `PoolError::BadIscsiTarget(target)`(400). 접두사 뒤에 한 글자 이상 있어야 한다. 웹 `iscsiTargetError`(`pools.errors.target`), oplctl은 서버의 400에 맡긴다.
- 공유 LVM 풀은 항상 공유(`is_shared` 참, `shared: false`는 400), raw만(qcow2는 400), `pool_kind_supports_snapshots`는 거짓. 구현: 문구는 "lvm-shared pools are always shared"(`create_pool`이 NFS·RBD와 함께 본다), "lvm-shared pools hold raw volumes"(`check_pool_format`, 드라이버도 같은 문구로 거절). 블록 풀인지는 `opl-api` `volumes::block_pool_kind(&StoragePool) -> Option<&'static str>`에 모았다(형식 검사, 만들기·가져오기의 형식 기본값 raw, 복제의 `copy_format`, 실행 중 크기 늘리기). `uses_block_snapshots`는 블록 스냅샷 종류만 본다. `pool_usable_on`은 NFS·RBD처럼 준비됐다는 상태가 있어야 참이다. `pool_root`는 `/dev/{vg}`이고 파일 저장소는 공유 LVM 풀을 거절한다("lvm-shared pools hold no volume files").
- 블록 스냅샷 종류는 없다. 공유 LVM 디스크가 있는 VM의 스냅샷은 409 `snapshot_unsupported`(스냅샷을 지원하지 않는 디스크의 기존 규칙). 예약 스냅샷도 그 기존 규칙을 따른다. 구현: `block_snapshot_kind`는 `None`이다. 기존 문구("only qcow2 disks in storage pool … can be snapshotted")는 맞지 않으므로 공유 LVM 디스크는 "VM … has disk … that cannot be snapshotted; disks in storage pool … cannot be snapshotted"다. 웹은 `SnapshotSupport`에 `{ kind: "pool"; disk }`를 더해 스냅샷 버튼을 이유(`snapshots.sharedLvm`)와 함께 막고, 예약을 켜면 `vmForm.scheduleSharedLvm`으로 알린다. 스냅샷이 없으므로 실행 중인 VM은 복제할 수 없다(멈춘 VM의 지금 상태로만).

## 3. 드라이버 (`storage/lvm.rs`의 공유 LVM)

계획 16의 `LvmOps`·`BlockTool`을 그대로 쓰고 두꺼운 LV 명령을 더한다(같은 LV에는 명령 하나, 바꾸는 명령은 죽이지 않는다). 구현: `LvmSharedStorage::new(qemu_img, Arc<dyn LvmOps>)`, 연결은 `Storages::with_lvm_shared`(opld `member.rs`, LVM-thin과 같은 `SystemLvm`). `LvmOps`에 `create_linear`·`activate_exclusive`·`zero`·`vg_info`를 더했다. `VgInfo { name, lock_type, size_bytes, free_bytes }`와 해석기 `parse_vgs`(`vgs --reportformat json --units b --nosuffix -o vg_name,vg_lock_type,vg_size,vg_free`, 마운터도 쓴다)는 공개다.

- 만들기: `lvcreate -q -an -L {bytes}b -n opl-{id} {vg}`(1 MiB 배수로 올림, 만들 때 켜지 않는다) → 배타 활성화 → `blkdiscard -z`로 0 채우기(SAN이 WRITE SAME·UNMAP으로 빨리 할 수 있다, 시간 제한 없음) → 끈다. 실패하면 지운다. 0으로 채웠으므로 `target_reads_zero`는 참. 구현: 켜지 않고 만들므로 `lvcreate`에 `-Zn -Wn`을 준다(처음 부분 0 채우기·서명 지우기는 켜야 할 수 있다 — 0 채우기는 뒤의 `blkdiscard`가 한다). `blkdiscard -f -z`(`-f`: 옛 데이터의 서명이 있어도 묻지 않는다). 이미 있으면 `VolumeExists`. 만든 뒤 끄지 못하면 경고만 남긴다(§4의 정리가 끈다). `create_volume`은 장치 경로를 돌려주지만 그 노드는 켜야 생긴다.
- 가져오기·복사의 대상: 만들기(0 채움) → 배타 활성화 → `qemu-img convert -n --target-is-zero` → 끈다. 구현: 가져오기는 원본의 가상 크기로 만들고 `-O raw`로 쓴다. 쓰다 실패하면 LV를 지운다. 같은 백엔드 복사는 `LvmSharedStorage::copy_volume`(원본 LV를 켜서 읽고, 취소돼도 끈다). 다른 백엔드에서 오는 복사는 `Storages`의 백엔드 사이 복사가 대상이 공유 LVM이면 만든 볼륨을 쓰는 동안만 켜고(`activate_volume`/`deactivate_volume`) `target_reads_zero`가 참이라 `--target-is-zero`로 쓴다.
- 지우기: `lvremove`. 다른 노드가 켜 두었으면 lvmlockd가 거절한다(`LV locked by other host`) → 일시 오류. 구현: lvmlockd 오류에 "locked by other host"가 있으면 `DriverError::Timeout("another node to stop using the lv")`, "held by other host"(VG 잠금)면 `Timeout("another node to release the shared vg lock")`이다(만들기·켜기·지우기·늘리기 모두). lvmlockd가 그 앞에 경고 줄("Retrying ex lock …")을 낼 수 있어, LVM 명령의 오류에는 stderr 첫 줄 대신 이 말이 든 줄을 담는다(`BlockTool::preferring`). 이 노드에서 켜진 LV는 `lvremove -y`가 그대로 지우므로(CI에서 확인) 먼저 끄지 않는다. LV가 없는데 그 LV를 바꾸는 명령이 아직 돌면(시간을 넘긴 만들기) `VolumeNotFound`가 아니라 일시 오류다.
- 크기: `lvextend -L` 뒤 늘린 부분(늘리기 전 크기부터 끝까지)을 `blkdiscard -f -z -o {늘리기 전 크기}`로 0으로 채운다(새 익스텐트에 지운 볼륨의 옛 데이터가 남아 있을 수 있다. 바꾸는 명령, 시간 제한 없음). 실행 중 VM의 디스크면 VM 노드(이미 켜 둔 노드)에서 돌고 켠 채로 채운다(기존 규칙). 꺼진 볼륨은 채우는 동안만 배타 활성화한다. 구현: 늘리기 전에 늘리기 전 크기를 LV 태그 `opl-zero-from-{바이트}`로 붙이고(`lvchange --addtag`) 다 채운 뒤 뗀다(`--deltag`). 0 채우기가 실패하면 오류(Task 실패)이고 태그가 남아, 다시 하면 LV가 이미 그 크기여도 태그의 위치부터 다시 채운다. 태그가 없고 이미 그 크기 이상이면 건너뛴다. 남은 태그는 LV를 켜서 장치를 내주는 쪽도 본다: VM 시작(`activate_volume`)과 복제·복사 원본(`volume_source`)이 켠 뒤 장치를 내주기 전에 남은 부분을 0으로 채우고 태그를 뗀다(공용 도우미 `activate_ready`, 실패하면 오류이고 자기가 켠 LV만 끈다). 이 0 채우기도 남은 크기에 비례하므로 그런 VM의 시작은 오래 걸릴 수 있다. 그렇지 않으면 게스트가 늘린 크기 전체를 보고 옛 데이터를 읽고, 나중의 크기 늘리기가 게스트가 쓴 데이터를 지운다. 태그의 위치가 LV 끝이면(태그만 붙고 늘리기 전) 채울 것이 없으므로 태그를 둔다. 크기 늘리기와 VM 시작은 겹치지 않는다(백엔드 공통 — qcow2의 `qemu-img resize`와 QEMU 시작도): reconciler의 resolve는 디스크 볼륨에 실행 중인(`running`) 크기 늘리기 Task가 있으면 켜지 않고 `ResolveError::VolumeResizing`("volume … is being resized")으로 실패로 세지 않고 `starting`에서 다시 시도한다. 대기 중(`pending`)인 Task는 막지 않는다. 그 Task는 VM의 줄에서 앞의 Task(예: 메모리 스냅샷 되돌리기)를 기다릴 수 있고 되돌리기는 VM을 다시 켜야 끝나므로, 막으면 서로 기다린다. 실행기는 Task를 `running`으로 기록하고(커밋돼 이 노드에 적용될 때까지 기다린다) 그 뒤에 VM 상태를 읽어 경로를 고르므로, 그 기록보다 먼저 켜기 시작한 VM은 실행기가 원하는 상태 켜짐으로 보고(꺼진 VM 경로를 타지 않는다), 뒤에 켜는 VM은 resolve가 `running` Task를 보고 기다린다. API 크기 늘리기는 쓰는 VM이 켜지거나 꺼지는 중이면(원하는 상태와 실제 상태가 다르거나 전원 Task가 끝나지 않았다) 409 `vm_busy`(`check_not_busy`)이고, Task 실행기는 원하는 상태도 꺼짐이고 멈춘 VM일 때만 꺼진 VM 경로(저장소로 늘리기)를 탄다(켜는 중이면 Task 실패). 그래서 Task가 꺼진 VM을 본 뒤 켜면 reconciler가 그 Task를 보고 기다리고, reconciler가 Task 없이 켜기 시작했으면 원하는 상태가 이미 켜짐이라 API·실행기가 거절한다. 원하는 상태가 그대로 켜짐인데 QEMU가 스스로 끝나 곧바로(백오프 없이) 다시 켤 때는 저장된 phase가 `running`인 채라 API·실행기가 실행 중 VM으로 볼 수 있으므로, worker가 디스크를 해석하기 전에 phase `starting`을 먼저 기록한다(그 전에 커밋된 Task는 resolve가 보고, 뒤의 Task는 API·실행기가 거절한다). 기록하지 못하면(예: 과반수 없음 — 그동안은 Task도 만들 수 없다) 그대로 다시 켠다(VM은 과반수 없이도 다시 켠다). 기록이 시간 초과(`write_timeout`, 5초)로 실패한 사이 과반수가 돌아와 크기 늘리기 Task가 들어오면 좁은 틈이 남는다. 과반수가 없으면 백오프 없는 다시 켜기가 그 시간만큼 늦어진다. 이미 도는 VM에 다시 붙을 때(`reattach`)는 QEMU가 이미 장치를 열었으므로 크기 늘리기 Task를 기다리지 않는다(`resolve::running_spec`). 드라이버도 같은 노드 안에서 노드 안 표시(`Zeroing`)로 가른다: 크기 늘리기와 켜기(VM 시작·복제 원본)는 표시를 잡은 뒤 LV를 읽고(읽은 활성 여부가 끝까지 맞다), 표시를 잡지 못하거나 그 LV를 바꾸는 LVM 명령이 도는 동안이면 LV를 건드리지 않고 일시 오류다. 다른 노드와는 lvmlockd의 배타 잠금이 가른다. `LvmOps::zero`가 시작 위치를 받고 `tags`(`lvs --noheadings -o lv_tags`)·`set_tag`가 더해졌다. 켜진 LV·꺼진 LV 모두 `lvextend`가 된다(CI에서 확인).
- 원본(`volume_source`): 배타 활성화하고 장치 경로, `release_source`가 끈다. 태그는 `InvalidSpec`.
- `activate_volume`: 배타 활성화(`lvchange -aey`). 다른 노드가 쥐고 있으면 일시 오류(메시지에 "다른 노드가 쓰는 중"). 구현: 이 노드에서 이미 켜져 있으면 다시 켜지 않는다. 메시지는 위의 "timed out waiting for another node to stop using the lv". `disk_location`은 LVM-thin과 같은 `/dev/{vg}/opl-{id}`(`DiskKind::Block`)이고 장치 노드가 없으면 `VolumeNotActive`(일시)다.
- 새 메서드 `Storage::deactivate_volume`(기본 할 일 없음): 공유 LVM은 `lvchange -an`(이미 꺼져 있으면 성공). 구현: 볼륨 LV가 없어도 성공이다. 이 노드가 0으로 채우는 중인 LV(`Zeroing` 표시, 크기 늘리기)는 끄지 않고 성공이다(켜진 채 남으면 §4의 정리가 끈다). 끄는 동안 표시를 쥐므로 그사이 크기 늘리기는 일시 오류로 기다린다. `Storages`가 백엔드로 전달한다. 정리용으로 `Storage::active_volumes(pool) -> Vec<Uuid>`(기본 빈 목록, 공유 LVM은 이 노드에서 켜진 `opl-{uuid}` LV)도 더했다.
- 정보: `lvs`(크기, 사용량은 크기와 같다). 용량: `vgs`의 크기·남은 공간.

## 4. 활성화의 수명

- **VM 시작:** resolve가 `activate_volume`(배타)을 부른다. 시작이 실패하면 켠 LV를 끈다. 구현: resolve(`runtime_spec`)가 켠 볼륨을 적어 두었다가 디스크가 여럿인 VM의 해석이 중간에 실패하면 이미 켠 것을 끈다. QEMU 시작이 실패하면 reconciler worker가 디스크를 끈다.
- **VM 멈춤:** reconciler가 QEMU 프로세스가 끝난 것을 본 뒤 그 VM의 공유 LVM 디스크를 끈다(`deactivate_volume`). 끄지 못하면 경고를 남기고 아래 정리가 다시 한다. 구현: worker가 디스크를 켰거나(시작) 도는 VM을 봤으면 표시(`disks_active`)를 두고, QEMU가 끝난 것(멈춤·충돌·포기)을 확인한 뒤 한 번 VM의 모든 디스크에 일반 `deactivate_volume`을 부른다(공유 LVM만 할 일이 있다).
- **정리:** opld 시작 때와 30분마다(남은 태그 정리와 같은 주기) 이 노드에서 켜져 있는 공유 LVM의 `opl-*` LV 가운데, 이 노드에서 실행 중인 VM의 디스크도 이 노드에서 도는 Task의 볼륨도 아닌 것을 끈다. 구현: opl-api Task 실행기의 `deactivate_idle_volumes`(시작할 때와 `TaskRunnerConfig::tag_sweep`마다, 실행기가 멈추면 함께 멈춘다)가 이 노드의 목록에 있는 풀마다 `active_volumes`를 읽는다. 하이퍼바이저에서 도는 VM과 이 노드에 놓여 `power: running`인 VM(시작하려는 VM)의 디스크, 이 노드의 끝나지 않은 Task의 대상(VM Task면 그 VM의 디스크), `Ready`가 아닌(만드는 중인) 볼륨은 건너뛰고, 리소스가 없는 볼륨 LV는 끈다. 켜진 LV를 먼저 읽고 VM·Task를 나중에 읽는다(LV를 켜는 VM·Task는 켜기 전에 이미 저장소에 있으므로 쓰는 LV를 끄지 않는다). 이미 켜져 있던 LV를 그 사이 시작한 VM이 그대로 쓸 수 있으므로 VM·Task는 LV마다 끄기 직전에 다시 읽는다. 열린 장치(실행 중인 VM의 디스크)는 `lvchange -an`이 실패해 켜진 채 남으므로 잘못 끄려 해도 경고만 남는다(드라이버 실제 시험 `an_open_volume_stays_active`).
- **노드 장애:** 죽은 노드의 잠금은 sanlock 임대가 끝나면(기본 수 분) 풀린다. 그 전에는 다른 노드에서 그 디스크로 VM을 켤 수 없다(일시 오류).

## 5. 마운터와 Task 노드

- 마운터(풀을 쓸 수 있는 Linux 노드마다):
  1. `iscsi`가 있으면 그 세션이 없을 때 `iscsiadm -m discovery -t sendtargets -p {portal}` 뒤 `iscsiadm -m node -T {target} -p {portal} --login`, 그다음 `udevadm settle`.
  2. `vgchange --lockstart {vg}`(처음에는 sanlock 임대 때문에 오래 걸릴 수 있다 — 죽이지 않는 명령, 이미 시작했으면 바로 끝난다).
  3. `vgs --reportformat json -o vg_name,vg_lock_type,vg_size,vg_free {vg}`: 잠금 종류가 `sanlock`·`dlm`이 아니면 준비 안 됨(`"volume group {vg} is not a shared (lvmlockd) VG"`). 용량은 크기·남은 공간.
  - 실패하면 이유(stderr 첫 줄). macOS는 `"shared LVM pools are not supported on macOS"`. 구현: macOS 문구는 다른 블록 풀처럼 종류 이름을 쓴 `"lvm-shared pools are not supported on macOS"`다(마운터·resolve 같다). `MountOps::iscsi_login(portal, target)`·`lvm_lockstart(vg)`·`lvm_vg(vg) -> VgInfo`. 세션은 `iscsiadm -m session` 출력의 넷째 칸(타깃 이름)으로 찾고 포털은 보지 않는다(호스트 이름 포털도 세션에는 주소로 보인다). `-m session`의 종료 코드 21은 세션 없음, `--login`의 15("already present")는 성공이다. 로그인·잠금 시작은 죽이지 않는 뒤 작업으로 풀마다 하나만 돌고(`lvm_shared_checks`, 풀을 이 노드에서 쓸 수 없게 되어도 끝날 때까지 두어 다시 쓸 수 있게 되면 그것을 기다린다) 확인 시간(10초) 안에 끝나지 않으면 `"iscsi login or vg lock start in progress"`로 알리며 다음 새로 고침이 결과를 거둔다. `vgs`만 시간 제한이 있다(`"vgs timed out"`, `--units b --nosuffix`).
- 볼륨 Task 노드: RBD와 같이 풀을 쓸 수 있는 온라인 노드 중 id가 가장 작은 노드(LV를 바꾸는 명령이 아직 돈다는 표시가 노드마다 따로이므로). 가져오기는 업로드를 받은 노드, VM 디스크 크기는 VM 노드(기존 규칙). 구현: `pool_node`가 RBD와 함께 공유 LVM 풀도 요청을 받은 노드를 앞세우지 않는다.

## 6. 클라이언트

- **oplctl:** `pool create NAME --lvm-shared VG [--iscsi-portal PORTAL --iscsi-target IQN]`(둘은 함께, 다른 종류와 함께 못 씀), 목록·상세의 종류 `lvm-shared`·위치 `VG` 또는 `VG @ PORTAL TARGET`, 형식 기본값 raw. 구현: `--iscsi-portal`·`--iscsi-target`은 서로와 `--lvm-shared`가 필요하고 `--lvm-shared`는 다른 종류·`--shared`와 함께 못 쓴다. 본문은 `{"kind":"lvm-shared","vg","iscsi"}`(iSCSI가 없으면 `null`)이고 `shared`는 적지 않는다.
- **웹:** 풀 만들기의 공유 LVM(VG, iSCSI 사용 여부, 포털, 타깃 — 서버와 같은 검사), 목록 종류·위치, `poolFormats` raw, 공유 LVM 디스크 VM의 스냅샷 버튼은 이유와 함께 막는다. 구현: 종류 "Shared LVM"(`pools.kinds.lvm-shared`, `pools.kindHint.lvm-shared`), "iSCSI 타깃에 로그인" 체크(`pools.iscsi`)를 고르면 포털·타깃 입력. `isShared`가 공유 LVM을 늘 공유로 보고(용량을 하나로 보인다) 위치는 oplctl과 같다. `poolFormats`는 raw만. `pools.deleteDescription`은 공유 VG의 LV도 남는다고 적는다.

## 7. 테스트

- **단위:** 타입·검증, API 규칙(공유·형식·스냅샷 409), 가짜 `LvmOps`로 공유 LVM 드라이버 각 동작(0 채우기, 배타 활성화, 다른 노드가 쥔 경우), 마운터(iscsiadm·lockstart·vgs, 잠금 없는 VG), reconciler의 끄기·정리, oplctl·웹.
- **실제(`lvm-shared-it`, Linux·root):** CI `linux-kvm`에 `targetcli-fb`(LIO, 파일 백스토어, 데모 모드 ACL)로 루프백 iSCSI 타깃, `open-iscsi`로 로그인, `sanlock`·`lvm2-lockd`(softdog, `host_id = 1`)로 공유 VG. `OPL_TEST_LVM_SHARED=VG`, `OPL_TEST_ISCSI=PORTAL,TARGET`. 드라이버 통합 시험과 opld E2E(공유 LVM 디스크 VM 켜고 끄기 — 끈 뒤 LV가 꺼졌는지, 디렉터리 풀로 복제, 지우기). sanlock·lvmlockd를 CI에서 띄울 수 없으면 iSCSI 로그인만 CI에서 시험하고 잠금 부분은 수동 시험으로 남긴다(설계에 기록). 구현: 기능 `lvm-shared-it`(opl-driver, opld). `crates/opl-driver/tests/lvm_shared_storage.rs`(5개: 만들기·0 채우기·배타 활성화·크기·지우기, 늘린 부분의 0 채우기(바로 뒤에 놓였다 지운 볼륨의 패턴이 보이지 않는지 — 그 볼륨이 바로 뒤 익스텐트에 있었고 늘린 부분이 그 자리에 놓였는지 `lvs -o seg_pe_ranges`로 확인, 켜진 볼륨은 켠 채로, 남은 태그는 켜기가 마저 채우는지), 연 장치는 끌 수 없음, vmdk 가져오기와 실패한 가져오기 정리, 백엔드 안팎 복사. 한 VG를 쓰므로 파일 안 시험은 정적 뮤텍스로 하나씩 돈다), `crates/opld/tests/lvm_shared.rs`(1개: `OPL_TEST_ISCSI`가 있으면 잠금을 멈추고 로그아웃한 뒤 opld가 처음부터 로그인·잠금 시작해 준비하는지, VM을 켜면 LV가 켜지고 `host_device`로 열리는지, 스냅샷 409, opld를 다시 띄울 때 남은 활성화 정리와 실행 중인 VM의 LV는 그대로인지, 멈춘 뒤 LV가 꺼지는지, 디렉터리 풀과 같은 풀로 복제, 지우기). opld 시험은 `--format raw`를 명시한다. CI 단계 "iSCSI target and a shared (lvmlockd) VG"(10분 제한): IQN `iqn.2003-01.org.linux-iscsi.opl:shared`, 2G 파일 백스토어, `127.0.0.1` 포털 로그인, `use_lvmlockd = 1`·`host_id = 1`, `wdmd sanlock lvmlockd`, VG `oplshared`(`vgcreate --shared`는 잠금 시작까지 약 24초). lvmlockd를 켜도 LVM-thin 시험의 로컬 VG는 그대로 된다. sanlock·lvmlockd가 CI에서 떴으므로 대안(로그인만 시험)은 쓰지 않았다. Linux 전용 resolve 시험 `activates_lvm_shared_volumes_and_waits_for_the_device`도 CI에서 돈다.

## 8. 알려진 한계

- 스냅샷이 없다(링크드 클론도 없다, 복제는 전체 복사).
- 새 볼륨은 0으로 채우므로 만드는 시간이 크기에 비례한다(SAN의 WRITE SAME 오프로드가 있으면 빠르다).
- 노드가 죽으면 sanlock 임대가 끝날 때까지 그 노드가 켜 둔 디스크를 다른 노드에서 쓸 수 없다.
- 각 노드에 `lvm2-lockd`·`sanlock`(또는 dlm)과 고유한 `host_id`, iSCSI면 `open-iscsi`가 있어야 한다. CHAP은 지원하지 않는다.
- macOS 노드는 공유 LVM을 쓸 수 없다.
- 끄지 못한 LV(경고만 남긴 경우)는 다음 정리(opld 시작 때나 30분마다)까지 그 노드에서 켜져 있고, 그동안 다른 노드에서 그 디스크로 VM을 켜거나 그 볼륨을 늘리거나 지우면 일시 오류로 기다린다.
- 죽은 노드에서 돌던(`running`) 크기 늘리기 Task는 그 노드가 돌아오거나 클러스터에서 빠질 때까지 끝나지 않으므로, 그동안 그 디스크의 VM은 다른 노드에서도 시작하지 않고 `starting`에서 기다린다(대기 중(`pending`) Task는 시작을 막지 않는다. 오프라인 노드의 대기 중 Task는 약 1분 뒤 정리 작업이 실패로 끝낸다). 백엔드 공통이다.
- LV를 바꾸는 명령이 돈다는 표시는 노드마다 따로다(RBD와 같다). 볼륨 Task를 한 노드로 모으고, 그 밖의 경우는 lvmlockd 잠금이 노드 사이를 막는다.
- 풀을 지워도 iSCSI 로그아웃이나 잠금 멈춤(`vgchange --lockstop`)은 하지 않는다(관리자가 한다).
