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
  - `vg`: LVM-thin과 같은 VG 이름 규칙.
  - `portal`: `호스트[:포트]` 또는 `[IPv6][:포트]`. 호스트는 NFS `server`와 같은 규칙, 포트는 1–65535.
  - `target`: `iqn.`·`eui.`·`naa.`로 시작하고 소문자·숫자·`.`·`:`·`-`만, 223바이트 이하.
- 공유 LVM 풀은 항상 공유(`is_shared` 참, `shared: false`는 400), raw만(qcow2는 400), `pool_kind_supports_snapshots`는 거짓.
- 블록 스냅샷 종류는 없다. 공유 LVM 디스크가 있는 VM의 스냅샷은 409 `snapshot_unsupported`(스냅샷을 지원하지 않는 디스크의 기존 규칙). 예약 스냅샷도 그 기존 규칙을 따른다.

## 3. 드라이버 (`storage/lvm.rs`의 공유 LVM)

계획 16의 `LvmOps`·`BlockTool`을 그대로 쓰고 두꺼운 LV 명령을 더한다(같은 LV에는 명령 하나, 바꾸는 명령은 죽이지 않는다).

- 만들기: `lvcreate -q -an -L {bytes}b -n opl-{id} {vg}`(1 MiB 배수로 올림, 만들 때 켜지 않는다) → 배타 활성화 → `blkdiscard -z`로 0 채우기(SAN이 WRITE SAME·UNMAP으로 빨리 할 수 있다, 시간 제한 없음) → 끈다. 실패하면 지운다. 0으로 채웠으므로 `target_reads_zero`는 참.
- 가져오기·복사의 대상: 만들기(0 채움) → 배타 활성화 → `qemu-img convert -n --target-is-zero` → 끈다.
- 지우기: `lvremove`. 다른 노드가 켜 두었으면 lvmlockd가 거절한다(`LV locked by other host`) → 일시 오류.
- 크기: `lvextend -L`. 실행 중 VM의 디스크면 VM 노드(이미 켜 둔 노드)에서 돈다(기존 규칙). 꺼진 볼륨은 켜지 않고 늘린다.
- 원본(`volume_source`): 배타 활성화하고 장치 경로, `release_source`가 끈다. 태그는 `InvalidSpec`.
- `activate_volume`: 배타 활성화(`lvchange -aey`). 다른 노드가 쥐고 있으면 일시 오류(메시지에 "다른 노드가 쓰는 중").
- 새 메서드 `Storage::deactivate_volume`(기본 할 일 없음): 공유 LVM은 `lvchange -an`(이미 꺼져 있으면 성공).
- 정보: `lvs`(크기, 사용량은 크기와 같다). 용량: `vgs`의 크기·남은 공간.

## 4. 활성화의 수명

- **VM 시작:** resolve가 `activate_volume`(배타)을 부른다. 시작이 실패하면 켠 LV를 끈다.
- **VM 멈춤:** reconciler가 QEMU 프로세스가 끝난 것을 본 뒤 그 VM의 공유 LVM 디스크를 끈다(`deactivate_volume`). 끄지 못하면 경고를 남기고 아래 정리가 다시 한다.
- **정리:** opld 시작 때와 30분마다(남은 태그 정리와 같은 주기) 이 노드에서 켜져 있는 공유 LVM의 `opl-*` LV 가운데, 이 노드에서 실행 중인 VM의 디스크도 이 노드에서 도는 Task의 볼륨도 아닌 것을 끈다.
- **노드 장애:** 죽은 노드의 잠금은 sanlock 임대가 끝나면(기본 수 분) 풀린다. 그 전에는 다른 노드에서 그 디스크로 VM을 켤 수 없다(일시 오류).

## 5. 마운터와 Task 노드

- 마운터(풀을 쓸 수 있는 Linux 노드마다):
  1. `iscsi`가 있으면 그 세션이 없을 때 `iscsiadm -m discovery -t sendtargets -p {portal}` 뒤 `iscsiadm -m node -T {target} -p {portal} --login`, 그다음 `udevadm settle`.
  2. `vgchange --lockstart {vg}`(처음에는 sanlock 임대 때문에 오래 걸릴 수 있다 — 죽이지 않는 명령, 이미 시작했으면 바로 끝난다).
  3. `vgs --reportformat json -o vg_name,vg_lock_type,vg_size,vg_free {vg}`: 잠금 종류가 `sanlock`·`dlm`이 아니면 준비 안 됨(`"volume group {vg} is not a shared (lvmlockd) VG"`). 용량은 크기·남은 공간.
  - 실패하면 이유(stderr 첫 줄). macOS는 `"shared LVM pools are not supported on macOS"`.
- 볼륨 Task 노드: RBD와 같이 풀을 쓸 수 있는 온라인 노드 중 id가 가장 작은 노드(LV를 바꾸는 명령이 아직 돈다는 표시가 노드마다 따로이므로). 가져오기는 업로드를 받은 노드, VM 디스크 크기는 VM 노드(기존 규칙).

## 6. 클라이언트

- **oplctl:** `pool create NAME --lvm-shared VG [--iscsi-portal PORTAL --iscsi-target IQN]`(둘은 함께, 다른 종류와 함께 못 씀), 목록·상세의 종류 `lvm-shared`·위치 `VG` 또는 `VG @ PORTAL TARGET`, 형식 기본값 raw.
- **웹:** 풀 만들기의 공유 LVM(VG, iSCSI 사용 여부, 포털, 타깃 — 서버와 같은 검사), 목록 종류·위치, `poolFormats` raw, 공유 LVM 디스크 VM의 스냅샷 버튼은 이유와 함께 막는다.

## 7. 테스트

- **단위:** 타입·검증, API 규칙(공유·형식·스냅샷 409), 가짜 `LvmOps`로 공유 LVM 드라이버 각 동작(0 채우기, 배타 활성화, 다른 노드가 쥔 경우), 마운터(iscsiadm·lockstart·vgs, 잠금 없는 VG), reconciler의 끄기·정리, oplctl·웹.
- **실제(`lvm-shared-it`, Linux·root):** CI `linux-kvm`에 `targetcli-fb`(LIO, 파일 백스토어, 데모 모드 ACL)로 루프백 iSCSI 타깃, `open-iscsi`로 로그인, `sanlock`·`lvm2-lockd`(softdog, `host_id = 1`)로 공유 VG. `OPL_TEST_LVM_SHARED=VG`, `OPL_TEST_ISCSI=PORTAL,TARGET`. 드라이버 통합 시험과 opld E2E(공유 LVM 디스크 VM 켜고 끄기 — 끈 뒤 LV가 꺼졌는지, 디렉터리 풀로 복제, 지우기). sanlock·lvmlockd를 CI에서 띄울 수 없으면 iSCSI 로그인만 CI에서 시험하고 잠금 부분은 수동 시험으로 남긴다(설계에 기록).

## 8. 알려진 한계

- 스냅샷이 없다(링크드 클론도 없다, 복제는 전체 복사).
- 새 볼륨은 0으로 채우므로 만드는 시간이 크기에 비례한다(SAN의 WRITE SAME 오프로드가 있으면 빠르다).
- 노드가 죽으면 sanlock 임대가 끝날 때까지 그 노드가 켜 둔 디스크를 다른 노드에서 쓸 수 없다.
- 각 노드에 `lvm2-lockd`·`sanlock`(또는 dlm)과 고유한 `host_id`, iSCSI면 `open-iscsi`가 있어야 한다. CHAP은 지원하지 않는다.
- macOS 노드는 공유 LVM을 쓸 수 없다.
