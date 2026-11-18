# 백업·복제·가져오기

백업과 복원, 원격 클러스터, 원격 복제(DR), VMware에서 가져오기. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

- [백업과 복원](#백업과-복원)
- [원격 클러스터](#원격-클러스터)
- [원격 복제(DR)](#원격-복제dr)
- [VMware에서 가져오기](#vmware에서-가져오기)

## 백업과 복원

백업은 VM 전체(디스크, UEFI 변수, VM 스펙)를 다른 저장소에 떠 둔 것이다. 스냅샷과 달리 VM 디스크의 저장소를
잃거나 VM을 지워도 남고, 언제든 **새 VM**으로 되살린다. 백업은 `backups`를 켠 **디렉터리·NFS 풀**(백업 풀)에 둔다.
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
# NAME                 VM   POOL  PHASE  CREATED               SIZE      TYPE  ORIGIN  REPLICAS
# before-upgrade       web  bk    ready  2026-11-04 03:00 UTC  412.5MiB  full  -       -
# web-20261105-030000  web  bk    ready  2026-11-05 03:00 UTC  18.2MiB   incr  -       -
orbivisorctl backup get before-upgrade                       # 디스크별 크기, UEFI 변수, 오류, 증분이면 부모와 체인(JSON)
orbivisorctl backup restore before-upgrade --name web2 --pool local  # 새 VM web2(꺼진 채), --node로 노드를 정할 수 있다
orbivisorctl backup delete before-upgrade                    # 파일과 리소스를 지운다(실패한 백업도)
```

웹은 사이드바의 "백업" 화면(모든 백업, VM이 지워진 백업 포함)과 VM 상세의 "백업" 탭(지금 백업, 복원)에서 하고,
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
- 웹은 사이드바 "백업" 아래 "백업 작업" 화면(만들기·편집·지우기·지금 실행, 다음·마지막 실행 시각은 UTC로 표시)에서
  한다. API는 `GET`·`POST /api/v1/backup-jobs`, `GET`·`PATCH`·`DELETE /api/v1/backup-jobs/{name}`,
  `POST /api/v1/backup-jobs/{name}/run`이고, 읽기는 모든 사용자, 쓰기는 operator 이상이다. 스케줄러의 확인 간격은
  `[backup] tick_secs`(기본 60초)다.

- **한계.** 중복 제거, 암호화(TPM 상태 `tpm2-00.permall`도 그대로 담긴다), 원격(S3) 저장소, 파일 단위 복원, 기존 VM 덮어쓰기는 없다.
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
    | 첫 조각이 `vms`·`volumes`·`storage-pools`·`networks`·`security-groups`·`snapshots`·`backups`·`backup-jobs`·`vm-imports`·`nodes`·`tasks`·`cluster`·`version`·`settings`·`subscription`, 또는 정확히 `auth/whoami` | viewer 이상 | admin |
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
(`backup replicate`) 보낸다. prod에서 원본을 지우면(사용자나 보존 규칙) dr의 사본도 지운다.

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
# NAME                 VM   POOL  PHASE  CREATED               SIZE      TYPE  ORIGIN  REPLICAS
# web-20261107-030000  web  bk    ready  2026-11-07 03:00 UTC  412.5MiB  full  -       dr:ready
orbivisorctl backup delete web-20261107-030000      # dr의 사본도 지운다

# dr에서
orbivisorctl backup list
# NAME                 VM   POOL   PHASE  CREATED               SIZE      TYPE  ORIGIN         REPLICAS
# web-20261107-030000  web  dr-bk  ready  2026-11-07 03:00 UTC  412.5MiB  full  prod/019a5b1c  -
```

- **수동 복제.** `orbivisorctl backup replicate <백업> --remote <원격> --pool <원격의 백업 풀> [--wait]`(API `POST
  /api/v1/backups/{key}/actions/replicate {"remote", "pool"}`, operator 이상, **202** Task). 원격이 등록되지 않았으면
  **404**, 원본이 `ready`가 아니면 **409** `backup_not_ready`, 지우는 중이거나 같은 전체 백업에서 시작한 체인의 어느
  백업이든(이 백업, 조상, 자식, 형제 — 전체 백업이면 그 증분들) 같은 원격으로 보내는 Task가 끝나지 않았으면 **409**
  `backup_busy`(복제 중인 그 백업의 이름), 파일이 있는 노드가 오프라인이면 **409**
  `node_offline`이다. 웹은 백업 표(백업 화면·VM 상세의 백업 탭)의 "원격으로 복제" 버튼(ready 백업, operator 이상):
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
  - 백업 파일을 그대로 보낸다(전체 백업은 디스크 전체, 증분 백업은 바뀐 블록만 — 중복 제거·압축 추가 없음). 증분
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
export OPL_ESXI_PASSWORD='…'                    # 또는 --password-file, 둘 다 없으면 터미널에서 묻는다(인자로는 받지 않는다)
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
- 웹은 사이드바 "가져오기" 화면에서 한다(목록, "OVA 올리기" — 진행률, 이름은 파일 이름에서; "ESXi에서 가져오기" —
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
