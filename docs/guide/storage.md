# 스토리지

NFS, LVM-thin, ZFS, Ceph 자동 구성과 RBD, 공유 LVM(iSCSI). 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

- [스토리지 풀과 NFS](#스토리지-풀과-nfs)
- [LVM-thin 풀](#lvm-thin-풀)
- [ZFS 풀](#zfs-풀)
- [Ceph 자동 구성](#ceph-자동-구성)
- [Ceph RBD 풀](#ceph-rbd-풀)
- [공유 LVM(iSCSI) 풀](#공유-lvmiscsi-풀)

## 스토리지 풀과 NFS

파일 풀은 두 종류다. **디렉터리 풀**(`--path`)은 노드의 디렉터리이고, **NFS 풀**(`--nfs`)은 NFS 서버의 내보내기를
orbivisord가 노드마다 마운트한 것이다. 어느 쪽이든 볼륨은 그 안의 qcow2·raw 파일이다. 볼륨이 블록 장치인
**LVM-thin 풀**(`--lvm-thin`)과 **ZFS 풀**(`--zfs`)은 [아래](#lvm-thin-풀)에, 볼륨이 Ceph RBD 이미지인
**Ceph RBD 풀**(`--rbd`)은 [그 뒤](#ceph-rbd-풀)에, 볼륨이 공유 LUN 위의 LV인 **공유 LVM 풀**(`--lvm-shared`)은
[마지막](#공유-lvmiscsi-풀)에 있다.
Ceph 클러스터부터 Orbivisor가 노드에 세우는 길은 [Ceph 자동 구성](#ceph-자동-구성)이다.

```bash
sudo apt install nfs-common                          # 노드마다 NFS 클라이언트(mount.nfs)
orbivisorctl pool create nas --nfs nas.lan:/srv/vm   # IPv6 서버는 --nfs '[fd00::5]:/srv/vm'
orbivisorctl pool create nas2 --nfs 10.0.0.5:/srv/vm --nfs-options vers=4.2,hard  # mount -o nosuid,nodev,noexec,retry=0 뒤에 붙는다
orbivisorctl pool create shared --path /mnt/vm --shared  # Orbivisor 밖에서 마운트한 공유 디렉터리(모든 노드가 같은 파일을 본다)
orbivisorctl pool set local --shared                 # 디렉터리 풀을 공유로(--no-shared는 노드 로컬로)
orbivisorctl pool list                               # NAME KIND LOCATION NODES READY(준비된 노드/쓸 수 있는 노드)
orbivisorctl pool get nas                            # 노드별 준비·용량·오류(-o json: 풀 리소스 그대로)
                                                     # 꺼진 노드는 node offline, 목록의 없는 노드는 node not found
```

- **NFS는 Linux 노드에서 root인 orbivisord만** 마운트한다. 내보내기는 orbivisord(root)가 쓸 수 있어야 한다(예: `no_root_squash`).
  macOS 노드는 NFS 풀을 마운트하지 않고 "nfs pools are not supported on macOS"로 알린다. 옵션을 주지 않으면 커널
  기본값으로 마운트한다.
- **마운트 옵션.** orbivisord는 늘 `nosuid,nodev,noexec,retry=0`을 붙이고 `--nfs-options`를 그 뒤에 붙인다(`retry=0`은
  시간 제한으로 `mount`를 끝낸 뒤 mount.nfs가 계속 다시 시도하지 않게 한다. 다시 시도는 orbivisord가 한다). 마운트의 성격을 바꾸는
  옵션(`bind`, `rbind`, `remount`, `move`, `suid`, `dev`, `exec`, `helper`, `bg`, `x-*`·`X-mount.*`)은 **400**으로 받지 않는다.
- **마운트 위치.** `{data_dir}/pools/{풀 id}`. orbivisord가 내려가도 마운트는 그대로 두고, 다시 올라오면 그대로 쓴다.
  풀을 지우면(볼륨이 남아 있으면 지금처럼 409) 각 노드가 언마운트하고 마운트 지점을 지운다. 그 풀의 디스크를 쓰는
  VM이 그 노드에서 돌고 있으면 VM이 멈출 때까지 미룬다(`umount -f`는 쓰지 않는다). 풀을 쓸 노드는 만들 때 `--node`로
  정한다(생략하면 모든 노드).
- **주의: `{data_dir}`를 통째로 지우지 않는다.** 마운트된 NFS 풀이 있는 채로 `rm -rf /var/lib/opl`을 하면 마운트를
  따라 들어가 NFS 내보내기의 볼륨 파일까지 지운다. 노드를 클러스터에서 빼거나 Orbivisor를 지워도 `{data_dir}/pools` 아래의
  NFS 마운트는 그대로 남는다. 먼저 orbivisord를 멈추고 `{data_dir}/pools` 아래를 `umount`한 뒤 지운다.
- **NFS 풀은 늘 공유 풀이다.** API에서 `shared`를 적지 않으면 `true`가 되고, `false`면 **400** `invalid_request`다.
  orbivisord 밖에서(fstab 등) 마운트한 공유 디렉터리는 디렉터리 풀을 공유로 만든다: `orbivisorctl pool create --path ... --shared`
  (API `"shared": true`, 웹은 "공유 스토리지" 체크). `--shared`는 `--nfs`와 함께 쓰지 않는다.
- **공유 여부 바꾸기.** 디렉터리 풀만 `orbivisorctl pool set POOL --shared|--no-shared`(관리자)로 바꾼다(API
  `PATCH /api/v1/storage-pools/{key}` `{"shared": bool}`, `If-Match` 선택. 웹은 스토리지 풀 목록의 "공유" 스위치와
  확인 대화상자). 다른 풀은 같은 값이어도 **400** `invalid_request`다(NFS·RBD·공유 LVM은 늘 공유, LVM-thin·ZFS는
  늘 로컬). 같은 값이면 바꾸지 않고 200이다. 로컬 → 공유는 언제나 된다(그 경로가 풀의 모든 노드에서 같은 공유 파일
  시스템인지는 관리자가 보증한다). 공유 → 로컬은 그 풀의 볼륨을 쓰는 VM이 볼륨을 만든 노드가 아닌 노드에 있거나
  마이그레이션 중이면 **409** `pool_in_use`(VM 이름)다. VM을 볼륨의 노드로 되돌린 뒤 다시 한다. 로컬이 되면 볼륨은
  만든 노드에 있는 것으로 돌아간다.
- **노드별 풀 상태.** 노드는 자기가 쓸 수 있는 풀마다 준비 여부·용량·오류를 노드 상태(`status.pools`)에 알린다.
  15초마다(`[pools] refresh_secs`), 그리고 풀을 만들거나 지우면 곧바로 확인한다. 디렉터리 풀은 디렉터리가 있고
  쓸 수 있어야(작은 파일을 만들고 지워 본다) 준비된다. NFS 풀은 마운트(30초 제한)와 쓰기 확인(10초 제한)이 되어야
  준비되고, 실패하면 `mount`의 오류나 "nfs server not responding"을 남긴다. 마운트는 풀마다 따로 돌아(진행 중이면
  "mount in progress") 멈춘 NFS 서버 하나가 다른 풀의 상태를 늦추지 않고, 실패한 마운트는 15초부터 두 배씩(최대 5분)
  기다렸다가 다시 시도한다. 상태에는 어느 풀의 것인지(`pool_id`)가 함께 적혀, 같은 이름으로 다시 만든 풀에 옛 상태를
  쓰지 않는다. orbivisord를 다시 시작해도 적힌 풀 상태는 지우지 않는다. 웹의 스토리지 풀 화면과 노드 목록에서도 보인다
  (꺼진 노드는 "오프라인", 용량은 온라인이고 준비된 노드의 값).
- **배치 규칙.** 자동 배치는 VM의 디스크 풀을 쓸 수 있는 노드만 고른다. NFS 풀은 준비됐다고 알린 노드여야 하고,
  디렉터리 풀은 준비되지 않았다고 알린 노드만 빠진다(상태를 아직 알리지 않은 노드는 쓸 수 있다고 본다. 화면에는
  그 노드가 "아직 알린 상태 없음"으로 보인다). 공유가
  아닌 풀의 볼륨은 그 파일이 있는 노드(볼륨을 만든 노드)에만 배치된다. 전에는 노드 목록이
  빈 디렉터리 풀이면 파일이 없는 노드를 고를 수 있었다. 공유 풀은 준비된 어느 노드든 된다.
- **볼륨 작업 노드.** 볼륨 만들기는 접속한 노드에서 풀이 준비됐으면 그 노드, 아니면 풀이 준비된 온라인 노드 가운데
  하나에서 한다. 그런 노드가 없으면 **409** `pool_unavailable`이다. 공유가 아닌 풀의 볼륨 지우기·크기 늘리기는
  볼륨이 있는 노드에서 한다. 그 노드가 클러스터에서 빠졌으면 접속한 노드에서 한다(파일이 없으니 지우기는 리소스만
  지운다). 배치된 적 없는 VM의 스냅샷 작업은 첫 디스크가 공유 풀이면 볼륨을 만든 노드가 아니라 풀이 준비된 노드에서 한다. 이미지 가져오기와 복제는 풀이 그 노드의 목록에 없으면 지금처럼 `pool_not_on_node`,
  목록에는 있지만 준비되지 않았으면 `pool_unavailable`(노드가 알린 오류)이다. 웹의 업로드·복제·볼륨 만들기
  대화상자는 그 노드에서(볼륨 만들기는 어느 온라인 노드에서든) 쓸 수 있는 풀만 보인다.
- **VM 시작.** 노드가 준비되지 않았다고 알린 풀이나 이 노드에 실제로 마운트되지 않은 NFS 풀의 디스크가 있으면
  VM은 꺼지지 않고 `starting`에 머물며(`last_error`에 이유) 잠시 뒤 다시 시도한다. 일시 오류라 실패 횟수로 세지
  않으므로 NFS 서버가 돌아오면 시작한다. 드라이버도 마운트되지 않은 NFS 풀(빈 마운트 지점)에는 볼륨을 만들거나
  읽지 않고, 풀의 파일시스템 호출이 30초 안에 끝나지 않으면(멈춘 hard 마운트) 기다리지 않고 일시 오류로 돌려준다.
  NFS를 지원하지 않는 노드(macOS)에 놓인 NFS 풀 디스크의 VM은 기다려도 시작할 수 없으므로 꺼지고 `last_error`에 이유가 남는다.
- **올릴 때.** 새 orbivisord는 첫 확인(시작 직후)에서 풀 상태를 채운다. 디렉터리 풀은 그 전에도 쓸 수 있다고 보지만,
  디렉터리가 없는 노드는 첫 확인 뒤 그 풀이 준비되지 않은 것으로 나온다. 공유가 아닌 풀의 VM은 이제 볼륨이 있는
  노드에만 배치된다. 공유 스토리지 위의 디렉터리 풀은 공유여야 VM이 다른 노드로 갈 수 있다. 기존 풀은
  `orbivisorctl pool set POOL --shared`로 바꾼다.

## LVM-thin 풀

**LVM-thin 풀**(`--lvm-thin VG/THINPOOL`)은 노드의 LVM thin pool이다. 볼륨마다 thin LV `opl-{볼륨 id}`를 만들고
VM에는 블록 장치(`/dev/{VG}/opl-{볼륨 id}`, QEMU `host_device`)로 붙인다.

```bash
# VG와 thin pool은 관리자가 노드마다 같은 이름으로 미리 만든다(Orbivisor는 만들지 않는다).
sudo apt install lvm2 thin-provisioning-tools
sudo vgcreate vg0 /dev/sdb
sudo lvcreate -T -L 500G vg0/tp
orbivisorctl pool create fast --lvm-thin vg0/tp      # --node로 쓸 노드를 정한다(생략하면 모든 노드)
orbivisorctl volume create db --pool fast --size 20G  # --format을 생략하면 raw(디렉터리·NFS 풀은 qcow2)
orbivisorctl pool get fast                           # 노드별 준비·thin pool 크기·남은 공간·오류
```

- **노드 로컬.** LV는 그 노드에만 있으므로 VM은 볼륨이 있는 노드에서만 돈다. `--shared`와 함께 쓸 수 없고 API의
  `"shared": true`는 **400** `invalid_request`다. VG·thin pool 이름은 `A-Za-z0-9+_.-`의 1–127자이고 `-`로 시작하지 않는다. LVM이 받지 않는 이름(`.`·`..`,
  `snapshot`·`pvmove`로 시작, `_tdata`·`_tmeta` 같은 LVM 내부 이름 조각을 담은 것)도 안 된다.
- **raw만.** LVM-thin 풀의 볼륨은 raw다. qcow2 볼륨을 만들거나 가져오면 **400** `invalid_request`("lvm-thin pools
  hold raw volumes"). `orbivisorctl volume create`/`import`와 웹은 풀 종류로 형식을 정한다. API의 가져오기(`/volumes/import`,
  `/volumes/import/check`)도 `format`을 생략하면 풀 종류로 정한다(LVM-thin은 raw, 그 밖은 qcow2). 가져온 이미지는
  raw로 바뀐다.
- **업그레이드 먼저.** LVM-thin(또는 NFS) 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다.
  예전 orbivisord는 모르는 종류의 풀을 읽지 못해 풀 목록부터 실패한다.
- **노드별 상태.** orbivisord는 15초마다(`[pools] refresh_secs`) `lvs`로 thin pool을 확인해(10초 제한) 크기와 사용률로 용량을 알린다. VG나
  thin pool이 없거나 thin pool이 아니면 준비되지 않은 것이고(`"vg0/tp is not a thin pool"`, `lvs`의 오류,
  `"lvs timed out"`), NFS 풀처럼 준비됐다고 알린 노드에만 VM과 볼륨이 간다. LVM 명령은 orbivisord(root)가 부르고 하나에
  60초를 넘으면 실패로 본다. 이때 `lvs` 말고 LV를 바꾸는 명령(`lvcreate`·`lvremove` 등)은 중간에 죽이면 LVM
  메타데이터가 어긋날 수 있으므로 죽이지 않고 뒤에서 끝나게 둔다. 그동안 같은 LV를 바꾸는 명령은 띄우지 않고
  잠시 뒤 다시 시도할 오류로 끝낸다.
- **macOS 미지원.** macOS 노드는 "lvm-thin pools are not supported on macOS"로 알리고, 그 노드에 놓인 LVM 디스크의
  VM은 이유와 함께 꺼진다.
- **VM 시작.** 비활성인 LV(노드를 다시 켠 뒤 등)는 VM을 시작할 때 orbivisord가 활성화한다(`lvchange -ay -K`). 장치는
  `/dev/{VG}/{LV}`, 그 링크가 없으면 `/dev/mapper`의 노드를 쓴다. 활성화나 LVM 명령이 실패해도 VM을 끄지 않고
  잠시 뒤 다시 시도한다(볼륨이 없거나 정의가 틀렸을 때만 이유와 함께 끈다).
- **스냅샷.** VM 스냅샷은 thin 스냅샷 LV(`opl-{볼륨 id}-s-{스냅샷 id}`)다. VM의 디스크가 모두 qcow2(디렉터리·NFS
  풀의 내부 스냅샷)이거나 모두 LVM-thin이어야 하고, 섞이면 **409** `snapshot_unsupported`("disks use different
  snapshot methods")다. LVM-thin 디스크의 VM은 **메모리 스냅샷이 없다**: `memory`를 생략하면 디스크만 찍고, `true`면
  **409** `snapshot_unsupported`다. 예약 스냅샷은 `memory: true`여도 이런 VM은 디스크만 찍는다(웹은 메모리 선택을
  숨긴다).
- **실행 중 스냅샷의 짧은 멈춤.** 실행 중인 VM은 QMP `stop`으로 잠깐 멈춰 블록 I/O를 비운 뒤 디스크마다 `lvcreate`
  하고 `cont`로 다시 돌린다(디스크 수만큼 걸린다. 멈춘 시간은 orbivisord 로그의 `paused_ms`). 하나라도 실패하면 먼저
  VM을 다시 돌리고 만든 스냅샷 LV를 지운다. 디스크 찍기가 시간을 넘겼으면(120초 한도나 LVM 명령의 60초) 찍기가
  끝난 디스크의 스냅샷 LV만 지우고, 아직 만들어지고 있을 수 있는 나머지는 남긴다(Task 오류에 태그가 나오고,
  다시 돌리지 못했으면 "; resuming the VM failed: …"가 붙는다). 남긴 것은 orbivisord가 60초 뒤부터 60초 간격으로 열
  번까지 다시 지워 보고, 그래도 남은 것(스냅샷이 없는 Orbivisor 태그)은 orbivisord가 다시 시작할 때 지운다. 멈춘 채 두지 않는다: 디스크 찍기는 120초를 넘기면 실패로 끝내고,
  `cont`는 일시 오류면 1초 간격으로 세 번까지 해 보며(VM이 꺼졌으면 할 것이 없다), Task가 시간을 넘겨 버려져도
  뒤에서 다시 돌린다. 디스크는 찍었는데 다시 돌리지 못하면 스냅샷은 그대로 두고 Task만 실패로 끝낸다("snapshot
  taken; resuming the VM failed: …"). orbivisord가 멈춘 채 죽었거나 다시 돌리지 못한 VM은 VM 실행 디렉터리의
  표시(`paused-by-opl`)를 보고 다시 붙을 때 이어서 실행한다(디스크를 다시 해석하지 않으므로 LVM이 응답하지 않아도
  된다. 실행 중인 VM에 남은 표시는 상태를 볼 때 지운다).
  하이퍼바이저가 실행 중이라고 알리는 VM만 멈춘다: 일시정지됐거나 QEMU가 스스로 멈춘 VM(`io-error`,
  `guest-panicked` 등)은 멈추지도 다시 돌리지도 않고 디스크만 찍고, QMP로 다룰 수 없는 VM은 디스크를 건드리기
  전에 실패한다.
- **되돌리기·지우기·복제·크기.** 되돌리기는 지금처럼 VM을 강제로 끈 뒤 스냅샷 LV에서 볼륨 LV를 새로 만든다(앞서
  실패한 되돌리기가 남긴 `opl-{볼륨 id}-revert-…` LV는 먼저 지운다. 한 단계가 시간을 넘기면 `lvs`로 다시 보고
  끝났으면 이어서 한다. 새 볼륨 LV는 활성화 건너뛰기 표시를 지운다(`lvchange -kn`)). 시간을 넘긴 이름 바꾸기가
  되돌리기가 실패한 뒤에 끝나 볼륨 LV가 `-revert-` 이름으로만 남으면, orbivisord가 그 LV를 쓸 때(VM 시작, 되돌리기,
  스냅샷, 지우기 등) 이름을 되돌린다. 그 LV의 명령이 아직 돌고 있으면 잠시 뒤 다시 시도하고(VM을 끄지 않는다),
  `-revert-` LV가 여럿이면 어느 것인지 모르므로 손대지 않고 오류로 알린다(관리자가 맞는 것을 `opl-{볼륨 id}`로
  `lvrename`한다). 스냅샷 지우기는 VM이 돌고 있어도 된다. 볼륨을
  지우면 볼륨 LV를 먼저 지우고(쓰고 있으면 실패하고 스냅샷은 남는다) 그 스냅샷 LV와 남은 `-revert-` LV도 함께
  지운다(볼륨 LV가 `-revert-` 이름으로만 남았으면 위처럼 이름을 되돌린 뒤 볼륨으로 지운다). 스냅샷에서 복제하면 대상 풀이 LVM-thin이면 raw, 디렉터리·NFS 풀이면 스냅샷을 담을 수 있게 qcow2로
  만들고, 대상은 스냅샷 때의 크기로 만든다. qcow2 디스크의 스냅샷에서 복제하면 `qemu-img`가 지금 볼륨 크기만큼만
  읽는다(볼륨을 늘린 뒤 찍은 스냅샷을, 그보다 작던 때의 스냅샷으로 되돌린 볼륨에서 복제하면 늘린 부분이 빠진다). 새 LV에 써 넣을 때 thin pool이 새 블록을 0으로 채우면
  (`lv_attr`의 `z`, 기본값) 0인 부분은 쓰지 않고, `lvcreate -Zn`으로 만든 thin pool이면 모두 쓴다. 크기 늘리기는
  실행 중이어도 되고 `lvextend` 뒤 QEMU에 LV의 실제 크기(extent 단위로 올라간다)를 알린다(LV가 이미 그 크기
  이상이면 `lvextend`는 건너뛴다). 기록되는 볼륨 크기도 그 크기다.
- **thin pool이 가득 차면** 게스트의 쓰기가 실패한다. Orbivisor는 thin pool을 늘리지 않으므로 노드별 남은 공간을 보고
  관리자가 늘린다(LVM의 `thin_pool_autoextend_threshold`도 쓸 수 있다).
- **호스트 LVM이 게스트 디스크를 읽지 않게.** 게스트가 디스크에 LVM을 쓰면 호스트의 LVM이 볼륨 LV에서 게스트의
  PV·VG를 찾아 활성화할 수 있다. lvm2 2.03의 기본값 `scan_lvs = 0`이면 LV를 보지 않으므로 그대로 두고, 바꿨다면
  `/etc/lvm/lvm.conf`의 `global_filter`로 `/dev/{VG}/opl-*`를 뺀다([ZFS 풀](#zfs-풀)의 zvol도 같다).

## ZFS 풀

**ZFS 풀**(`--zfs POOL/DATASET`)은 노드의 ZFS 데이터셋이다. 볼륨마다 그 바로 아래에 sparse zvol
`opl-{볼륨 id}`를 만들고(`volmode=dev`, `snapdev=visible`, `com.sun:auto-snapshot=false`) VM에는 블록 장치(`/dev/zvol/{DATASET}/opl-{볼륨 id}`,
QEMU `host_device`)로 붙인다. 블록 장치라는 점, 스냅샷 규칙, 실행 중 스냅샷의 짧은 멈춤은 [LVM-thin 풀](#lvm-thin-풀)과
같다. 다른 점만 적는다.

```bash
# zpool과 데이터셋은 관리자가 노드마다 같은 이름으로 미리 만든다(Orbivisor는 만들지 않는다).
sudo apt install zfsutils-linux
sudo zpool create tank /dev/sdb
sudo zfs create tank/opl
orbivisorctl pool create z --zfs tank/opl            # --node로 쓸 노드를 정한다(생략하면 모든 노드)
orbivisorctl volume create db --pool z --size 20G    # --format을 생략하면 raw
orbivisorctl pool get z                              # 노드별 준비·데이터셋 크기·남은 공간·오류
```

- **노드 로컬, raw만.** zvol은 그 노드에만 있으므로 VM은 볼륨이 있는 노드에서만 돈다. `--shared`와 함께 쓸 수
  없고 API의 `"shared": true`는 **400** `invalid_request`다. 볼륨은 raw이고 qcow2 볼륨을 만들거나 가져오면 **400**
  ("zfs pools hold raw volumes"). `orbivisorctl`·웹·API 가져오기는 형식을 생략하면 raw로 정한다. 데이터셋 이름은 `/`로
  나뉜 구성 요소가 `A-Za-z0-9_.:-`로 되고(비거나 `.`·`..`이거나 `-`로 시작하면 안 된다) 맨 앞(풀 이름)이 영문자로
  시작하는 177자 이하다(zvol 스냅샷 이름 `{DATASET}/opl-{볼륨 id}@{스냅샷 id}`가 ZFS 이름 상한 255자 안에 들게).
  볼륨 크기는 1 MiB 배수로 올린다.
- **업그레이드 먼저.** ZFS 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다(예전 orbivisord는
  모르는 종류의 풀을 읽지 못한다).
- **노드별 상태.** orbivisord는 15초마다 `zfs get -Hp -o value available,used,type`으로 데이터셋을 확인해(10초 제한)
  `available`+`used`를 크기로, `available`을 남은 공간으로 알린다. 데이터셋이 없으면(`zfs`의 오류), zvol이면
  (`"tank/opl is a volume, not a filesystem dataset"`), 시간을 넘기면(`"zfs timed out"`) 준비되지 않은 것이고,
  준비됐다고 알린 노드에만 VM과 볼륨이 간다. ZFS 명령은 orbivisord(root)가 `LC_ALL=C`로 부르고 하나에 60초를 넘으면
  실패로 본다. LVM처럼 읽기(`zfs list`·`zfs get`) 말고 바꾸는 명령은 죽이지 않고 뒤에서 끝나게 두며, 그동안 같은
  zvol을 바꾸는 명령은 띄우지 않고 잠시 뒤 다시 시도할 오류로 끝낸다.
- **macOS 미지원.** macOS 노드는 "zfs pools are not supported on macOS"로 알리고, 그 노드에 놓인 ZFS 디스크의
  VM은 이유와 함께 꺼진다.
- **VM 시작.** zvol은 늘 활성이므로 활성화할 것이 없다. 장치 노드가 아직 없으면(udev) VM을 끄지 않고 잠시 뒤 다시
  시도한다. 새 zvol은 만든 뒤 udev가 장치를 다 다루기를 기다리고(`udevadm settle`, 10초. udev가 없거나 시간을
  넘기면 기록만 하고 장치 노드가 잠시 사라지지 않는지 두 번 본다) 장치 노드를 10초까지 기다리며, 생기지 않으면
  지우고 실패한다. 되돌린 뒤와 스냅샷·볼륨을 지우기 전에도 udev를 기다린다(스냅샷을 찍은 뒤에는 VM이 멈춘 시간을
  늘리지 않게 기다리지 않는다).
- **스냅샷.** VM 스냅샷은 ZFS 스냅샷 `{DATASET}/opl-{볼륨 id}@{스냅샷 id}`다. VM의 디스크는 모두 qcow2이거나 모두
  LVM-thin이거나 모두 ZFS여야 한다(LVM-thin과 ZFS가 섞여도 **409** "disks use different snapshot methods").
  메모리 스냅샷은 없다(`memory: true`면 **409**, 예약은 디스크만). 실행 중인 VM은 LVM-thin처럼 잠깐 멈추고, 같은
  풀의 디스크를 `zfs snapshot` 한 명령으로 함께(원자적으로) 찍는다. 스냅샷 지우기는 VM이 돌고 있어도 된다.
  찍기가 시간을 넘겨 남긴 스냅샷은 LVM-thin처럼 뒤에서 다시 지우고, 남은 것은 orbivisord가 다시 시작할 때 지운다.
  Task 제한 시간에 걸리거나 orbivisord가 다시 시작해 중단된 스냅샷 Task도 같다. 뒤에서 지울 때 그 zvol에 시간을 넘긴
  명령이 아직 돌고 있으면 스냅샷이 곧 생길 수 있으므로 지운 것으로 보지 않고 다음 차례에 다시 해 본다.
- **가장 최근 스냅샷으로만 되돌린다.** `zfs rollback`은 더 나중의 스냅샷을 지워야 하므로 Orbivisor는 그렇게 하지
  않는다. ZFS 디스크가 있는 스냅샷이 VM의 가장 최근 스냅샷(만든 시각, 같으면 id 순, 만드는 중인 것 포함)이
  아니면 **409** `snapshot_unsupported`("zfs disks can only revert to the latest snapshot")로 Task를 만들기 전에
  거절한다. 더 나중의 스냅샷이 같은 ZFS 볼륨을 담지 않았으면(그 뒤에 디스크를 바꿨다) 막지 않는다. Task도 VM을
  끄기 전에 디스크마다 가장 최근 ZFS 스냅샷이 그것인지 다시 보고, 아니면 VM을 그대로 두고 더 최근 스냅샷의
  이름(Orbivisor 밖에서 만든 것이면 그 ZFS 이름, 시간을 넘긴 찍기가 남겨 orbivisord가 뒤에서 지우는 것이면 그렇다고 —
  잠시 뒤 다시 하거나 orbivisord를 다시 시작한다)과 함께 실패한다. 드라이버도 한 번 더 본다. 웹은
  그런 스냅샷의 되돌리기 버튼을 이유와 함께 막는다. 더 오래된 스냅샷으로 가려면 그 뒤의 스냅샷을 먼저 지운다.
  되돌리기는 지금처럼 VM을 강제로 끈 뒤 하고, `zfs rollback` 앞뒤로 zvol 장치의 커널 버퍼를 비운다(`blockdev
  --flushbufs`. 비우지 못하면 옛 내용이 읽힐 수 있으므로 Task가 실패하고, 앞에서 못 비우면 되돌리지 않는다).
- **자동 스냅샷 도구 주의.** opl zvol에 Orbivisor 밖에서 스냅샷이 생기면 그보다 앞의 스냅샷으로는 되돌릴 수 없다.
  zvol에 `com.sun:auto-snapshot=false`를 두므로 이 속성을 따르는 zfs-auto-snapshot은 찍지 않는다. sanoid처럼 이
  속성을 보지 않는 도구는 설정에서 이 데이터셋을 빼거나(`autosnap = no`) 재귀 설정이 opl zvol에 닿지 않게 한다.
- **호스트 LVM이 게스트 디스크를 읽지 않게.** 게스트가 디스크에 LVM을 쓰면 호스트의 LVM이 zvol 장치(`/dev/zd*`)에서
  게스트의 PV·VG를 찾아 활성화할 수 있다. `/etc/lvm/lvm.conf`의 `devices` 절에 `global_filter = [ "r|^/dev/zd.*|" ]`를
  둔다(필터가 이미 있으면 맨 앞에 더한다). LVM-thin 풀의 `/dev/{VG}/opl-*`도 같은 이유로 뺀다([위](#lvm-thin-풀)).
- **지우기·복제·크기.** 볼륨을 지우면 zvol을 스냅샷과 함께 지운다(`zfs destroy -r`, VM이 쓰고 있으면 실패한다.
  방금 닫힌 장치를 udev 등이 아직 열고 있어 "busy"면 1초 간격으로 세 번까지 다시 해 본다. 스냅샷 지우기도 같다).
  시간을 넘긴 만들기가 아직 돌고 있어 zvol이 없으면 볼륨을 지운 것으로 보지 않고 잠시 뒤 다시 해 본다(LVM-thin도 같다).
  스냅샷에서 복제하면 스냅샷 장치(`/dev/zvol/…@{스냅샷 id}`)를 읽어 대상 풀이 블록 풀(ZFS·LVM-thin)이면 raw,
  디렉터리·NFS 풀이면 qcow2로 만들고, 대상은 스냅샷 때의 크기로 만든다(`zfs send`가 아니라 `qemu-img convert`로
  복사한다). 크기 늘리기는 실행 중이어도 되고 `zfs set volsize` 뒤 QEMU에 zvol의 실제 크기(1 MiB 단위로
  올라간다)를 알린다(이미 그 크기 이상이면 건너뛴다).
- **데이터셋이 가득 차면** 게스트의 쓰기가 실패한다(sparse zvol은 미리 자리를 잡지 않는다). 노드별 남은 공간을
  보고 관리자가 zpool을 늘린다.

## Ceph 자동 구성

Orbivisor가 클러스터 노드에 **Ceph를 설치하고 구성한다**: 노드에 Ceph 꾸러미를 apt로 깔고 모니터(mon)·매니저(mgr)를 세우고,
노드의 디스크 통째에 OSD(ceph-volume LVM 위의 BlueStore)를 만들고, Ceph 풀을 만들어 [Ceph RBD 풀](#ceph-rbd-풀)로 등록한다
(Proxmox VE의 `pveceph`와 같은 자리). Orbivisor 클러스터 하나에 Orbivisor가 관리하는 Ceph 클러스터 하나다. **이미 있는 바깥
Ceph는 넘겨받지 않는다** — 바깥 Ceph는 지금처럼 `pool create … --rbd`로 쓴다(아래 "파일 위치"의 거절 참고).

```bash
orbivisorctl ceph init node1 --public-network 10.0.0.0/24   # 노드 1에 꾸러미 설치, 첫 mon·mgr (기다림, --no-wait)
# ceph: installing-packages on node1 … ceph: cluster 5f0c… ready (monitor and manager on node1)
orbivisorctl ceph mon add node2 && orbivisorctl ceph mon add node3   # mon은 셋(또는 다섯) 권장, 한 번에 하나, 1개에서 바로 3개로
orbivisorctl ceph mgr add node2                              # standby 매니저
orbivisorctl node disks node2                                # DEVICE BY-ID SIZE TYPE MODEL STATE
# /dev/sdb  wwn-0x5000c500a1b2c3d4  2.0TiB  hdd  ST2000  available
# /dev/sdc  -                        2.0TiB  hdd  -       wipeable: gpt partition table, ext4 filesystem on /dev/sdc1
# /dev/sda  -                        120GiB  ssd  -       in use: system disk (/ is on it)
orbivisorctl ceph osd add node2 /dev/sdb                     # ceph: osd.0 on node2 /dev/sdb is up
orbivisorctl ceph osd add node3 /dev/sdc --wipe              # 터미널이면 장치 이름을 다시 쳐서 확인(--yes면 묻지 않음)
orbivisorctl ceph pool create vms                            # size 기본 min(3, OSD가 up인 호스트 수)
# ceph: storage pool vms (Ceph pool vms, size 3) is ready
orbivisorctl ceph client install --all                       # 데몬이 없는 노드에 ceph-common·qemu-block-extra(선택)
orbivisorctl ceph status                                     # health·검사·mon 정족수·mgr·OSD·PG·사용량·풀(-o json)
orbivisorctl ceph osd list                                   # ID HOST DEVICE CLASS SIZE USED UP IN
```

웹은 메뉴 **Ceph**(풀 아래)다. Ceph가 없으면 admin에게 마법사(① init 노드와 망 — 노드 주소로 미리 채움, 위트니스·오프라인 노드
막음 ② 만들기 진행(단계 목록, 실패하면 이유와 "다시 하기") ③ mon 더하기 ④ OSD — 노드마다 디스크 표 ⑤ 풀 ⑥ 클라이언트
꾸러미, ③~⑥은 건너뛸 수 있음)를, 있으면 상태(health 배지와 검사, mon 정족수, mgr, 호스트별 OSD, PG 상태 막대, 사용량, 풀 표와
Orbivisor 풀 링크, admin의 더하기·만들기 단추)를 10초마다 새로 읽어 보인다. 풀 화면은 관리 Ceph의 RBD 풀에 "Orbivisor Ceph"
표시를, 노드 화면은 "Ceph" 칸(mon·mgr·osd)과 메뉴 "디스크…"를 둔다.

- **요구.** Linux 노드(위트니스 빼고), apt로 Ceph를 받을 수 있는 저장소: Ubuntu 24.04 = ceph 19.2(squid), Debian 13 = 18.2(reef).
  18 미만은 거절("ceph 17.2.7 quincy is too old; Orbivisor sets up Ceph 18 (reef) or newer"). mon·mgr·OSD 노드는 **모두 같은 판**이어야
  한다("node \"node2\" has ceph 18.2.7 reef but the cluster runs 19.2.3 squid; upgrade the packages first"). 노드 시계를 맞추고(NTP — mon
  정족수가 시계 차이에 민감하고, 한 번에 하나인 Ceph Task의 자리도 노드 시계로 60초 유예를 잰다) 노드 사이에 **3300·6789·6800-7300/tcp**를 연다. OSD 디스크는 통째로 쓰고 5 GiB 이상이어야 한다.
  모든 노드가 이 버전이어야 한다(노드가 알리는 기능 `ceph-setup` — 없으면 바꾸는 요청이 **409** `cluster_upgrading`).
- **흐름.** `ceph init`(첫 mon·mgr) → mon 더하기(셋 권장, 다섯까지 — 여섯째는 **409** `ceph_limit`, 짝수면 CLI·웹이 "an odd
  number of monitors (3 or 5) keeps quorum best"로 경고) → OSD → 풀 → (선택) 클라이언트 꾸러미. 일은 모두 그 노드의 Task(`ceph-init`·
  `ceph-mon-add`·`ceph-mgr-add`·`ceph-osd-add`·`ceph-pool-create`·`ceph-client-install`)가 하고 진행률에 단계 이름(`installing-packages`,
  `waiting-for-quorum`, `preparing-osd` …)을 적는다. mon·mgr Task는 클러스터 전체에서 한 번에 하나(**409** `ceph_busy` — Raft의 Ceph
  리소스에 자리를 잡으므로 다른 노드의 API로 함께 보내도 하나만 지나간다), OSD는 노드마다 차례로(OSD Task는 새 OSD가 Ceph에서 up이 된
  뒤에 끝난다). mon을 더하기 전에 Ceph의 monmap을 보고 Orbivisor가 모르는 mon(앞서 실패한 더하기가 남긴 것 등)이나 정족수 밖의 mon이
  있으면 **409** `ceph_busy`로 거절하며 그 이름과 손으로 치우는 명령(`ceph mon remove NAME`)을 알린다. **둘째 mon이 위험하다**: mon
  둘의 정족수는 둘 다 있어야 하므로 새 mon이 다른 mon에 닿지 못하면(포트 3300·6789가 막힘 등) Ceph 전체가 멈춘다 — 더하기 전에 포트를
  확인하고 1개에서 바로 3개로 늘린다(상태 503 문구에도 이 힌트가 붙는다). 끝나지 않은 mon·mgr Task의 노드가 꺼져 있으면 `ceph_busy`가
  "node X is offline; bring it back or remove it with `cluster remove-node`"를 덧붙인다 — 노드를 되살리거나 빼면(빠진 노드의 Task는
  정리 작업이 실패로 끝낸다) 다음 Task를 보낼 수 있다. 꾸러미 설치(`[ceph] packages` —
  기본 `ceph-mon ceph-mgr ceph-osd ceph-volume ceph-common lvm2 qemu-block-extra`, 클라이언트는 `client_packages`)는 빠진 것만
  `apt-get install`(설치 하나 30분 한도)이다. apt가 한도를 넘기거나 Task가 끊겨 dpkg가 중간에 멈췄으면(다음 apt가 "dpkg was
  interrupted"로 실패) 그 노드에서 `dpkg --configure -a`를 한다.
- **디스크 안전.** 노드가 `lsblk`·`pvs`·`ceph-volume lvm list`로 디스크마다 정한다(API의 확인 뒤 Task가 노드에서 다시 본다).
  **쓰는 중**(지우기로도 못 씀 — **409** `disk_in_use`): 읽기 전용·이동식, 5 GiB 미만, 자기나 파티션·LV에 마운트(`/`·`/boot`면
  "system disk"), 스왑, ZFS 풀 멤버, 소프트웨어 RAID 멤버, Orbivisor LVM 풀의 VG, 이 Ceph의 OSD, 열린 LV가 있는 VG(돌고 있는 남의 OSD
  등 — "in use by a running Ceph OSD or another device"). **지워야 함**(`--wipe`가 있어야 —
  없으면 **409** `disk_not_empty`, 이유를 모두 적음): 파티션 표·파티션, 마운트되지 않은 파일 시스템, 풀이 아닌 VG의 PV, 다른 Ceph의
  흔적. 다른 디스크에 걸친 VG는 `ceph-` VG여도 쓰는 중이다. `ceph-volume raw` 모드로 만든 남의 OSD(LVM 없음)는 열린 LV로 보이지
  않아 "다른 Ceph의 흔적"(지워야 함)으로 보이므로 돌고 있는 OSD가 아닌지 확인한 뒤 지운다. 지우기(`ceph-volume lvm zap --destroy`)는 명시하고 확인해야 하며(CLI는 장치 이름을 다시 치거나 `--yes`, 웹은 장치 이름 입력),
  루트·마운트·스왑·ZFS·RAID·풀 VG 디스크는 어떤 경우에도 지우지 않는다(CLI `--wipe`는 노드의 디스크 목록에 있는 디스크만 묻고, CLI·웹은
  확인한 디스크를 by-id 경로로 보낸다). Task에는 `/dev/disk/by-id/…`가 있으면 그것을 적는다(재부팅으로
  이름이 바뀌어도 같은 디스크). 루프 장치는 `[ceph] allow_loop_devices = true`(시험용)일 때만 보인다.
- **풀 크기.** `--size`가 없으면 min(3, OSD가 up인 호스트 수)이다. 호스트가 하나면 **400** "only one host has OSDs; pass size 1 to
  keep a single copy (no redundancy)"이라 `--size 1`을 적어야 하고(웹은 확인 상자), 호스트보다 큰 size는 **400**. size 2는 받되 "size 2
  can lose data when one copy is down; 3 is recommended"로 경고한다. 호스트가 적은 클러스터가 HEALTH_WARN에 머물지 않게 `.mgr` 풀의
  size를 호스트 수로 낮춘다(1로는 size 1을 적었을 때만). size 1은 `mon_allow_pool_size_one`을 **클러스터 전체**(`ceph config set
  global`)에 켠다 — 그 뒤로는 손으로 만드는 다른 풀도 size 1로 둘 수 있다. 만든 Ceph 풀에는 rbd application의 표시
  `orbivisor-pool = {Orbivisor 풀 이름}`을 단다. Ceph에 같은 이름의 풀이 이미 있으면, application이 없거나 그 표시가 같은 이름이면 앞 풀
  Task가 만들다 멈춘 풀로 보고 이어서 하고(size 다시 맞춤·PG 기다림·`rbd pool init`·등록), 그 밖(cephfs·rgw 등, 표시 없는 rbd 풀, 다른
  Orbivisor 풀의 것)은 쓰는 풀의 복제 수를 바꾸지 않도록 실패한다("…already exists for …; Orbivisor does not change it — choose another Ceph
  pool name"). 어느 Orbivisor RBD 풀이든(바깥 conf여도) 이미 그 Ceph 풀 이름을 쓰면 **409** `pool_exists`다.
- **비밀.** Ceph 키(mon·admin·bootstrap-osd·클라이언트)는 API가 만들어 **Raft에 둔다**(펜싱 BMC 비밀번호와 같다 — Raft 저장소와 그
  백업에 키가 있다). API 응답·watch에는 나가지 않는다. 노드의 클라이언트 키 `client.orbivisor`는 `profile rbd`(mon·osd·mgr)뿐이고
  admin 키는 mon·mgr 노드에만 둔다. Ceph 설정을 쓰는 것은 admin뿐이고 상태 읽기는 모든 역할이다.
- **파일 위치.** 모든 비위트니스 노드에 클라이언트 설정 `/etc/opl/ceph/ceph.conf`와 `ceph.client.orbivisor.keyring`(0600,
  `[ceph] client_dir`)을 쓰고 mon이 바뀌면 고친다. 등록하는 RBD 풀은 사용자 `orbivisor`, conf `/etc/opl/ceph/ceph.conf`라 **`/etc/ceph`를
  쓰는 바깥 Ceph RBD 풀과 같은 노드에서 함께 쓸 수 있다.** 풀의 conf 경로는 풀 Task를 돈 노드의 `{client_dir}/ceph.conf`이고 모든 노드가
  그 경로로 붙으므로 **`[ceph] client_dir`은 모든 노드에서 같아야 한다.** `/etc/ceph/ceph.conf`(첫 줄 `# managed by orbivisord; edits are
  overwritten`)는 mon·mgr·OSD 노드에만, admin 키링은 mon·mgr 노드에만 쓴다(OSD만 있는 노드는 OSD가 up인지를 `client.orbivisor`로 본다).
  표시 없는 `/etc/ceph/ceph.conf`가 이미 있거나 `/var/lib/ceph/{mon,mgr,osd}`에 다른 fsid의 데몬이, `/var/lib/ceph/{fsid}/`(cephadm)가
  있으면 그 노드의 Ceph Task는 아무것도 바꾸기 전에 실패한다("…not written by Orbivisor (another Ceph cluster?);
  Orbivisor does not adopt an existing Ceph — use it as an rbd storage pool instead, or move the file away").
- **상태 보기.** `ceph status`·웹·`GET /api/v1/ceph/status`는 Raft가 아니라 mon 노드에서 그때그때 읽는다(이 노드가 mon이면 직접,
  아니면 온라인 mon 노드에 함께 노드 사이 RPC로 물어 먼저 온 답, `[ceph] status_cache_secs` 5초 캐시 — 읽지 못한 것도 그동안 두어 닿지
  않는 mon을 요청마다 기다리지 않음, `?refresh=true`). 읽지 못하면 마지막 보고서를 `stale`과
  오류로 보이고(CLI "warning: Ceph status from 40s ago: …", 웹 경고 띠), 그것도 없으면 **503** `ceph_unreachable`이다. Raft의 Ceph
  리소스(`GET /api/v1/ceph`)는 fsid·망·mon·mgr·OSD 노드·판·부트스트랩 오류만 담는다.
- **HA 검사와의 관계.** 관리 RBD 풀의 [HA 상태 검사](ha.md#ha-상태-검사) 풀 경로 검사는 다른 RBD 풀처럼 `client.orbivisor`·관리 conf로
  `rados stat`을 한다. Ceph 전체가 멈추면 모든 노드의 검사가 함께 실패하고 과반 보호가 VM을 옮기지 않는다. 상태 화면은 HA 검사를
  대신하지 않는다.
- **실패 뒤 다시 하기.** 단계마다 지금 상태를 보고 건너뛰므로(설치됨·설정 같음·mon 저장소 있음·유닛 active·정족수에 듦) 같은 명령을
  다시 부르면 이어서 한다. 부트스트랩이 실패하면 `ceph status`·웹이 마지막 오류를 보이고, **같은 노드로** `ceph init`을 다시 부른다(다른
  노드는 **409** `ceph_exists` "Ceph is being set up on node \"node1\"; retry there"). 부트스트랩 노드가 지워졌거나 꺼져 있고 아직 mon이
  서지 않았으면 다른 노드로 `ceph init`을 부르면 그 노드로 옮겨(같은 fsid·키) 다시 한다(웹 상태 화면의 "다시 하기"도 그때 온라인 노드를
  고르게 한다). 옛 노드의 끝나지 않은 Ceph Task는 실패로 끝나고, 이미 돌던 부트스트랩 Task는 옮겨 간 것을 보고 mon을 만들기 전에(또는
  Raft에 쓰기 전에) 멈춘다. 옛 노드가 돌아오면 그 노드의 `ceph-mon@…`을 멈추고 `/var/lib/ceph/mon/ceph-…`와 `/etc/ceph/ceph.conf`를 치운다
  — 그 노드에 나중에 mon을 더하면 남은 부트스트랩 저장소를 쓰지 않고 "…abandoned Ceph bootstrap…; systemctl disable --now ceph-mon@X; mv
  …"로 실패한다. 첫 mon이 선 뒤 아직 준비(`ready`)되기 전에 부트스트랩 노드를 영영 잃었으면 그 노드를 `cluster remove-node`로 빼서(Raft의
  mon 기록이 빈다) 다른 노드로 `ceph init`을 다시 부른다. **준비된 클러스터의 mon 노드를 모두 잃으면 Orbivisor는 다시 세우지 않는다**
  (**409** `ceph_exists` — mon을 손으로 되살리거나 Raft의 Ceph 기록을 손으로 지워야 한다). `lvm create`가 도중에 멈춰 켤 수 없는 OSD는
  오류가 손으로 치우는 명령(`ceph osd purge N --yes-i-really-mean-it; ceph-volume lvm zap --destroy DEV`)을 알린다. orbivisord가 다시 시작해 끊긴 Task도 다시
  부르면 이어진다.
- **노드 빼기.** mon·mgr·OSD가 있던 노드를 `cluster remove-node`(웹 "클러스터에서 빼기")로 빼면 Orbivisor의 Ceph 기록(mon·mgr·OSD 노드)
  에서도 빠진다. Ceph 쪽은 바꾸지 않으므로 응답(CLI는 stderr의 `warning:`, 웹은 알림)이 알리는 대로 손으로 치운다: `ceph mon remove
  NAME`, `ceph osd purge ID --yes-i-really-mean-it`, `ceph auth del mgr.NAME`.
- **업그레이드.** Ceph 꾸러미는 [노드 업그레이드](getting-started.md#업데이트)(`apt-get dist-upgrade`)가 함께 올린다. 데몬 재시작 순서는 사람이 지킨다:
  mon 노드부터 하나씩 → mgr → OSD(노드마다 `ceph osd set noout` 뒤 재시작, 끝나면 `unset`). 노드끼리 판이 다르면 새 mon·mgr·OSD 더하기를
  막는다.
- **넣지 않은 것.** mon·mgr·OSD 빼기와 디스크 교체, 유지보수·비우기 때 자동 `noout`, CephFS·RGW, 바깥 Ceph 넘겨받기, Ceph 업그레이드
  지휘, 장치 클래스별 CRUSH 규칙과 풀, 키 교체, Orbivisor 풀을 지울 때 Ceph 풀 지우기, Ceph 클러스터 지우기. 지금은 `ceph` 명령으로
  손으로 한다(mon 노드의 `/etc/ceph`에 admin 키링이 있다).

## Ceph RBD 풀

**Ceph RBD 풀**(`--rbd POOL[/NAMESPACE]`)은 Ceph 클러스터의 풀(과 그 안의 RBD 네임스페이스)이다. 볼륨마다 raw RBD
이미지 `opl-{볼륨 id}`를 만들고(`--image-format 2`) QEMU가 `rbd` 블록 드라이버로 Ceph에 직접 붙는다(커널 매핑
`rbd map`을 하지 않으므로 노드에 블록 장치가 생기지 않는다). 모든 노드가 같은 이미지를 보는 **공유 풀**이다. 스냅샷
규칙과 실행 중 스냅샷의 짧은 멈춤은 [LVM-thin 풀](#lvm-thin-풀)과 같다. 다른 점만 적는다.
Ceph 클러스터를 Orbivisor가 노드에 설치·구성하고 풀까지 등록하게 하려면 [Ceph 자동 구성](#ceph-자동-구성)을 쓴다(이 절은
관리자가 이미 운영하는 바깥 Ceph를 쓰는 길이다).

```bash
# Ceph 클러스터와 풀은 관리자가 미리 만든다(Orbivisor는 만들지 않는다).
ceph osd pool create rbd && rbd pool init rbd
# 노드마다: rbd·ceph 명령과 QEMU의 rbd 블록 드라이버, ceph.conf와 키링
sudo apt install ceph-common qemu-block-extra
sudo cp ceph.conf ceph.client.admin.keyring /etc/ceph/
orbivisorctl pool create ceph --rbd rbd              # client.admin, /etc/ceph/ceph.conf
orbivisorctl pool create vms --rbd rbd/opl --rbd-user opl --rbd-conf /etc/ceph/opl.conf  # 네임스페이스·사용자·conf
orbivisorctl volume create db --pool ceph --size 20G  # --format을 생략하면 raw
orbivisorctl pool get ceph                           # 노드별 준비·Ceph 풀 크기·남은 공간·오류, user·conf
```

- **공유, raw만.** VM은 풀이 준비된 어느 노드에서나 돈다(볼륨을 만든 노드에 묶이지 않는다). `--shared`는 필요 없고
  함께 쓸 수 없으며 API의 `"shared": false`는 **400** `invalid_request`("rbd pools are always shared")다. 쓸 노드는
  `--node`로 정한다(생략하면 모든 노드). 볼륨은 raw이고 qcow2 볼륨을 만들거나 가져오면 **400**("rbd pools hold raw
  volumes"). `orbivisorctl`·웹·API 가져오기는 형식을 생략하면 raw로 정한다. 볼륨 크기는 1 MiB 배수로 올린다.
- **이름과 접속.** Ceph 풀·네임스페이스·사용자는 `A-Za-z0-9_.-`의 1–63자이고 `-`로 시작하지 않는다. 사용자는
  `client.` 없이 적는다(`client.opl`이 아니라 `opl`. 생략하면 `admin`. `client.`로 시작하면 **400** "write the Ceph user
  without client."). `--rbd-conf`는 노드의 ceph.conf 절대
  경로이고(제어 문자·`,`·`:`·`=` 없이, 생략하면 Ceph 기본 `/etc/ceph/ceph.conf`) 키링은 그 conf가 가리키는 곳(기본
  `/etc/ceph/ceph.client.{사용자}.keyring`)에 노드마다 둔다. orbivisord와 orbivisord가 띄우는 QEMU가 같은 사용자로 읽는다. 사용자는
  그 풀의 이미지를 만들고 지우고 스냅샷할 수 있어야 하고 `ceph df`를 읽을 수 있어야 한다. `admin`이 아닌 사용자는
  `ceph auth get-or-create client.opl mon 'profile rbd' osd 'profile rbd pool=rbd' mgr 'profile rbd pool=rbd'`로
  만든다(CI가 이 caps의 사용자로 네임스페이스와 명시적 conf를 써서 시험한다).
- **업그레이드 먼저.** RBD 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다(예전 orbivisord는
  모르는 종류의 풀을 읽지 못한다).
- **노드별 상태.** orbivisord는 먼저 VM을 띄우는 QEMU에 rbd 블록 드라이버가 있는지 한 번 본다(없으면 `"qemu has no rbd
  block driver"` — Ubuntu는 `qemu-block-extra`). 그다음 15초마다 `rbd ls`로 풀(네임스페이스)에 닿는지 보고
  `ceph df detail`에서 그 풀의 `max_avail`+`stored`를 크기로, `max_avail`을 남은 공간으로 알린다(Ceph 풀에 바이트
  쿼터가 있으면 남은 공간은 쿼터에서 `stored`를 뺀 것까지다. 같은 Ceph 풀을 네임스페이스만 달리해 여러 Orbivisor 풀이
  나눠 쓰면 각 풀이 Ceph 풀 전체의 크기·남은 공간을 보인다. 합쳐 10초 제한,
  넘으면 `"rbd timed out"`). 실패하면 `rbd`·`ceph`의 오류 첫 줄이 이유로 남고, 준비됐다고 알린 노드에만 VM과
  볼륨 작업이 간다. RBD 명령은 `LC_ALL=C`로 부르고 하나에 60초(이미지를 지우는 `rbd snap purge`·`rbd rm`과 되돌리는
  `rbd snap rollback`은 이미지 크기에 따라 오래 걸리므로 600초)를 넘으면 실패로 본다. LVM처럼 읽기 말고 바꾸는 명령은
  죽이지 않고 뒤에서 끝나게 두며, 그동안 이 노드에서 같은 이미지를 바꾸는 명령은 띄우지 않고 잠시 뒤 다시 시도할
  오류로 끝낸다. 이 표시는 노드마다 따로이므로 RBD 볼륨의 만들기·지우기·크기 늘리기(쓰는 VM이 없을 때) Task는 요청을
  받은 노드가 아니라 늘 풀이 준비된 온라인 노드 가운데 id가 가장 작은 노드가 한다.
- **macOS 미지원.** Homebrew QEMU에는 rbd 드라이버가 없다. macOS 노드는 "rbd pools are not supported on macOS"로
  알리고, 그 노드에 놓인 RBD 디스크의 VM은 이유와 함께 꺼진다.
- **VM 시작.** 활성화할 것이 없고 이미지가 있는지(`rbd info`)만 본다. 이미지가 없으면 VM을 이유와 함께 끄고,
  Ceph에 닿지 않는 등 `rbd`가 실패하면 끄지 않고 잠시 뒤 다시 시도한다. 이 노드에서 그 이미지를 바꾸는 명령(시간을
  넘긴 되돌리기 등)이 아직 돌고 있어도 잠시 뒤 다시 시도한다.
- **스냅샷.** VM 스냅샷은 RBD 스냅샷 `opl-{볼륨 id}@{스냅샷 id}`다. VM의 디스크는 모두 qcow2이거나 모두
  LVM-thin이거나 모두 ZFS이거나 모두 RBD여야 한다(블록 풀끼리 섞여도 **409** "disks use different snapshot
  methods"). 메모리 스냅샷은 없다(`memory: true`면 **409**, 예약은 디스크만). 실행 중인 VM은 LVM-thin처럼 잠깐
  멈추고 디스크를 하나씩 찍는다(RBD에는 여러 이미지의 원자적 스냅샷이 없지만 VM이 멈춘 동안 찍으므로 VM에게는
  한 시점이다). 스냅샷 지우기는 VM이 돌고 있어도 된다. 찍기가 시간을 넘겨 남긴 스냅샷은 LVM-thin처럼 뒤에서 다시
  지우고, 남은 것은 orbivisord가 시작할 때와 그 뒤 30분마다 지운다. 공유 풀이므로 이 정리는 풀을 쓸 수 있는 온라인 노드
  가운데 id가 가장 작은 노드 하나가 맡고, 다른 노드가 막 찍은 스냅샷을 건드리지 않게 만든 지 30분이 지난 것만
  지운다.
- **아무 스냅샷으로나 되돌린다.** ZFS와 달리 더 오래된 스냅샷으로도 되돌릴 수 있고 그 뒤의 스냅샷은 남는다(다시
  더 최근 스냅샷으로 갈 수 있다). 되돌리기는 지금처럼 VM을 강제로 끈 뒤 `rbd snap rollback`을 한다. 이미지를 연
  클라이언트(다른 노드의 QEMU 등)가 있으면 잠시 뒤 다시 해 볼 오류로 끝난다.
- **지우기·복제·크기.** 볼륨을 지우면 이미지를 스냅샷과 함께 지운다(`rbd snap purge`, `rbd rm`). 이미지를 연
  클라이언트가 있으면(`rbd status`) 스냅샷도 지우지 않고 잠시 뒤 다시 해 볼 오류로 끝난다. 시간을 넘긴 만들기가 아직 돌고 있어 이미지가 없으면 볼륨을 지운 것으로 보지 않고 잠시 뒤 다시 해 본다.
  풀을 지워도 Orbivisor 리소스만 지우고 Ceph 풀과 그 안의 다른 이미지는 그대로 둔다. 복제·복사는 `rbd clone`이 아니라
  `qemu-img convert`로 하는 전체 복사다(QEMU의 rbd 드라이버로 사용자·conf·네임스페이스를 넘긴다). 스냅샷에서
  복제하면 그 RBD 스냅샷을 읽어 대상 풀이 블록 풀(RBD·LVM-thin·ZFS)이면 raw, 디렉터리·NFS 풀이면 qcow2로 만들고,
  대상은 스냅샷 때의 크기로 만든다. 크기 늘리기는 실행 중이어도 되고 `rbd resize` 뒤 QEMU에 새 크기를 알린다(이미
  그 크기 이상이면 건너뛴다).
- **Ceph 풀이 가득 차면** 게스트의 쓰기가 실패한다(이미지는 thin이다). `orbivisorctl pool get`의 남은 공간을 보고
  관리자가 Ceph 클러스터를 늘린다.

## 공유 LVM(iSCSI) 풀

**공유 LVM 풀**(`--lvm-shared VG`)은 모든 노드가 보는 LUN(iSCSI 타깃이나 FC·멀티패스 SAN) 위의 **공유 VG**다.
관리자가 큰 LUN 하나에 공유 VG를 한 번 만들면 Orbivisor가 그 안에 볼륨마다 두꺼운 LV `opl-{볼륨 id}`를 만들고, VM에는
[LVM-thin 풀](#lvm-thin-풀)처럼 블록 장치(`/dev/{VG}/opl-{볼륨 id}`, QEMU `host_device`)로 붙인다. 여러 노드가 같은
VG를 바꾸므로 노드 사이 잠금은 LVM의 **lvmlockd**(sanlock 또는 dlm)가 맡고, LV는 쓰는 노드 하나에서만 켠다(배타
활성화). `--iscsi-portal`·`--iscsi-target`을 주면 orbivisord가 노드마다 그 타깃에 로그인한다. 모든 노드가 같은 LV를 보는
**공유 풀**이고 **스냅샷이 없다**.

관리자가 미리 할 일(Orbivisor는 타깃·LUN·VG를 만들지 않고 잠금 데몬을 설치하지 않는다):

```bash
# 1. 스토리지: LUN 하나를 모든 노드에 내보낸다. iSCSI면 타깃의 이니시에이터 ACL로 노드만 들인다(CHAP은 지원하지 않는다).
# 2. 노드마다: 잠금 데몬과 iSCSI 이니시에이터. sanlock은 watchdog이 필요하다(하드웨어가 없으면 softdog).
sudo apt install lvm2-lockd sanlock open-iscsi
sudo modprobe softdog                                # 하드웨어 watchdog이 없을 때만(부팅 때 올라오게 둔다)
sudo sed -i 's/^\s*#\?\s*use_lvmlockd = .*/\tuse_lvmlockd = 1/' /etc/lvm/lvm.conf   # global 절의 use_lvmlockd = 1
printf 'local {\n\thost_id = 1\n}\n' | sudo tee /etc/lvm/lvmlocal.conf   # host_id는 노드마다 달라야 한다(1–2000)
sudo systemctl enable --now wdmd sanlock lvmlockd
# 3. 한 노드에서 한 번: LUN에 로그인해 공유 VG를 만든다(잠금 시작까지 수십 초 걸릴 수 있다).
sudo iscsiadm -m discovery -t sendtargets -p san.lan
sudo iscsiadm -m node -T iqn.2003-01.org.example:storage -p san.lan --login
sudo vgcreate --shared san /dev/disk/by-path/ip-10.0.0.9:3260-iscsi-iqn.2003-01.org.example:storage-lun-0
# 그다음은 Orbivisor가 한다: 노드마다 로그인하고 잠금을 시작한다(vgchange --lockstart).
orbivisorctl pool create san --lvm-shared san --iscsi-portal san.lan:3260 --iscsi-target iqn.2003-01.org.example:storage
orbivisorctl pool create fc --lvm-shared fcvg        # 노드가 이미 LUN을 보면(FC 등) iSCSI 옵션 없이
orbivisorctl volume create db --pool san --size 20G  # --format을 생략하면 raw
orbivisorctl pool list                               # LOCATION: san @ san.lan:3260 iqn.2003-01.org.example:storage
orbivisorctl pool get san                            # 노드별 준비·VG 크기·남은 공간·오류
```

- **공유, raw만, 스냅샷 없음.** VM은 풀이 준비된 어느 노드에서나 돈다. `--shared`는 필요 없고 함께 쓸 수 없으며
  API의 `"shared": false`는 **400** `invalid_request`("lvm-shared pools are always shared")다. 볼륨은 raw이고 qcow2
  볼륨을 만들거나 가져오면 **400**("lvm-shared pools hold raw volumes"). `orbivisorctl`·웹·API 가져오기는 형식을 생략하면
  raw로 정한다. 공유 LVM 디스크가 있는 VM의 스냅샷은 **409** `snapshot_unsupported`("disks in storage pool … cannot
  be snapshotted")이고 그런 VM의 예약 스냅샷도 실패한다(웹은 스냅샷 버튼을 이유와 함께 막고 예약을 켜면 알린다).
  그래서 복제는 멈춘 VM의 지금 상태로만 한다.
- **이름과 접속.** VG 이름은 [LVM-thin 풀](#lvm-thin-풀)과 같은 규칙이다(틀리면 **400** "invalid lvm name …"). 포털은
  `호스트[:포트]`이고 IPv6 주소는 대괄호로 감싼다(`[fd00::5]:3260`, 포트 1–65535). 타깃은 `iqn.`·`eui.`·`naa.`로
  시작하고 소문자·숫자·`.`·`:`·`-`로 된 223자 이하다. 틀리면 **400**("invalid iscsi portal …", "invalid iscsi
  target …"). `--iscsi-portal`과 `--iscsi-target`은 함께 쓴다.
- **업그레이드 먼저.** 공유 LVM 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다(예전 orbivisord는
  모르는 종류의 풀을 읽지 못한다).
- **노드별 상태.** orbivisord(root)는 15초마다 풀을 준비한다: iSCSI 타깃이 있으면 그 타깃의 세션이 없을 때 `iscsiadm`으로
  찾아 로그인하고(`-m discovery -t sendtargets`, `-m node --login`, 그 뒤 `udevadm settle`. 세션은 타깃 이름으로 찾는다),
  `vgchange --lockstart`로 VG 잠금을 시작한 뒤(이미 시작했으면 곧바로 끝난다) `vgs`(10초 제한)로 VG를 본다. 잠금
  종류가 `sanlock`·`dlm`이면 준비되고 VG의 크기·남은 공간을 알린다. 잠금이 없는 VG는 `"volume group san is not a
  shared (lvmlockd) VG"`, 그 밖의 실패는 명령의 오류 첫 줄이다(`vgs`가 시간을 넘기면 `"vgs timed out"`). 로그인과
  처음 잠금 시작은 sanlock 임대 때문에 오래 걸릴 수 있으므로 죽이지 않는다: 10초 안에 끝나지 않으면 `"iscsi login or
  vg lock start in progress"`로 알리고 다음 확인에서 결과를 거둔다. 풀을 지워도 로그아웃하거나 잠금을 멈추지 않는다.
- **macOS 미지원.** macOS 노드는 "lvm-shared pools are not supported on macOS"로 알리고, 그 노드에 놓인 공유 LVM
  디스크의 VM은 이유와 함께 꺼진다.
- **볼륨 만들기.** LV는 켜지 않고 만든 뒤(`lvcreate -an -Zn -Wn`, 크기는 1 MiB 배수로 올린다) 이 노드에서 배타
  활성화해 `blkdiscard -z`로 모두 0으로 채우고 끈다. 0 채우기는 크기에 비례해 걸리므로 시간 제한이 없다(SAN이 WRITE
  SAME·UNMAP 오프로드를 하면 빠르다). 켜거나 채우지 못하면 LV를 지운다. 두꺼운 LV는 만들 때 자리를 잡으므로 VG가
  모자라면 만들기가 실패하고 게스트의 쓰기가 나중에 실패하지는 않는다. 가져오기·복사의 대상도 이렇게 만든 뒤
  `qemu-img convert -n --target-is-zero`로 써 넣고 끈다.
- **활성화의 수명.** LV는 쓰는 동안만 그 노드에서 켜져 있다. VM을 시작할 때 orbivisord가 디스크 LV를 배타 활성화하고
  (`lvchange -aey`), QEMU가 끝나면(멈춤·충돌·포기) 그리고 시작이 실패하면 끈다(`-an`. 끄지 못하면 경고만 남긴다).
  켜진 채 남은 LV(orbivisord가 그 전에 내려갔을 때 등)는 orbivisord가 시작할 때와 그 뒤 30분마다 끈다. 이때 이 노드에서
  실행 중이거나 시작하려는 VM의 디스크, 이 노드에서 끝나지 않은 작업의 볼륨, 만드는 중인 볼륨은 그대로 둔다.
- **다른 노드가 쓰는 중.** 다른 노드가 켜 둔 LV를 켜거나 지우거나 늘리려 하면 lvmlockd가 거절하고, Orbivisor는 잠시
  뒤 다시 해 볼 오류로 끝낸다("timed out waiting for another node to stop using the lv", VG 잠금이면 "… to release
  the shared vg lock"). VM은 꺼지지 않고 `starting`에서 다시 시도한다. **노드가 죽으면** 그 노드가 켜 둔 LV는 sanlock
  임대가 끝날 때까지(기본 수 분) 다른 노드에서 쓸 수 없다. 한 노드에서 **끄기가 실패하면**(경고만 남긴다) 그
  노드의 정리(orbivisord 시작 때와 30분마다)가 끌 때까지 다른 노드에서 그 볼륨을 켜거나 늘리거나 지울 수 없다(같은 일시
  오류).
- **볼륨 작업 노드.** LVM 명령은 LVM-thin처럼 하나에 60초를 넘으면 실패로 보되 LV를 바꾸는 명령은 죽이지 않고, 그
  표시가 노드마다 따로이므로 공유 LVM 볼륨의 만들기·지우기·크기 늘리기(쓰는 VM이 없을 때) 작업은 [Ceph RBD
  풀](#ceph-rbd-풀)처럼 풀이 준비된 온라인 노드 가운데 id가 가장 작은 노드가 한다. 가져오기는 올린 파일이 있는 노드,
  실행 중인 VM의 디스크 크기 늘리기는 VM의 노드에서 한다.
- **지우기·복제·크기.** 볼륨을 지우면 LV를 지운다(`lvremove`). 시간을 넘긴 만들기가 아직 돌고 있어 LV가 없으면 지운
  것으로 보지 않고 잠시 뒤 다시 해 본다. 풀을 지워도 Orbivisor 리소스만 지우고 VG와 그 안의 다른 LV는 그대로 둔다.
  복제·복사는 원본 LV를 켜서 읽고 끝나면 끈다. 대상 풀이 블록 풀이면 raw, 디렉터리·NFS 풀이면 qcow2로 만든다.
  크기 늘리기는 실행 중이어도 된다. `lvextend` 뒤 늘린 부분을 `blkdiscard -z`로 0으로 채우고(새 익스텐트에 지운
  볼륨의 옛 데이터가 남아 있을 수 있다. 시간 제한 없음) 실행 중이면 QEMU에 새 크기를 알린다. 꺼진 볼륨은 채우는
  동안만 배타 활성화한다. 늘리기 전 크기를 LV 태그(`opl-zero-from-…`)로 적어 두고 다 채우면 뗀다. 0 채우기가
  실패하면 작업이 실패하므로 **크기 늘리기가 실패하면 다시 한다**: LV가 이미 그 크기여도 남은 부분을 다시 채운다.
  다시 하기 전에 VM을 시작하거나 그 볼륨을 복제·복사하면 LV를 켤 때 남은 부분을 먼저 마저 채우고 태그를 뗀다(옛
  데이터를 내주지 않는다). 이 0 채우기도 남은 크기에 비례하므로 **0 채우기가 실패한 볼륨의 VM은 다음 시작이 오래
  걸릴 수 있다**. 크기 늘리기 작업이 실행 중인(`running`) 디스크의 VM은 켜지 않고 `starting`에서 기다리고(아직
  시작하지 않은(`pending`) 작업은 막지 않는다), VM이 켜지거나
  꺼지는 중이면 그 디스크의 크기 늘리기는 409 `vm_busy`다(모든 풀이 같다). 크기 늘리기 작업을 하던 **노드가
  죽으면** 그 작업은 그 노드가 돌아오거나 클러스터에서 빠질 때까지 끝나지 않으므로, 그동안 그 디스크의 VM은
  시작하지 않고 `starting`에서 기다린다(오프라인 노드의 아직 시작하지 않은 작업은 약 1분 뒤 실패로 정리된다).
- **호스트 LVM이 게스트 디스크를 읽지 않게.** [LVM-thin 풀](#lvm-thin-풀)과 같다(lvm2 기본값 `scan_lvs = 0`이면
  그대로 둔다).
