# 백업·복제·가져오기

백업과 복원, 백업 저장소(중복 제거·암호화·S3·검증·라이브 복원), 파일 단위 복원, 원격 클러스터, 원격 복제(DR), VMware에서 가져오기. 처음 보는
사람은 [README](../../README.md)의 빠른 시작부터.

- [백업과 복원](#백업과-복원)
- [백업 저장소(중복 제거·암호화·S3)](#백업-저장소중복-제거암호화s3)
- [파일 단위 복원](#파일-단위-복원)
- [원격 클러스터](#원격-클러스터)
- [원격 복제(DR)](#원격-복제dr)
- [VMware에서 가져오기](#vmware에서-가져오기)

## 백업과 복원

백업은 VM 전체(디스크, UEFI 변수, VM 스펙)를 다른 저장소에 떠 둔 것이다. 스냅샷과 달리 VM 디스크의 저장소를
잃거나 VM을 지워도 남고, 언제든 **새 VM**으로 되살린다. 백업은 `backups`를 켠 **디렉터리·NFS 풀**(백업 풀)에 두거나,
중복 제거·암호화·S3를 쓰는 **백업 저장소**(계획 68, 아래 [백업 저장소](#백업-저장소중복-제거암호화s3))에 둔다. 이 절은 백업 풀의 백업이다.
노드 사이 로컬 디스크 복제(위 "로컬 디스크 복제와 HA")와 함께 쓸 수 있다 — 복제 비트맵과 증분 백업 비트맵은 따로다.
CD-ROM 매체와 cloud-init 씨앗은 담지 않는다 — 복원은 없는 ISO의 드라이브를 비우고 경고한다("설치 매체와 cloud-init").

```bash
orbivisorctl pool create bk --nfs nas.lan:/srv/backup --backups  # 백업 풀(디렉터리 풀이면 --path /srv/backup)
orbivisorctl pool set local --backups true                   # 있는 dir·NFS 풀을 백업 풀로(false는 끄기)
orbivisorctl backup create web --pool bk                     # 이름은 web-20261104-030000(UTC)처럼 정해진다
orbivisorctl backup create web --pool bk --name before-upgrade
orbivisorctl backup create web --pool bk --incremental       # 할 수 있으면 증분(아래 "증분 백업")
# backup web-20261105-030000 of vm web created (incremental)     할 수 없었으면 (full: <이유>)
orbivisorctl backup list                                     # --vm web이면 지금 web인 VM과 web이라는 이름으로 남은(지운 VM) 백업
# NAME                 VM   TARGET  PHASE  CREATED               SIZE      TYPE  ORIGIN  REPLICAS  VERIFIED
# before-upgrade       web  bk      ready  2026-11-04 03:00 UTC  412.5MiB  full  -       -         -
# web-20261105-030000  web  bk      ready  2026-11-05 03:00 UTC  18.2MiB   incr  -       -         -
orbivisorctl backup get before-upgrade                       # 디스크별 크기, UEFI 변수, 오류, 증분이면 부모와 체인(JSON)
orbivisorctl backup restore before-upgrade --name web2 --pool local  # 새 VM web2(꺼진 채), --node로 노드를 정할 수 있다
orbivisorctl backup delete before-upgrade                    # 파일과 리소스를 지운다(실패한 백업도)
```

웹은 보호 › 백업 탭(모든 백업, VM이 지워진 백업 포함)과 VM 상세의 "백업" 탭(지금 백업, 복원)에서 하고,
스토리지 풀 화면의 "백업 저장" 스위치로 백업 풀을 정한다. API는 `POST /api/v1/vms/{name}/backups`
(`{"pool": …, "name": …, "incremental": …}` — `incremental`은 기본 false), `GET /api/v1/backups[?vm=]`,
`GET`·`DELETE /api/v1/backups/{name}`, `POST /api/v1/backups/{name}/restore`(`{"name": …, "pool": …, "node": …}`)이고
만들기·지우기·복원은 작업(Task)을 돌려준다.
`DELETE /api/v1/backups/{name}?force=true`(`orbivisorctl backup delete NAME --force`)는 파일을 지울 노드가 없을 때 리소스만
지우고 200(`{"message": …}`)이다.

- **백업 풀.** dir·NFS 풀만 백업을 담는다(다른 풀은 400). 백업 풀에도 지금처럼 VM 디스크를 둘 수 있다. 백업이 남은
  풀은 지울 수 없고 `backups`를 끄거나 `shared`를 바꿀 수도 없다(409 `pool_in_use`, "holds backups"). 백업 디렉터리를
  다른 서버에 두려면 NFS 풀을 쓴다 — 공유가 아닌 디렉터리 풀은 노드 로컬이라 그 노드의 디스크를 잃으면 함께 잃는다.
  공유가 아닌 풀의 백업은 쓴 노드를 기억하고(`status.node`) 그 노드에서만 복원하고 지운다: 복원에서 다른 `node`를
  주면 409 `backup_on_other_node`, 그 노드가 오프라인이면 409 `node_offline`, 대상 풀을 그 노드에서 쓸 수 없으면 409
  `pool_not_on_node`다. 그 노드를 잃었으면 `orbivisorctl backup delete NAME --force`(`DELETE …?force=true`)로 리소스만
  지운다(파일은 남는다). 풀째 지워진
  백업(저장소에서 풀이 사라짐)은 보통의 지우기가 리소스만 지운다.
- **실행 중·꺼진 VM.** 실행 중인 VM은 디스크마다 메모리 없는 임시 스냅샷(이름은 하이픈 없는 백업 id)을 찍어 그때의
  내용을 복사하고 끝나면 지운다. VM은 계속 돈다(블록 풀 디스크는 스냅샷을 찍는 동안만 잠깐 멈춘다). 그래서 실행
  중인 VM의 디스크는 모두 스냅샷을 지원해야 한다 — dir·NFS 풀의 raw 디스크나 공유 LVM 풀 디스크가 있으면 409
  `vm_running`("stop it to back it up")이고, VM을 끄면 된다. 스냅샷처럼 방식이 다른 디스크(qcow2와 블록 풀, 서로 다른
  블록 백엔드)가 섞여도 409 `snapshot_unsupported`다. 블록 풀 디스크를 위해 멈춘 VM을 이어서 실행하지 못하면 한 번 더
  해 보고, 그래도 안 되면 디스크를 읽지 않고 백업이 실패한다. 백업 작업이 도는 동안 그 VM의 시작·정지·수정·지우기와
  마이그레이션은 409 `vm_busy`다(강제 정지는 된다). 작업은 VM의 노드에서 돈다. 배치된 적 없는 꺼진 VM은 백업 풀과
  디스크를 모두 읽을 수 있는 온라인 노드에서 한다(없으면 409 `no_node`).
- **crash-consistent.** 백업은 그 순간에 전원을 뽑은 것과 같은 디스크다(게스트 파일 시스템을 얼리지 않는다). 파일
  시스템 저널은 되살아나지만, 데이터베이스처럼 메모리에 쓰기를 쥐는 앱은 게스트 안에서 덤프를 떠 두거나 백업 동안
  쓰기를 멈춰 일관성을 맞춘다. 꺼진 VM의 백업은 일관적이다. 디스크 파일은 `qemu-img`가 닫을 때 디스크에 내려
  쓰고(`-t writeback`), `vars.fd`와 `manifest.json`은 fsync한 뒤 이름을 바꾸고 백업 디렉터리도 fsync한 다음에야
  `ready`가 된다.
- **복원.** 늘 새 VM이다(기존 VM을 덮어쓰지 않는다). 디스크마다 대상 풀에 새 볼륨 `{새 이름}-disk0`, `-disk1`…을
  만든다(블록 풀이면 raw, 아니면 qcow2). 새 VM은 백업 때의 VM 스펙(CPU·메모리·NIC·펌웨어 등)과 UEFI 변수를 받고, 새
  MAC, 꺼진 채, 노드·예약 스냅샷 없음, HA 끔이다. VM 스펙은 클러스터의 백업 기록에서 가져오고 `manifest.json`이
  그 기록(VM 스펙, 디스크 수)과 다르면 실패한다. 백업 디렉터리의 디스크·`vars.fd`는 일반 파일이어야 한다(심볼릭
  링크는 거절). 스펙의 네트워크와 보안 그룹이 지금 있어야 한다(400). 복원은 백업
  풀과 대상 풀을 함께 쓸 수 있는 노드에서 돈다. 실패하면 만든 볼륨을 지운다. 이름을 원래대로 돌리려면 원본을
  지운 뒤 그 이름으로 복원한다.
- **권한.** 읽기는 모든 사용자, 만들기·지우기·복원은 operator 이상이다(VM이 지워진 백업도 operator가 지운다).
- **오래 걸린다.** 진행률은 없고 작업에 시간 제한이 없다. `orbivisorctl backup create`·`restore`는 24시간까지 기다리고,
  그 뒤에도 작업은 돈다(`orbivisorctl task get`). 전체 백업 하나는 디스크 크기만큼 읽고 압축해 쓴다(증분 백업은 바뀐
  블록만 — 아래).

**증분 백업.** 실행 중인 VM은 QEMU dirty bitmap으로 디스크마다 바뀐 블록을 따라간다. `incremental`(`backup create
--incremental`, 웹 "지금 백업"의 "증분(가능하면)", 백업 작업의 `full_every`)로 청하면 같은 풀에 있는 그 VM의 바로
앞 백업을 **부모**로, 그 뒤로 바뀐 블록만 담은 백업을 만든다. 청하지 않으면 지금처럼 전체 백업이다.

- **비트맵 규칙.** 백업이 성공할 때마다(전체든 증분이든, 청하지 않은 백업도) 디스크마다 비트맵 `opl-<백업 id(하이픈
  없음)>`을 그 백업의 시점에 만들고 다른 `opl-` 비트맵을 지운다. 그래서 VM마다 "마지막으로 성공한 백업 뒤로 바뀐 블록"
  하나만 있고, 그 백업만 다음 증분의 부모가 된다. 비트맵을 만들지 못해도(경고) 백업은 성공하고 다음은 전체다. 실패한
  백업은 새 비트맵을 지우고 앞 비트맵을 남긴다(다음 시도도 같은 부모에서 증분). 증분 대상 파일을 닫지 못하면(QEMU가 닫을
  때 파일의 메타데이터를 쓴다) 그 증분은 실패다. qcow2 디스크(dir·NFS 풀)의 비트맵은 qcow2 파일에 저장돼 VM을 다시 켜도
  남는다(QEMU가 정상으로 끝날 때 저장한다). 블록 풀 디스크(raw — LVM-thin·ZFS·RBD·공유 LVM(iSCSI))의 비트맵은 QEMU
  안에만 있어 VM을 다시 켜면 사라지므로 다시 켠 뒤 첫 백업은 전체다. 라이브 마이그레이션은 비트맵을 함께 옮긴다(qcow2의
  것은 공유 이미지로, raw 디스크의 것은 마이그레이션 스트림으로 — raw 디스크에 비트맵이 있을 때만 QEMU의 `dirty-bitmaps`
  능력을 켠다). 꺼진 VM을 백업하면 qcow2 디스크에만 비트맵을 둔다. QEMU가 비정상으로 끝나면(SIGKILL, 호스트 장애 — HA
  복구 포함) qcow2 파일의 비트맵은 inconsistent로 남는다. 그런 비트맵은 쓸 수 없고 마이그레이션·디스크 크기 늘리기를
  막으므로 orbivisord가 VM을 켤 때(그리고 마이그레이션·실행 중 크기 늘리기 전에) 지운다 — 다음 백업은 전체(`no dirty bitmap on
  disk{i}`)다. 꺼진 qcow2 디스크의 크기를 늘리면 비트맵도 지운다(크기가 바뀌면 어차피 전체).
- **꺼진 VM은 늘 전체다.** 증분은 실행 중인 VM만 한다. 그 밖에 증분을 할 수 없으면 오류 없이 전체로 하고 이유를
  `status.full_reason`(`backup get`, 웹 "전체로 한 이유")에 적는다: `VM is stopped`, `no earlier backup of this VM in
  pool "P"`, `no dirty bitmap on disk{i}`(그 디스크에 비트맵이 없음 — 블록 풀 VM을 다시 켰거나 디스크를 더함),
  `dirty bitmap on disk{i} is inconsistent`(비트맵이 기록하지 않거나, 비정상 종료 뒤 지우지 못함 — 비정상 종료·HA
  복구 뒤에는 VM을 켤 때 지우므로 보통 `no dirty bitmap on disk{i}`가 보인다), `dirty bitmaps do not match a ready
  backup in pool "P"`(마지막 백업이 다른 풀이거나 지워짐 — 같은 VM을 두 풀에 번갈아 백업하면 늘 전체다), `disks
  changed since backup "B"`(디스크를 바꾸거나 크기를 늘림), `chain already has N incremental backup(s)`(백업 작업의
  `full_every`), `some nodes do not support incremental backups yet`(아래 업그레이드), `backup "P" is stored on another
  node`·`files of backup "P" are missing on this node`(아래 다른 노드의 부모), `backup "P" is being deleted`(부모를 지우는
  작업이 이미 돌고 있음). 스냅샷 되돌리기는 디스크를 되돌리기 전에 비트맵을 지우므로 그 뒤 첫 백업은 전체다. 증분을
  만드는 사이 부모가 지워지면 그 증분은 실패하고("parent backup "P" was deleted during the backup") 비트맵을 지워
  다음은 전체다.
- **다른 노드의 부모.** 공유가 아닌(디렉터리) 백업 풀의 백업은 쓴 노드에만 있다(`status.node`). VM이 다른 노드로
  옮겨졌으면 비트맵은 맞아도 부모 파일이 그 노드에 없어 체인을 복원할 수 없으므로 전체로 한다(위 이유). 백업 풀이
  NFS(공유)면 어느 노드에서나 이어서 증분한다.
- **체인.** 증분 백업은 부모 없이는 복원할 수 없다. 부모를 거슬러 전체 백업까지가 그 백업의 **체인**이다. 목록의
  `TYPE`은 `full`·`incr`(웹 "종류" 배지와 "부모:" 링크)이고, 증분의 `SIZE`는 자기 파일 크기다. `backup get`은 증분
  백업에 `chain: {"backups": [전체부터 이것까지 이름], "stored_bytes": 체인 파일 크기의 합}`을 더하고(다 만들지 않은
  백업이 체인에 있거나 목록에 없는 조상이 있으면 `stored_bytes`는 없다 — 목록에 없는 조상은 id로 적는다), 웹은 크기
  아래 "체인 <크기>"를 보인다(같은 경우 보이지 않는다). 복원은 체인의 모든 백업이 같은 풀에 `ready`로 있고 디스크
  수·크기가 맞아야 한다(아니면 작업 실패 "backup chain is broken: …"). 체인 중간에서도 복원한다(그 시점의 디스크).
  체인을 원격으로 복제했으면 `chain.replicas`에 원격 사본마다 원격에 있는 체인 백업 수가 붙는다
  (아래 "증분 백업 복제"의 "진행").
- **지우기는 자식부터.** 다른 백업의 부모인 백업을 지우면 **409** `backup_in_use`("backup "B" is the parent of N
  incremental backup(s); delete them first", `--force`여도)다. 가장 새 증분부터 지운다. 만드는 중인 증분도 부모를
  막고, 실패한 증분은 막지 않는다. 병합(체인 줄이기)은 없다.
- **Orbivisor 밖에서 디스크를 고쳤으면**(VM을 끄고 `qemu-img`·`qemu-io`·`guestfish`로 쓰기, 볼륨 파일 바꾸기) 비트맵이
  그 쓰기를 모르므로 다음 백업을 `incremental` 없이(전체로) 한다. 그 뒤로는 다시 증분이 맞다. qcow2 디스크에 비트맵이
  있으면 `qemu-img snapshot -a`(되돌리기)가 "Device has active dirty bitmaps"로 실패한다 — Orbivisor의 스냅샷 되돌리기를
  쓰거나 먼저 `qemu-img bitmap --remove <파일> opl-<id>`로 지운다(그 뒤 첫 백업은 전체).
- **업그레이드 중.** 클러스터의 모든 노드(위트니스 포함)에 증분 백업 기능이 있어야 `incremental`을 받는다(아니면
  **409** `cluster_upgrading`). 기능이 없는 노드가 하나라도 있으면 백업은 비트맵을 만들지 않고 증분 요청은 전체로 한다
  (옛 노드는 되돌리기 전에 비트맵을 지우지 못하므로). 증분 백업의 복원은 복원하는 노드에 그 기능이 있어야 한다(**409**
  `cluster_upgrading` "node "X" does not support incremental backups yet; upgrade it first").
- **원격 복제.** 증분 백업도 원격으로 복제한다. 복제는 체인을 전체 백업부터 차례로 보내고(prod에 `ready`로 적힌 앞
  백업은 건너뛴다) 원격의 사본도 같은 부모 링크의 체인이 된다. 백업 작업에 `full_every`와 `replicate`를 함께 둘 수 있다.
  원격의 모든 노드에 증분 복제 기능이 있어야 한다(아래 "원격 복제(DR)"의 "증분 백업 복제").

**파일 형식과 Orbivisor 없이 열기.** 백업 하나는 백업 풀 루트의 `backups/{백업 id}/` 디렉터리다(id는 `orbivisorctl backup
get NAME`의 `id`; NFS 풀이면 NFS 서버의 내보내기 아래 같은 경로). 그 안에 디스크마다 압축 qcow2 `disk0.qcow2`,
`disk1.qcow2`…(backing file 없음, 스냅샷 없음), UEFI VM이면 변수 파일 `vars.fd`, TPM VM이면 TPM 상태
`tpm2-00.permall`(매니페스트 `tpm_state: true`, swtpm `--tpmstate dir=`로 연다 — [vTPM과 Secure Boot](vms.md#vtpm과-secure-boot)), 그리고 `manifest.json`(형식 버전
`version: 1`, 백업 id, VM id·이름, VM 스펙 전체, 디스크 목록과 크기, 만든 시각)이 있다. 증분 백업의 매니페스트는
`version: 2`이고 부모 백업 id `parent`가 더 있다. 표준 qcow2라 Orbivisor 없이도 연다:

```bash
qemu-img info backups/<id>/disk0.qcow2                                  # 크기 확인
qemu-img convert -O raw backups/<id>/disk0.qcow2 disk0.raw              # raw 이미지로(dd로 디스크에 써도 된다)
qemu-system-x86_64 -m 2048 -drive file=backups/<id>/disk0.qcow2,snapshot=on   # 원본을 바꾸지 않고 켜 보기
jq .vm_spec backups/<id>/manifest.json                                 # CPU·메모리·NIC 등 VM 설정
```

UEFI VM은 OVMF(arm64는 AAVMF) 코드 파일과 함께 `vars.fd`의 사본을 `-drive if=pflash`로 준다. 형식 버전 1·2가 아닌
매니페스트는 복원하지 않는다(작업이 실패한다).

증분 백업의 디스크 파일은 바뀐 블록만 담고 헤더에 부모(backing file)가 **없다** — 혼자 열면 바뀌지 않은 곳이 0으로
보인다. 체인은 `manifest.json`의 `parent`로만 이어지므로, 열 때 `json:` 이미지 옵션으로 부모를 직접 적는다(자식이
바깥, 맨 끝 전체 백업은 `"backing": null`). 증분이 여럿이면 `backing` 안에 차례로 겹친다:

```bash
jq -r .parent backups/<증분 id>/manifest.json          # 부모 id(전체 백업이면 null)
CHAIN='json:{"driver":"qcow2","file":{"driver":"file","filename":"backups/<증분 id>/disk0.qcow2"},
  "backing":{"driver":"qcow2","file":{"driver":"file","filename":"backups/<전체 id>/disk0.qcow2"},"backing":null}}'
qemu-img convert -O qcow2 "$CHAIN" disk0-full.qcow2   # 체인을 이미지 하나로(이것을 위처럼 연다)
qemu-img compare "$CHAIN" disk0-full.qcow2            # Images are identical.
```

**백업 작업(예약 백업과 보존).** 백업 작업은 정한 VM들을 정한 시각에 백업 풀로 백업하고, 보존 규칙에 맞지 않는
오래된 백업을 지운다.

```bash
orbivisorctl backup-job create nightly --pool bk --all --at 03:00 --keep-daily 7 --keep-weekly 4  # 모든 VM, 매일 03:00 UTC
orbivisorctl backup-job create prod --pool bk --label tier=prod --at 01:30 --days mon,thu --keep-last 3
orbivisorctl backup-job create db --pool bk --vm db --vm db2 --at 02:00 --keep-last 2 --keep-monthly 6 --disabled
orbivisorctl backup-job create weekly-full --pool bk --all --at 03:00 --keep-daily 14 --full-every 6  # 매일 증분, 증분 6개마다 전체
orbivisorctl backup-job list
# NAME         POOL  VMS  SCHEDULE                       KEEP              ENABLED  NEXT                  LAST
# nightly      bk    all  daily 03:00 UTC                daily=7,weekly=4  true     2026-11-05 03:00 UTC  2026-11-04 03:00 UTC (12 ok, 1 failed)
# weekly-full  bk    all  daily 03:00 UTC, full every 6  daily=14          true     2026-11-05 03:00 UTC  -
orbivisorctl backup-job get nightly            # 일정·보존·다음 실행·마지막 실행 결과와 VM별 오류(JSON)
orbivisorctl backup-job set prod --at 04:00 --keep-last 5 --enabled true  # 준 값만 바꾼다(보존은 준 단위만, 나머지는 그대로)
orbivisorctl backup-job set prod --full-every 0  # 늘 전체 백업으로
orbivisorctl backup-job run nightly            # 지금 실행(스케줄러의 다음 확인 때 시작한다)
orbivisorctl backup-job delete nightly         # 작업만 지운다. 만든 백업은 남는다
```

- **대상.** `--all`(모든 VM), `--vm NAME`(여러 번), `--label KEY=VALUE`(여러 번이면 모두 맞는 VM) 중 하나다. VM은
  실행할 때마다 다시 고르므로 나중에 만든 VM과 라벨을 붙인 VM도 들어간다.
- **일정은 UTC다.** `--at HH:MM`(UTC)에, `--days`(`mon`…`sun`)를 주면 그 요일에만 돈다(없으면 매일). 리더 노드가
  1분마다 확인하므로 정한 시각에서 1분쯤 늦게 시작할 수 있다. 리더가 꺼져 있던 사이 놓친 실행은 여러 번이어도 한
  번만 하고, 리더가 바뀌면 새 리더가 진행 중인 실행을 이어 간다. 작업을 끄면(`--disabled`, `set --enabled false`)
  예약 실행도 지금 실행도 하지 않는다(진행 중인 실행은 이미 요청한 백업만 끝까지 기다린다).
- **한 번에 하나씩.** 실행은 대상 VM을 이름 순으로 하나씩 백업 작업(Task)으로 요청한다. 한 노드에서 동시에 도는
  백업은 orbivisord 설정 `[backup] max_concurrent`(기본 2, 손으로 만든 백업도 센다)까지이고 나머지는 차례를 기다린다.
  이미 백업 중인 VM, 실행 중이라 백업할 수 없는 VM(위의 409와 같은 이유), `--vm`으로 골랐는데 없는 VM은 건너뛰고
  작업의 `errors`에 남긴다. 실행 도중 지운 VM은 그냥 빠진다. 작업이 만든 백업의 이름은 보통처럼
  `{VM 이름}-{UTC 시각}`(겹치면 `-2`…, 63자를 넘지 않게 VM 이름 부분을 줄인다)이고, 백업의
  `spec.job`(`orbivisorctl backup get`)과 웹 백업 목록의 작업 열에 작업이 보인다.
- **마지막 실행.** 큐가 비고 이번에 만든 백업이 모두 끝나면 `last_run`에 시작·끝 시각과 성공(`ok`)·실패(`failed`,
  실패한 백업과 백업하지 못한 VM) 수를 적는다. 백업하던 노드가 죽어 백업 작업(Task)이 끝나지 못한 백업도 실패로
  센다. 실행 중에 `backup-job run`은 409 `job_running`이다. 일정을 바꿔도 이미 요청한 지금 실행은 그대로 돈다.
- **보존 규칙(Proxmox와 같다).** 실행이 끝나면 VM마다 그 작업이 만든 `ready` 백업을 최신부터 보며 `--keep-last N`개를
  먼저 남기고, 그다음 `--keep-daily`(UTC 날짜)·`--keep-weekly`(ISO 주)·`--keep-monthly`(달)마다 그 기간의 가장 새
  백업을 하나씩 N개 기간까지 남긴다(어느 것이 더 새 것인지는 만든 순서로 정하고, 기간은 만든 시각으로 나눈다).
  남지 않은 백업은 지운다. 예: `--keep-last 2 --keep-daily 7`이면 가장 새 백업 둘과, 지난 7일(백업이 있는 날
  기준) 각각의 마지막 백업이 남는다. 보존 값을 모두 0으로 두면(기본) 지우지 않는다.
  그 작업의 실패한 백업은 같은 VM의 더 새 성공한 백업이 생기면 지운다. 손으로 만든 백업과 다른 작업의 백업은
  건드리지 않는다. 실행이 끝난 뒤에는 스케줄러가 확인할 때마다 보존 규칙을 다시 보므로, 그때 지우지 못한
  백업(복제·복원 중이라 `backup_busy`)은 그 일이 끝나면 지우고, 실행 뒤에 보존 값을 줄이면 다음 확인 때 지운다.
  지우기 Task가 실패한 백업은 다음 실행이 끝날 때 다시 지운다.
  남는 백업(실패하지 않은 모든 백업 — 다른 작업·손으로 만든 증분도)의 조상은 보존 규칙이 지울 것이어도 남긴다(그
  체인으로 복원해야 하므로). 지울 것은 자식부터 지우고, 자식이 남아 `backup_in_use`인 부모는 다음 확인 때 지운다.
  그래서 증분을 쓰면 보존 값보다 백업이 더 남을 수 있다 — 작업이 만든 체인은 `full_every`가 상한이다(수동
  `--incremental`은 상한 없음).
- **증분 작업(`full_every`).** `--full-every N`(API `full_every`, 웹 "전체 백업 사이의 증분 백업 수")이 0보다 크면
  작업은 백업을 증분으로 청하고(위 "증분 백업"), 체인에 증분이 N개 쌓이면 다음은 전체로 해 새 체인을 시작한다(이유
  `chain already has N incremental backup(s)`). 예: 매일 + `--full-every 6`이면 주마다 전체 하나와 증분 여섯이다. 할 수
  없으면(꺼진 VM 등) 그 백업만 전체다. 0(기본)이면 늘 전체다. 모든 노드에 증분 백업 기능이 있어야 하고(409
  `cluster_upgrading`), `--replicate`와 함께 두면 증분 백업도 체인 순서로 원격에 보낸다(아래 "원격 복제(DR)").
  `backup-job list`의 `SCHEDULE` 칸에는 `, full every N`, 웹 작업 표의 일정 칸에는 "증분, 증분 n개마다 전체"가 보인다.
- **작업을 지워도 백업은 남는다.** 그 백업은 더는 보존 규칙으로 지워지지 않으니 `orbivisorctl backup delete`로 지운다.
- 웹은 보호 › 백업 작업 탭(만들기·편집·지우기·지금 실행, 다음·마지막 실행 시각은 UTC로 표시)에서
  한다. API는 `GET`·`POST /api/v1/backup-jobs`, `GET`·`PATCH`·`DELETE /api/v1/backup-jobs/{name}`,
  `POST /api/v1/backup-jobs/{name}/run`이고, 읽기는 모든 사용자, 쓰기는 operator 이상이다. 스케줄러의 확인 간격은
  `[backup] tick_secs`(기본 60초)다.

- **한계.** 백업 풀의 백업에는 중복 제거·암호화(TPM 상태 `tpm2-00.permall`도 그대로 담긴다)·검증·S3가 없다 — 그것은 아래
  [백업 저장소](#백업-저장소중복-제거암호화s3)가 한다(옛 풀 백업은 `backup convert`로 옮긴다). 파일 단위 복원, 기존 VM 덮어쓰기는 없다.
  다른 Orbivisor 클러스터로 사본을 보내는 것은 아래 "원격 복제(DR)"다. 증분 백업은: 꺼진 VM은 늘 전체(매일 백업하는
  꺼진 VM은 매일 전체를 쓴다), 블록 풀 VM은 다시 켠 뒤 첫 백업이 전체, VM마다 비트맵이 하나라 두 풀에 번갈아 백업하면
  늘 전체, 체인 병합과 중간 백업 지우기 없음, 증분 파일만으로는 복원할 수 없음(Orbivisor 밖에서는 위처럼 체인을 적어
  연다).
- **남을 수 있는 것.** 작업을 만들기 전에 실패하거나 작업이 사라져 `pending`에 머문 백업은 저절로 치워지지 않는다 —
  `orbivisorctl backup delete`로 지운다. 실행 중 백업이 임시 스냅샷을 지우지 못하면 qcow2 디스크에 그 태그가 남을 수 있다
  (블록 풀 디스크는 orbivisord가 나중에 지운다; qcow2는 VM을 끄고 `qemu-img snapshot -d <태그> <파일>`). 복원이 볼륨을
  다 만든 뒤 VM을 만들기 전에 orbivisord가 죽으면 `{새 이름}-disk{i}` 볼륨이 남는다(복제도 같다) — `orbivisorctl volume delete`로
  지운다. 백업 디렉터리의 파일 작업에는 시간 제한이 없어, NFS 서버가 멈추면(hard 마운트) 백업·지우기·복원 작업이
  서버가 돌아올 때까지 멈춘다.

## 백업 저장소(중복 제거·암호화·S3)

**백업 저장소**(`BackupStore`, 계획 68)는 백업 풀의 디렉터리 하나나 S3 호환 객체 저장소의 버킷(접두사)에 둔 **중복 제거 청크
저장소**다. 디스크를 고정 4 MiB 청크로 나눠 청크마다 한 번만 저장하고(같은 VM의 다음 백업, 같은 템플릿의 클론, 다른 VM의 같은
블록이 모두 나눠 쓴다), 백업마다 **완전한 인덱스**(청크 id의 차례 — 1 TiB 디스크에 8 MiB)를 둔다. 그래서 증분 백업에도 체인이
없다: 아무 백업이나 지우고, 아무 백업이나 혼자 복원한다. 그 위에 노드에서 하는 **AES-256-GCM 암호화**, 청크를 다시 읽어 보는
**검증**, 쓰지 않는 청크를 거두는 **가비지 수집(GC)**, 디스크를 다 받기 전에 VM을 켜는 **라이브 복원**, 저장소 사이 **복사**(계층),
다른 클러스터의 **읽기 전용 붙이기**(DR)가 있다.

위의 백업 풀 백업(qcow2 파일, 증분 체인)은 그대로 남고 그대로 동작한다. 저장소를 만들지 않은 클러스터는 계획 67과 바이트까지
같다. 저장소·저장소 백업·라이브 복원은 클러스터의 모든 노드(위트니스 포함)에 기능 `backup-store`가 있어야 하고, 아니면 **409**
`cluster_upgrading`이다.

- [저장소 만들기](#저장소-만들기)
- [저장소로 백업과 복원](#저장소로-백업과-복원)
- [암호화와 복구 키](#암호화와-복구-키)
- [S3 호환 저장소](#s3-호환-저장소)
- [검증](#검증)
- [가비지 수집(GC)과 보존](#가비지-수집gc과-보존)
- [라이브 복원](#라이브-복원)
- [옛 백업 옮기기와 저장소 사이 복사](#옛-백업-옮기기와-저장소-사이-복사)
- [저장소 백업의 원격 복제](#저장소-백업의-원격-복제)
- [S3로 DR — 읽기 전용 붙이기와 넘겨받기](#s3로-dr--읽기-전용-붙이기와-넘겨받기)
- [권한·할당량·경보](#권한할당량경보)
- [PBS·Veeam과 견주기](#pbsveeam과-견주기)
- [한계와 계획 68이 아직 하지 않는 것](#한계와-계획-68이-아직-하지-않는-것)

### 저장소 만들기

```bash
# 백업 풀(--backups를 켠 dir·NFS 풀) 안 저장소: {풀 루트}/store/{저장소 id}/
orbivisorctl backup-store create local --pool bk
# S3 저장소, 새 키로 암호화하고 키를 암호 문구로 감싸 저장소 안에도 둔다(escrow — 아래 "암호화와 복구 키")
orbivisorctl backup-store create offsite --s3-endpoint https://s3.eu-central-1.amazonaws.com --region eu-central-1 \
  --bucket acme-backup --prefix orbivisor --access-key-id AKIA… --secret-key-file /root/s3-secret \
  --encrypt --escrow-passphrase-file /root/escrow-pass --verify-at 06:00 --verify-days sun
# recovery key: OPK1-ABCD-EFGH-…
# store it now; backups in this store cannot be recovered without it
# backup store offsite created
orbivisorctl backup-store list
# NAME     BACKEND                   ENCRYPTED    SNAPSHOTS  STORED   DEDUP  LAST-GC               LAST-VERIFY           PHASE
# local    pool:bk                   no           42         61.3GiB  5.8x   2026-11-23 05:00 UTC  -                     ready
# offsite  s3:acme-backup/orbivisor  3f:9a:1c:07  42         58.9GiB  6.0x   2026-11-23 05:01 UTC  2026-11-22 06:14 UTC  ready
orbivisorctl backup-store get offsite          # 스펙·상태(JSON, 비밀 키·암호화 키는 빠진다)
orbivisorctl backup-store check offsite        # 이 노드에서 지금 열어 보기 + 노드마다 보고한 닿음
orbivisorctl backup-store set offsite --gc-at 04:30 --verify-new true   # 일정·동시 요청 수·S3 자격 증명·CA만 바꾼다
orbivisorctl backup-store delete local         # 리소스만 지운다(백업이 남아 있으면 409 store_in_use)
```

- **만들기는 Task를 기다린다.** API(`POST /api/v1/backup-stores`)는 201 `{store, recovery_key?, task}`를 주고 초기화 Task
  (`backup-store-init`)가 저장소에 `store.json`(형식·청크 크기·키 지문)과 주인 표식 `owner.json`을 쓴다. 그동안 저장소는
  `initializing`, 끝나면 `ready`, 실패하면 `failed`와 까닭이다. 로컬 저장소는 초기화가 `chunks/0000`~`ffff` 디렉터리 65,536개를
  미리 만든다(몇 초~수십 초). 만들기 전에 API 노드가 그 저장소에 닿으면 미리 읽어 보고 400 `store_key_required`(암호화 저장소인데
  키 없음)·`store_key_mismatch`(키 지문이 다름)·409 `store_owned_elsewhere`(다른 클러스터가 쓰는 저장소)를 곧바로 준다. 같은 풀의
  저장소·같은 버킷/접두사를 두 번 만들면 409 `store_target_in_use`다.
- **풀 안 저장소.** 풀은 `backups`가 켜진 dir·NFS 풀이어야 한다. 저장소가 든 풀은 지우기·`backups` 끄기·`shared` 바꾸기가
  409 `pool_in_use`다. **공유가 아닌 dir 풀의 저장소는 초기화한 노드에만 있다**(`status.node`) — 그 노드의 VM만 거기로 백업하고
  (다른 노드면 409 `store_not_on_node`), 그 노드를 잃으면 저장소도 잃는다. 여러 노드가 쓰려면 NFS 풀이나 S3를 쓴다.
- **일정.** GC는 기본 매일 05:00 UTC(`--gc-at`·`--gc-days`), 예약 검증은 `--verify-at`을 줄 때만 켜진다(`--verify-days`,
  `--reverify-days N` 기본 30, `--verify-mode full|metadata`, `--verify-new`). 웹 마법사는 GC 매일 05:00, 검증 일요일 06:00을
  기본으로 채운다. 리더의 백업 작업 스케줄러가 시각이 되면 Task를 만든다(놓친 실행은 한 번).
- **지우기.** 저장소 리소스를 지워도 **저장소 안 데이터는 그대로다**(버킷·디렉터리를 지우는 API는 없다). 그 저장소의 백업을
  먼저 모두 지워야 한다. 데이터까지 지우려면 리소스를 지운 뒤 손으로: 로컬은 `rm -rf {풀 루트}/store/{저장소 id}`(id는
  `backup-store get`의 `status.store_id`), S3는 `aws s3 rm --recursive s3://<버킷>/<접두사>/`.
- **노드마다 닿음.** 노드마다 60초에 한 번 쓸 수 있는 저장소의 `store.json`을 읽어 닿는지 `NodeStatus.backup_stores`에 적는다
  (바뀔 때만). `backup-store check`(API `POST …/actions/check` — 늘 200 `{ok, checked_here, error?, nodes}`)와 웹 서랍의 "연결
  확인"이 그것을 보인다. 온라인 노드 가운데 하나라도 닿지 않으면 경보 `backup_store_unavailable`이 참이다.
- **웹.** 보호 › **백업 저장소** 탭(`/backup-stores`): 목록, 만들기 마법사(백엔드 → 암호화 → 일정 → 새 키면 복구 키), 서랍(설정,
  키 카드, 노드별 닿음, GC·검증 지금, 읽기 전용이면 scan·넘겨받기). [웹 화면](web-ui.md#백업-저장소-화면).

**저장소 안 모습.** 만들기 뒤 바뀌지 않는 `store.json`, 주인 표식 `owner.json`, 감싼 키 `keys/<지문>.json`(escrow를 쓸 때),
청크 `chunks/<id 앞 4자>/<id>`, 백업마다 `snapshots/<VM id>/<백업 id>/`(매니페스트 `index.json`, 디스크 인덱스
`disk{i}.fidx`, VM 설정 블롭 `config`, `vars.fd`·`tpm2-00.permall` 블롭), GC 기준 시각 표식 `gc/`, 검증이 옆으로 옮긴 깨진 청크
`bad/`. 청크는 lz4로 압축하고(줄지 않으면 그대로) 암호화 저장소면 암호화한다. 이 형식은 Orbivisor 고유라 **Orbivisor 없이
qcow2처럼 열 도구는 없다** — 꺼내려면 어느 Orbivisor 클러스터에서든 읽기 전용으로 붙여 복원한다(아래 "S3로 DR").

### 저장소로 백업과 복원

```bash
orbivisorctl backup create web --store offsite                  # 이름은 풀 백업과 같은 규칙
orbivisorctl backup create web --store offsite --incremental    # 할 수 있으면 바뀐 청크만 읽는다
# backup web-20261123-030000 of vm web created (incremental): new 14.0MiB of 40.0GiB
orbivisorctl backup list --vm web
# NAME                 VM   TARGET         PHASE  CREATED               SIZE     TYPE  ORIGIN  REPLICAS  VERIFIED
# web-20261122-030000  web  store:offsite  ready  2026-11-22 03:00 UTC  6.1GiB   full  -       -         ok
# web-20261123-030000  web  store:offsite  ready  2026-11-23 03:00 UTC  14.0MiB  incr  -       -         never
orbivisorctl backup restore web-20261122-030000 --name web2 --pool local   # 보통 복원(꺼진 새 VM)
orbivisorctl backup delete web-20261122-030000                  # 순서 제약 없음 — 청크는 다음 GC가 거둔다
orbivisorctl backup-job create nightly --store offsite --all --at 03:00 --keep-daily 14 --full-every 6
```

웹은 VM 상세 "백업" 탭과 보호 › 백업의 "지금 백업"에서 대상(백업 풀 | 저장소)을 고른다. API는 `POST /api/v1/vms/{key}/backups`에
`{"store": …}`(`pool`과 둘 중 하나).

- **실행 중인 VM은 fleecing으로 읽는다.** 디스크 스냅샷을 찍지 않는다: 게스트를 잠깐 멈춘 사이 디스크 위에 copy-before-write
  필터를 끼워 그 시점의 내용을 VM 상태 디렉터리의 성긴 파일(`fleece-{i}.img`)에 붙잡고, QEMU가 유닉스 소켓 NBD로 시점 내용을
  내보내면 orbivisord가 4 MiB씩 읽어 해시·압축·암호화해 올린다. 백업하는 동안 게스트가 덮어쓴 블록의 옛 내용만 fleece 파일에
  쌓이고, 백업이 끝나면 지운다. 그래서 **모든 풀(dir·NFS qcow2, LVM-thin·ZFS·RBD, 공유 LVM)에서 실행 중 백업이 된다** — 풀
  백업의 409 `vm_running`·`snapshot_unsupported`가 저장소 백업에는 없고, 블록 풀 VM도 멈추지 않으며 qcow2에 임시 스냅샷 태그가
  남지 않는다.
- **fleece 공간.** 시작 전 VM 상태 디렉터리의 파일시스템에 `max([backup] fleece_reserve_min_mib(기본 2048), 디스크 크기 합의
  10%)`가 비어 있어야 하고(아니면 백업이 곧바로 실패), 백업하는 동안 1초마다 남은 공간이 `fleece_min_free_mib`(기본 1024)
  아래로 내려가면 내보내기와 fleecing을 떼고 백업을 실패시킨다. fleece가 차도 **게스트 쓰기는 실패하지 않는다**(QEMU가 시점
  내보내기를 깨뜨리고 그 뒤 읽기만 실패 — 계획 68 탐침). 쓰기가 아주 많은 VM은 상태 디렉터리(`{data_dir}`)를 넉넉한 곳에 둔다.
- **꺼진 VM은 `qemu-nbd`로 디스크 전체를 읽는다**(`qemu-img` 옆의 `qemu-nbd` — Debian `qemu-utils`, macOS Homebrew `qemu`). 읽는
  시간만 들고 공간·전송은 바뀐 청크만이다(나머지는 "있음 확인"으로 건너뜀). qcow2 디스크에는 끝에 비트맵을 둬 다음 실행 중
  백업이 증분이 된다.
- **증분(`incremental`).** 풀 백업과 같은 비트맵 규칙으로 같은 VM·같은 저장소의 마지막 백업을 **base**로 골라
  (`status.base`), QEMU dirty bitmap이 바뀌었다고 알린 청크만 읽고 나머지는 base 인덱스의 id를 옮긴다. 결과 인덱스는 늘 완전하다.
  할 수 없으면 전체를 읽고 `status.full_reason`에 까닭을 적는다(풀 백업의 까닭들에 더해 `a chunk of the base backup failed
  verification` — base의 청크가 검증에서 깨졌으면 다음은 전체 읽기). 백업 작업의 `full_every N`은 base를 따라 연속으로 비트맵으로
  읽은 수가 N이면 다음을 전체 읽기로 한다(Veeam의 active full처럼 비트맵을 씻을 뿐 공간은 늘지 않는다). 목록의 `TYPE`은 base가
  있으면 `incr`이지만 혼자 복원된다. VM의 비트맵은 마지막 백업 것 하나만 남으므로 한 VM을 두 작업(다른 저장소나 풀)이 번갈아
  백업하면 서로의 비트맵을 지워 둘 다 늘 전체 읽기다 — 한 VM의 증분 백업은 작업 하나로 하고 다른 곳에는 `copy_to`·복제로 보낸다.
- **크기.** `status.store_stats`에 논리 크기(`logical_bytes`), 읽은 바이트(`read_bytes`), 새 청크 수(`new_chunks`)와 새로 쓴
  바이트(`new_bytes` — 목록의 `SIZE`), 이미 있던 청크(`reused_chunks`), 0 청크(`zero_chunks`)가 있다. 웹 백업 표는 "새 데이터 / 논리"를 보인다. 저장소 전체의 쓴
  공간·dedup 배율은 GC가 잰다(`backup-store list`의 `STORED`·`DEDUP`).
- **커밋.** 청크 → 블롭 → 인덱스 → 매니페스트 차례로 쓰고 매니페스트가 있어야 백업이 완전하다(`ready`). 실패하면 그 백업의
  객체를 지우고 `failed`다(올린 청크는 다음 GC가 거둔다).
- **복원.** 풀 백업과 같다(늘 새 VM, 백업 때의 스펙·UEFI 변수·TPM 상태). orbivisord가 백업을 노드 안 NBD로 내보내고 `qemu-img
  convert`가 대상 풀의 새 볼륨에 쓴다(0 청크는 쓰지 않는다). 복원 노드는 저장소와 대상 풀을 함께 쓸 수 있는 온라인 노드다(`--node`가
  그렇지 않으면 409 `store_not_on_node`, 저장소가 `ready`가 아니면 409 `store_not_ready`). 백업 노드를 잃어도 S3·NFS 저장소의 백업은
  다른 노드가 복원한다.
- **지우기.** 체인이 없으므로 **아무 백업이나 지운다**. 만드는 중인 백업의 base만 409 `backup_in_use`다. 읽기 전용 저장소의
  백업은 409 `store_read_only`(`--force`는 리소스만 지운다), 저장소 리소스가 없어졌으면 리소스만 지운다.
- **백업 작업.** `--store S`로 만들고, `backup-job set J --store S`로 풀 작업을 저장소로 바꾼다(그 뒤의 백업만 — 그 작업의 풀
  백업은 보존 규칙대로 줄어든다, 몰래 옮기거나 지우지 않는다). `backup-job list`의 `POOL` 칸은 저장소 작업이면 `-`다(`backup-job
  get`의 `spec.store`).
- **설정**(`[backup]`): `fleece_reserve_min_mib = 2048`, `fleece_min_free_mib = 1024`, `serve_cache_mib = 256`(복원·라이브 복원이
  쓰는 풀린 청크 캐시, 8~1048576).

### 암호화와 복구 키

암호화는 만들 때만 고른다(나중에 켜거나 끄거나 키를 바꾸지 못한다). 노드가 청크를 올리기 전에 암호화하므로 저장소(S3 업체,
NFS 관리자, 훔친 백업 디스크)는 내용을 보지 못한다.

- **키.** 저장소마다 무작위 주 키(32바이트)에서 HKDF로 데이터 키(AES-256-GCM)·청크 id 키·nonce 키·매니페스트 인증 키를 나눈다.
  청크 id는 평문 해시가 아니라 HMAC이라 "알려진 파일의 해시"로 내용을 맞혀 볼 수 없고, nonce는 청크 id에서 정해져(결정적) 같은
  청크는 같은 바이트다 — 중복 제거가 그대로 되고 키 하나에 쓸 수 있는 청크 수에 실용 한계가 없다. 매니페스트는 HMAC으로 인증해
  고친 매니페스트를 거절한다. 키 **지문**(`3f:9a:…`, 키를 알려 주지 않음)은 리소스·`store.json`에 평문으로 있다.
- **키는 클러스터(Raft)에 있다**(원격 토큰·S3 비밀 키와 같은 자리). API 응답·watch·감사 로그·로그에서 뺀다. 노드는 어차피 VM의
  평문을 가지므로 위협 모델은 저장소 쪽이다.
- **복구 키(종이 키).** 새 키로 만들면 응답에 한 번 `OPK1-…`(73자, 네 자씩 묶음, 체크섬 포함)를 준다. CLI는 찍은 뒤 경고하고, 웹
  마법사는 글·QR·복사·인쇄 창을 보이고 "보관했습니다"를 누르기 전에는 닫히지 않는다. 다시 보기는 루트 `cluster.admin`의
  `orbivisorctl backup-store key export S`(API `POST …/key/export`, 감사에 남음).
- **저장소 안 escrow(선택).** `--escrow-passphrase-file F`(만들 때) 또는 `backup-store key escrow S --passphrase-file F`(나중에,
  `cluster.admin`, 8자 이상)는 주 키를 암호 문구로 감싸(argon2id) 저장소의 `keys/<지문>.json`에 둔다. 암호 문구는 어디에도
  저장하지 않는다. 클러스터를 통째로 잃어도 **저장소와 암호 문구만으로** 다른 클러스터가 연다.
- **가져오기.** 이미 있는 암호화 저장소를 붙일 때는 `--recovery-key-file F` 또는 `--escrow-unlock-file F`(escrow 암호 문구)로
  키를 준다. 지문이 다르면 400 `store_key_mismatch`.

> **키를 잃으면 백업을 되살릴 길이 없다.** 클러스터가 살아 있는 동안은 키가 Raft에 있지만, 클러스터를 잃는 날(DR)에는 복구 키나
> escrow 암호 문구가 유일한 열쇠다. 복구 키를 인쇄해 클러스터 밖(금고, 비밀번호 관리자)에 두고, escrow를 쓰면 암호 문구도 따로 둔다.

**암호화 저장소에서 평문으로 남는 것.**

| 저장소를 가진 사람이 볼 수 있는 것 | 볼 수 없는 것 |
|---|---|
| `store.json`(형식·청크 크기·키 지문), `owner.json`(주인 클러스터 이름·CA 지문·하트비트) | 청크 내용(디스크 데이터) |
| 스냅샷 경로(VM id·백업 id), 매니페스트의 시각·디스크 수·가상 크기·청크 수·0 청크 수 | VM 이름·스펙·cloud-init user-data(`config` 블롭) |
| 인덱스(키가 든 청크 id의 차례 — 디스크 안 어느 청크끼리 같은지) | UEFI 변수(`vars.fd`), TPM 상태 |
| 청크 객체 크기(압축률의 힌트), 객체를 쓴 시각, 같은 저장소 안 같은 청크라는 사실 | 다른 키 저장소의 청크와 같은지 |

평문 저장소도 VM 이름이 경로에 들어가지 않는다(id만). TPM 상태가 평문 파일로 남는 풀 백업(`tpm2-00.permall`)보다 나아진 점이다.

### S3 호환 저장소

- **끝점.** `--s3-endpoint https://host[:port]`(경로·질의 없이), `--bucket`, `--prefix`(버킷 안 자리, 한 버킷에 여러 저장소),
  `--region`(기본 `us-east-1`), `--path-style`(경로 방식 `https://host/버킷/…` — 대개 MinIO·SeaweedFS·Ceph RGW; 없으면 가상 호스트
  `버킷.host`), `--ca-file`(사설 CA PEM — 없으면 시스템 루트), `--max-streams N`(동시 요청, 기본 8, 1~64).
  `--insecure-http`는 시험용이다(자격 증명과 데이터가 평문으로 오간다 — 웹 마법사는 `https://`만 받는다).
- **자격 증명.** `--access-key-id`와 비밀 키 파일(`--secret-key-file` — 명령줄·환경에 비밀을 두지 않는다). 바꾸기는
  `backup-store set S --access-key-id … --secret-key-file …`, CA는 `--ca-file`·`--no-ca`. 버킷은 미리 만든다. 필요한 권한은
  그 접두사에 대한 `s3:PutObject`·`GetObject`·`DeleteObject`와 버킷의 `s3:ListBucket`(`HeadObject`·`CopyObject`는 그 안에 든다).
- **요구 사항.** 읽은-뒤-쓰기 강한 일관성(AWS S3, MinIO, SeaweedFS, Ceph RGW, Wasabi는 그렇다). 없을 때만 쓰기는
  `If-None-Match: *`를 쓰고, 모르는 구현이면 쓴 뒤 다시 읽어 견준다. 서명은 SigV4(직접 구현 — AWS 공개 벡터로 시험).
- **다시 하기.** 5xx·429·연결 끊김·시간 넘김(요청 120초)은 지수 대기로 60초 동안 다시 한다. 인증서 검증 실패와 4xx는 곧바로
  실패한다(403은 자격 증명이나 버킷 정책). 그래서 짧은 S3 장애 동안 백업·복원은 멈췄다 이어 간다.
- **비용 메모.** 새 청크마다 `HEAD` + `PUT`, GC는 청크 1000개마다 `LIST` 하나와 지울 청크마다 `DELETE`(하나씩, 병렬), 검증 `full`은
  모든 청크를 `GET`(내보내기 요금) — `metadata`는 `HEAD`만이다.
- **확인한 구현.** GitHub CI의 `backup-s3` 작업은 **SeaweedFS**(S3 게이트웨이)로 HTTP·사설 CA HTTPS를 시험한다(MinIO는 2025년
  가을부터 이미지·바이너리를 내지 않는다). AWS·Wasabi·Ceph RGW는 CI가 아니다 — 쓰기 전에 손으로 확인한다: 저장소를 만들고
  `backup-store check`가 모든 노드에서 ok인지, 작은 VM을 백업 → `backup verify --wait` → 복원 → `backup-store gc --wait`가 되는지.

### 검증

```bash
orbivisorctl backup verify web-20261123-030000 --wait     # 백업 하나를 지금
# backup web-20261123-030000 verified: ok
orbivisorctl backup-store verify offsite --wait           # 일정의 규칙대로 고른 백업(오래 검증하지 않은 것부터)
orbivisorctl backup-store verify offsite --backup a --backup b
```

- **무엇을 보나.** 매니페스트 인증(암호화 저장소)·`config` 블롭·인덱스 해시·디스크 수와 크기, 그리고 `full`이면 모든 청크를 읽어
  AEAD 태그·압축 풀기·id 다시 계산(한 실행에서 같은 청크는 한 번), `metadata`면 청크가 있고 크기가 맞는지만(`HEAD`).
- **깨진 청크.** 지우지 않고 `bad/<id>.<시각>`으로 옮긴다. 그 청크를 가리키는 모든 백업이 `status.verify.state = failed`이고 경보
  `backup_verify_failed`가 참이다. 다음 백업이 같은 내용을 만나면 청크가 없다고 보고 다시 올려 **스스로 낫는다**(base의 청크가
  깨졌으면 다음 백업은 전체 읽기). 나은 뒤 다시 검증하면 `ok`가 되고 경보가 풀린다. 읽기 전용 저장소는 옮기지 않고 결과만 적는다.
- **기록.** 백업마다 `status.verify {at, state, bad_chunks, error}`(목록 `VERIFIED` — `ok`·`failed`·`never`, 웹 "검증" 배지), 저장소
  `last_verify`·`bad_chunks`. 저장소에 닿지 못하면 Task가 실패하고 `last_verify.error`.
- **일정.** 예약 검증은 `reverify_days`(기본 30)보다 오래전에 검증한 백업을 고른다. `verify_new`면 새 백업이 `ready`가 되자마자
  하나의 Task로 검증한다.
- **풀 백업은 검증하지 않는다**(`backup verify`가 400 — 저장소로 옮긴 뒤 검증한다).

### 가비지 수집(GC)과 보존

```bash
orbivisorctl backup-store gc offsite --wait
# GC of backup store offsite done
orbivisorctl backup-store get offsite | jq '.status.last_gc, .status.usage'
```

- **표시·쓸기.** GC(`backup-store-gc` Task)는 저장소 시계로 기준 시각 T0를 잡고, 모든 인덱스가 가리키는 청크를 표시하고, 시작 때
  돌던 백업이 끝나기를 기다린 뒤, 짧은 쓰기 잠금 안에서 늦게 끝난 백업까지 표시하고, 표시되지 않았고 T0 − 5분보다 오래된 청크를
  지운다. 백업과 **동시에** 돈다 — 백업의 커밋 구간과 쓸기만 서로 기다리고, 쓸기 뒤에 커밋하는 백업은 건너뛴 청크가 아직 있는지 다시
  보고 없으면 다시 올린다. (이 규칙은 백업·GC 걸음을 무작위로 섞는 모형 시험과, CI에서 백업 다섯과 GC 셋을 함께 돌린 뒤 모든 백업의
  검증으로 확인했다.)
- **결과.** `last_gc`(시각, 지운 청크·바이트 `removed_chunks`·`removed_bytes`, 젊어서 남긴 청크 `kept_young`, 오류)와 `usage`(스냅샷 수,
  청크 수, 논리 바이트, 저장 바이트 — `DEDUP` = 논리 ÷ 저장). 표시 집합은 Bloom 필터라 1% 안팎의 쓰레기가 다음 GC까지 남을 수 있다(지우면 안 되는 것을 지우는 쪽으로는
  틀리지 않는다).
- **기다림 한도.** 시작 때 돌던 백업을 24시간까지 기다리고, 넘으면 아무것도 지우지 않고 실패한다. 같은 저장소의 GC·검증은 한
  노드 안에서는 차례로 돌지만 다른 노드에서는 함께 돌 수 있다(GC는 가리키지 않는 청크만, 검증은 가리키는 청크만 건드려 서로 엇갈리지
  않는다). 읽기 전용 저장소와 넘겨받힌 저장소(주인 표식이 이 클러스터가 아님 — 저장소를 `failed`, "store was taken over by cluster
  X")에서는 돌지 않는다.
- **보존.** 백업 작업의 보존 규칙은 VM과 **대상(풀·저장소)마다** 따로 고른다. 저장소 백업은 체인이 없으므로 조상 보호 없이 규칙대로
  지운다(풀 백업은 지금처럼 조상 보호). 지운 백업의 공간은 다음 GC가 거둔다.

### 라이브 복원

```bash
orbivisorctl backup restore web-20261123-030000 --name web-dr --pool local --live   # 곧바로 켠다
orbivisorctl vm live-restore status web-dr
# vm web-dr: copying 37% (14.8GiB of 40.0GiB)
orbivisorctl backup restore web-20261123-030000 --name web-dr --pool local --live --wait   # 갈아탈 때까지 기다림
orbivisorctl vm live-restore cancel web-dr --discard     # VM·새 디스크·게스트가 쓴 것을 버린다
```

라이브 복원은 디스크를 다 옮기기 전에 새 VM을 켠다. 디스크마다 노드 로컬 오버레이(qcow2) 아래에 백업을 NBD로 두고(저장소 백업은
`orbivisord backup-serve` 헬퍼, Linux systemd면 `orbivisor-bkserve-<id>` scope라 orbivisord가 다시 떠도 산다), QEMU의 `blockdev-mirror`
(`write-blocking`)가 뒤에서 대상 풀의 새 볼륨으로 옮긴다. 게스트가 아직 옮기지 않은 블록을 읽으면 그 청크를 저장소에서 곧바로
읽는다. 다 옮기면 대상 볼륨으로 갈아타고(`job-complete`) 오버레이·헬퍼를 걷는다 — 그 뒤 VM은 보통 VM과 같다(스냅샷·마이그레이션·
백업·HA).

- **단계**(`status.live_restore.phase`, 웹 VM 머리의 띠): `starting`(볼륨·헬퍼·오버레이·켜기) → `copying`(진행
  `done_bytes`/`total_bytes`, 30초마다) → `pivoting` → 끝. `paused`는 백업을 읽지 못해 VM이 멈춘 것(헬퍼·저장소가 돌아오면 Task가
  `cont`), `stopped`는 강제 정지(`vm start`가 이어 감), `failed`는 Task가 실패한 것(취소하거나 새 Task로 이음).
- **API.** `POST /api/v1/backups/{key}/restore`에 `"live": true`(202 Task `backup-live-restore`). 보통 복원의 권한에 더해 새 VM 폴더의
  `vm.power`가 필요하다. 웹은 복원 대화상자의 "라이브 복원(바로 켬)" 스위치. 노드는 서버가 저장소와 대상 풀에 닿는 노드로 고른다.
- **끝날 때까지 막히는 것.** 마이그레이션·스냅샷·백업·디스크 핫플러그·볼륨 크기 바꾸기·템플릿으로 바꾸기·노드 사이 복제·`vm stop`·
  지우기·PATCH의 디스크·노드 바꾸기는 **409** `vm_restoring`("wait for the live restore or cancel it")이다. 강제 정지와 CPU·메모리·NIC
  핫플러그는 된다. HA 관리자는 라이브 복원 VM을 되살리지·옮기지 않는다.
- **죽거나 끊기면.** Task는 이어 가는 Task다(시간 제한 없음, orbivisord가 다시 떠도 이어 감):

  | 무엇이 | 일어나는 일 |
  |---|---|
  | orbivisord가 다시 뜸 | 헬퍼·QEMU·mirror는 그대로 돌고 새 orbivisord가 Task를 잇는다 |
  | 헬퍼가 죽음 | QEMU가 NBD 요청을 붙잡고(`reconnect-delay` 300초) Task가 같은 소켓으로 헬퍼를 다시 띄운다 — 게스트는 몇 초 멈칫할 뿐 |
  | 저장소가 끊김 | 헬퍼가 60초 다시 한 뒤 읽기 오류 → VM `paused` → 돌아오면 Task가 `cont` |
  | QEMU가 죽음 | 같은 오버레이·헬퍼로 다시 켜고 mirror를 처음부터(재시작 정책과 관계없이) |
  | 노드가 죽음 | 오버레이가 그 노드에 있어 HA로 되살리지 않는다. 노드가 돌아오면 이어 간다. 노드를 영영 잃으면 취소하고 다시 복원한다(그사이 게스트가 쓴 것은 잃음) |

- **취소.** `vm live-restore cancel VM --discard`(API `POST /vms/{key}/live-restore/cancel {"discard": true}`, 새 VM의 `vm.delete`)는
  VM을 끄고 VM·대상 볼륨·오버레이·헬퍼를 남김없이 지운다. `discard` 없이는 400, 라이브 복원 중이 아니면 409 `vm_not_restoring`.
- **옛 풀 백업도** 라이브 복원한다(헬퍼 없이 QEMU가 qcow2 체인을 곧바로 읽는다 — 공유가 아닌 풀은 그 노드에서만).
- **자리.** 오버레이는 VM 상태 디렉터리의 `lr-disk{i}.qcow2`(게스트가 쓴 블록만 — 그만큼 공간이 든다), 헬퍼 소켓·로그는
  `{data_dir}/backup-run/`.

### 옛 백업 옮기기와 저장소 사이 복사

```bash
orbivisorctl backup convert web-20261105-030000 --store local     # 옛 풀 백업(증분이면 체인째 읽음) → 새 저장소 백업 web-20261105-030000-st
orbivisorctl backup convert --all-in-pool bk --store local         # 풀의 ready 백업을 만든 차례로 하나씩(옮긴 것은 건너뜀)
orbivisorctl backup copy web-20261123-030000 --store offsite --wait   # 다른 저장소로(대상에 없는 청크만) → web-20261123-030000-offsite
orbivisorctl backup-job create nightly --store local --all --at 03:00 --keep-daily 7 \
  --copy-to offsite --copy-keep-daily 30 --copy-keep-monthly 12  # 빠른 로컬 7일 + S3 사본 30일·12달
orbivisorctl backup-job set nightly --no-copy                      # 사본 보내기 끔(만든 사본은 남는다)
```

- **옮기기**(`backup-convert` Task)는 원본을 지우지 않는다(확인한 뒤 손으로 지운다). 새 백업의 VM·스펙·작업·레이블·만든 시각은
  원본 그대로이고 `spec.source = {backup, kind: convert}`다. 원본 파일이 있는 노드가 저장소를 써야 하고(409 `store_not_on_node`),
  같은 원본을 같은 저장소로 두 번은 409 `backup_exists`. 웹은 백업 표 동작 "저장소로 옮기기".
- **복사**(`backup-copy` Task)는 원본을 노드 안 NBD로 내보내 대상 저장소에 백업처럼 쓴다: 대상에 있는 청크는 올리지 않고, 키가
  다르면 대상 키로 다시 암호화한다. 이름 `{원본}-{저장소}`, `spec.source = {backup, kind: copy}`. 작업의 `copy_to`는 작업
  저장소의 `ready` 백업 가운데 사본이 없는 것을 스케줄러가 줄 세우고(`[backup] max_concurrent` 안, 실패한 복사는 실행마다 한 번
  다시), 사본은 `copy_to.keep`(없으면 작업의 보존)으로 사본 저장소 안에서 따로 고른다. 웹은 백업 표 "저장소로 복사"와 작업 폼의
  "다른 저장소로 복사".

### 저장소 백업의 원격 복제

저장소 백업도 [원격 복제(DR)](#원격-복제dr)로 원격 클러스터에 보낸다. 대상 자리에 원격의 **백업 저장소 이름**을 준다
(`orbivisorctl backup replicate B --remote dr --pool <dr의 저장소>`, 작업 `--replicate dr/<dr의 저장소>`). 보내는 쪽은 인덱스의 청크
id를 4096개씩 물어 **원격에 없는 청크만** 보낸다 — 체인이 없어도 증분 복제가 저절로 된다. 받는 쪽은 청크를 자기 키로 풀어 id를
맞춰 본 뒤 쓰므로 **두 저장소가 같은 키(지문)이거나 둘 다 평문**이어야 한다(dr 저장소를 prod 저장소의 복구 키로 만든다 — 웹의 복제
대화상자·작업 폼은 키가 다른 원격 저장소를 "(다른 키)"로 보이고 고르지 못하게 한다). 원격의 모든
노드에 `backup-store`가 있어야 한다. 지우기 전파·사본 상태(`REPLICAS`)는 풀 백업과 같다.

### S3로 DR — 읽기 전용 붙이기와 넘겨받기

원격 복제 없이도, 주 클러스터(prod)가 쓰는 S3 저장소를 DR 클러스터(dr)가 **읽기 전용으로 붙여** 복원할 수 있다. prod를 통째로
잃어도 S3와 키(복구 키 또는 escrow 암호 문구)만 있으면 된다.

```bash
# dr에서: 같은 버킷·접두사를 읽기 전용으로, escrow 암호 문구로 키를 연다
orbivisorctl backup-store create prod-s3 --read-only --s3-endpoint https://s3.eu-central-1.amazonaws.com \
  --region eu-central-1 --bucket acme-backup --prefix orbivisor --access-key-id AKIA… --secret-key-file /root/s3-secret \
  --escrow-unlock-file /root/escrow-pass
orbivisorctl backup-store scan prod-s3 --wait        # 저장소의 백업을 dr의 백업 리소스로(다시 하면 새것만)
orbivisorctl backup list                             # ORIGIN이 prod/…인 백업
orbivisorctl backup restore <백업> --name web --pool local --live   # 곧바로 켠다
# prod를 영영 잃었으면 저장소의 주인이 된다(옛 주인의 하트비트가 24시간보다 오래됐을 때, 아니면 --force)
orbivisorctl backup-store take-over prod-s3
```

- **주인 표식.** 저장소를 만든 클러스터가 `owner.json`(클러스터 이름·CA 지문·하트비트)을 쓴다. 다른 클러스터가 같은 자리를 쓰기
  모드로 만들면 409 `store_owned_elsewhere`다. 한 저장소에 두 클러스터가 쓰면 GC가 서로의 진행 중 백업을 보지 못하므로 막는다.
- **읽기 전용 저장소**에서는 복원·라이브 복원·검증(옮기지 않음)·scan이 되고, 백업·지우기·GC는 409 `store_read_only`다. 풀 안 저장소를
  읽기 전용으로 붙이려면 디렉터리 이름을 알아야 하므로 `--store-id <id>`를 준다.
- **scan**(`backup-store-scan` Task)은 저장소의 매니페스트마다 `ready` 백업 리소스를 만든다: 리소스 id = 저장소의 백업 id, 이름
  `{VM 이름}-{만든 시각}`, 루트 폴더, `spec.origin`은 `owner.json`의 클러스터·CA 지문. 저장소에서 사라진 백업은 `failed`("snapshot is
  gone from the store")로 표시만 한다.
- **넘겨받기**(`cluster.admin`, API `POST …/actions/take-over`)는 Task 없이 곧바로 주인 표식을 이 클러스터로 바꾸고 `read_only`를 끈다.
  옛 주인의 하트비트(리더가 한 시간마다, GC도 끝날 때 — 아직 주인일 때만 씀)가 24시간 안이면 `--force` 없이는 409
  `store_owned_elsewhere`다. 옛 주인이 살아 있으면 다음 GC(시작할 때, 쓸기 잠금을 쥔 뒤, 쓸기를 마친 뒤 — 돌고 있던 GC도)에서 표식이
  바뀐 것을 보고 그 저장소를 `failed`("store was taken over by cluster X")로 멈춘다. GC는 넘겨받은 표식을 덮어쓰지 않는다.
- **DR 시험.** 계획 68의 GitHub `backup-s3` 작업은 두 클러스터로 이 차례(prod 암호화 저장소·escrow → dr 읽기 전용 붙이기·scan·라이브
  복원이 원본과 같음 → 읽기 전용 백업 거절 → 넘겨받기 거절 뒤 `--force` → prod GC가 "taken over"로 멈춤)를 돈다.

### 권한·할당량·경보

| 일 | 필요한 것 |
|---|---|
| 저장소 보기(비밀 뺀 보기) | 루트 `system.read` |
| 저장소 만들기·바꾸기·지우기, GC·검증 지금, 연결 확인, scan | 루트 `storage.config` |
| 키 내보내기·escrow 쓰기, 넘겨받기 | 루트 `cluster.admin` |
| 저장소로 백업 | VM `backup.create`(+ 할당량 `backup_bytes`) — 풀처럼 저장소 자체 권한은 보지 않는다 |
| 백업 검증·옮기기·복사 | 백업 `backup.create` |
| 복원 / 라이브 복원 | 지금 복원 그대로 / + 새 VM 폴더 `vm.power` |
| 라이브 복원 취소 | 새 VM `vm.delete` |
| 백업 작업의 `store`·`copy_to` | 루트 `backup.job` |

웹은 저장소 목록(루트 `system.read`)을 읽을 수 있을 때만 저장소를 대상으로 보인다. 그래서 폴더에서만 `backup.create`를 가진 운영자는
웹의 "지금 백업"·옮기기·복사에서 백업 풀만 고르게 된다(서버는 저장소도 받는다) — 그런 운영자는 `orbivisorctl backup create --store`나
저장소를 정한 백업 작업을 쓴다.

- **할당량 `backup_bytes`**(`orbivisorctl folder quota set /teamA --backup-bytes 2T`, 웹 폴더 할당량 "백업(GiB)"): VM 폴더 하위 트리 VM들의
  백업이 차지한다고 보는 바이트 — 저장소 백업은 **그 백업이 처음 쓴 바이트**(`new_bytes`), 풀 백업은 파일 크기 합. 중복 제거된 고유
  공간과 다르고(지우면 줄어듦), 미리 알 수 없어 **만들 때 이미 넘었으면** 409 `quota_exceeded`다(진행 중인 백업 하나는 넘길 수 있다).
  VM을 옮기면 백업 몫도 따라가고, 지운 VM의 백업은 백업 때의 폴더 몫이다.
- **경보**(리더가 평가, 노드·VM 거르기 없음): `backup_verify_failed`(백업마다 — 다시 검증해 ok이거나 지우면 풀림),
  `backup_store_unavailable`(저장소마다 — 온라인 노드 하나라도 닿지 않으면, 상세에 노드와 까닭).
  `orbivisorctl alert rule create bk-verify --condition backup_verify_failed --target ops-mail`, 웹 경보 규칙의 "백업 저장소" 묶음.
- **감사.** `backup-store create offsite (s3 https://…/acme-backup/orbivisor, encrypted 3f:9a:…)`, `key export`, `take-over … (from prod)`,
  `backup create web → store offsite`, `backup restore … (live)`, `vm live-restore cancel … (discard)` 따위 — 비밀은 남기지 않는다.

### PBS·Veeam과 견주기

| | Orbivisor 백업 저장소 | Proxmox Backup Server | Veeam |
|---|---|---|---|
| 중복 제거 | 고정 4 MiB 청크, 클러스터의 모든 VM 사이 | 같은 방식(VM 이미지 고정 4 MiB) | 작업·저장소 단위 블록 dedup |
| 증분 | dirty bitmap, 백업마다 완전한 인덱스(체인 없음) | 같음 | 체인(forever forward·synthetic full) |
| 암호화 | 노드에서 AES-256-GCM, 복구 키 + 저장소 안 escrow | 클라이언트 AES-256-GCM, 키 파일 + 종이 키 | 작업 암호(+ Enterprise Manager 복구) |
| 저장 자리 | 백업 풀(dir·NFS)·S3 호환 직접 | 데이터스토어(로컬 디스크), S3는 근래 기술 미리보기 | 저장소·SOBR 용량 계층(S3) |
| 검증 | 예약·새 백업 검증, 깨진 청크 격리·스스로 낫기 | 예약 검증 | SureBackup(부팅 검증), 상태 검사 |
| 라이브 복원 | 모든 풀, 이어 가는 Task(헬퍼·데몬·QEMU 죽음) | 있음 | Instant Recovery |
| 따로 둘 서버 | 없음(클러스터 노드가 함) | PBS 서버 | 백업 서버·프록시·저장소 |
| 없는 것 | 불변 백업, 부팅 검증, 테이프 | — | — |

PBS·Veeam 칸은 공개 문서의 일반적인 값이고 이 저장소에서 확인한 것이 아니다.

### 한계와 계획 68이 아직 하지 않는 것

- **불변 백업 없음**(S3 Object Lock·`chattr +i`) — prod의 관리자 권한을 빼앗기면 백업을 지울 수 있다. 사본을 다른 자격 증명의 다른
  저장소에 두고(`copy_to`, 원격 복제), 버킷 쪽 버전 관리·수명 주기를 쓴다.
- **키를 바꾸지 못하고**(새 키 저장소로 `backup copy`), 외부 KMS·HSM·백업마다 다른 키·복구 키 여럿이 없다. 저장소의 암호화를
  나중에 켜거나 끄지 못한다.
- **꺼진 VM은 늘 디스크 전체를 읽는다**(공간·전송은 새 청크만). 같은 VM을 풀과 저장소에 번갈아 백업하면 비트맵이 하나라 늘 전체
  읽기다(저장소 공간은 늘지 않는다).
- **fleece**: 지킴이는 1초마다 보므로 1초에 1 GiB보다 빨리 쓰는 게스트는 앞지를 수 있다 — 그래도 게스트 쓰기는 실패하지 않고 그
  백업만 실패한다(계획 68 탐침). 상태 디렉터리 공간을 넉넉히 둔다.
- **라이브 복원**: 오버레이가 노드 로컬이라 끝날 때까지 HA가 없고, 노드를 영영 잃으면 라이브 복원 뒤 게스트가 쓴 것을 잃는다. 첫
  읽기는 4 MiB 청크 단위라 S3 지연만큼 느리다. 끝나기 전 막히는 동작은 위 목록.
- **풀 백업 검증 없음**(옮긴 뒤 검증). 깨진 청크를 다른 사본에서 자동으로 고치지 않는다(다음 백업이 같은 내용을 다시 올릴 때만 낫는다).
- **한 저장소에 한 클러스터만 쓴다**(다른 클러스터는 읽기 전용). 테이프, PBS 형식 내보내기, Azure·GCS 네이티브 API 없음.
- **S3**: 강한 일관성 구현만, 지우기는 하나씩(`DeleteObjects` 없음), multipart 없음(인덱스는 1 GiB 안 — 128 TiB 디스크까지), 대역폭
  제한 없음. CI는 SeaweedFS만 — AWS·Wasabi·Ceph RGW는 위 손 확인.
- **용량**: GC는 청크 수만큼 나열하고 Bloom 필터(청크마다 약 10비트)를 메모리에 둔다. 아주 큰 저장소(청크 수억 개)의 GC 시간은 재지
  않았다.
- **로컬 저장소**(공유 아닌 dir 풀)는 그 노드에 묶인다. 백업 데이터를 지키려면 NFS나 S3를 쓴다.
- 부팅 검증(SureBackup), 꺼진 VM의 증분 읽기, zstd 압축, 노드 백업 대역폭 제한은 다음 계획이다. 파일 단위 복원은 계획 69
  ([파일 단위 복원](#파일-단위-복원)).

## 파일 단위 복원

**파일 단위 복원**(계획 69)은 VM을 통째로 되살리지 않고 백업 안의 파일을 꺼낸다(Proxmox Backup Server의 file restore, Veeam의
FLR과 같은 자리). 백업 하나를 **일회용 도우미 마이크로 VM**에 읽기 전용으로 붙여 디스크 → 파티션(MBR·GPT)·LVM 논리 볼륨 →
파일시스템 → 디렉터리를 웹·CLI·API로 찾아보고, 파일 하나는 그대로, 여러 항목·디렉터리는 **zip**이나 **tar.gz**로 흘려 내려받는다.
고르면 실행 중인 VM 안으로 **게스트 에이전트**를 거쳐 되돌려 쓴다. 디스크를 복원하지 않는다 — 필요한 블록만 백업에서 읽는다.

모든 백업에서 된다: 저장소 백업(평문·암호화, 로컬·S3), 백업 풀의 옛 풀 백업(증분이면 체인 전체를 붙여 그 시점을 봄), 원격에서 받은
사본, 읽기 전용으로 붙인 저장소의 백업(DR 클러스터에서 원본 없이 파일 꺼내기). 백업은 **읽기만** 한다 — 백업 데이터·매니페스트는
바뀌지 않고, 도우미 VM에는 쓰기 가능한 백업 장치가 없다. 이 기능을 쓰지 않는 클러스터·백업은 계획 68과 바이트까지 같다.

클러스터의 모든 노드(위트니스·관찰 노드 포함)에 기능 `file-restore`가 있어야 하고, 아니면 **409** `cluster_upgrading`이다. 기능과
따로, 세션을 여는 노드에는 **도우미 이미지**가 있어야 한다(아래 [도우미 이미지 설치](#도우미-이미지-설치)) — 이미지가 있는 노드가
없으면 **409** `file_restore_unavailable`("install orbivisor-restore-image").

- [찾아보기와 내려받기](#찾아보기와-내려받기)
- [지원하는 디스크와 파일시스템](#지원하는-디스크와-파일시스템)
- [VM으로 되돌려 쓰기](#vm으로-되돌려-쓰기)
- [RHEL·Rocky 게스트 에이전트 설정](#rhelrocky-게스트-에이전트-설정)
- [도우미 이미지 설치](#도우미-이미지-설치)
- [노드 설정 `[file_restore]`](#노드-설정-file_restore)
- [보안 모형](#보안-모형)
- [권한·감사·다른 기능과의 관계](#권한감사다른-기능과의-관계)
- [시간과 속도](#시간과-속도)
- [PBS·Veeam과 견주기 (파일 단위 복원)](#pbsveeam과-견주기-파일-단위-복원)
- [한계와 계획 69가 아직 하지 않는 것](#한계와-계획-69가-아직-하지-않는-것)

### 찾아보기와 내려받기

**세션**은 백업 하나를 붙인 도우미 VM이다. 처음 열면 도우미가 부팅해 디스크를 탐지하고(하드웨어 가속 노드 1~2초, TCG 노드 몇 초 —
[시간과 속도](#시간과-속도)), 같은 사용자가 같은 백업을 다시 열면 열린 세션을 그대로 쓴다. 나열·내려받기·되돌려 쓰기가
**15분** 없으면 닫히고, 열린 지 **8시간**이면 닫힌다(진행 중 내려받기는 끊김). 화면을 떠나도 세션은 열려 있어 돌아와서 다시 쓴다.

**웹**: 보호 › 백업 표(또는 VM 상세의 "백업" 탭)의 행 동작 **"파일 찾아보기"** → 화면 `/backups/<백업 id>/files`. 머리에 상태
칩("시작 중 0:03" → "준비됨", 하드웨어 가속이 없는 노드면 "느린 노드: 1분까지")과 닫힐 때까지 남은 시간, 닫기 단추가 있다. 왼쪽은
디스크마다 묶은 **볼륨 나무**(파일시스템·레이블·크기, 찾아볼 수 없는 볼륨은 흐리게 까닭을 글로), 오른쪽은 **파일 표**(이름·크기·
수정 시각·모드·uid:gid, 심볼릭 링크 대상, 체크 상자, 끝까지 내리면 다음 쪽). **내려받기**는 고른 것이 없으면 지금 폴더 전체를 zip,
일반 파일 하나면 그대로, 여럿·폴더면 zip이고, 메뉴에서 tar.gz(권한·소유자·링크 보존)를 고른다. 화면 구성은
[웹 화면 둘러보기](web-ui.md#파일-찾아보기-화면).

```bash
orbivisorctl backup files open web-20261123-030000 --wait     # 세션을 열고 준비될 때까지(없어도 아래 명령이 열고 기다린다)
# opening a file restore session for backup web-20261123-030000 (the helper VM is booting; this can take a minute)
# file restore session 0190a5b2-… (ready)
orbivisorctl backup files volumes web-20261123-030000
# VOLUME       DISK  KIND            FS           LABEL  SIZE      STATE      NOTE
# disk0p1      0     partition       vfat         -      600.0MiB  unmounted  -
# disk0p2      0     partition       xfs          -      1.0GiB    unmounted  -
# disk0p3      0     partition       LVM2_member  -      38.4GiB   unmounted  LVM physical volume
# lvm/rl/root  0     logical_volume  xfs          -      34.4GiB   unmounted  -
orbivisorctl backup files ls web-20261123-030000 lvm/rl/root:/etc/nginx        # 디렉터리는 끝에 /, 링크는 -> 대상
orbivisorctl backup files ls web-20261123-030000 lvm/rl/root:/etc/nginx -l     # MODE OWNER SIZE MODIFIED NAME
orbivisorctl backup files get web-20261123-030000 lvm/rl/root:/etc/nginx/nginx.conf             # 파일 하나 그대로 → ./nginx.conf
orbivisorctl backup files get web-20261123-030000 lvm/rl/root:/etc/nginx lvm/rl/root:/etc/hosts  # 여럿·디렉터리 → zip
orbivisorctl backup files get web-20261123-030000 lvm/rl/root:/home/app --format tar.gz -O app.tar.gz
orbivisorctl backup files get web-20261123-030000 lvm/rl/root:/var/log/app.log -O - | tail      # -O -는 표준 출력
orbivisorctl backup files sessions                          # 내 열린 세션(--all은 모두 — admin)
orbivisorctl backup files close web-20261123-030000         # 백업 이름(그 백업의 내 세션)이나 세션 id
```

- 경로는 `VOLUME:/경로`이고 볼륨만 주면 그 루트다. 한 번에 고르는 경로는 같은 볼륨이어야 한다. 볼륨 id는 `disk{i}`(파티션 표 없이
  디스크 전체), `disk{i}p{n}`(파티션 번호 — MBR 논리 파티션은 5부터), `lvm/{vg}/{lv}`이고 세션 안에서만 뜻이 있다.
- **`get`의 출력 파일은 `-O`(`--out-file`)다**(`-o`는 전역 출력 형식 `-o json`). 없으면 서버가 정한 이름으로 지금 디렉터리에 쓴다:
  파일 하나는 그 이름, zip·tar.gz는 `<백업>-<마지막 조각>.zip`(여럿·루트면 볼륨 id). 진행은 표준 오류에.
- **형식**: `auto`(기본 — 일반 파일 하나면 `raw`, 아니면 `zip`), `raw`(일반 파일 하나만), `zip`, `tar.gz`. **zip**은 Windows 탐색기가 바로
  열고, 심볼릭 링크·장치·FIFO는 넣지 않는다(풀 때 링크가 대상 밖을 가리키지 않게) — 빠진 것과 읽기 오류 난 파일, 이름을 바꾼 항목
  (UTF-8이 아닌 바이트·`\`·제어 문자는 `%XX`)은 아카이브 끝의 `ORBIVISOR-RESTORE-REPORT.txt`에 적는다. **tar.gz**(pax)는 모드·uid·
  gid·나노초 시각·심볼릭 링크·하드 링크를 그대로 둔다(장치·FIFO는 빼고 보고서) — **권한까지 되살리려면 tar.gz를 받아 root로 푼다.**
- 한 번에 내려받는 항목 수는 `download_max_entries`(기본 100만, 도우미가 건너뛴 항목도 셈)까지, 경로 이름의 합은 256 MiB까지이고
  바이트 한도는 없다(흐름). 한도에 닿으면 받은 데까지로 아카이브를 맺고 보고서 첫 줄에 `stopped: more than …`을 적는다 — 더 작은
  폴더로 나눠 받는다. zip은 한 항목의 이름(경로)이 65,535바이트를 넘으면(깊은 나무) 그 항목과 그 아래를 빼고 보고서에 적는다(tar.gz는
  담는다). 보고서는 처음 1000줄(256 KiB)까지 적고 나머지는 `… and N more line(s)`로 센다. 한 파일의 읽기 오류(S3 끊김·깨진
  블록)는 그 파일만 오류 항목으로 끝내고 보고서에 적는다 — 처음부터 다시 받는다. `raw`는 흐름을 끊는다(`the download was cut off`).
- **멈춘 내려받기는 그 흐름만 기다린다.** 도우미는 흐름마다 4 MiB까지만 앞서 보내고 orbivisord가 받아 쓴 만큼 더 보낸다(통신 규약 판 2).
  그래서 브라우저가 내려받기를 멈추거나 게스트 쓰기가 느려도 같은 세션의 나열·다른 내려받기는 막히지 않고, 멈춘 흐름은 이어 받으면
  계속된다(세션이 열려 있는 동안 — 도는 흐름이 있으면 유휴로 닫지 않는다).
- **API**(태그 `file-restore`, `/api/v1` 아래): `POST /backups/{key}/file-restore` → 202(새로)·200(다시 씀) `{session, task}`,
  `GET /file-restore/sessions[?all=true][&backup=]`, `GET`·`DELETE /file-restore/sessions/{id}`, `GET /file-restore/sessions/{id}/volumes/{volume}/entries?path=…|path_b64=…&cursor=&limit=`
  (볼륨 id의 `/`는 `%2F`, 쪽 크기 기본 200·상한 1000), `…/volumes/{volume}/stat?path=`, `POST /file-restore/sessions/{id}/download-link
  {volume, paths, format, name?}` → `{url, expires_in: 60}`, 그 `url`(`/api/v1` 기준)을 **같은 사용자의 인증으로 60초 안에 한 번** GET.
  두 걸음인 까닭은 큰 zip을 브라우저 메모리에 담지 않고 브라우저 내려받기 관리자에 맡기기 위해서다. 이름이 UTF-8이 아니면 `EntryView.raw`
  (조각의 base64)를 쓰고, 경로 조각은 `{"b64": …}`, 디렉터리는 `path_b64`(조각 바이트를 `/`로 이은 것의 URL-safe base64)로 보낸다.
- **세션 단계**는 Task(`file-restore-session`, 대상 = 세션 id)의 `status.progress.stage`(`booting`·`ready`·`closing`)이고, 보기의
  `phase`는 `booting`·`ready`·`failed`·`closed`다. 실패·닫힘의 까닭(예 `closed after 15 min without use`, `the helper VM exited …`)은
  `error`에 있다. 세션 노드가 아닌 노드의 API로도 나열·내려받기가 된다(노드 사이 RPC로 넘김, 끊으면 도우미 쪽 흐름도 취소).
- **오류 코드**: 400 `invalid_path`(경로 0개·1000개 넘음·잘못된 경로 — 링크를 만들기 전에 본다, `raw`에 일반 파일 아님), 404 `session_not_found`·`volume_not_found`·
  `entry_not_found`, 409 `backup_not_ready`·`backup_busy`·`store_not_ready`·`file_restore_unavailable`·`session_not_ready`·`session_closed`·
  `volume_not_mountable`, 410 `link_expired`, 429 `file_restore_busy`(노드 세션이 꽉 참·세션당 동시 흐름 4), 502 `file_restore_io`
  (백업을 읽지 못함), 413 `file_restore_too_large`(걷기 한도 — 위), 503 `node_unreachable`, 504 `file_restore_timeout`(도우미가 60초 안에
  답하지 않음).
- **세션 목록**(`GET /file-restore/sessions`)도 다른 세션 경로처럼 백업의 `backup.file_restore`를 다시 본다 — 권한을 빼면 목록에서도
  사라진다. `?backup=`에 없는(또는 볼 수 없는) 백업을 주면 404가 아니라 빈 목록이다.

### 지원하는 디스크와 파일시스템

| 종류 | 붙이는 법(늘 `ro,nodev,nosuid,noexec`) | 비고 |
|---|---|---|
| ext2·ext3·ext4 | 저널 재생 → 안 되면 `noload` | |
| XFS(v4·v5) | `nouuid` → 안 되면 `norecovery,nouuid` | 같은 UUID의 XFS 둘(복제한 VM)도 함께 붙음 |
| Btrfs | `subvolid=5` → 안 되면 `rescue=nologreplay` | 최상위를 붙여 모든 하위 볼륨이 디렉터리(`@`, `root` …)로 보임 |
| NTFS | `ntfs3`, `iocharset=utf8` | 빠른 시작·최대 절전으로 더러운 볼륨도 읽기 전용으로 붙음. 압축·스파스 파일 읽음 |
| FAT12/16/32(vfat) | `utf8,shortname=mixed` | EFI 시스템 파티션 포함 |
| exFAT | `iocharset=utf8` | |
| 파티션 | MBR(확장·논리 파티션 포함)·GPT, 파티션 표 없는 디스크 | |
| LVM2 | 선형·줄무늬·미러·thin LV가 따로 볼륨 | PV 자체는 "LVM physical volume" |
| LUKS·BitLocker | 붙이지 않음 | "encrypted (LUKS) — not supported" |
| swap·ZFS·mdraid·ReFS·HFS+·F2FS·Windows 동적 디스크 | 붙이지 않음 | "<종류> — not supported" |

**저널은 재생한다.** 백업은 crash-consistent라(게스트 에이전트 얼리기가 없으면) 저널에 커밋만 된 변경이 있을 수 있다. 원본은 읽기
전용으로 두고 디스크마다 **버리는 qcow2 덮개** 위에 붙여 저널을 재생하므로 최근 파일이 빠지지 않고 백업은 바뀌지 않는다. 재생이
실패하면 재생 없이 붙이고 볼륨에 "journal not replayed — recent changes may be missing"을 단다(웹은 경고 표시). 덮개 합이
`overlay_max_mib`(기본 2 GiB)를 넘으면 세션을 닫는다("the disk overlays grew past N MiB …") — 보통 재생은 수십 MiB다(실측 9.2 MiB). 덮개는 2초마다, 부팅 중(에이전트 인사
전 — 파티션 표·저널은 그때 이미 읽힌다)에도 본다. 파일시스템 고치기(`fsck`)는 하지 않는다.

### VM으로 되돌려 쓰기

고른 파일·디렉터리를 **실행 중인 VM** 안으로 QEMU 게스트 에이전트(`guest-file-open`·`-write`·`-flush`·`-close`)를 거쳐 쓴다(Veeam의
"Restore to original location"과 같은 자리). 대상은 원래 VM이 기본이고 다른 VM도 된다. VM에 게스트 에이전트 채널이 있고
(`orbivisorctl vm set web --guest-agent on` — [게스트 에이전트](vms.md#게스트-에이전트)) 게스트 안에 `qemu-guest-agent`가 돌아야 한다.
자격 증명도 네트워크도 필요 없다.

```bash
orbivisorctl backup files restore web-20261123-030000 lvm/rl/root:/etc/nginx --vm web --to /etc --wait
# restored 3 file(s) (4198 bytes) and 1 directory into VM web:/etc; kept existing files, wrote nginx.conf.restored-20261124-101500; owners, modes and times were not restored
orbivisorctl backup files restore web-20261123-030000 lvm/rl/root:/etc/hosts --vm web --to /etc --conflict overwrite
orbivisorctl backup files restore win-20261123-030000 disk0p2:/Users/kim/Documents/report.docx --vm win --to 'C:\restored'
```

- **충돌**(`--conflict`, 기본 `keep`): `keep` — 있는 파일은 그대로 두고 `<이름>.restored-YYYYMMDD-HHMMSS`(UTC, 그것도 있으면 `-2` …)로
  옆에 쓴다. `skip` — 있는 파일은 건너뛴다. `overwrite` — 임시 `<이름>.orbivisor-<task>`에 다 쓴 뒤 `mv -f`로 바꿔 넣는다(쓰다 끊겨도
  원래 파일은 그대로). 대상이 있는지는 게스트 에이전트로 읽기로 열어 본다 — **없다고 확실할 때만**(Linux `No such file or directory`·
  `Not a directory`, Windows 오류 2·3) 없는 것으로 보고, 읽기로 열 수 없으면(SELinux·Windows 공유 잠금 …) 어느 정책이든 그 파일을
  `cannot tell whether it exists in the guest: …`로 실패로 적고 쓰지 않는다(있는 파일을 비우지 않게).
- **디렉터리 만들기와 `overwrite`는 `guest-exec`도 있어야 한다**(Linux `/bin/mkdir -p --`·`/bin/mv -f -T --`·`/bin/rm -f --`를 argv로 —
  셸 없음, Windows는 PowerShell `-EncodedCommand`에 경로를 작은따옴표 글자로). 에이전트가 `guest-exec`를 막으면 이미 있는 대상 디렉터리
  바로 아래에 파일만 쓸 수 있다 — 디렉터리를 고르면 **400** `guest_mkdir_unavailable`, `overwrite`면 **400** `guest_overwrite_unavailable`.
  파일 쓰기(`guest-file-*`)를 막으면 **409** `guest_agent_restricted`(RHEL·Rocky의 기본 — [다음 절](#rhelrocky-게스트-에이전트-설정)).
- **대상 디렉터리**(`--to`): Linux는 `/`로 시작하는 절대 경로, Windows는 `C:\…`(UNC 없음). `.`·`..`·빈 조각은 받지 않고 Windows는 금지
  문자·예약 이름(`CON`·`NUL`·`COM1` …)과 260자 넘는 경로를 거절한다(**400** `invalid_target_dir`, 항목이면 건너뛰고 결과에). 심볼릭 링크·
  장치는 건너뛰고, UTF-8이 아닌 이름도 건너뛴다(그런 파일은 tar.gz로 받는다).
- **소유자·모드·시각·ACL·xattr은 되돌리지 않는다.** 만든 파일은 에이전트의 사용자(Linux root, umask 따름) 것이다 — 결과 문구에
  "owners, modes and times were not restored"가 붙는다. 권한이 중요하면 tar.gz로 받아 푼다.
- **한도**: 한 번에 `guest_max_files`(1만 개)·`guest_max_bytes`(4 GiB) — 미리 알 수 없어 넘는 순간 멈추고 실패한다(이미 쓴 파일은 남고
  결과에 목록). 한 파일이 실패하면(거절된 열기·쓰기, 백업 읽기 오류) 다 쓰지 못한 그 파일을 지우고(`guest-exec`가 없으면 남기고 결과에)
  이어 가며, 하나라도 실패하면 Task는 실패다. 속도는 초당 1~3 MB 안팎이다(1 MiB마다 에이전트 잠금을 쥐고 놓아 게스트 정보 수집·
  얼리기를 오래 막지 않음).
- **Task**는 `file-restore-to-guest`(대상 = VM, 세션 노드에서 돎)이고 `status.progress.stage = writing`·`done_bytes`로 진행을 본다.
  성공해도 결과 문구는 `status.error`(경고)에 있다 — 웹은 작업 알림, CLI는 `--wait`로 보인다. 지금은 Task를 취소하는 API가 없다.
  세션이 닫히거나 Task가 버려지면 지금 쓰던 파일을 닫고 지운다.
- **API**: `GET /vms/{key}/guest/restore-caps` → `{os, file_write, exec, reason?}`(VM `system.read`, 꺼진 VM 409 `vm_not_running`, 에이전트
  없음 409 `guest_agent_unavailable`), `POST /file-restore/sessions/{id}/restore-to-vm {volume, paths, vm, target_dir, conflict}` → 202 Task.
  웹의 "VM으로 복원" 대화상자는 VM을 고를 때마다 restore-caps를 물어 막힌 선택을 까닭과 함께 끈다.
- **Windows 게스트**는 CI에 Windows 이미지가 없어 손으로 확인한다: VirtIO 드라이버 ISO의 guest agent를 설치하고 VM의 게스트 에이전트를
  켠 뒤, (1) restore-caps가 `{"os": "windows", "file_write": true, "exec": true}`인지, (2) 폴더 하나를 `--to 'C:\restored'`로 되돌려
  쓰고 탐색기에서 내용을 보는지, (3) 같은 파일을 `keep`으로 다시 써 `.restored-…` 파일이 생기는지, (4) `overwrite`로 바꾼 뒤
  `C:\restored`에 `.orbivisor-` 임시 파일이 남지 않았는지 본다.

### RHEL·Rocky 게스트 에이전트 설정

RHEL·Rocky·AlmaLinux의 `qemu-guest-agent`는 **기본으로 파일 읽기·쓰기와 `guest-exec`를 막는다**. 막혀 있으면 웹 대화상자가
"the guest agent blocks guest-file-* (the default on RHEL/Rocky: allow them in /etc/sysconfig/qemu-ga)"를 보이고 API는 409
`guest_agent_restricted`다. **게스트 안에서** `/etc/sysconfig/qemu-ga`의 설정을 바꾸고 에이전트를 다시 시작한다. 설정 줄은 판마다
다르다 — 파일을 열어 어느 꼴인지 본다.

```bash
# 게스트 안(root). 지금 허락 목록·막는 목록 보기
grep -E '^(FILTER_RPC_ARGS|BLOCK_RPCS|BLACKLIST_RPC)' /etc/sysconfig/qemu-ga
# (1) RHEL·Rocky 9·10 꼴 — FILTER_RPC_ARGS="--allow-rpcs=guest-sync-delimited,…"(허락 목록): 쓰기 넷과 exec 둘을 더한다
sed -i '/^FILTER_RPC_ARGS=.*--allow-rpcs=/ s/"$/,guest-file-open,guest-file-write,guest-file-flush,guest-file-close,guest-exec,guest-exec-status"/' /etc/sysconfig/qemu-ga
# (2) 옛 꼴 — BLACKLIST_RPC=guest-file-open,…(또는 BLOCK_RPCS, 막는 목록): 그 줄에서 위 여섯 이름을 지운다
systemctl restart qemu-guest-agent
```

- 파일만 되돌려 쓸 것이면 `guest-file-open`·`-write`·`-flush`·`-close` 넷이면 된다. 디렉터리 만들기와 `overwrite`는 `guest-exec`·
  `guest-exec-status`도 필요하다. **`guest-exec`는 게스트 안 root로 아무 명령이나 돌리는 권한이다** — Orbivisor는 `mkdir`·`mv`·`rm`만
  argv로 부르지만, 에이전트 채널에 닿는 쪽(VM의 `vm.config`를 가진 사람·노드 root)을 믿을 때만 연다.
- 바꾼 뒤 확인: `GET /api/v1/vms/web/guest/restore-caps`가 `"file_write": true`(와 `"exec": true`). 에이전트 정보는 1분마다 다시 묻고
  restore-caps는 요청할 때 묻는다.
- SELinux가 에이전트의 쓰기를 막으면 그 파일 열기가 거절로 끝나 Task 결과에 남는다(`ausearch -m avc -c qemu-ga`로 확인). 이 절의 설정
  줄은 RHEL 계열 꾸러미의 일반적인 모양이고 실제 RHEL 게스트로 시험하지 않았다(막힘은 가짜 에이전트의 `supported_commands`로 시험).

### 도우미 이미지 설치

도우미 이미지는 Debian 13 커널(잠금 파일로 해시를 못 박은 꾸러미)과 우리 initramfs(정적 Rust 에이전트가 PID 1, Debian의 `lvm`·
`blkid`, 고른 커널 모듈)다. 셸·busybox·네트워크는 없다. 노드마다 `[file_restore] image_dir`(기본 Linux `/usr/share/orbivisor/restore`,
macOS `{data_dir}/restore-image`) 아래 `<arch>/`에 커널(x86_64 `vmlinuz`, aarch64 `Image`)·`initrd.img`·`manifest.json`이 있어야 한다.
orbivisord는 시작할 때와 그 뒤 1분마다 `manifest.json`이 바뀌었으면 파일 해시와 통신 규약 판, **이미지 판이 이 orbivisord의 판과 같은지**를 보고,
지나면 노드 상태에 알린다(`NodeStatus.file_restore` — 이미지 판·아키텍처·가속·세션 수). 지나지 못하면 로그에 한 줄(`file restore
disabled: no helper image in /usr/share/orbivisor/restore/x86_64` 따위)을 남기고 그 노드는 세션을 받지 않는다 —
다른 노드에 이미지가 있으면 거기서 연다. 다시 시작할 필요는 없다.

- **Debian·Ubuntu(deb, amd64)**: `orbivisor-restore-image` 꾸러미(약 20 MB — 커널 12 MB·initrd 7 MB, `/usr/share/orbivisor/restore/x86_64/`)를 `orbivisor`가
  `Recommends:`로 둔다 — apt 기본 설정이면 함께 깔린다. 아니면 `apt install orbivisor-restore-image`. `Depends: orbivisor (= 판)`이라
  업그레이드 때 함께 올라간다. GPL 바이너리의 출처는 `/usr/share/doc/orbivisor-restore-image/debian.lock`(snapshot.debian.org의 같은 판).
- **Rocky·소스 빌드 노드(aarch64 포함)**: 노드의 소스 트리(orbivisord를 빌드한 것과 **같은 판**)에서 빌드해 기본 자리에 둔다. bsdtar·zstd·
  python3·curl과 musl 타깃이 필요하다(에이전트는 `rust-lld`로 C 툴체인 없이 빌드됨). 받은 deb은 `~/.cache/orbivisor-restore-image`에
  두고(아키텍처마다 약 115 MB) 다음 빌드에 다시 쓴다.

  ```bash
  sudo dnf install bsdtar zstd python3 curl
  rustup target add aarch64-unknown-linux-musl                 # x86_64 노드면 x86_64-unknown-linux-musl
  sudo install -d -o "$USER" /usr/share/orbivisor/restore/aarch64
  packaging/restore-image/build.sh --arch aarch64 --out /usr/share/orbivisor/restore/aarch64
  # build-restore-image: /usr/share/orbivisor/restore/aarch64: Image … bytes, initrd.img … bytes
  orbivisorctl node get svc01 | jq .status.file_restore      # {"image_version": "orbivisor 0.1.0 lock-…", "arch": "aarch64", "accel": "tcg", …}
  ```

  orbivisord를 `ORBIVISOR_VERSION`을 주고 빌드했으면 같은 값을 `--version`으로 준다(이미지 판 `orbivisor <판> lock-<debian.lock 해시 앞 12자>`가
  orbivisord 판으로 시작해야 한다). **orbivisord를 올릴 때마다 이미지도 다시 빌드한다** — 판이 다르면 그 노드는 알리지 않는다(굴려 올리는 동안은
  올린 노드에서만 세션이 열린다). 빌드 결과는 같은 입력이면 바이트까지 같다(zstd 판이 같을 때).
- **macOS(개발)**: 같은 스크립트를 `--out <data_dir>/restore-image/aarch64`로. HVF로 돈다(권한 내림·sandbox는 없다 — 개발용).
- **설치 ISO**는 아직 이미지 꾸러미를 넣지 않는다(설치 뒤 `apt install orbivisor-restore-image`). arm64 deb·RPM은 없다.

### 노드 설정 `[file_restore]`

`orbivisord.toml`의 `[file_restore]`(모두 생략 가능, 모르는 키는 시작 실패):

| 키 | 기본 | 뜻 |
|---|---|---|
| `image_dir` | Linux `/usr/share/orbivisor/restore`, macOS `{data_dir}/restore-image` | 도우미 이미지(`<arch>/manifest.json` …) |
| `max_sessions` | 4 | 노드당 세션(0이면 이 노드는 세션을 받지 않음, 64까지) |
| `memory_mib` · `vcpus` | 512 · 2 | 도우미 VM(128~16384 MiB · 1~16). systemd scope의 `MemoryMax`는 메모리 + 256 MiB |
| `boot_timeout_secs` | 300 | 에이전트 인사까지(10~3600 — TCG 노드를 생각해 넉넉히) |
| `idle_minutes` · `max_hours` | 15 · 8 | 쓰지 않으면 닫음(1~10080) · 세션 최대 시간(1~168) |
| `overlay_max_mib` | 2048 | 디스크 덮개 합(저널 재생이 쓰는 곳, 64 이상) |
| `run_as` | `nobody` | 도우미 QEMU가 디스크·소켓을 연 뒤 내려갈 사용자(Linux, orbivisord가 root일 때). `""`이면 내리지 않음 |
| `guest_max_bytes` · `guest_max_files` | 4 GiB · 10000 | VM으로 되돌려 쓰기 한 번의 상한 |
| `download_max_entries` | 1000000 | zip·tar.gz 내려받기와 VM으로 되돌려 쓰기 한 번의 항목 수 상한(건너뛴 항목도 셈) |

모든 키의 주석 달린 예시는 `/usr/share/orbivisor/orbivisord.toml.example`의 `[file_restore]`에 있다.

```toml
[file_restore]
max_sessions = 2       # 작은 노드
memory_mib = 1024      # 큰 Btrfs·XFS 메타데이터
```

### 보안 모형

백업 안 파일시스템은 **믿을 수 없는 입력**이다(테넌트가 만든 디스크, 랜섬웨어가 망가뜨린 디스크). 그래서 호스트 커널도 orbivisord도 그것을
해석하지 않는다 — 해석은 세션마다 새로 띄우고 버리는 도우미 VM의 커널이 한다(PBS의 restore VM과 같은 길). 그 커널이 뚫려도 공격자는
**그 세션의 백업 디스크만 읽을 수 있는 네트워크 없는 VM** 안에 있고, 그 뒤에 벽이 차례로 있다:

| 벽 | 무엇을 막나 |
|---|---|
| 게스트 커널·에이전트 | 마운트는 늘 `ro,nodev,nosuid,noexec`. 에이전트가 PID 1(셸 없음)이라 게스트 안에 다른 프로그램이 돌지 않고, `lvm`·`blkid`는 argv로만 부른다. 경로는 조각마다 `O_NOFOLLOW`로 따라가 심볼릭 링크로 볼륨 밖에 나가지 못한다 |
| QEMU 장치 면 | virtio-blk(디스크)·virtio-serial 하나(통신)·직렬 콘솔(로그 파일)·virtio-rng만. **네트워크·화면·USB·9p·virtiofs 없음**(`-nodefaults -no-user-config -net none -display none`) |
| QEMU 프로세스 | Linux systemd면 `orbivisor-fr-<id>.scope`(`MemoryMax`·`TasksMax=64`·`CPUQuota` vCPU × 100%), orbivisord가 root면 디스크·소켓을 연 뒤 `-run-with user=nobody`(옛 QEMU `-runas`)로 내려감 — 내릴 수 없는 QEMU면 세션이 실패한다(몰래 root로 돌리지 않음). seccomp를 넣어 빌드한 QEMU면 `-sandbox on,obsolete=deny,spawn=deny,resourcecontrol=deny`(내리지 않을 때는 `elevateprivileges=deny`까지 — 내릴 때 그것을 걸면 QEMU 자신의 setuid가 막힌다). svc01 Rocky `qemu-kvm`에서 Uid 65534·Seccomp 2를 확인했다. upstream QEMU를 seccomp 없이 빌드했으면 sandbox는 없다 |
| 백업 원본 | QEMU `read-only=on`. 저장소 백업은 orbivisord 안 NBD 서버(쓰기는 `EPERM`)를 세션 디렉터리의 0600 유닉스 소켓으로, 옛 풀 백업은 qcow2를 읽기로 연다. 도우미가 덮개를 아무리 써도 백업은 그대로다 |
| orbivisord 쪽 통신 규약 | 프레임·이름(1024바이트)·경로 깊이(256)·링크 대상(4096)·볼륨 수(256)·쪽 항목(1000)·걷기 항목(건너뜀 포함)·걷기 경로 바이트(256 MiB)에 상한, 흐름마다 받을 몫 4 MiB(넘겨 보내면 위반 — orbivisord가 흐름마다 쌓는 메모리가 이것으로 묶인다), 선언한 크기와 데이터가 다르면 그 흐름 오류, 첫 답·데이터 사이 60초(세 번 이어지면 세션 닫음). 어긋나면 세션을 닫는다(`agent protocol violation: …` — 걷기 한도는 그 흐름만 끝낸다). 보고서·결과 문구는 처음 몇 줄만 들고 나머지는 센다. 게스트가 보낸 글은 로그·감사에서 제어 문자를 이스케이프하고 자른다 |
| 아카이브 | zip·tar.gz 작성기가 깊이 우선 차례를 다시 본다 — 부모는 앞서 받은 디렉터리, 같은 경로는 한 번, 절대 경로·`..` 없음, zip 이름은 65,535바이트까지. zip에는 링크를 넣지 않는다. 그래서 받은 아카이브를 풀어도 대상 디렉터리 밖에 아무것도 생기지 않는다. zip은 끝에 쓸 이름 목록(중앙 디렉터리)을 들고 있으므로 바꾼 이름(`%XX`) 기준 이름 장부가 256 MiB에 닿으면 거기서 맺는다(보고서 맨 앞 `stopped: the zip's list of names reached …` — tar.gz로 받거나 나누어 받는다). 노드 사이로 흘리는 조각은 256 KiB 이하 |
| 게스트로 되돌려 쓰기 | 이름은 백업에서 온 믿을 수 없는 값이라 셸을 거치지 않고(argv, Windows는 인코딩한 스크립트와 작은따옴표 글자), 조각마다 OS 규칙으로 거른다. 덮어쓰기는 임시 파일 뒤 이름 바꾸기 |

- **암호화 저장소의 키는 orbivisord를 떠나지 않는다.** orbivisord가 청크를 받아 풀고 평문 블록을 NBD로 도우미에 준다(`backup-serve` 헬퍼를 쓰지
  않음 — 세션은 orbivisord와 생애를 같이한다). 도우미는 평문을 보지만 그것은 사용자가 지금 열람하려는 바로 그 데이터다.
- **내려받기 링크**는 32바이트 무작위 토큰, 60초·한 번만, 그리고 GET 때 **같은 사용자의 인증을 다시 본다** — 링크가 새어도 남이 쓰지
  못한다(다른 사용자 403, 쓴·지난 링크 410). 응답은 `Cache-Control: no-store`.
- **열린 세션의 요청마다 권한을 다시 본다** — 권한을 빼면 다음 요청부터 403이고 세션은 유휴로 닫힌다. 다른 사용자의 세션은 보이지
  않는다(루트 `cluster.admin`만 `--all`로 보고 닫음).
- **남은 것 치우기**: 세션을 닫거나 도우미가 죽으면 QEMU(Shutdown → SIGTERM → SIGKILL)·scope·세션 디렉터리 둘(`{run_dir}/file-restore/<id>`,
  `{data_dir}/file-restore/<id>` — 덮개·콘솔 로그)을 지운다. orbivisord가 죽었다 뜨면 시작할 때 제 두 디렉터리에 남은 세션의 도우미(명령줄에
  `orbivisor-fr-<id>`가 있는 프로세스만)·그 세션의 `orbivisor-fr-<id>.scope`·두 디렉터리를 치운다 — 같은 호스트의 다른 orbivisord(다른 `run_dir`)의
  도우미는 건드리지 않는다.
- **도우미는 모두 같은 사용자(`run_as`, 기본 `nobody`)로 돈다.** 한 도우미의 커널을 뚫고 QEMU 프로세스까지 빠져나오면 같은 노드의
  다른 도우미(와 `nobody`로 도는 다른 데몬)에 신호를 보낼 수 있다(다른 세션의 덮개·소켓은 root의 0700 디렉터리 안이라
  열지 못한다). `nobody`로 도는 다른 데몬과 떼려면 `run_as`를 도우미 전용 시스템 사용자(`useradd -r -s /usr/sbin/nologin
  orbivisor-fr`)로 바꾼다. 세션끼리 떼는 세션마다의 uid는 아직 없다(다음 계획).
- **되돌려 쓰기 Task의 결과 문구에는 파일 이름이 있다.** 되돌려 쓴·건너뛴·실패한 경로를 처음 10개까지 적고(`… and N more`), 그
  Task의 주인은 대상 VM이다. 그래서 그 VM(폴더)에 `system.read`만 있는 사용자도 `backup.file_restore` 없이 그 이름들을 Task 목록에서
  본다. 그 파일은 이미 VM 안에 있으므로 받아들인 것이지만, 이름 자체가 민감한 백업이면 되돌려 쓸 VM의 읽기 권한을 함께 따져 본다.
- **macOS 노드**는 HVF 자식 프로세스로 돌고 권한 내림·sandbox·scope가 없다 — 개발용이다.

### 권한·감사·다른 기능과의 관계

| 일 | 필요한 것 |
|---|---|
| 세션 열기·나열·내려받기·닫기(자기 세션) | 백업 `backup.file_restore`(백업의 주인 = VM, 지운 VM이면 백업 때 폴더) |
| 세션 Task·되돌려 쓰기 Task 보기(`GET /tasks/{id}`, `--wait`) | 세션 Task는 백업, 되돌려 쓰기 Task는 대상 VM의 `system.read` — 폴더에만 권한이 있어도 따라간다 |
| 남의 세션 보기(`--all`)·닫기 | 루트 `cluster.admin` |
| VM으로 되돌려 쓰기 | 백업 `backup.file_restore` + 대상 VM `vm.config` |
| restore-caps 보기 | 대상 VM `system.read` |

**`backup.file_restore`는 새 권한이고 내장 `operator`에 들어 있지 않다**(통째 복원 `backup.restore`와 따로 — 파일 꺼내기는 폴더·풀·할당량
문이 없는 데이터 읽기다). `admin`은 가진다. 헬프데스크에게 파일 꺼내기만 맡기려면 사용자 정의 역할을 만든다
([폴더와 권한](folders.md#사용자-정의-역할)):

```bash
orbivisorctl role create helpdesk-files --privileges system.read,backup.file_restore --description "백업에서 파일 꺼내기"
orbivisorctl grant add --group helpdesk --role helpdesk-files --on /teamA
```

역할·토큰 범위에 `backup.file_restore`를 쓰는 것도 모든 노드에 기능 `file-restore`가 있어야 한다(아니면 409 `cluster_upgrading` — 옛 노드는
이 이름을 몰라 권한 판단이 노드마다 달라지므로).

- **감사**: `backup file-restore open web-20261123-030000 (session 0190a5b2, node node1)`, `… download-link … disk0p1:/etc/hostname (auto)`,
  `backup file-restore download web-… lvm/rl/root:/etc (zip, 12 entries, 3.1 MiB)`(흐름이 끝날 때 — 끊기면 `interrupted after …`, 실패면
  `failed after …`), `backup file-restore to-vm web-… disk0p1:/etc → vm web:/srv (keep)`, `… close …`. 경로는 처음 5개 + `and N more`,
  이름은 제어 문자 이스케이프·200자. 유휴·최대 시간·도우미 죽음으로 스스로 닫힌 것은 요청이 아니라 감사가 아니고 Task 결과에 남는다.
- **백업 지우기**: 세션이 열린 백업은 **409** `backup_in_use`("a file restore session is open …"). `orbivisorctl backup delete B --force`는
  그 백업의 세션을 모두(누가 열었든) 닫고 지운다(증분의 부모라 거절할 지우기는 세션을 닫지 않는다). 여는 요청과 지우기가 엇갈리면
  세션 Task가 시작할 때 지우는 중인 백업을 다시 보고 실패한다. **보존**(백업 작업)은 세션이 열린 백업을 그 틱에 건너뛰고 세션이 닫힌 뒤 다음 틱에 지운다.
  저장소 GC는 세션과 무관하다(지우지 않은 백업의 청크는 표시됨).
- **할당량**: 세션은 폴더 할당량에 세지 않는다 — 볼륨·VM·백업을 만들지 않고, 노드 자원(메모리·덮개)은 노드 한도가 지킨다.
- **노드 고르기**: 이미지가 있고 온라인이고 자리가 있으며 백업을 읽을 수 있는 노드(저장소 백업은 저장소에 닿는 노드, 풀 백업은 그
  풀을 읽는 노드 — 공유가 아닌 dir 풀이면 백업을 쓴 노드) 가운데 **API를 받은 노드** → 세션이 적은 노드 → 가속(KVM·HVF)이 TCG보다 →
  이름 차례. 유지보수 노드는 뺀다. **노드 비우기**(drain)는 그 노드의 세션을 닫는다.
- **라이브 복원 중인 백업**도 열 수 있다(둘 다 읽기만). 세션은 VM이 아니라 HA·마이그레이션 대상이 아니다. 노드가 죽거나 orbivisord가 다시
  뜨면 세션은 실패로 끝나고(이어 가지 않는 Task) 다시 연다. 위트니스는 세션을 받지 않는다.

### 시간과 속도

계획 69에서 잰 값(도우미 QEMU 시작 → 에이전트 인사, 흘리기는 받기만):

| 곳 | 부팅 | 흘리기 |
|---|---|---|
| Apple Silicon Mac(HVF, Homebrew QEMU) | 1.2~1.3초(고정물 디스크 11개) | 227 MB/s |
| svc01(Rocky 10 aarch64, TCG, QEMU 10.1) | 2.9~3.1초 | 88 MB/s(upstream QEMU)·109 MB/s(Rocky `qemu-kvm`) |
| x86_64 TCG(이 Mac, 탐침) | 7.0초 | — |
| x86_64 KVM(GitHub `file-restore`) | 1.7초(디스크 1개)·2.1초(13개) | 481 MB/s |
| x86_64 강제 TCG(GitHub `file-restore`) | 8.4초(1개)·8.9초(13개) | 35 MB/s |

웹·CLI에서 세션 열기는 이 부팅에 탐지(0.2~0.9초)와 Task 시작이 더해진다. 흘리기는 virtio-serial이라 TCG에서는 CPU에 묶인다.
VM으로 되돌려 쓰기는 게스트 에이전트의 1 MiB 쓰기마다 왕복이 있어 **초당 1.5~2.5 MB**(svc01 TCG·이 Mac HVF, 3 MiB 파일)이고, 작은
파일 하나는 0.5~0.8초다 — 큰 것은 내려받아 다른 길로 옮긴다.

### PBS·Veeam과 견주기 (파일 단위 복원)

| | Orbivisor | Proxmox VE + PBS | Veeam B&R |
|---|---|---|---|
| 격리 | 세션마다 네트워크 없는 도우미 마이크로 VM(Debian 커널, 정적 Rust 에이전트), scope·권한 내림·sandbox | restore VM(같은 길) | Windows 마운트 서버 / Linux 도우미 어플라이언스 |
| 파일시스템 | ext2/3/4·XFS·Btrfs·NTFS·FAT·exFAT, MBR·GPT·LVM(thin 포함), 저널 재생(버리는 덮개) | 비슷한 범위 | 넓음(ReFS 등) |
| 백업 종류 | 저장소 백업(암호화·S3 포함)·옛 풀 백업 체인·받은 사본·읽기 전용 저장소 | PBS 백업 | Veeam 백업 |
| 내려받기 | 파일 그대로·zip·tar.gz(pax, 권한·링크), 흘려 보냄, 일회용 링크 | zip·tar.zst | 탐색기 복사·내려받기 |
| VM으로 되돌리기 | 게스트 에이전트(자격 증명 없음, keep·skip·overwrite) | 없음(내려받기) | 게스트 자격 증명으로 원래 자리(Windows·Linux) |
| 따로 둘 서버 | 없음(클러스터 노드가 함, 이미지 꾸러미 하나) | PBS | 백업 서버·마운트 서버 |
| 없는 것 | 카탈로그·이름 검색, LUKS·BitLocker, 권한 되돌리기 | — | — |

PBS·Veeam 칸은 공개 문서의 일반적인 값이고 이 저장소에서 확인한 것이 아니다.

### 한계와 계획 69가 아직 하지 않는 것

- **카탈로그·이름 검색이 없다** — 백업마다 열어 디렉터리로 찾는다. 백업 사이 차이, 파일 미리 보기(텍스트·이미지)도 없다.
- **LUKS·BitLocker·ZFS·ReFS·HFS+·F2FS·mdraid·Windows 동적 디스크·저장소 공간**은 알아보기만 하고 열지 않는다. 같은 VG UUID를 가진 LVM
  디스크 둘(복제한 VM의 디스크를 한 백업에)은 확인하지 않았다.
- **첫 세션은 부팅을 기다린다**(TCG 노드 몇 초 — 세션 다시 쓰기·유휴 15분으로 덜어 냄). TCG에서 흘리기는 CPU에 묶인다.
- **되돌려 쓰기**는 권한·소유자·시각·ACL을 되돌리지 않고, 실행 중인 VM만, 게스트 에이전트가 허락할 때만(RHEL·Rocky는 설정 필요),
  `guest-exec` 없으면 있는 디렉터리 바로 아래 파일만 된다. 속도는 초당 몇 MB다. Task 취소 API가 없다. 꺼진 VM 디스크에 직접 쓰기,
  ISO로 묶어 꽂기는 없다. Windows 게스트는 손 확인만.
- **zip**은 심볼릭 링크·장치·FIFO를 넣지 않는다(보고서 항목). 읽기 오류 난 파일은 오류 항목으로 남는다.
- **이미지 배포**: deb는 amd64만, Rocky·소스 노드·macOS는 빌드 스크립트, 설치 ISO에는 아직 없다. orbivisord를 올릴 때 이미지도 같은 판이어야 한다.
- **macOS 노드**는 권한 내림·sandbox·scope가 없다(개발용).
- 실행 중인 VM의 디스크·스냅샷·복제본 찾아보기, 세션 공유, 내려받기 대역폭 제한, 더 빠른 부팅(따뜻한 도우미)은 다음 계획이다.

## 원격 클러스터

다른 Orbivisor 클러스터를 원격(`Remote`)으로 등록하면 이 클러스터에서 그 클러스터의 상태(노드·VM 수, 버전, CPU·메모리)를
보고, 그 클러스터의 API를 이 클러스터를 거쳐 부를 수 있다(VM 목록, 시작·정지 등). 두 클러스터는 대등하다: 어느 쪽도
다른 쪽의 멤버가 아니고 Raft를 함께 쓰지 않는다. 서로 보려면 양쪽에서 따로 등록한다.

- **권한은 두 번.** 원격은 이 클러스터의 리소스라 로그인한 누구나(viewer 이상) 보고, admin만 등록·고치기·지우기·새로
  모으기를 한다(operator도 **403**). 원격 클러스터 쪽에서 할 수 있는 일은 등록할 때 넣은 그 클러스터의 API 토큰 주인의
  역할이 다시 정한다. 원격 클러스터에서 viewer 사용자로 만든 토큰을 등록하면 이 클러스터의 admin도 그곳에서는 읽기만
  한다(바꾸는 요청은 원격의 **403**이 그대로 온다). API 토큰은 만든 사용자의 역할을 그대로 가지므로, 원격에 필요한 만큼의
  역할(VM을 시작·정지하려면 보통 operator — admin 토큰은 쓰지 않는다)로 사용자를 따로 만들고 그 사용자로 토큰을 만든다.
- **지문으로 믿기.** 원격과의 TLS는 원격 클러스터의 CA 하나만 믿는다(시스템 루트는 쓰지 않는다). 그 CA는 등록할 때
  원격의 `GET /api/v1/cluster/ca`에서 받아 준 지문(`sha256:…`)과 같고 정확히 인증서 하나인지 확인한 뒤 원격과 함께
  저장한다(덧붙인 다른 CA를 믿게 되지 않게 둘 이상이면 받지 않는다). 그 뒤로 노드는 연결할 때 CA를 다시 받지 않고 저장한
  것만 믿는다(재시작한 노드도). 지문은 원격 클러스터 노드의 `orbivisorctl cluster status`(`CA fingerprint:` 줄)나 웹 클러스터
  화면, `cluster init` 출력에서 확인한다. 호스트 이름은 확인하지 않는다(노드 인증서의 이름은 노드마다 다르므로 그 CA가
  서명했는지와 기간만 본다). 원격 클러스터의 CA가 바뀌면(클러스터를 새로 만듦) `orbivisorctl remote set <이름> --ca-fingerprint
  <새 지문> --token-file <파일>`로 고칠 때까지 닿지 않는다.
- **등록할 때의 확인**(이 순서, 하나라도 실패하면 저장하지 않는다): 입력 검사 **400**(`url`은 `https://<호스트>[:포트]` —
  경로·쿼리·조각·사용자 정보가 없고 끝 `/`는 뗀다, 지문은 대소문자와 `AB:CD:…` 꼴도 받아 `sha256:<소문자 64자>`로, 토큰은
  `opl_<id>_<비밀>` 모양, 이름은 리소스 이름 규칙이고 `probe`는 쓸 수 없다) → 롤링 업그레이드 중이면 **409**
  `cluster_upgrading` → 원격의 CA를 받지 못하면 **502** `remote_unreachable` → 지문이 다르면 **400**
  `remote_fingerprint_mismatch`(메시지에 받은 지문) → 받은 CA가 이 클러스터의 CA면 **400** `remote_is_local` → 그 CA로
  연결해 토큰으로 원격의 `GET /api/v1/auth/whoami`를 읽어 **401**이면 **400** `remote_unauthorized`(그 밖의 실패는 **502**
  `remote_unreachable`) → 저장하고 곧 상태를 한 번 모아(10초 제한) 응답에 넣는다. CA 받기부터 whoami까지 모두 30초 안에
  끝나야 한다(넘으면 **502** `remote_unreachable` "timed out checking the remote cluster" — `probe`의 CA 받기도 같다).
  검증 없이 CA를 받는 것(등록·`probe`)은 아무 주소에나 닿으므로 오류에 원격이 준 말을 옮기지 않는다: 2xx가 아니면 HTTP
  상태 코드만(`HTTP 404: Not Found`), 연결 실패는 `cannot connect`, TLS 실패는 `TLS handshake failed`, 2xx지만 PEM
  인증서가 아니면 `not an Orbivisor cluster`.
- **토큰은 나가지 않는다.** 응답에는 토큰 대신 토큰의 id(`token_id` — 원격의 `orbivisorctl token list`·API 토큰 화면에서 어느
  토큰인지 알아보게)만 있고, 원격은 watch로도 내보내지 않는다(웹은 10초마다 다시 읽는다). 바꾸기(`PUT`)에서 `token`을 빼면
  `url`·`ca_fingerprint`(정규화한 값)가 모두 그대로일 때만 저장된 토큰을 두고, 하나라도 바뀌면 **400** "token must be given
  again when url or ca_fingerprint changes"다(저장된 토큰을 다른 곳으로 보내지 않는다). `url`이나 `ca_fingerprint`가 바뀌면
  등록 때와 같은 확인(CA를 다시 받아 저장)을 하고, 토큰만 바뀌면 저장한 CA로 토큰만 확인한다. 이름은 바꿀 수 없다.
  `If-Match`가 다르면 **412**지만, 확인하는 사이에 상태만 바뀌었으면(상태 수집이 revision을 올림) 다시 확인하지 않고 새
  revision에 쓴다 — **412**는 그 사이 spec(주소·지문·토큰)이 바뀐 때만이다. 원격을 지워도 원격 클러스터의 토큰은 남으므로
  그곳에서 따로 지운다.
- **노드에서 원격으로.** 원격에 연결하는 것은 브라우저가 아니라 이 클러스터의 노드다(상태는 리더가, 중계는 요청을 받은
  노드가). 모든 노드가 원격 URL에 닿아야 한다. 연결은 요청마다 새로 열고(HTTP/1.1), 연결·TLS 10초, 요청을 다 보낸 뒤
  응답 헤더 10초, 본문 조각 사이 60초 제한이며 리다이렉트는 따르지 않는다. CA는 등록 때 저장한 것을 쓴다.

```bash
# 원격 클러스터(dr)에서: 이 클러스터가 쓸 사용자(읽기만 시키려면 --role viewer)
orbivisorctl user create prod-link --role operator
# 그 사용자로 로그인해 API 토큰을 만든다(웹이면 그 사용자로 로그인해 API 토큰 화면에서). 토큰은 다시 볼 수 없다.
orbivisorctl login --server https://10.0.1.11:8006 --ca-fingerprint sha256:… --user prod-link
orbivisorctl token create prod > /root/dr-token  # 이 파일을 prod 쪽으로 옮긴다
orbivisorctl cluster status                    # CA fingerprint: sha256:… — 아래에서 보이는 지문과 비교한다

# 이 클러스터(prod)에서(admin)
orbivisorctl remote add dr --url https://10.0.1.11:8006 --token-file /root/dr-token
# the remote cluster CA fingerprint is sha256:3f9a…
# Is this the CA of the remote cluster? [y/N] y
# remote dr added (reachable)
orbivisorctl remote add dr --url https://10.0.1.11:8006 --ca-fingerprint sha256:3f9a… --token-file -  # 지문을 미리 알면
orbivisorctl remote list
# NAME  CLUSTER  VERSION  NODES  VMS    STATUS
# dr    dr       0.37.0   3/3    12/15  reachable
orbivisorctl remote show dr  # url·ca fingerprint·token id·status·changed at·cluster·version·nodes·vms·cpus·memory
orbivisorctl remote refresh dr  # 곧 다시 모아 show처럼 보인다(admin)
orbivisorctl remote vms dr  # 원격의 VM 목록(vm list와 같은 표)
orbivisorctl remote vm dr start web    # 시작
orbivisorctl remote vm dr stop web     # ACPI 종료(로컬 vm stop과 같음)
orbivisorctl remote vm dr stop web --force  # 곧바로 끄기(로컬 vm stop --force와 같음)
orbivisorctl remote set dr --url https://10.0.1.12:8006 --token-file /root/dr-token  # 주소·지문을 바꾸면 토큰도 다시
orbivisorctl remote set dr --token-file /root/new-token  # 새 토큰만(주지 않은 것은 그대로, 읽은 revision으로 — 그 사이 다른 admin이 바꿨으면 412)
orbivisorctl remote remove dr
```

- **orbivisorctl.** `remote add`에 `--ca-fingerprint`가 없으면 orbivisorctl이 직접(인증서를 검증하지 않고) 원격의 CA를 받아 지문을 보이고,
  `--yes`면 그대로 믿고, 터미널이면 `[y/N]`로 묻고(`y`·`yes`만 계속), 터미널이 아니면(스크립트) "check the fingerprint
  (`orbivisorctl cluster status` on a node of the remote cluster) and run again with --ca-fingerprint sha256:…"로 끝난다(종료 1).
  `--token-file -`이면 표준 입력에서 읽고 앞뒤 공백을 뗀다 — 그때는 표준 입력으로 지문 확인을 물을 수 없으므로
  `--ca-fingerprint`나 `--yes`가 없으면 토큰을 읽거나 원격에 연결하기 전에 "reading the token from stdin needs
  --ca-fingerprint or --yes"로 끝난다. `remote vm`은 원격의 Task를 기다리지 않고 "vm web on remote dr: stop requested (task
  <id>)"(`--force`면 `force stop requested`)만 보인다(결과는 `remote vms`로). `-o json`이면 API 응답 그대로다.
- **상태 수집.** 리더가 5초마다(`tick`) 모을 때가 된 원격을 찾아, 원격마다 30초마다(`interval`) 원격의 `GET /cluster`,
  `/nodes`, `/vms`를 읽는다(원격들을 동시에, 각각 10초 제한 — 그래서 원격 토큰은 적어도 viewer여야 한다). 상태는
  `cluster_name`, `version`(위트니스를 포함한 노드 버전 가운데 가장 낮은 것), `nodes{total, online}`·`cpus`·`memory_gib`
  (위트니스를 뺀 노드), `vms{total, running}`이다. 읽지 못하면 `reachable: false`와 `error`(이유)만 남고 나머지는 비운다.
  모은 것이 저장된 것과 같으면 쓰지 않는다(Raft 부하) — 그래서 `checked_at`은 마지막 확인 시각이 아니라 상태가 마지막으로
  **바뀐** 시각이다(orbivisorctl `changed at`). 리더가 바뀌면 새 리더가 곧 모은다. `orbivisorctl remote refresh`(웹 "새로고침", API
  `POST /remotes/{key}/actions/refresh`)는 기다리지 않고 지금 모은다(10초 제한). 모은 상태를 적지 못하면(그 사이 수집기가
  먼저 적음) 모은 것 대신 저장된 원격을 돌려준다(등록·바꾸기의 응답도 같다).
- **중계.** `/api/v1/remotes/{key}/api/{*path}`(모든 메서드)는 그 원격의 `/api/v1/{path}`로 메서드·쿼리·본문을 저장한
  토큰(`Authorization: Bearer`)으로 보내고, 상태와 본문을 그대로 흘려 돌려준다(본문 한도 없음 — 업로드·내려받기도 된다).
  - 권한: 아래 허용 목록의 경로를 `GET`·`HEAD`로 읽는 것은 원격을 읽을 수 있는 누구나(viewer 이상), 그 밖은 메서드와
    관계없이 이 클러스터의 admin이다. 그다음 원격이 토큰 주인의 역할로 다시 확인한다. 원격 토큰이 읽을 수 있더라도 사용자·
    토큰·인증 영역·감사 로그·텔레메트리처럼 계정과 보안에 관한 것과 원격의 원격(`remotes` — 중계 사슬)은 이 클러스터의
    viewer·operator가 읽지 못하게 한다. 첫 조각은 퍼센트 인코딩을 풀어 본다(`%75sers`도 `users`).

    | 경로(`{*path}`) | `GET`·`HEAD` | 그 밖의 메서드 |
    |---|---|---|
    | 첫 조각이 `vms`·`volumes`·`storage-pools`·`backup-stores`·`networks`·`security-groups`·`snapshots`·`backups`·`backup-jobs`·`vm-imports`·`nodes`·`tasks`·`cluster`·`version`·`settings`·`subscription`, 또는 정확히 `auth/whoami` | viewer 이상 | admin |
    | 그 밖(`users`·`tokens`·`realms`·`audit`·`telemetry`·`remotes`·다른 `auth/*`·모르는 조각) | admin | admin |

  - 보내는 헤더는 `Content-Type`·`Content-Length`·`If-Match`·`Accept`뿐이다(쿠키·`Origin`·원래 `Authorization` 등은 보내지
    않고 `Host`는 원격 주소). 돌려주는 헤더는 `Content-Type`·`Content-Length`·`ETag`·`Retry-After`·`Content-Disposition`뿐이고,
    원격이 보낸 HTML·SVG 따위가 이 클러스터 출처의 페이지로 실행되지 않게 `Content-Type`은 `application/json`(매개변수
    허용)일 때만 그대로, 그 밖(없음 포함)은 `application/octet-stream`으로 하며 늘 `X-Content-Type-Options: nosniff`와
    `Content-Security-Policy: default-src 'none'; sandbox`를 붙인다.
  - 경로 조각에 `.`·`..`·빈 조각이 있으면(퍼센트 인코딩 `%2e` 등도) **400** `invalid_request`, `watch`로 시작하는 경로와 VM
    콘솔(`vms/<VM>/console`, WebSocket)은 **404** `remote_path_unsupported`, 원격에 닿지 못하면(연결·TLS·시간 제한) **502**
    `remote_unreachable`이다. 원격이 저장된 토큰을 거절하면(**401** — 원격에서 토큰을 지움 등) 본문을 버리고 **502**
    `remote_unauthorized` "the remote cluster refused the stored token; update it with `orbivisorctl remote set --token-file`"다
    (이 클러스터의 로그인 문제로 보이지 않게 — orbivisorctl은 이 메시지를 그대로 보이고 웹은 로그아웃하지 않는다). 원격의 그 밖의
    오류 응답(**403** 포함 **4xx**·**5xx**)은 그대로 온다.
  - 감사: 중계한 바꾸는 요청은 이 클러스터의 감사 로그에 `action` `"<METHOD> /remotes/{key}/api/{*path}"`, `target`
    `remotes/<키>`, `detail` `remote=<이름> <METHOD> /<경로>`로 남고, 원격 클러스터의 감사 로그에는 토큰 주인의 요청으로 남는다.
  - 중계는 API 문서(OpenAPI)에 넣지 않는다(원격 API가 그대로 보이므로). 원격 쪽 API는 원격의 `openapi.json`을 본다.
- **웹.** "원격 클러스터" 메뉴(모두에게): 원격마다 카드(이름, 연결됨·연결 안 됨, URL, 클러스터 이름, 버전, 노드
  online/total, VM running/total, CPU, 메모리, 닿지 않으면 오류 문구), 10초마다 다시 읽는다. admin은 추가(이름, URL을 넣고
  "CA 받기" — 브라우저는 다른 출처를 읽지 못하므로 노드가 받은 지문을 보인다, 지문 확인 체크, API 토큰), 고치기(토큰을
  비우면 그대로, URL·CA가 바뀌면 다시 넣으라고 알림), 지우기, 새로고침을 한다. 카드의 이름을 누르면 그 원격의 VM 목록
  (중계 `GET /vms`, 10초마다)이고, admin은 VM마다 시작·정지(ACPI)·강제 정지(확인 뒤)를 요청한다(orbivisorctl `remote vm
  start|stop [--force]`와 같은 이름, 원격 쪽 Task는 지켜보지 않고 요청했다고만 알린다). 중계의 **401**은 이 클러스터의
  세션이 끝난 것이므로 다른 화면처럼 로그인으로 돌아간다.
- **API.** `GET`·`POST /api/v1/remotes`, `GET`·`PUT`(`If-Match` 선택)·`DELETE /api/v1/remotes/{key}`(본문
  `{"name","url","ca_fingerprint","token"}` — `PUT`의 `name`은 생략하거나 지금 이름), `POST /api/v1/remotes/probe {"url"}`
  (admin — 인증서를 검증하지 않고 CA를 받아 정규화한 `url`과 `ca_fingerprint`를 돌려준다, 받지 못하거나 30초를 넘으면
  **502** `remote_unreachable`),
  `POST /api/v1/remotes/{key}/actions/refresh`(admin), 그리고 위의 중계. 응답은 `{"id","name","revision","url",
  "ca_fingerprint","token_id","status":{"reachable","checked_at","error","cluster_name","version","nodes":{"total","online"},
  "vms":{"total","running"},"cpus","memory_gib"}}`이다.

설정(노드마다, 생략하면 아래 기본값 — 리더인 노드의 값을 쓴다):

```toml
[remotes]
interval = "30s"   # 원격마다 상태를 모으는 간격
tick = "5s"        # 모을 때가 된 원격을 찾는 간격
```

- **한계.**
  - 원격 토큰은 Raft에 평문으로 있다(구독 키·LDAP 서비스 계정 비밀번호·OIDC 클라이언트 비밀과 같음 — Raft 데이터는 노드의
    root만 읽는다). 위트니스 노드와 데이터 디렉터리 백업에도 그대로 있다. 이 클러스터의 노드 하나(위트니스 포함)나 그
    백업을 가진 사람은 원격 클러스터를 그 토큰의 역할로 다룰 수 있으므로, 원격에는 필요한 만큼의 역할(보통 operator)
    사용자의 토큰을 등록한다.
  - watch(실시간 갱신)와 VM 콘솔은 중계하지 않는다. 원격 VM은 10초마다 다시 읽어 보이고, 콘솔은 원격 클러스터에 직접 연다.
  - 원격 URL은 노드 하나의 주소다. 그 노드가 꺼지면 원격의 다른 노드가 살아 있어도 `reachable: false`이므로 주소를 바꾼다
    (`remote set --url` — 토큰도 다시).
  - 원격 클러스터의 CA가 바뀌면(클러스터를 새로 만듦) `remote set --ca-fingerprint <새 지문> --token-file`로 고칠 때까지
    닿지 않는다(노드는 저장한 CA만 믿는다).
  - `checked_at`은 상태가 바뀐 시각이라 "마지막 확인"을 알 수 없다(`reachable`·`error`만 믿는다). 원격 Task(시작·정지의
    결과)는 지켜보지 않는다. 원격 쪽 감사 기록은 토큰 주인으로 남아 이 클러스터의 누가 요청했는지는 이 클러스터의 감사
    로그에만 있다.
  - 웹의 고치기는 `If-Match` 없이 보낸다(상태 수집이 revision을 올리므로) — 두 admin이 동시에 고치면 나중 것이 남는다.
  - 중계는 원격의 **401**을 모두 **502** `remote_unauthorized`로 바꾸므로, 토큰 거절이 아닌 401(중계한 `auth/login`에 틀린
    비밀번호 따위)도 그렇게 보인다. `auth/login`은 허용 목록 밖이라 이 클러스터의 admin만 닿으므로 받아들인 한계다.

## 원격 복제(DR)

백업을 등록한 원격 클러스터(위 "원격 클러스터")의 백업 풀로 복사해 두면, 주 사이트(클러스터 prod)를 통째로 잃어도
DR 사이트(클러스터 dr)에서 그 사본을 복원해 VM을 되살린다. 사본은 dr의 **보통 백업**이다: dr의 `orbivisorctl backup
list`·웹 백업 화면에 보이고, dr에서 복원하고 지운다. 백업 작업이 만든 백업은 저절로(`replicate`), 아무 백업이나 손으로
(`backup replicate`) 보낸다. prod에서 원본을 지우면(사용자나 보존 규칙) dr의 사본도 지운다. 손으로 보내기와 백업 작업의 원격
대상은 루트 `cluster.admin`이다(dr은 폴더를 버리고 루트에 두므로 dr의 모든 읽기 사용자에게 보인다, [폴더와 권한](folders.md)).

- **보내는 길.** 복제는 prod의 Task(`backup-replicate`)가 한다. 백업 파일이 있는 노드(공유가 아닌 풀은 백업을 쓴
  노드, NFS면 그 풀을 쓸 수 있는 노드 — 지우기와 같다)가 원격 등록의 URL·CA·토큰으로 dr의 받기 API를 부른다:
  `POST /backups/receive`(매니페스트와 출처)로 dr에 `receiving` 백업을 만들고, 디스크 파일(`disk0.qcow2`…)과
  `vars.fd`(UEFI)를 파일마다 읽으면서 SHA-256을 계산해 `PUT /backups/{id}/files/{file}`로 흘려 보내고(본문을
  메모리에 모으지 않는다), dr이 받은 내용의 SHA-256과 크기를 돌려주면 보낸 것과 비교해 다르면 실패한다. 다 보내면
  `POST /backups/{id}/actions/complete`로 dr이 `manifest.json`을 써 사본이 `ready`가 된다. 원본이 `ready`여야
  보낸다(**409** `backup_not_ready`). Task에 시간 제한은 없고(디스크 크기만큼 걸린다), 원격 연결의 본문 조각 사이
  60초 제한만 있다. dr은 받는 동안 128 MiB마다 디스크에 내려 쓰고 끝에 한 번 더 내려 쓴 뒤 답하므로, prod는 파일을 다
  보낸 뒤 그 응답을 120초까지 기다린다(다른 요청은 10초). dr은 파일마다 받은 SHA-256을 사본의 디스크
  (`status.disks[].sha256`)에, 마지막으로 받은 시각을 `status.received_at`에 적고, prod는 맞게 보낸 파일의 SHA-256을
  원본의 디스크(`status.disks[].sha256`)에 적는다. dr은 본문 조각이 60초 넘게 오지 않으면 그 파일을 버린다(멈춘
  업로드 — 복제 Task는 실패하고 다시 보낼 수 있다). 진행률은 없다(증분 백업은 체인의 몇 백업을 보냈는지만 — 아래
  "증분 백업 복제").
- **dr 쪽 준비.**
  - **백업 풀.** 사본을 둘 dr의 백업 풀(`backups`를 켠 dir·NFS 풀). 백업 풀이 아니거나 없으면 dr이 **400**이고,
    남은 공간이 원본 디스크 파일 크기의 합보다 작으면 **507** `insufficient_storage`다.
  - **operator 토큰.** prod에 dr을 등록할 때 쓴 토큰의 주인이 dr에서 operator 이상이어야 한다(받기와 원격 사본 지우기가
    백업 쓰기). viewer 토큰이면 dr의 **403**이 복제 Task의 오류와 사본 기록에 남는다. admin 토큰은 쓰지 않는다. 이
    토큰은 dr의 백업을 지울 수 있다(아래 "한계").
  - **같은 버전.** 두 클러스터는 같은 Orbivisor 버전이어야 한다. prod가 보내는 매니페스트·출처에 dr이 모르는 필드가
    있으면 dr이 **400**으로 거절한다(업그레이드는 dr부터).
  - **파일을 받는 노드.** dr에서 파일을 쓰는 것은 받기 요청을 받은 노드, 곧 prod에 등록한 원격 URL의 노드다. 그래서
    백업 풀이 NFS면 아무 노드나 되지만, 공유가 아닌 디렉터리 풀이면 그 풀이 있는 노드의 주소로 원격을 등록해야
    한다(아니면 **409** `pool_not_on_node` — "register this cluster as a remote with the URL of a node holding the
    pool, or use a shared (NFS) backup pool"). 그 노드를 잃으면 사본도 잃으므로 DR에는 NFS 백업 풀이 낫다. 받은 사본은
    dir 풀이면 그 노드의 것이라(`status.node`) 그 노드에서 복원한다.
  - **복원할 곳.** VM 디스크를 둘 풀, 그리고 prod VM의 NIC가 쓰는 **네트워크와 보안 그룹을 같은 이름으로** 미리
    만들어 둔다. 복원은 백업 때의 VM 스펙을 그대로 쓰므로 없으면 **400**이다(복원에서 바꿀 수 없다).
- **사본의 이름과 출처.** 사본의 이름은 원본 이름이고 dr에 이미 있으면 `-2`, `-3`…을 붙인다. `spec.origin`에 보낸
  클러스터 이름(`cluster`), 그 CA 지문(`ca_fingerprint`), 원본 id(`backup`), 원본을 만든 백업 작업 이름(`job`, 손으로
  만든 백업이면 없음)이 있다. dr의 `orbivisorctl backup list`는 `ORIGIN` 열에 `<보낸 클러스터>/<원본 id 앞 8자>`, 웹은 이름
  옆에 "<클러스터>에서 받음" 배지(툴팁과 화면 읽기에 원본 id·작업)를 보인다. 사본에는 `spec.job`이 없으므로 dr의 백업
  작업 보존 규칙은 사본을 지우지 않는다(사본은 prod의 보존 규칙이 원본을 지울 때 함께 지워진다). 같은 원본(CA 지문과
  원본 id)의 사본은 dr의 백업 풀마다 하나뿐이다(같은 원본을 dr의 다른 풀로 보내면 다른 사본이다).
- **사본 상태.** prod의 원본 `status.replicas`에 (원격, 풀)마다 `{remote, pool, backup(dr의 사본 id), state, error, at}`이
  있다. `state`는 `copying`(보내는 중) → `ready` 또는 `failed`(`error`에 이유)다. `orbivisorctl backup list`의 `REPLICAS` 열은
  `<원격>:<상태>`(쉼표로), 웹 백업 표의 "원격 사본" 열은 사본마다 상태 아이콘과 원격 이름(툴팁에 원격/풀·상태·오류,
  화면 읽기에 원격/풀·상태)이고 실패한 사본은 그 아래에 "원격 / 풀: 오류"를 글로 보인다. `orbivisorctl backup get`(JSON)에서
  전체를 본다. 이미 `ready`인 사본을 다시 확인하는 동안(아래 "다시 하기") 기록은 `ready` 그대로다: 확인이 성공하면
  `at`만 고치고, dr에 닿지 못하면 `ready`로 둔 채 `error`에 이유를 적는다(복제 Task는 실패, 다음 실행이 다시
  확인한다). dr에 없으면(404) `copying`으로 바꿔 다시 보내고, 다른 사본이거나 해시가 다르면 `failed`다.
- **다시 하기.** 같은 백업을 같은 원격·풀로 다시 보내면 dr의 사본으로 이어 간다. 복제 Task는 먼저 사본 기록의 id를 dr에
  묻고(`GET /backups/{id}` — **404**면 dr에서 지운 것이므로 새로 받기부터 한다), 기록이 없으면 받기를 시작해 dr이
  **409** `replica_exists`로 알려 주는 사본을 읽는다. 그 사본이 이 백업의 사본인지 — 출처의 원본 id, 요청한 풀, 부모
  사본(증분이면 dr의 부모 사본 id, 전체면 없음), VM 스펙, 디스크 이름과 크기, UEFI 변수 — 확인하고, 다르면 "a different
  backup claims to be the copy of <원본 id> on remote …; delete it there"로 실패한다(dr에서 그 백업을 지운다). 사본이
  `ready`면 원본에 적힌 디스크 해시(보낼 때 적은 것 — 해시가 없는 옛 백업만 원본 파일을 읽어 계산해 적는다)와 dr이 받은
  해시를 비교해 모두 같을 때만 보내지 않고 성공하고, 해시가 없거나 다르면 "… is ready but its files do not match …;
  delete it there and replicate again"으로 실패한다(ready 사본은 파일을 다시 받지 않는다). 아직 `receiving`이면 파일을
  모두 다시 보낸다(같은 파일·반쯤 받은 `.part`는 덮는다). 보내다 멈춘 사본(`receiving`)은 dr의 정리 작업이 만든 지,
  그리고 마지막으로 파일을 받은 지 24시간이 지나면 파일과 함께 지운다. 백업 작업은 실행마다 한 번 실패한 사본을 다시
  보내고 ready 사본도 dr에 있는지 확인한다(아래).

```bash
# dr에서: 백업 풀(dir 풀이면 prod에 등록한 URL의 노드에), 같은 이름의 네트워크·보안 그룹, operator 토큰(위 "원격 클러스터")
orbivisorctl pool create dr-bk --nfs nas2.lan:/srv/dr-backup --backups

# prod에서: dr을 원격으로 등록한 뒤
orbivisorctl backup replicate before-upgrade --remote dr --pool dr-bk    # Task id와 따라가는 방법을 보이고 끝난다
orbivisorctl backup replicate before-upgrade --remote dr --pool dr-bk --wait  # 끝날 때까지(24시간까지) 기다린다
orbivisorctl backup-job create nightly --pool bk --all --at 03:00 --keep-daily 7 --replicate dr/dr-bk
orbivisorctl backup-job set db --replicate dr/dr-bk  # 있는 작업에 켜기(대상 바꾸기도 같다)
orbivisorctl backup-job set db --no-replicate       # 끄기(이미 보낸 사본은 남는다)
orbivisorctl backup list
# NAME                 VM   TARGET  PHASE  CREATED               SIZE      TYPE  ORIGIN  REPLICAS  VERIFIED
# web-20261107-030000  web  bk      ready  2026-11-07 03:00 UTC  412.5MiB  full  -       dr:ready  -
orbivisorctl backup delete web-20261107-030000      # dr의 사본도 지운다

# dr에서
orbivisorctl backup list
# NAME                 VM   TARGET  PHASE  CREATED               SIZE      TYPE  ORIGIN         REPLICAS  VERIFIED
# web-20261107-030000  web  dr-bk   ready  2026-11-07 03:00 UTC  412.5MiB  full  prod/019a5b1c  -         -
```

- **수동 복제.** `orbivisorctl backup replicate <백업> --remote <원격> --pool <원격의 백업 풀> [--wait]`(API `POST
  /api/v1/backups/{key}/actions/replicate {"remote", "pool"}`, 루트 `cluster.admin`, **202** Task). 원격이 등록되지 않았으면
  **404**, 원본이 `ready`가 아니면 **409** `backup_not_ready`, 지우는 중이거나 같은 전체 백업에서 시작한 체인의 어느
  백업이든(이 백업, 조상, 자식, 형제 — 전체 백업이면 그 증분들) 같은 원격으로 보내는 Task가 끝나지 않았으면 **409**
  `backup_busy`(복제 중인 그 백업의 이름), 파일이 있는 노드가 오프라인이면 **409**
  `node_offline`이다. 웹은 백업 표(백업 화면·VM 상세의 백업 탭)의 "원격으로 복제" 버튼(ready 백업, `cluster.admin`):
  원격을 고르면 그 원격의 백업 풀을 중계(`GET /storage-pools`)로 읽어 고르게 하고, 보낸 뒤 Task를 따라간다. 복제 중인
  백업을 지우면 **409** `backup_busy`("being replicated")다(보내는 동안 파일을 지우지 않게).
- **작업 복제.** 백업 작업에 `replicate: {"remote", "pool"}`(`backup-job create|set --replicate <원격>/<풀>`, 웹 백업
  작업의 "원격 클러스터로 복제")을 주면, 리더의 스케줄러가 확인할 때마다(`[backup] tick_secs`) 그 작업이 만든 `ready`
  백업 가운데 그 (원격, 풀) 사본 기록이 없는 것을 만든 순서로 복제 Task로 보낸다 — 켜기 전에 만든 그 작업의 백업도
  보낸다. 기록이 마지막 실행을 시작하기 전의 것이면 상태와 관계없이 다시 보내므로, `failed`(또는 복제 Task 없이
  `copying`으로 남은) 사본은 실행마다 한 번 다시 보내고, `ready` 사본도 실행마다 한 번 복제 Task가 dr에 물어 같은
  파일이 있으면 기록 시각만 고치고 dr에서 지웠으면 다시 보낸다(dr에 묻고 적힌 해시를 비교할 뿐 원본 파일을 다시 읽지
  않는다). 복제 Task는
  노드마다 하나씩, 그 노드의 백업 Task와 합쳐 `[backup] max_concurrent`(손으로 만든 백업·복제도 센다)까지이고, 백업
  Task도 복제 Task를 센다. 꺼 둔 작업은 보내지 않는다. 그 작업의 보존 규칙이 지울 백업은 확인·복제하지 않고, 실행이
  끝날 때 지울 백업이 복제 중이라 지우지 못하면(`backup_busy`) 확인할 때마다 다시 해서 복제 Task가 끝나면 지운다.
  `PATCH /api/v1/backup-jobs/{name}`의 `replicate`는 객체면 바꾸고 `null`이면 끄고 없으면 그대로다
  (`--no-replicate`가 `null`). 등록되지 않은 원격은 **400**이고 원격의 풀은 보낼 때 dr이 확인한다(틀리면 Task 실패).
  `full_every`가 있는 작업의 증분 백업도 보낸다(아래 "증분 백업 복제" — 만든 순서로 보내므로 증분의 Task는 보통 앞
  백업이 이미 `ready`라 그 증분 파일만 보낸다).
- **증분 백업 복제.** 증분 백업 X를 보내면 복제 Task가 X의 체인(위 "백업과 복원"의 "증분 백업" — 전체 백업부터
  X까지)을 앞에서부터 차례로 본다. X가 아닌 백업은 그 (원격, 풀) 사본 기록이 `ready`이고 사본 id가 있으면 dr에 묻지
  않고 건너뛰고, 아니면 위와 같이 보낸다. 앞 백업이 끝나야 다음을 보내고, X는 늘 보낸다(이미 `ready`면 위 "다시
  하기"처럼 확인만). 그래서 체인을 처음 보낼 때는 전체 백업부터 모두, 그 뒤에는 새 증분 파일만 간다. 증분은 dr에 있는
  부모 사본의 id와 함께 보내고(받기 요청의 `parent`), dr의 사본은 그 부모 사본을 `status.parent`로 가리킨다 — dr에서도
  보통 증분 백업처럼 체인으로 복원하고(`TYPE`이 `incr`), 부모 사본은 자식 사본이 있는 동안 지울 수 없다(**409**
  `backup_in_use`). dr이 쓰는 매니페스트는 버전 2이고 `backup`·`parent`가 dr의 id다.
  - **원격 기능.** 체인에 증분이 있으면 보내기 전에 dr의 `GET /nodes`로 모든 노드(위트니스 포함)에 증분 복제 기능
    (`incremental-replication`)이 있는지 보고, 없으면(노드 목록이 비어도) Task가 `remote cluster "dr" does not support
    incremental backups; upgrade it`로 실패한다(dr부터 업그레이드). 계획 40 노드는 증분 백업을 알아도 증분 받기
    (`parent`가 있는 받기 요청)를 모르므로 업그레이드해야 한다. 전체 백업만 보낼 때는 보지 않는다.
  - **부모 사본이 없으면 다시 보낸다.** dr에서 부모 사본을 지웠거나 맞지 않으면(같은 풀에 `ready`가 아님, 다른 원본·CA
    지문, 공유가 아닌 풀의 다른 노드, 지우는 중) dr이 **409** `replica_parent_missing`으로 거절한다(받기 요청, 드물게
    완료 요청 — 받는 동안 없어짐). Task는 그 부모의 사본 기록을 `failed`로 바꾸고(사본 id는 둔다 — 다음에 dr에 물어 보고
    없으면 새로 받는다) 체인을 처음부터 한 번 더 본다. 이번에는 `ready`로 적힌 앞 백업도 건너뛰지 않고 위 "다시
    하기"처럼 dr에 물어 확인하므로(없으면 다시 보낸다) 조상의 사본까지 지웠어도 한 Task가 체인 끝까지 다시 보낸다. 한
    Task에서 한 번만 하고, 또 거절되면 실패한다.
  - **실패.** 체인의 백업이 없거나 `ready`가 아니면 "backup chain is broken: …", 앞 백업을 보내지 못하면 `cannot
    replicate backup "<이름>" of the chain: <이유>`로 실패하고 그 백업과 X의 사본 기록이 `failed`와 이유가 된다(기록이
    `ready`였으면 — 위 "다시 하기"의 확인 — `ready`로 두고 `error`만 적는다; 다시 복제하면 이미 `ready`인 앞 백업은
    건너뛴다). 체인 중간 백업이 마지막 `replica_parent_missing`으로 끝나도 그 기록은 `copying`으로 남지 않고 `failed`와
    이유다. orbivisord가 다시 시작해 Task가 중단되면(`interrupted: …`) 정리가 X와 함께 체인에서 `copying`으로 적힌 앞 백업의
    기록도 `failed`와 그 이유로 바꾼다(`ready`·`failed` 기록은 그대로). 체인은 같은 풀의 같은 노드에 있으므로(공유가
    아닌 풀) Task는 X의 노드에서 돈다.
  - **진행.** Task에 진행 메시지는 없다(orbivisord 로그에 체인 백업마다 한 줄). 대신 체인 백업들의 사본 기록으로 계산한
    상태를 보인다 — 체인의 어느 백업이든 그 (원격, 풀) 사본 기록이 있으면 복제 중이 아니어도 보인다: `orbivisorctl backup
    get <증분>`의 `chain.replicas`에 (원격, 풀)마다 `{"remote", "pool", "progress": "k of n copied"}`(체인 n개 가운데
    원격에 있는 백업 k개 — 그 사본 기록이 `ready`이고 사본 id가 있는 것; 체인을 전체 백업까지 따라갈 수 있을 때만),
    웹 백업 표의 "원격 사본" 열에 증분이면 "<원격> / <풀>: 체인 n개 중 k개가 원격에 있음"(다 있으면 숨긴다).
- **지우기 전파.** prod에서 원본을 지우는 Task(`backup delete`, 백업 작업의 보존 규칙)는 원본 파일을 먼저 지우고(지우지
  못하면 Task가 실패하고 dr은 건드리지 않는다), 그다음 `replicas`의 사본 id가 있는 사본마다(`failed`도 — 받다 만 사본이
  dr에 있다) dr의 `DELETE /backups/{id}`를 부른다(dr이 지우기 Task를 시작한다 — 끝나기를 기다리지 않는다, **404**는 이미
  지운 것으로 성공). dr에 닿지 못하거나 거절하면 원본은 그대로 지우고, Task는 성공하되 `error`에 "backup deleted, but
  remote copies may be left: …" 경고를 남긴다(`orbivisorctl backup delete`는 `warning: …` 줄로, 웹은 닫을 때까지 남는 경고
  알림으로 보인다). 그 사본은 dr에서 `ORIGIN`으로 찾아 지운다. 원본은 자식부터 지우므로 dr의 사본도 자식부터 지워진다.
  dr이 부모 사본 지우기를 **409** `backup_in_use`로 거절하면(자식 사본의 dr 지우기 Task가 아직 끝나지 않았다) 다시
  요청하며 60초까지 기다리고, 그래도 거절되면(자식 사본을 지우지 못했다) 같은 경고다 — dr에서 자식부터 지운다. `backup
  delete --force`(파일을 지울 노드가 없어 리소스만 지움)는 원격을 건드리지 않는다. dr에서 사본을 지우면 백업 작업이 다음
  실행에 다시 보낸다(위).
- **업그레이드 중.** 복제·받기·`replicate`가 있는 작업 만들기·고치기는 모든 노드가 이 버전이어야 하고(아니면 **409**
  `cluster_upgrading`), 스케줄러는 모두 올라올 때까지 보내지 않는다. 받는 dr도 이 버전이어야 한다(모르는 필드는
  **400**, 증분 백업은 위 "원격 기능").
- **받기 API(dr 쪽, 백업 쓰기 — operator 이상).** 보통은 prod의 복제 Task만 부른다.
  - `POST /api/v1/backups/receive {"name"?, "pool", "manifest", "origin", "parent"?}` → **201** `{"id", "name"}`.
    `parent`는 증분 백업일 때 이 클러스터에 있는 부모 사본의 id다(전체 백업이면 뺀다). 풀이 없거나 백업 풀이 아니거나
    매니페스트가 올바르지 않으면(형식 버전 — `parent`가 없으면 버전 1에 매니페스트 `parent` 없음, 있으면 버전 2에
    매니페스트 `parent` 있음 —, VM 이름·스펙, 디스크 수가 스펙과 같고 디스크 i가 `disk{i}`·`disk{i}.qcow2`) **400**, 이
    노드에서 풀을 쓸 수 없으면 **409** `pool_not_on_node`·`pool_unavailable`, 같은 원본의 사본이 같은 풀에 있으면
    **409** `replica_exists`(`details.backup`이 그 id), `parent`의 사본이 같은 풀에 `ready`가 아니거나 출처(CA 지문,
    원본 id가 매니페스트의 `parent`)가 맞지 않거나 공유가 아닌 풀이면 이 노드의 것이 아니거나 지우는 중이면 **409**
    `replica_parent_missing`(`details.parent`가 그 id), `parent`가 있는데 증분 복제 기능(`incremental-replication`)이
    없는 노드가 있으면 **409** `cluster_upgrading`, 풀의 남은 공간이 디스크 파일 크기의 합보다 작으면 **507**
    `insufficient_storage`.
  - `PUT /api/v1/backups/{key}/files/{file}`(본문은 파일 그대로, `Content-Length` 필수 — 없으면 **411**) → **200**
    `{"sha256", "size"}`. `file`은 매니페스트의 디스크 파일이나 UEFI면 `vars.fd`뿐(그 밖은 **400**)이고,
    `Content-Length`는 디스크 파일이면 매니페스트의 `stored_bytes`와 같아야, `vars.fd`면 64 MiB까지다(아니면 본문을
    읽기 전에 **400**). `{file}.part`에 받으며 128 MiB마다 내려 쓰고, 끝에 fsync한 뒤 이름을 바꾸고, 디스크 파일의 해시와
    받은 시각을 사본 상태에 적는다. 본문 조각이 60초 넘게 오지 않으면 받던 `.part`를 지우고 **400** `invalid_request`
    ("the upload stalled")다(그 파일을 곧바로 다시 보낼 수 있다). 받는 중이 아니면 **409** `backup_not_receiving`, 다른 노드가 받는 백업이면
    **409** `backup_on_other_node`, 같은 파일을 이 노드가 받고 있으면 **409** `backup_busy`.
  - `POST /api/v1/backups/{key}/actions/complete` → **200** 백업. 디스크 파일의 크기가 매니페스트와 같고 `vars.fd`가
    있어야 한다(아니면 **409** `backup_incomplete`, `details.files`). 지우는 Task가 걸려 있으면 **409** `backup_busy`.
    증분 사본이면 부모 사본을 다시 보고 없어졌거나 `ready`가 아니거나 지우는 중이면 받기와 같은 **409**
    `replica_parent_missing`이다. `manifest.json`의 `backup`은 dr의 사본 id로 쓴다(증분이면 버전 2에 `parent`도 dr의 부모
    사본 id).
  - dr의 감사 로그에는 이 요청들이 토큰 주인의 것으로 남는다. `receiving` 백업은 복원할 수 없고, 지우면 받은 파일도
    지운다.

**DR 절차.**

1. dr에 백업 풀(NFS가 좋다), VM 디스크 풀, prod와 같은 이름의 네트워크·보안 그룹, prod가 쓸 operator 사용자와 그 API
   토큰을 만든다.
2. prod에 dr을 원격으로 등록하고(`orbivisorctl remote add dr …` — dir 백업 풀이면 그 노드의 URL), 백업 작업에
   `--replicate dr/<풀>`을 준다. `orbivisorctl backup list`의 `REPLICAS`가 `dr:ready`인지, 실패면 `error`를 본다.
3. prod를 잃으면 dr에서 `orbivisorctl backup list`(웹 백업 화면)로 `ORIGIN`이 `prod/…`인 사본을 골라 `orbivisorctl backup restore
   <사본> --name <VM> --pool <풀>`로 복원하고 시작한다. 새 VM은 새 MAC이다(MAC으로 묶은 DHCP 예약·라이선스는 다시
   맞춘다). 사본이 원본의 마지막 백업 시각까지의 데이터다(RPO는 백업 주기).
4. dr에서 돌린 VM을 prod로 되돌리려면 같은 방법을 거꾸로(dr에 prod를 원격으로 등록하고 백업을 복제해 prod에서
   복원) 한다. prod를 새로 만들었으면 옛 원본이 없어 dr의 옛 사본은 보존 규칙으로 지워지지 않으므로 dr에서 지운다.

- **한계.**
  - 풀 백업은 백업 파일을 그대로 보낸다(전체 백업은 디스크 전체, 증분 백업은 바뀐 블록만 — 중복 제거·압축 추가 없음). 저장소
    백업은 원격 저장소에 없는 청크만 보낸다(위 [저장소 백업의 원격 복제](#저장소-백업의-원격-복제)). 증분
    체인을 처음 보내면 전체 백업부터 모두 간다. 대역폭 제한이 없어 큰 백업은 링크를 채운다(스케줄러는 노드마다 복제
    하나). 진행률이 없다(증분 체인은 원격에 있는 백업 수만).
  - 증분 복제의 건너뛰기는 prod의 사본 기록만 믿는다: 앞 백업의 기록이 `ready`면 dr에 묻지 않으므로, dr에서 (자식
    사본이 없는) 부모 사본을 지웠으면 다음 증분을 보낼 때 dr의 `replica_parent_missing`을 받고서야 알고 체인을 처음부터
    확인하며 다시 보낸다(한 Task에서 한 번).
  - 보내는 동안 멈추면 처음부터 다시 보낸다(파일 단위로 다시 쓴다 — 이어 보내기 없음). 파일 하나를 24시간 넘게
    보내면(마지막으로 받은 파일 뒤 24시간) dr이 그 `receiving` 사본을 지우므로 그런 복제는 끝나지 못한다.
  - 원본을 지울 때 dr에 닿지 못하거나(꺼짐·토큰 거절) `force`로 지우면 사본이 dr에 남는다. dr에서 사본을 지우면 prod는
    다음 작업 실행(손으로 만든 백업이면 다음 `orbivisorctl backup replicate`)에서야 알고 다시 보낸다.
  - 원본을 지울 때 dr이 부모 사본 지우기를 `backup_in_use`로 거절하면 사본마다 60초까지 기다리므로, 원격 사본이 많은
    백업의 지우기 Task는 시간 제한(600초)을 넘을 수 있다(다시 지우면 된다).
  - **prod 쪽 권한이 dr의 백업에 닿는다.** prod에 등록한 operator 토큰은 dr의 모든 백업을 지울 수 있고, prod에서
    원본을 지우면(사용자·보존 규칙) dr의 사본도 곧바로 지워진다 — prod를 빼앗긴 사람이나 실수가 DR 사본까지 없앨 수
    있다(지우기 지연·보관 잠금은 없다). 오래 지켜야 할 것은 dr 쪽에서 따로 보관한다(예: 사본을 복원한 VM을 dr의 백업
    작업으로 백업). 반대로 dr의 operator는 출처를 꾸민 사본을 만들 수 있다(받는 쪽은 출처를 확인하지 않는다). prod는
    사본의 원본 id·풀·스펙·디스크를 확인하고 ready 사본은 해시를 비교하므로 다른 내용의 사본에 붙지는 않지만, 그런
    사본이 있으면 복제가 실패하므로 dr에서 지워야 한다.
  - 원격 토큰이 Raft에 평문인 것, 원격 URL이 노드 하나인 것 등 "원격 클러스터"의 한계를 그대로 가진다. 복원은 VM
    스펙을 바꾸지 못하므로 네트워크·보안 그룹 이름을 dr에 맞춰 둔다. 자동 장애 조치·복구 계획 실행·연속(디스크 단위)
    복제는 없다.

## VMware에서 가져오기

VMware VM을 OVA 파일로 내보내거나 ESXi·vCenter 데이터스토어에서 바로 읽어 Orbivisor로 가져온다. 가져오기(`vm-import`)는
세 단계다: OVA를 올리거나 ESXi의 VM을 고르면 orbivisord가 OVF(또는 `.vmx`와 디스크 디스크립터)를 읽어 원본 VM(CPU, 메모리,
펌웨어, 디스크, NIC)과 Orbivisor VM 제안을 보이고, 사용자가 이름·풀·네트워크 매핑과 고칠 값을 정해 실행하면 디스크를
볼륨으로 옮겨 **꺼진** 새 VM을 만든다. 실행이 실패하면 원본이 남으므로 값을 바꿔 다시 실행한다.
VM 여러 대를 한 번에, 켜진 VM을 짧은 중단으로 옮기려면 아래 [vCenter 일괄 이전과 따뜻한 이전](#vcenter-일괄-이전과-따뜻한-이전)을
쓴다(계획 72). Orbivisor VM을 VMware로 가져가는 OVA 내보내기는 [VM 안내](vms.md#ova로-내보내기)에 있다.

**OVA 만들기.** VMware에서 VM을 먼저 끈다(스냅샷이 있으면 통합하거나 지운다).

- vSphere Client의 "Export OVF Template"은 `.ovf`·`.mf`·`.vmdk` 파일들을 내려받는다(OVA 하나가 아니다). 그 폴더를
  `ovftool`로 OVA 하나로 묶는다: `ovftool web01/web01.ovf web01.ova`. `ovftool`로 바로 내보내도 된다:
  `ovftool vi://user@vcenter/DC/vm/web01 web01.ova`.
- VMware Workstation·Fusion은 "File → Export to OVF…"에서 파일 이름을 `.ova`로 정한다.
- 디스크는 VMDK(streamOptimized·monolithicSparse)여야 한다. 압축(`ovf:compression`)·분할(`ovf:chunkSize`) 디스크,
  VM 여럿이 든 OVA, 디스크가 32개 또는 NIC가 16개를 넘는 VM은 받지 않는다(400 `invalid_ova`).

```bash
orbivisorctl import ova web01.ova --name web01  # 먼저 받을 수 있는지 묻고 올린다(이 노드에 원본이 남는다)
orbivisorctl import list
# NAME   NODE   SOURCE  PHASE  VM
# web01  node1  web01   ready  -
orbivisorctl import get web01                   # 원본 VM, 제안, 디스크와 NIC, 경고
# proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs
orbivisorctl import run web01 --name web --pool local --net "VM Network=lan"  # 디스크를 볼륨으로 옮겨 꺼진 VM web을 만든다
orbivisorctl import run web01 --name web --pool local --net lan --disk-bus sata --nic-model e1000e --memory 4096 --new-macs
orbivisorctl vm start web
orbivisorctl import delete web01                # 가져오기와 남은 원본 파일을 지운다(만든 VM은 남는다)
```

**ESXi·vCenter에서 바로 가져오기.** 내보내기 없이 orbivisord가 ESXi(또는 vCenter)의 데이터스토어 브라우저
(`https://호스트/folder`)에서 `.vmx`와 디스크를 HTTPS로 읽는다. VMware에서 VM을 **끄고**, 스냅샷이 있으면 통합하거나
지운다(스냅샷 디스크는 400 "the VM has snapshots; consolidate or delete them in VMware first", RDM도 400). 디스크는
VMFS·flat VMDK(디스크립터 + `-flat.vmdk`)여야 한다. 아래의 제안·네트워크 매핑·MAC·상태 등은 OVA와 같다.

```bash
export ORBIVISOR_ESXI_PASSWORD='…'                    # 또는 --password-file, 둘 다 없으면 터미널에서 묻는다(인자로는 받지 않는다)
orbivisorctl import esxi-browse --host esx1.example.com --user root
# error: the ESXi certificate fingerprint is AB:CD:…; check it and rerun with --fingerprint AB:CD:…
orbivisorctl import esxi-browse --host esx1.example.com --user root --fingerprint AB:CD:…
# DATASTORE   VMX              NAME   GUEST      CPU  MEMORY  DISKS  LOCKED
# datastore1  web01/web01.vmx  web01  ubuntu-64  2    1.0GiB  1      false
orbivisorctl import esxi web01 --host esx1.example.com --user root --fingerprint AB:CD:… \
  --datastore datastore1 --vmx web01/web01.vmx
orbivisorctl import get web01                   # SOURCE: esxi://esx1.example.com/datastore1/web01/web01.vmx
orbivisorctl import run web01 --name web --pool local --net lan  # 디스크를 내려받아 볼륨으로 옮긴다
```

- **인증서 지문.** ESXi 인증서는 대개 자체 서명이라 시스템 CA로 검증하지 않고 SHA-256 지문을 고정한다. 지문 없이
  연결하면 orbivisord가 서버 인증서의 지문을 알려 준다(409 `untrusted_certificate`). ESXi 호스트에서 직접 얻은 지문
  (예: ESXi 셸의 `openssl x509 -in /etc/vmware/ssl/rui.crt -noout -fingerprint -sha256`)과 같은지 확인한 뒤
  `--fingerprint`로 다시 한다. 지문이 다르면 비밀번호를 보내기 전에 끊는다(409 `fingerprint_mismatch`). 사용자
  이름·비밀번호가 틀리면 400 `esxi_auth`, 연결할 수 없으면 502 `esxi_unreachable`, 지문 모양이 틀리면(SHA-256 16진 64자가
  아니면) 연결하기 전에 400이다.
- **찾기.** `esxi-browse`는 데이터스토어마다 맨 위 디렉터리의 첫 `.vmx`를 읽어(500개까지) 보인다. 읽지 못한 VM은 NAME
  자리에 이유가 나온다. `LOCKED`가 `true`면 VM 디렉터리에 잠금 파일(`.lck`로 끝나는 항목, NFS의 `.lck-…`)이나 켜진
  VM만 가지는 스왑 파일(`.vswp`)이 있다 — 켜져 있을 수 있으니 끈다(만들 때도 경고한다). 이 파일들로 짐작할 뿐이므로
  켜진 VM을 가져오지 않게 직접 확인한다. **일시 중단된 VM**(디렉터리에 `.vmss`)도 `LOCKED`가 `true`이고 만들 때 경고한다
  ("the VM is suspended; power it off in VMware before importing (its memory state is not imported)") — 메모리
  상태는 가져오지 않으므로 VMware에서 다시 켠 뒤 게스트를 정상 종료한다. 목록을 읽지 못한 데이터스토어·폴더는 오류 없이
  건너뛴다 — 보여야 할 VM이 없으면 orbivisord 로그를 본다. ESXi `/folder` 페이지의 모양은 문서와 화면을 보고 가짜 ESXi로
  흉내 내 시험했을 뿐 실제 ESXi에서는 시험하지 않았다.
- **씬 디스크와 여유.** `/folder`는 디스크를 통째로 주므로 씬 디스크도 **전체 크기로** 내려받는다. 실행 노드의 업로드
  디렉터리(`{data_dir}/uploads`)에 가장 큰 디스크 용량 + 1 GiB의 여유가 필요하고(아니면 507; 작업도 디스크마다 다시
  본다), 시간도 전체 크기만큼 걸린다. 디스크마다 `disk.raw`로 받아 볼륨으로 옮기고 지운다(풀에는 0이 아닌 부분만
  남는다). 받은 길이가 디스크립터와 다르면 실패한다. 내려받기는 취소할 수 없고, 실패하면 처음부터 다시 받는다. 전체
  시간 제한은 없어(응답·조각마다 60초만 기다린다) ESXi가 바이트를 아주 느리게 보내면 작업이 오래 붙잡힌다.
- **비밀번호는 받은 노드의 메모리에만.** 비밀번호는 클러스터 상태·작업·로그에 남지 않고 `import esxi`를 받은 노드의
  메모리에만 있다. 실행 작업도 그 노드에서 돌며(실행 요청은 어느 노드로 보내도 된다) 끝나거나 가져오기를 지우면 잊는다.
  그 노드의 **orbivisord를 다시 시작하면** 끝나지 않은 ESXi 가져오기는 `failed`("the ESXi credentials are no longer on
  this node (opld restarted or the request was interrupted); create the import again")가 된다 — 지우고 다시 만든다.
  그 노드가 꺼져 있을 때 `--force`로 지운 가져오기의 비밀번호는 그 orbivisord가 다시 시작할 때까지 메모리에 남는다(쓰이지는
  않는다). ESXi 비밀번호를 바꾸면 남은 가져오기는 옛 비밀번호를 쓰므로 지우고 다시 만든다.
- **vCenter.** `--host`에 vCenter 주소, `--datacenter`에 데이터센터 이름(ESXi 단독 호스트는 기본값 `ha-datacenter`)을 준다.
  vCenter의 `/folder` 페이지는 ESXi의 것과 같은 모양이라고 보고 만들었고 실제 vCenter에서는 아직 시험하지 않았다.
- **분산 스위치·NSX.** vCenter 분산 스위치(VDS)의 NIC는 원본 네트워크가 포트 그룹 이름으로, `.vmx`에 이름이 없으면(보통은
  없다) `dvs:{포트 그룹 id}`로 보이고, NSX 네트워크의 NIC는 `nsx:{id}`로 보인다. 둘 다 경고가 나오고, 실행 때
  `--net "dvs:dvportgroup-1001=lan"`처럼 매핑한다.
- **권한과 연결.** 가져오기 쓰기 권한(operator 이상)이 있으면 orbivisord 노드에서 아무 호스트:포트로 TLS 연결을 시도하게 할 수
  있다(지문을 믿기 전에도 인증서를 받으려고 연결한다). 관리자는 operator 권한을 그만큼 믿어야 한다(연결할 곳의 허용
  목록은 아직 없다).
- `ova`·`esxi`는 예약 이름이라 예약 전에 그 이름으로 만든 옛 가져오기는 이름이 아니라 id로 다룬다
  (`orbivisorctl import get <id>`).
- CD-ROM·플로피·USB·사운드는 경고로 보이고 가져오지 않는다.

- **제안.** 게스트 OS(`vmw:osType`)가 Windows(`win…`)면 디스크 `sata`·NIC `e1000e`(Windows가 드라이버를 갖고 있다),
  그 밖은 `virtio-blk`·`virtio`다. vCPU는 원본의 소켓당 코어로 나누고(나누어떨어지지 않으면 소켓 하나), 메모리는
  그대로(최소값보다 작으면 최소값), 펌웨어는 원본의 `efi`면 `uefi`, 아니면 `bios`, 머신은 `q35`, CPU 모델은 `host`다.
  첫 디스크로 켠다(디스크가 여러 컨트롤러에 걸쳐 있으면 첫 디스크가 부트 디스크인지 확인하라는 경고가 나온다). SATA
  디스크가 6개를 넘으면 `virtio-blk`를 제안한다(AHCI 포트 수, 실행 때 `--disk-bus sata`로 6개를 넘기면 400).
  CD-ROM·플로피·USB 같은 장치는 가져오지 않고 경고로 보인다. `import run`의 `--disk-bus`·`--nic-model`·`--firmware`·`--sockets`·`--cores`·`--memory`가
  제안을 바꾼다.
- **네트워크 매핑.** 원본 NIC의 네트워크마다 Orbivisor 네트워크를 정해야 한다(`--net "원본=Orbivisor"`, 원본 네트워크가
  하나면 `--net lan`만). 이름이 같은 Orbivisor 네트워크가 있으면 제안에 들어 있고, 실행 때 그 원본 네트워크를 주지
  않으면 제안대로 매핑한다. 매핑도 제안도 없거나 없는 네트워크면 400이다.
- **MAC과 원본 끄기.** 게스트의 고정 IP·라이선스가 MAC에 묶인 경우가 많아 원본 MAC을 그대로 쓴다. **VMware의 원본
  VM을 끈 뒤에** 가져온 VM을 켠다(같은 네트워크에 같은 MAC이 둘이면 안 된다). 새 MAC을 받으려면 `--new-macs`다.
  다른 Orbivisor VM이 같은 MAC을 쓰면 409 `mac_in_use`다(실행 작업도 디스크를 옮기기 전에 다시 본다).
- **Windows.** 처음에는 제안대로 `sata`·`e1000e`로 켠다. virtio로 바꾸려면 게스트에
  [virtio-win](https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/) 드라이버를 먼저 넣고(VM에
  작은 `virtio-blk` 볼륨을 하나 더 붙여 켜면 Windows가 드라이버를 붙인다), VM을 끄고 웹의 VM 편집(또는 API
  `PUT /api/v1/vms/{name}`)으로 디스크 버스·NIC 모델을 바꾼다. 드라이버 없이 부트 디스크를 `virtio-blk`로 바꾸면 Windows가 켜지지 않는다
  (`INACCESSIBLE_BOOT_DEVICE`).
- **VMware Tools.** 게스트 안에서 지운다(Windows: 앱 및 기능, Linux: `open-vm-tools` 패키지). Linux 게스트는 virtio
  드라이버를 커널에 갖고 있지만, 네트워크 장치 이름이 바뀌어(`ens192` → `enp1s0` 등) 네트워크 설정을 고쳐야 할 수 있다.
- **UEFI 부트 항목.** UEFI 변수(NVRAM)는 옮기지 않는다. 대부분 OS 기본 경로(`\EFI\BOOT\BOOTX64.EFI` 등)로 켜지지만,
  켜지지 않으면 콘솔에서 펌웨어 메뉴(켤 때 Esc)의 Boot Manager로 OS 부트 로더를 고른다.
- **원본은 올린 노드에 남는다.** OVA는 접속한 노드의 `{data_dir}/uploads/imports/{가져오기 id}/source.ova`에 두고(올리기에
  파일 크기 + 1 GiB, 실행에 가장 큰 디스크 파일 + 1 GiB의 여유가 필요하다, 아니면 507; 실행 작업도 디스크를 꺼내기
  전마다 다시 보고 모자라면 실패한다), 실행도 그 노드에서 한다. 그래서
  풀은 그 노드에서 쓸 수 있어야 하고(409 `pool_not_on_node`), 그 노드가 꺼져 있으면 실행할 수 없다(409
  `node_offline`). 위트니스 노드에는 올릴 수 없다(400). 실행이 끝나면 원본을 지운다. 원본 노드가 클러스터에서 빠졌거나
  꺼진 채 돌아오지 않으면 `orbivisorctl import delete web01 --force`로 기록만 지운다(원본 파일은 그 노드에 남는다). ESXi
  가져오기도 `import esxi`를 받은 노드에서 실행한다.
- **볼륨.** 디스크마다 OVA에서 꺼내(`.mf`가 있으면 SHA-1·SHA-256·SHA-384·SHA-512를 확인한다; 모르는 알고리즘이거나
  `.mf`에 없는 디스크는 확인하지 않고 경고한다) 볼륨 `{VM 이름}-disk{i}`(풀의 기본 형식:
  dir·NFS는 qcow2, 블록 풀은 raw)로 옮긴다. 해시가 다르면 실행이 실패하고("checksum mismatch for …") 그때까지 만든
  볼륨을 지운다. 실행은 디스크 크기만큼 걸리므로 작업(Task) 시간 제한이 없고, `orbivisorctl import run`은 24시간까지 기다린다.
- **상태.** `ready`(실행할 수 있다; 실패한 실행의 이유는 `error`), `importing`(실행 중, 지울 수 없다 — 409
  `import_busy`), `done`(만든 VM이 `vm`에), `failed`(원본을 쓸 수 없다, 예: orbivisord가 다시 시작할 때 원본 파일이 없음 —
  지우고 다시 올린다). 실행 중에 orbivisord가 멈추면 다시 시작할 때 `ready`로 돌아간다(VM을 이미 만들었으면 `done`;
  그렇게 끝나지 못한 가져오기는 다시 실행하면 `done`이 되고 409 `import_done`이다).
  원본 노드가 클러스터에서 빠져 실행 작업이 실패로 끝나면 `importing`에 남지만, 끝나지 않은 작업이 없으면 다시
  실행하거나 지울 수 있다(노드가 없으면 `--force`). 웹의 지우기 단추는 `importing`에서도 누를 수 있고, 아직 실행 중이면
  서버가 `import_busy`로 거절한다.
- **아키텍처.** 가져온 VM은 x86 `q35`다. aarch64(macOS) 노드에도 가져올 수는 있지만 켜려면 x86_64 노드가 필요하다.
- 웹은 가상 머신 › 가져오기 탭에서 한다(목록, "OVA 올리기" — 진행률, 이름은 파일 이름에서; "ESXi에서 가져오기" —
  호스트·포트·사용자·비밀번호·데이터센터로 연결 → 처음 보는 인증서면 지문을 보이고 "이 지문을 믿고 계속" → VM 고르기
  (잠금 경고, 읽지 못한 VM은 고를 수 없음) → 가져오기 이름 → 만들기, 비밀번호는 브라우저에 저장하지 않는다; 항목 화면 —
  원본 요약·경고, 이름·풀·CPU·메모리·펌웨어·디스크 버스·NIC 모델·네트워크 매핑·MAC 유지, 실행과 진행, 만든 VM 링크,
  지우기). 강제 지우기는 `orbivisorctl`로 한다. API는 `POST /api/v1/vm-imports/ova/check?name=&size=`,
  `POST /api/v1/vm-imports/ova?name=`(본문 OVA, 201), `POST /api/v1/vm-imports/esxi/browse`(본문 연결
  `{"host", "port", "user", "password", "datacenter", "fingerprint"}`), `POST /api/v1/vm-imports/esxi`(`{"name", "connection",
  "datastore", "vmx"}`, 201), `GET /api/v1/vm-imports`, `GET`·`DELETE /api/v1/vm-imports/{name}`(`?force=true`),
  `POST /api/v1/vm-imports/{name}/run`(`{"name": …, "pool": …, "networks": {…}, …}`, 202 작업)이고, 읽기는 모든 사용자,
  올리기·ESXi 연결·실행·지우기는 operator 이상이다.
- **한계.** 실행 중인 VMware VM 옮기기, 증분 동기화, VMware 스냅샷 체인, VMware Tools 제거·virtio 드라이버 넣기(게스트에서
  한다), OVA 서명(`.cert`, 확인하지 않고 경고), 압축·분할 디스크, 여러 VM이 든 OVA, VM 일괄 가져오기, VDDK·NFC, 내려받기
  이어 받기·취소·전체 시간 제한은 없다. ESXi에서는 디스크립터가 따로 없는 VMDK·여러 extent·RDM을 받지 않는다.
- **남을 수 있는 것.** 실행이 볼륨을 다 만든 뒤 VM을 만들기 전에 orbivisord가 죽으면 `{VM 이름}-disk{i}` 볼륨이 남는다(복제·복원과
  같다) — `orbivisorctl volume delete`로 지운다.

### vCenter 일괄 이전과 따뜻한 이전

계획 72. 저장한 vCenter·ESXi **원본 연결**(`vmware-source`)의 인벤토리에서 VM을 여러 대(한 계획에 200대까지) 고르고, 매핑 규칙
한 번(네트워크·데이터스토어 → 풀·폴더·노드·장치 종류·MAC)으로 **이전 계획**(`vmware-migration`)을 만든다. 계획의 VM마다
항목(`vmware-vm`)이 생기고, 리더의 스케줄러가 한도를 지키며 VM마다 이어 가는 Task로 옮긴다. 원본 VM은 **어떤 경우에도 지우지
않는다** — 원본에 하는 일은 우리 스냅샷 만들기·지우기, CBT 켜기·끄기, 전환 때 끄기와 보호 설정뿐이다.

- **차가운 이전(`--cold`)**: 꺼진 VM의 디스크를 한 번 통째로 복사한다(켜진 VM은 미리 확인이 막는다). 전환은 VM 만들기·켜기만이다.
- **따뜻한 이전(`--warm`)**: 원본이 **켜진 채** 우리 스냅샷 위에서 전체 복사하고, 그 뒤 주기(`--sync-every`, 기본 60분)마다
  CBT(`QueryChangedDiskAreas`)가 알려 준 바뀐 블록만 다시 옮긴다. 회차마다 "새 스냅샷 만들기 → 앞 스냅샷 지우기(합치기)"를 해서
  기반 flat 파일이 늘 지난 회차의 모습이 되고, 바뀐 구역을 그 파일에서 범위 읽기로 읽는다(VDDK가 필요 없다). **전환** 때만 원본을
  끄고(게스트 종료 또는 전원 끄기) 마지막 델타를 옮긴 뒤 VM을 만들어 켠다 — 내리는 시간은 마지막 델타 크기 ÷ 잰 속도다.

```bash
# 1. 원본 연결(admin). 지문 없이 주면 서버 인증서 지문을 알려 준다(비밀번호는 보내지 않는다).
export ORBIVISOR_VMWARE_PASSWORD='…'                          # 또는 --password-file, 둘 다 없으면 터미널에서 묻는다
orbivisorctl vmware source add vc1 --host vcenter.example.com --user migr@vsphere.local
# error: … fingerprint AB:CD:…; check it and rerun with --fingerprint AB:CD:…
orbivisorctl vmware source add vc1 --host vcenter.example.com --user migr@vsphere.local --fingerprint AB:CD:…
# vmware-source vc1 added (vcenter 8.0.2)
orbivisorctl vmware source browse vc1 --path DC1/vm/Prod  # PATH NAME POWER OS CPU MEMORY DISKS SNAPSHOTS CBT

# 2. 계획(초안) → VM마다 미리 확인 → 시작
orbivisorctl vmware migration create wave1 --source vc1 --warm \
  --vm DC1/vm/Prod/web01 --vm DC1/vm/Prod/db01 \
  --net "VM Network=lan" --net dvpg-web=web --datastore ds1=fast --pool slow \
  --folders mirror --folder /prod --sync-every 30m
orbivisorctl vmware migration preflight wave1            # VM LEVEL PROBLEMS — 막힌 VM이 있으면 끝 코드 1
orbivisorctl vmware migration start wave1                # 막힌 VM은 failed로 남고 나머지가 시작한다
orbivisorctl vmware migration get wave1                  # VM STATE ROUND COPIED LAST-DELTA ESTIMATE NEXT-SYNC

# 3. 전환(점검 창에) → 확인 → 마무리 또는 되돌리기
orbivisorctl vmware vm cutover wave1-web01 --wait        # vmware-vm wave1-web01 is done (downtime 252s)
orbivisorctl vmware vm finalize wave1-web01 --wait       # 우리 스냅샷·CBT를 치운다(되돌릴 수 없게 됨)
orbivisorctl vmware vm revert wave1-db01 --confirm db01 --wait   # 대상 VM을 끄고 원본을 다시 켠다
```

- **상태.** 항목은 `draft`(시작 전) → `pending` → `copying`(전체 복사) → `syncing`(따뜻한 — 델타 회차를 돈다) 또는 `ready`(차가운 —
  복사 끝) → `cutting-over`(걸음 `preparing`·`stopping-source`·`final-sync`·`creating-vm`·`starting-vm`) → `done` → `finalized`나
  `reverted`. 실패는 `failed`(이유는 `error`, `vmware vm retry`가 체크포인트부터 다시 한다), 전환 중 실패해 원본을 되살렸으면
  `rolled-back`(다시 전환할 수 있다), 취소는 `cancelled`(우리 스냅샷·CBT·대상 볼륨을 치운다). **계획을 취소해도 전환한(`done`)
  VM과 전환 중인 VM은 건드리지 않는다** — 대상 VM은 돌고 원본은 꺼진 채 남아 `finalize`나 `revert`로 끝낸다(전환 중이던 VM이
  되돌려지면 그때 치운다). 취소 정리가 세 번 실패하면 스케줄러가 멈추고(동기화 실패 수와 따로 센다) `vmware vm cancel`이 다시
  한다. 다른 노드의 로컬 풀에 있는 대상 볼륨은 그 노드가 다시 시작할 때 지운다(기록을 남긴다). 회차·전체 복사·전환은 끊기거나
  orbivisord가 다시 떠도 Raft에 적힌 체크포인트·걸음부터 잇는다. 리더가 바뀌어도 새 리더의 스케줄러가 다음 틱(15초)에 잇는다.
- **한도.** 계획의 `--parallel`(동시 동기화·전환, 기본 4), `--per-host`(ESXi 호스트마다 2), `--per-node`(대상 노드마다 2)와
  대역(`limits.bandwidth_mib_s`, API·웹)을 지킨다. 델타·전환이 새 전체 복사보다 먼저 돈다. 사용자 스냅샷이 있는 VM은 따뜻한
  이전에서 그 회차를 건너뛰고 경고한다(스냅샷을 지우면 다음 주기에 잇는다). 회차는 읽기 직전에 스냅샷 나무를 다시 봐서, 검사 뒤에
  생긴 스냅샷이 있어도 건너뛴다(전환의 마지막 델타면 되돌린다 — 놓친 쓰기로 `done`이 되지 않게). 이미 내용이 있는 대상에 전체로
  다시 복사하면(CBT 초기화·디스크 늘림·`full`) 원본에서 쓰이지 않은 곳을 0으로 쓴다. 14일(`stop_after_days`) 동안 전환하지 않으면 원본에
  스냅샷이 오래 남지 않게 동기화를 멈춘다(`retry`가 다시 시작).
- **미리 확인.** VM마다 계획 71의 만들기 검사(대상 폴더 권한 먼저, 할당량·풀 공간·네트워크·노드 — 숨은 풀·네트워크는 새지
  않는다)와 VMware 쪽 표(`vmware_snapshots`·`vmware_ft`·`vmware_template`·`vmware_rdm`·`vmware_independent_disk`·`vmware_multi_writer`·
  `vmware_needs_vddk`(vSAN·vVols)·`vmware_powered_on`(차가운)·`vmware_no_tools`(따뜻한 + 게스트 종료, 경고)·`vmware_restricted`(무료
  ESXi) …)를 칸(`source`·`disks.N`·`nics.N`·`guest` …)과 함께 준다. 매핑되지 않은 네트워크는 막힘이다(`network_unmapped`).

**전환 점검표.**

1. 계획 상세의 "예상 내릴 시간"(마지막 델타 ÷ 잰 속도)을 본다. `cutover.max_final_delta_gib`를 주면 끄기 직전 델타가 그보다 클 때
   원본을 끄지 않고 멈춘다(`final_delta_too_large`).
2. 게스트 맞추기를 **원본에서 미리** 한다(아래) — 다음 델타가 그 바뀐 파일을 옮긴다.
3. 전환: 원본 보호(`protect_source`, 기본 켬)면 원본 NIC의 "켤 때 연결"을 끄고 설명에 "Migrated to Orbivisor VM …"을 붙인 뒤, 켜진
   채 델타 한 번 → 게스트 종료(VMware Tools, `shutdown_timeout_s` 기본 600초 — 넘기면 원본은 켜진 채 멈춤; 그 뒤 최대 120초 더
   지켜보다 느린 게스트가 꺼지면 다시 켠다. 그 뒤에 꺼지면 6시간 안의 다음 동기화(5분 안에 당겨짐)가 다시 켜고 기록·감사에 남긴다)
   또는 전원 끄기 → 마지막
   델타 → VM 만들기(원본 MAC 그대로, `--new-macs`면 새 MAC) → 켜기(`--no-start`면 꺼진 채). 걸음마다 Raft에 먼저 적는다.
4. **자동 되돌림**: `final-sync` 뒤 어느 걸음에서든 실패하면 대상 VM을 지우고(볼륨은 남김) NIC·설명을 되돌린 뒤 원본을 다시 켠다 →
   `rolled-back`(이유는 `error`). 되돌림까지 실패하면 `failed`와 손으로 할 일("stop and delete Orbivisor VM …, then turn "Connect At
   Power On" back on …, then power on … in vCenter")을 남기고, `retry`가 되돌림을 다시 한다(전환과 같은 권한, 겹친 다시 하기는
   409 `migration_busy`). 할당량·용량을 넘으면 VM 만들기에서 막혀 같은 되돌림을 한다. 대상 VM을 **켠 뒤**(`starting-vm`) 되돌린
   따뜻한 이전은 그 게스트가 대상 디스크에 썼을 수 있어 대상 내용을 버리고 `copying`(처음부터 전체 복사)으로 돌아간다. 이 자동
   되돌림은 전환 Task가 처음 켜다 `running`을 보기 전에 실패했을 때만이다: 대상 VM이 켜졌거나 켜졌을 수 있으면(orbivisord가 다시 떠
   `starting-vm`부터 잇는 등) 절대 자동으로 되돌리지 않고 — 이때는 vCenter에 연결하지도 않는다 — 켜지 못해도 `done`에 오류를 남기고
   VM·디스크를 둔다. 그 뒤 돌아갈 길은 `revert`뿐이다.
5. 새 VM이 잘 돌면 `finalize`(우리 스냅샷·우리가 켠 CBT를 지움, 원본 보호는 남김 — 실패하면 `done`에 오류를 남기고 다시
   마무리한다). 안 되면 `revert --confirm <Orbivisor VM 이름>`:
   대상 VM을 끄고(지우지 않음) NIC·설명을 되돌리고 원본을 켠다 → `reverted`. **전환 뒤 대상 VM에서 쓴 데이터는 원본으로 가지
   않는다.** 내린 시간은 항목 `downtime_s`·기록 "cut over; downtime 4m12s"·감사 로그에 남는다.
6. **예약 전환**: `--cutover-at 2026-12-01T02:00:00Z`면 그 시각이 지난 뒤 스케줄러가 `ready`·`syncing`(회차 ≥ 1) 항목을 같은 한도
   안에서 한 번씩 전환한다(실패해 되돌린 항목을 다시 끄지 않는다).

**게스트 맞추기.** Orbivisor는 게스트 안에서 명령을 돌리지 않는다(virt-v2v도 없다).

- **RHEL·Rocky·CentOS**: initramfs에 virtio 드라이버가 없을 수 있다(미리 확인 `guest_initramfs`). 원본에서
  `dracut -f --add-drivers 'virtio_blk virtio_net virtio_pci'`(또는 `--no-hostonly`)를 돌린 뒤 다음 회차를 기다린다.
- **Windows**: 제안은 SATA·e1000e다(드라이버가 있음). `--driver-iso virtio-win`을 주면 그 ISO 볼륨을 CD로 붙여 만든다 — 켠 뒤
  드라이버를 넣고 virtio로 바꾼다([위](#vmware에서-가져오기)의 Windows 항목). pvscsi·vmxnet3 흉내 장치는 없다.
- **네트워크**: NIC 이름이 바뀔 수 있다(`ens192` → `enp1s0`, 미리 확인 `guest_nic_renamed`). MAC은 그대로라 DHCP 예약은 산다.
  `reserve_ips`(웹 "Reserve guest IPs")면 SDN 네트워크의 NIC마다 VMware Tools가 알린 IP를 IPAM에 예약한다(그새 쓰였으면 다른
  주소와 경고).
- VMware Tools는 켠 뒤 게스트에서 지운다.

**원본 안전.** 원본 VM·디스크·사용자 스냅샷을 지우거나 바꾸는 호출은 없다. 우리 스냅샷 이름은 `orbivisor-migration-…`이고 Raft에
적힌 moref만 지운다. 전환 뒤 원본은 꺼진 채 "켤 때 연결"이 꺼져 있어 누가 켜도 같은 MAC·IP가 둘이 되지 않는다. 회차마다 원본
데이터스토어에 델타 크기만큼 합치기 I/O가 생긴다(VMware 백업 작업과 겹치면 그 회차를 건너뛴다).

**디스크 읽는 길.** 기본은 데이터스토어 `/folder` 범위 읽기다. vCenter면 VM이 있는 ESXi 호스트에 호스트 티켓
(`AcquireGenericServiceTicket`)으로 바로 붙고(호스트 인증서는 vCenter가 알려 준 것으로 고정), 안 되면 vCenter `/folder`로 내려간다.
vSAN·vVols 데이터스토어는 `/folder`로 읽을 수 없어 **VDDK** 길이 필요하다(`vmware source add … --vddk [--vddk-libdir DIR]`).
VDDK는 VMware 독점 라이브러리라 Orbivisor가 넣지 않는다: 운영자가 노드마다 `nbdkit`과 `nbdkit-vddk-plugin`(배포판 꾸러미)을 깔고
Broadcom 사이트에서 받은 VDDK를 `/opt/vmware-vix-disklib-distrib`에 풀어 노드 설정에 적는다.

```toml
# /etc/orbivisor/orbivisord.toml
[vmware]
nbdkit = "/usr/sbin/nbdkit"
vddk_libdir = "/opt/vmware-vix-disklib-distrib"   # 원본의 --vddk-libdir가 먼저
```

**실제 vCenter에서 손으로 확인하기.** 실험실에 vCenter·ESXi가 없어 아래는 가짜 vSphere(의미)와 govmomi vcsim(SOAP 꼴)으로만
시험했다. 처음 쓰는 vSphere에서는 버려도 되는 작은 Linux VM(디스크 둘, Tools 켜짐)으로 먼저 본다:

1. `vmware source add`·`check`·`browse` — 종류·판, 폴더·클러스터·호스트·분산 포트 그룹 이름이 vSphere Client와 같은지.
2. 따뜻한 계획(`--sync-every 5m`)을 시작하고 vSphere Client의 그 VM "Snapshots"에 `orbivisor-migration-…` 하나만 보이는지, 회차마다
   바뀌고 합치기가 끝나는지(Tasks 창), 게스트에서 파일을 쓴 뒤 다음 회차의 `LAST-DELTA`가 작게 늘어나는지.
3. 전환 → 원본이 꺼지고 NIC "Connect At Power On"이 꺼지고 설명이 붙었는지, 새 VM이 켜지는지(게스트 부팅), 쓴 파일이 있는지.
4. `revert` → 원본이 연결된 채 다시 켜지는지. 다시 전환 → `finalize` → 원본에 우리 스냅샷이 없고 CBT가 꺼졌는지(우리가 켰을 때).
5. 무료 ESXi는 API가 읽기 전용이라 차가운 이전만 된다(`vmware_restricted`) — 그 거절 문구를 확인한다.

호스트 티켓 한 번 쓰기, 켜진 VM 기반 flat의 `/folder` 범위 읽기, 합치기 시간, vSAN·VDDK 처리량은 이 손 확인이 처음 보는 것이다.

**웹.** 가상 머신 › 가져오기 탭의 보기 넷: 가져오기(계획 29·30) | **VMware 이전**(계획 목록·상세 — VM별 상태 칩·회차·복사
막대·마지막 델타·예상/실제 내릴 시간·다음 동기화, 고른 VM 일괄 전환·취소, 서랍의 디스크별 진행·기록·되돌리기·마무리·다시 하기)
| **VMware 원본**(연결·지문 확인·다시 확인) | **내보내기**. 새 계획은 마법사다: 원본·이름·방식 → 인벤토리 나무("VM과 폴더" |
"호스트와 클러스터", 찾기·전원 거르기, 이미 다른 계획에 든 VM은 고를 수 없음) → 매핑(고른 VM이 쓰는 네트워크·데이터스토어만) →
한도·전환 정책 → 미리 확인(VM마다 칩, "막힌 VM은 빼고 시작").

**권한.** 원본 보기 `cluster.read`, 원본 만들기·바꾸기·지우기 `cluster.admin`(비밀번호는 Raft에만 — API·watch·감사·로그·`Debug`에
없다; 호스트·포트·사용자를 바꾸면 비밀번호도 다시 줘야 한다, 아니면 400 `password_required`). 계획·항목 보기 루트 `system.read`,
만들기·시작·멈춤·취소·다시 하기 루트 `vm.create`, 전환·되돌리기·마무리와 원본 전원을 다루는 다시 하기·취소(전환 중이거나 되돌림이
실패한 VM)는 **대상 폴더의** `vm.create` + `vm.power`. 기능 `vmware-migrate`가 없는 노드가 있으면 409 `cluster_upgrading`.

**한계.** 실제 vCenter·ESXi·VDDK에서 시험하지 않았다(위 손 확인). 사용자 스냅샷·독립 디스크·RDM·FT·멀티라이터 디스크 VM은
따뜻한 이전을 받지 않는다(차가운 이전은 꺼진 VM의 일부만). 게스트를 고치지 않고, 실제 게스트 부팅은 실험실에서 확인하지 못했다
(aarch64 TCG, VMware 게스트 없음 — 시험은 바이트 같음만). 시험 부팅, 앱 그룹 전환·전환 훅, 원본 포트 그룹 바꾸기, 태그·리소스 풀·
DRS 규칙 옮기기는 없다. 속도는 가짜 vSphere 루프백 수치(디버그 빌드 전체 복사 107 MiB/s)뿐이다.
