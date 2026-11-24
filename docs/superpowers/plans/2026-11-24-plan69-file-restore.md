# Orbivisor 계획 69: 파일 단위 복원 — 백업 안 파일 찾아보기·내려받기, 게스트 에이전트로 되돌려 쓰기, 일회용 도우미 VM 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** VM 통째 복원 없이 백업 안 파일을 꺼낸다(Proxmox Backup Server file restore·Veeam FLR): 백업 하나(저장소 백업·옛 풀 백업 체인·받은 사본·읽기 전용 저장소)를 **일회용 도우미 마이크로 VM**에 읽기 전용으로 붙여 디스크 → 파티션(MBR·GPT)·LVM → 파일시스템(ext4·XFS·Btrfs·NTFS·FAT·exFAT)을 웹·CLI·API로 찾아보고, 파일 하나는 그대로·여러 항목은 **zip**·**tar.gz**로 흘려 내려받고, 선택으로 실행 중 VM 안으로 **게스트 에이전트(`guest-file-*`)**로 되돌려 쓴다. 믿을 수 없는 게스트 파일시스템은 호스트 커널·opld가 해석하지 않는다.

**Architecture:**
- **격리는 도우미 VM**: Debian 13 커널(잠금 파일로 고정) + 우리 initramfs(정적 Rust 에이전트가 PID 1, `lvm`·`blkid`, 고른 모듈), `-kernel/-initrd` 직접 부팅, 네트워크·화면 없음, `systemd-run --scope opl-fr-<id>.scope`(메모리·작업 수 한도), `-run-with user=nobody`(8.2는 `-runas`), seccomp QEMU면 `-sandbox on`. 백업 원본은 QEMU `read-only=on`, 디스크마다 **버리는 qcow2 덮개** 위에 붙여 저널은 재생하되 백업은 바뀌지 않는다.
- **원본**: 저장소 백업은 **opld 안 NBD 서버**(계획 68 `serve` — 키는 opld를 떠나지 않고 도우미는 평문 블록만), 옛 풀 백업은 QEMU가 qcow2 체인 json을 바로 연다.
- **통신은 virtio-serial**(모든 호스트·가속 — vsock은 macOS·HVF에 없음): 틀 붙인 프레임(길이·흐름 번호·종류), JSON 요청·답과 1 MiB 데이터 조각, 흐름 여럿. opld는 게스트가 보낸 모든 것에 한도(이름·깊이·항목 수·시간)를 건다.
- **세션은 Task** `FileRestoreSession { backup }`(대상 = 세션 id, 이어 가지 않음): 같은 사용자·백업은 다시 씀, 노드당 4개, 유휴 15분·최대 8시간·부팅 300초·덮개 2 GiB. API 노드가 세션 노드가 아니면 새 노드 사이 RPC `FileRestore`(단항 + 서버 흐름)로 넘긴다.
- **내려받기**는 일회용 토큰 링크(60초·한 번·같은 사용자) + GET 흐름, zip(zip64·deflate, 심볼릭 링크 빼고 보고서 항목)·tar.gz(pax, 권한·링크 보존)는 직접 짠 흘려 쓰기 작성기 + `flate2`(순수 Rust).
- **게스트로 되돌려 쓰기**는 `FileRestoreToGuest` Task: QGA `guest-file-open/write/flush/close`(1 MiB마다 에이전트 잠금을 쥐고 놓음), 디렉터리·덮어쓰기는 `guest-exec`가 허락될 때만(argv, 셸 없음), 충돌 `keep`(기본)·`skip`·`overwrite`(임시 파일 뒤 이름 바꿈), 권한·소유자는 되돌리지 않음. 권한은 백업 `backup.file_restore` + VM `vm.config`.
- **이 기능을 쓰지 않는 클러스터와 백업은 바이트까지 지금과 같다**(새 리소스 종류 없음, 백업 형식·VM 명령줄 그대로). 기능 하나 **`file-restore`**, 새 권한 **`backup.file_restore`**(operator에 넣지 않음), 이미지는 노드마다 `NodeStatus.file_restore`로 알림(deb `orbivisor-restore-image`, Rocky·macOS는 빌드 스크립트).

**Tech Stack:** Rust(opl-types·opl-cluster·새 opl-restore·opl-qemu·opl-driver·opl-peer·opl-api·oplctl·opld), 에이전트는 `x86_64`·`aarch64-unknown-linux-musl` 정적(`rust-lld`), QEMU 8.2·10.x·Homebrew 11.1(`-kernel` 직접 부팅, virtio-blk·virtio-serial, `-run-with`·`-runas`·`-sandbox`), Debian 13 커널·lvm2·util-linux(snapshot.debian.org 고정), bsdtar·zstd(이미지 빌드), flate2(miniz_oxide, 새 의존성), QGA `guest-file-*`·`guest-exec`, React 19·TanStack Query.

**설계 문서:** `docs/superpowers/specs/2026-11-24-file-restore-design.md`(상위: `2026-11-23-backup-store2-design.md` §12.7, `2026-11-18-vtpm-secure-boot-guest-agent-design.md`, `2026-11-18-folders-permissions-quotas-2fa-design.md`)
**선행 계획:** 계획 1~68 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 세션: 저장소 백업·옛 풀 백업(체인)·받은 사본·읽기 전용 저장소 백업, 같은 사용자·백업은 다시 씀, 노드 고르기(이미지·읽을 수 있음·API 노드 먼저) | 실행 중 VM 디스크·스냅샷·복제본 찾아보기 |
| MBR(논리 포함)·GPT·표 없는 디스크, LVM2(선형·thin), ext2/3/4·XFS·Btrfs(모든 하위 볼륨)·NTFS(ntfs3)·FAT·exFAT, 덮개 위 저널 재생과 재생 없는 대체 | LUKS·BitLocker(알아보고 "지원 안 함"), ZFS·ReFS·mdraid·동적 디스크 |
| 나열(쪽 나눔)·항목 정보, 파일 그대로·zip·tar.gz 흘려 내려받기, 보고서 항목 | 카탈로그·이름 검색·백업 사이 차이·미리 보기 |
| 게스트 에이전트로 되돌려 쓰기(파일, `guest-exec`면 디렉터리·덮어쓰기), 한도 4 GiB·1만 개 | 권한·소유자·ACL 되돌리기, 꺼진 VM에 쓰기, ISO로 꽂기 |
| 도우미 이미지 빌더(재현, x86_64·aarch64), deb `orbivisor-restore-image`, 이미지 검사·노드 알림 | RPM, arm64 deb, 이미지 자동 내려받기 |
| 권한 `backup.file_restore`, 감사, 기능 `file-restore`, API·CLI·웹, GitHub `file-restore` | Windows 게스트 실제 시험(손 확인) |

## 실행 환경

- **이 Mac(aarch64 HVF):** 모든 Rust·웹 변경과 `cargo test --workspace`, opl-restore 순수 시험(프레임 해석 proptest·zip·tar·해석 고정물)과 호스트 임시 디렉터리 위 에이전트 몸통, aarch64 이미지 빌드(bsdtar·zstd, 에이전트는 `aarch64-unknown-linux-musl` + `rust-lld`), 실제 QEMU `qemu-it`+`restore-it`(Homebrew QEMU, HVF — ext4·vfat·MBR·GPT 고정물만: Homebrew `e2fsprogs`·`dosfstools`·`mtools`·`util-linux`), API(`MockHypervisor`·가짜 도우미·가짜 QGA), `pnpm -C web typecheck`·`lint`·`test`, 로컬 브라우저 E2E(`web-it`).
- **GitHub(Ubuntu 24.04 x86_64 KVM, root, 깨끗한 러너):** 새 **`file-restore`** 작업(x86_64 이미지 빌드 두 번·해시 같음, aarch64 이미지 빌드만, 모든 고정물(XFS·Btrfs·NTFS·exFAT·LVM thin 포함), 실제 QEMU 시험·opld E2E·실제 QGA 되돌려 쓰기, `-accel tcg` 강제 한 번), `package`에 `orbivisor-restore-image` deb 빌드·설치, `check (ubuntu)`의 `storage-it`에 `unzip`·`bsdtar` 견줌.
- **배포 서버 svc01(Linux aarch64, `qemu-tcg`):** 일반 시험 묶음 전체, 소스 빌드 aarch64 이미지로 `restore-it`(TCG 부팅 시간·MB/s).
- **테스트 VM:** svc01·svc02에 배포해 기능 `file-restore`·`NodeStatus.file_restore`(aarch64·tcg), 계획 68 `lab-store` 백업 열기("파일시스템 없음"), ext4 고정물 디스크 VM 백업 → 웹에서 파일·zip 내려받기 → 닫기 → 지움.

## 설계 결정

- **격리는 호스트 커널 마운트·사용자 공간 Rust 파서·libguestfs 대신 도우미 마이크로 VM:** 커널 파일시스템 드라이버는 악의적 이미지에 단단하지 않고, Rust 읽기 crate에는 XFS·Btrfs·LVM·저널 재생이 없다. 도우미가 뚫려도 네트워크 없는 VM 안이고 그 뒤에 QEMU 장치 면·scope·권한 내림·sandbox가 있다.
- **커널은 호스트 것(supermin) 대신 Debian 커널 싣기:** Rocky 커널에 Btrfs·ntfs3가 없고 macOS에는 커널이 없다. 해시로 못 박은 Debian 꾸러미를 그대로 쓴다.
- **통신은 vsock·네트워크 대신 virtio-serial:** macOS·HVF·모듈 없는 Linux 모두에서 같고 QGA로 이미 쓰는 꼴. 대가는 TCG 대역폭.
- **원본은 읽기 전용 + 버리는 qcow2 덮개:** crash-consistent 백업도 저널을 재생해 맞는 내용을 보이고 백업은 바뀌지 않는다. 재생이 실패하면 재생 없이 붙이고 알린다.
- **암호화 저장소는 `backup-serve` 헬퍼 대신 opld 안 NBD 서버:** 세션은 opld와 생애를 같이하므로 키를 다른 프로세스에 넘길 까닭이 없다.
- **세션은 새 리소스·노드 메모리만 대신 Task:** 주인·노드·취소·목록·감사를 그대로 쓰고 다른 노드의 API가 찾는다.
- **내려받기는 zip + tar.gz(직접 짠 흘려 쓰기 + flate2), 일회용 토큰 GET:** Windows는 zip, 권한·링크는 tar. 큰 아카이브를 브라우저 메모리에 담지 않는다. zstd(C)는 피한다.
- **게스트로 되돌려 쓰기는 자격 증명 SSH·SMB·ISO 대신 QGA:** 비밀 없이 이미 있는 채널. 덮어쓰기는 임시 파일 + 이름 바꾸기(`guest-exec`)일 때만 받아 끊겨도 원본을 잃지 않는다.
- **권한은 `backup.restore` 대신 새 `backup.file_restore`(operator 제외):** 파일 꺼내기는 폴더·풀·할당량 문이 없는 데이터 읽기라 따로 준다(계획 62 새 권한 규칙).

## Task

각 Task(스테이지)는 하나의 커밋이며, 앞 Task의 커밋 위에서 순서대로 한다. 스테이지마다 혼자 컴파일되고 시험된다. 새 API는 스테이지 5에서 연다(그 전에는 기능 게이트와 임시 막기가 새 칸을 막는다). 설계와 다른 것·결정은 설계 문서 §18에 스테이지마다 남긴다.

- [x] **Task 1: 파일 복원 타입 — 세션·게스트 되돌려 쓰기 TaskOp, 경로·볼륨·항목 보기, 노드 file_restore 상태, 권한 backup.file_restore, 기능 file-restore(opl-types·opl-cluster)** — `file_restore.rs`(`EntryPath`·`PathPart`·`VolumeView`·`EntryView`·`ListPage`·`FileRestoreSessionView`·`GuestRestoreCaps`·`GuestConflict`), `TaskOp::FileRestoreSession`·`FileRestoreToGuest`, `NodeStatus.file_restore`, `Privilege::BackupFileRestore`(operator 제외), 순수 `guest_path_ok`·`guest_target_name`·`content_disposition`·`choose_session_node`, 역할·grant 쓰기의 기능 게이트, 임시 API 막기, serde 하위 호환, `schema.d.ts`.
- [x] **Task 2: opl-restore — 통신 규약, 에이전트 몸통(Fs trait·탐지 해석·마운트 옵션·나열·읽기·걷기), zip·tar.gz 흘려 쓰기, 에이전트 바이너리(opl-restore)** — 새 crate, `flate2`(`rust_backend`)와 `deny.toml`, 프레임 해석기 proptest, blkid·lvm 고정물 해석, 호스트 임시 디렉터리 위 몸통 시험(심볼릭 링크 탈출 막기·동시 흐름·취소), `storage-it`에서 `unzip -t`·`bsdtar -tf`로 견줌, musl 빌드에 C 빌드 스크립트 없음 검사.
- [x] **Task 3: 도우미 이미지 빌더와 deb, 고정물 디스크 스크립트(packaging·ci)** — `packaging/restore-image/{build.sh,debian.lock}`(snapshot.debian.org·SHA-256, ELF `NEEDED` 검사, 재현), `orbivisor-restore-image` deb(`Depends: orbivisor (= 판)`, `orbivisor`의 `Recommends`), `packaging/tests/restore-image-test.sh`, `crates/opl-restore/tests/fixtures/make-disks.sh`(루트 없이, LVM만 root), `package` 작업, 커널 설정 탐침을 §18에.
- [x] **Task 4: 도우미 VM 띄우기 — 명령줄·덮개·NBD/qcow2 원본·scope·권한 내림·sandbox, 에이전트 채널 클라이언트, 남은 것 치우기, 이미지 검사와 노드 알림(opl-qemu·opl-driver·opld)** — `build_restore_helper_cmdline`, `opl-driver/src/file_restore.rs`(`start_helper`·`HelperClient`·`stop_helper`·`sweep_leftovers`·덮개 지킴이), 가짜 하이퍼바이저의 가짜 도우미, `[file_restore]` 설정·이미지 해시 검사·`NodeStatus.file_restore`, `restore-it` 기능, `qemu-it` 탐침(부팅 시간·MB/s·저널 재생·`nouuid`·LVM thin·uid·sandbox)을 §18에.
- [x] **Task 5: 파일 복원 세션 API — 세션 Task·노드 고르기·유휴/최대 시간, 나열·내려받기 토큰과 zip/tar.gz 흐름, 노드 사이 FileRestore RPC, 백업 지우기·보존과의 관계, 감사, oplctl backup files(opl-api·opl-peer·oplctl·opld)** — `file_restore.rs`·`tasks/file_restore.rs`, `FileRestoreCall`·`FileRestoreDownload`, `backup_in_use`·보존 건너뜀, 경로표·오류·OpenAPI·`schema.d.ts`, CLI `open|volumes|ls|get|sessions|close`, 권한 행렬(`insta`), opld E2E(암호화 저장소·풀 증분 체인·도우미 `kill -9`·opld 다시 시작).
- [x] **Task 6: 게스트 에이전트로 되돌려 쓰기와 GitHub file-restore 작업(opl-qemu·opl-driver·opl-peer·opl-api·oplctl·ci)** — QGA `file_open`·`file_write`·`file_flush`·`file_close`·`exec`·`exec_status`·`supported_commands`, 하이퍼바이저 `guest_file_*`(잠금은 쓰기마다), `GuestAgentOp` 새 갈래, `FileRestoreToGuest` Task(keep·skip·overwrite·한도·취소), `GET /vms/{key}/guest/restore-caps`, CLI `backup files restore`, 이미지 빌더 `--test-guest`와 실제 QGA E2E, `.github/workflows/ci.yml`의 `file-restore`, 측정값을 §18에.
- [x] **Task 7: 파일 찾아보기 화면 — 볼륨 나무·파일 표·부팅 상태·내려받기·VM으로 복원 대화상자, 백업 표 동작(web)** — `web/src/fileRestore/`(늦게 읽음), `BackupTable.tsx`·`VmBackups.tsx` 동작, 경로 `/backups/:id/files`, i18n en·ko, vitest·axe·Playwright, 번들 크기.
- [x] **Task 8: 백업 가이드의 파일 단위 복원·도우미 이미지 설치·보안 모형·RHEL 에이전트 설정, 아키텍처, 설계 문서 스테이지 보고, 계획 69 실행 기록(docs)** — `docs/guide/backup.md`(지원 파일시스템 표, 안 되는 것, PBS·Veeam 견주기, "계획 69가 아직 하지 않는 것"·한계), `docs/architecture.md`, `web-ui.md`·`development.md`, `docs/index.md`, README 한 줄, 이 문서.

## 완료 기준 (계획 69)

- 옛 JSON(`TaskOp`·`NodeStatus`·역할·grant)이 바이트 그대로이고, 백업 형식·매니페스트·VM 명령줄이 계획 68과 같다. 업그레이드 중 새 쓰기는 409 `cluster_upgrading`, 옛 노드는 `backup.file_restore`를 버린다.
  - **결과:** 스테이지 1의 serde 하위 호환 시험(`opl-types` — 새 칸이 비면 쓰지 않아 옛 `NodeStatus`·`TaskOp`·역할 JSON 그대로), 백업·명령줄 시험(`backup_tasks`·`backup_store_tasks`·`cmdline`)이 고치지 않고 통과(도우미 명령줄은 새 함수 `build_restore_helper_cmdline` — VM 명령줄은 그대로). 옛 노드가 있으면 세션·되돌려 쓰기·역할과 토큰 범위의 `backup.file_restore`가 409 `cluster_upgrading`(`file_restore_types`·`file_restore`).
- 저장소 백업(평문·암호화, 로컬·S3)·옛 풀 백업(증분 체인 포함)을 열어 MBR·GPT·LVM(선형·thin) 위의 ext4·XFS·Btrfs·NTFS·FAT·exFAT 고정물 파일을 나열하고, 내려받은 파일·zip·tar.gz를 풀면 원본과 SHA-256이 같다. 더러운 저널은 재생된 내용을 보이고, LUKS는 "지원 안 함"으로 보인다. 백업 데이터는 세션 앞뒤로 바이트 그대로다.
  - **결과:** 실제 도우미(`restore_helper`, 이 Mac HVF와 svc01 TCG): 고정물 11개 — ext4(저장소 NBD 원본)·더러운 저널 ext4(재생됨)·XFS 둘(같은 UUID `nouuid`)·XFS 더러운 로그(재생됨)·MBR(논리 vfat)·GPT(ext4·EFI vfat)·LVM 선형·씬·LUKS("encrypted (LUKS) — not supported")·vfat·exFAT(빈 것) — 파일마다 SHA-256이 `expected.sha256`과 같다. opld E2E(1)(2): 암호화 로컬 저장소 백업과 옛 풀 전체·증분 체인을 열어 raw·zip·tar.gz를 풀어 견줌, 증분 세션은 재생된 저널, 전체 세션은 옛 내용. 원본은 QEMU `read-only=on`(저장소 백업은 opld NBD 서버가 쓰기를 거절)이고 저널 재생은 버리는 덮개에 쓰므로 백업에 쓸 길이 없다 — 세션 앞뒤 백업 바이트를 따로 견주는 시험은 없다. **Btrfs·NTFS 고정물은 GitHub `file-restore`에서만**(Mac·svc01에 도구 없음), **S3 저장소 백업의 세션은 실제 S3로 시험하지 않았다**(로컬 저장소와 같은 opld NBD 서버 길).
- 도우미 VM은 네트워크 장치가 없고, Linux에서 QEMU가 `nobody`로 돌며(seccomp QEMU면 sandbox), 세션을 닫거나 도우미·opld가 죽으면 scope·덮개·소켓이 남지 않는다. 프레임 해석기는 무작위 입력에 패닉하지 않고 한도를 넘는 답은 세션을 닫는다.
  - **결과:** 명령줄 시험(`-nodefaults -no-user-config -net none -display none`, 네트워크 장치 없음). svc01 root: `-run-with user=nobody` 뒤 QEMU `Uid 65534`, Rocky `qemu-kvm`에서 `Seccomp 2`(`HELPER_SANDBOX_DROPPED`), `OPL_RESTORE_SCOPE=1`이면 `opl-fr-<id>.scope`(`MemoryMax=768M`·`TasksMax=64`·`CPUQuota=200%`)이고 끝내기·`kill -9`·치우기 뒤 scope가 남지 않는다. opld E2E(3)(4): 도우미 `kill -9` → Task 실패·치움, opld `SIGKILL` → 다시 뜨면 남은 QEMU·두 세션 디렉터리를 치움. 해석기 proptest(무작위 바이트·자르기·거대 길이 — 패닉 없음)와 손으로 짠 에이전트의 규약 위반·침묵 세 번이 세션을 닫음(`opl-restore`·`opl-driver`).
- 세션은 같은 사용자·백업이면 다시 쓰이고, 노드 한도·유휴·최대 시간·덮개 한도가 지켜지며, 세션 노드가 아닌 API 노드에서도 나열·내려받기가 되고 클라이언트가 끊으면 에이전트 흐름이 취소된다. 열린 세션의 백업 지우기는 409 `backup_in_use`.
  - **결과:** `file_restore`(가짜 도우미): 다시 쓰기(200)·노드 한도(`file_restore_busy`)·유휴 닫힘(시계)·덮개 한도는 실제 도우미 시험(1바이트 한도면 첫 회차에 닫힘)·`backup_in_use`와 `force`. `two_nodes`: 다른 노드 세션을 A의 API로 나열·raw·zip·토큰·끊은 내려받기 다섯 뒤에도 답함·닫기(끊으면 노드 사이 흐름이 버려져 에이전트 흐름 `Cancel`).
- 게스트 에이전트로 되돌려 쓰기가 실제 QGA에 keep·skip·overwrite로 파일·디렉터리를 쓰고, 에이전트가 막은 명령은 409 `guest_agent_restricted`, `exec` 없는 디렉터리·덮어쓰기는 400, 취소가 임시 파일을 지운다.
  - **결과:** 실제 QGA: opld E2E(5)(시험 게스트 — Debian `qemu-guest-agent`, 이 Mac HVF·svc01 TCG)가 디렉터리·3 MiB 파일(새 자리), `keep`(`hello.txt.restored-…`), `overwrite`(임시 → `mv -f -T`), 링크 건너뜀을 쓰고 VM을 끈 뒤 다시 백업해 열어 견줌(`.orbivisor-` 임시 파일 없음). 가짜 QGA(`MockGuestFs`): 막힌 에이전트 409 `guest_agent_restricted`, exec 없는 디렉터리 400 `guest_mkdir_unavailable`·`overwrite` 400 `guest_overwrite_unavailable`, 한도, 쓰기 실패 뒤 다 쓰지 못한 파일 지움(`files_are_restored_into_a_running_guest`·`restoring_into_a_guest_checks_the_agent_and_the_request`). Task 취소 API는 없어(설계 §12.2 7) 버려진 Task의 Drop 지킴이가 지운다. Windows 게스트는 명령줄 모양 단위 시험 + 가이드의 손 확인 절차.
- API: 권한 행렬(`backup.file_restore` 없음 403, operator 기본 403, 되돌려 쓰기는 `vm.config`도), 토큰(만료·두 번째·다른 사용자), 감사 문구(경로·바이트·항목 수), 409들(`file_restore_unavailable`·`file_restore_busy`·`session_not_ready`·`volume_not_mountable`).
  - **결과:** `file_restore`의 권한 시험(`sessions_need_the_privilege_and_stay_with_their_owner` — 권한 없음 403·operator 403·헬프데스크 역할 됨·남의 세션 404·`all`은 admin·권한을 빼면 다음 요청 403), 되돌려 쓰기는 VM `vm.config`도, 토큰(만료·두 번째 410·다른 사용자 403), 감사 문구 셋(open·download·to-vm — 경로 5개 + `and N more`, 항목 수·바이트), 409들(`file_restore_unavailable`·`file_restore_busy`·`session_not_ready`·`volume_not_mountable`). 설계의 `insta` 행렬 대신 표 단언으로 썼다.
- 웹: 볼륨 나무·파일 표·부팅 칩·내려받기·VM으로 복원 대화상자가 vitest로, 찾아보기 → 파일 내려받기가 브라우저 E2E로 보인다.
  - **결과:** vitest(볼륨 나무·까닭·경고, 파일 표 쪽 넘김·거르기·키보드, 부팅 칩·TCG 안내, 내려받기 링크 네 가지·429, VM으로 복원 대화상자의 막힌 선택·RHEL 까닭·요청 본문, 백업 표 동작, axe)와 브라우저 E2E `browser_browses_backup_files`(보호 › 백업 → 파일 찾아보기 → 준비됨 → 디렉터리 → `hello.txt`·볼륨 루트 zip → 닫기, 이 Mac HVF 25초)가 통과.
- 부팅 시간(`Hello`까지)과 흐름 MB/s를 이 Mac HVF·CI KVM·CI 강제 TCG·svc01 TCG에서 재 §18에 적는다.
  - **결과:** §18 스테이지 3·4·6: 이 Mac HVF 부팅 1.15~1.33초·흘리기 227 MB/s, svc01 TCG 2.9~3.1초·88 MB/s(upstream QEMU)·109 MB/s(Rocky `qemu-kvm`), 이 Mac x86_64 TCG 부팅 7.0초, 게스트로 되돌려 쓰기 2.5 MB/s(HVF)·1.5 MB/s(svc01). **CI KVM·CI 강제 TCG 값은 GitHub `file-restore` 첫 실행 뒤 채운다**(아래 GitHub 탐침).
- `cargo fmt`·`clippy -D warnings`·`cargo test --workspace`, `pnpm typecheck`·`lint`·`test`, 로컬 브라우저 E2E가 통과하고 svc01 일반 시험과 GitHub `file-restore`·`package`·`linux-kvm`을 포함한 모든 시스템 작업이 통과한다.
  - **결과:** 이 Mac에서 스테이지마다 통과(아래 실행 후 기록). svc01·GitHub은 아래 "svc01 일반 시험"·"GitHub 탐침"에 채운다.

## 알고 받아들인 제한

- **부팅 시간:** 첫 세션은 도우미 부팅을 기다린다 — 하드웨어 가속 노드 1~2초, svc01 TCG 약 3초(x86_64 TCG 7초)에 탐지·Task 시작이 더해진다. 같은 사용자·백업은 세션을 다시 쓰고 유휴 15분까지 열어 두어 덜어 낸다. 웹은 TCG 노드면 "느린 노드: 1분까지"를 보인다.
- **TCG 대역폭:** virtio-serial 흘리기는 CPU에 묶인다 — svc01 TCG 88~109 MB/s(가속 노드 227 MB/s). 노드 고르기가 가속 노드를 먼저 고른다.
- **파일시스템:** LUKS·BitLocker·ZFS·ReFS·HFS+·F2FS·mdraid·Windows 동적 디스크는 알아보기만 한다. 같은 VG UUID를 가진 LVM 디스크 둘은 시험하지 않았다. 저널 재생이 실패하면 재생 없이 붙이고 알린다. Btrfs·NTFS는 GitHub에서만 실제로 붙여 본다.
- **게스트로 되돌려 쓰기:** 권한·소유자·시각·ACL을 되돌리지 않는다(tar.gz 안내). RHEL·Rocky 에이전트는 기본으로 막혀 `/etc/sysconfig/qemu-ga`를 고쳐야 한다(가이드 절차 — 실제 RHEL 게스트로는 확인하지 않음). `guest-exec`가 없으면 있는 디렉터리 바로 아래 파일만. 속도 초당 1.5~2.5 MB. Task 취소 API가 없다. Windows는 손 확인.
- **zip:** 심볼릭 링크·장치·FIFO는 넣지 않고 보고서 항목으로 적는다. 한 파일의 읽기 오류는 오류 항목으로 남고 보고서에 적힌다(다시 받기).
- **이미지 배포:** deb는 amd64만(`orbivisor-restore-image`), Rocky·소스 노드·macOS는 빌드 스크립트, 설치 ISO에는 아직 없다. 이미지 판이 opld 판과 같아야 해서 소스 빌드 노드는 opld를 올릴 때마다 이미지도 다시 빌드한다.
- **macOS 노드:** HVF 자식 프로세스라 권한 내림·sandbox·scope가 없다(개발용).

## 실행 후 기록 (2026-11-24)

- 7개 스테이지를 단계 커밋으로 구현했다(설계·계획 `9b693509`, `11f55d10` 타입·클러스터, `a7fb88f0` opl-restore, `7bf836ff` 도우미 이미지 빌더·deb·고정물, `96e5cf91` 도우미 VM 띄우기·채널 클라이언트·이미지 검사, `72cb7096` 세션 API·노드 사이 RPC·CLI, `f561252a` 게스트로 되돌려 쓰기·GitHub `file-restore`, `a9098d97` 웹, 이 문서 커밋). 설계와 다른 것·결정은 설계 문서 §18에 스테이지마다 남겼다. 주요한 것: opl-restore는 opl-types에 기대지 않음(ring의 C 빌드 — 한도·볼륨 id 규칙을 따로 두고 같은지 시험), `Walk{follow}` 대신 `hardlinks`, 나열은 이름 바이트 차례·커서는 이름, `openat2` 대신 조각마다 `O_NOFOLLOW`, 이미지 빌드에 python3(`imagetool.py` — cpio를 직접 써 재현), arm64 커널은 푼 `Image`, 모듈은 풀어서 넣고 암호 모듈·`nls_ascii`를 먼저(svc01 부팅에서 찾음), 명령줄은 `-display none`·덧붙이는 콘솔 chardev, **권한을 내릴 때 sandbox에서 `elevateprivileges=deny`를 뺌**(QEMU setuid가 SIGSYS), QEMU가 받는 옵션은 `-version`으로 미리 물음, 세션 단계는 `TaskStatus.progress.stage`, 보존은 닫힌 뒤 다음 틱에 지움, `get -O`(`-o`는 전역), 하이퍼바이저 `guest_file` 하나, Windows 명령은 `-EncodedCommand`, exec 없는 `overwrite`는 `guest_overwrite_unavailable`, 비우기가 세션을 닫고 유지보수 노드를 뺌, 시험 게스트는 따로 둔 잠금 파일(`test-guest.lock`), 화면을 떠나도 세션을 닫지 않음, 고른 것이 없으면 "폴더 내려받기".
- 새 시험(브랜치 diff의 `#[test]`·`#[tokio::test]` 수, proptest 블록 제외): opl-restore 65, opl-types 16, opl-api 19(`file_restore` 8·`file_restore_types`·`two_nodes` 포함), opl-driver 14(`restore_helper` 포함), opl-qemu 3, opl-peer 3, oplctl 5, opld 8(`file_restore` 4·`web` 1 포함) — Rust 133개. 실제 시험은 opl-restore `storage-it` `archive_tools`, opl-driver `restore-it` `restore_helper`, opld `restore-it` `file_restore`(E2E (1)~(5)), opld `web-it` `browser_browses_backup_files`. 셸 `packaging/tests/restore-image-test.sh`(CI `shell`)와 `build-deb-test.sh`의 이미지 꾸러미 갈래. 웹 vitest 1586개(155 파일)와 Playwright `file-restore.spec.ts`.
- **Mac 시험**(aarch64 HVF, Homebrew QEMU): 스테이지마다 fmt·clippy `--all-features -D warnings`·`cargo test --workspace`와 그 스테이지의 `storage-it`·`restore-it`·`web-it` 시험이 통과했다. 스테이지 6 커밋에서 `cargo test --workspace` 4173개 통과, opld E2E (1)~(5) 70초. 스테이지 7에서 웹 typecheck·lint·vitest 1586개·build, `web-it` 셋(파일 찾아보기 25초) 통과, `schema.d.ts` 다시 만듦(스테이지 1·5·6). 도우미 이미지·시험 게스트(aarch64)는 이 Mac에서 빌드했다(격리 rustup의 musl 타깃, `rust-lld`).
- **탐침(설계 §18 스테이지 3·4·6):** 커널 설정(두 아키텍처 같음 — 필요한 것은 `m`·`y`), 부팅(Mac HVF 1.26초, svc01 TCG 2.96초 — 고정물 8개를 탐지·마운트·나열·읽기·끄기까지 4.0초, Mac x86_64 TCG 7.0초), 흘리기(svc01 88·109 MB/s, Mac 227 MB/s), 권한 내림·sandbox(Rocky `qemu-kvm` Uid 65534·Seccomp 2, upstream QEMU 10.1.2는 `-runas` 없음·seccomp 없음), scope 속성, 덮개 9.2 MiB, 게스트로 되돌려 쓰기 1.5~2.5 MB/s, svc01 Rocky AAVMF가 TCG에서 `-kernel` 게스트를 띄우지 못함(→ `OPL_TEST_UEFI_*`).
- **웹 번들(eager, gzip -9):** 470,957 B → 475,397 B, **+4.4 KB**(대부분 en·ko 문구). 파일 찾아보기 화면 `FileRestorePage`는 늦게 읽는 조각(8.4 KB).
- **스테이지 8 확인:** 문서의 CLI·설정 키·환경 변수를 코드에서 읽어 맞췄다(`backup files` 하위 명령과 `-O`·`--format`·`--conflict`·`--to`·`--all`, `[file_restore]` 열두 키의 기본값·범위, `OPL_RESTORE_*`·`OPL_TEST_QEMU`·`_ACCEL`·`_SYSTEMD`·`_UEFI_CODE`·`_UEFI_VARS`, `build.sh` 인자, restore-caps 까닭 문구, 오류 코드, 감사 문구). `folders.md` 권한 표에 `backup.file_restore`를 더하고, `Config::qemu_nbd`의 문서 주석이 스테이지 4에서 `file_restore_image_dir`에 붙어 있던 것을 제자리로 옮겼다.
- 리뷰: 백엔드·웹 두 번씩. 1차(`c49c80a9` 백엔드 — 헬퍼가 끝없는 `Skipped`로 opld 메모리를 채움, 64 KiB 넘는 zip 이름이 압축을 깸, 항목 수만 세던 메모리 한도, 폴더 범위 사용자의 Task 403, 느린 흐름 하나가 채널을 막음 → **흐름마다 받을 몫(규약 판 2)**, 부팅 중 덮개 지킴이, 열기·지우기·닫기 경쟁, 세션 목록 권한; `acec4a30` 웹 — 다음 쪽 실패의 끝없는 요청, 노드 일시 단절에 상태를 잃음, 엉뚱한 VM 미리 고름, `vm.config` 없는 VM, 큰 폴더, 숨은 글자 이름). 2차(`08742fb1` 백엔드 — 노드 사이 zip 끝 덩어리가 64 MiB gRPC 한도를 넘음, zip 장부가 이스케이프한 이름을 세지 않음, 건너뛴 폴더 목록의 제곱 비용, CI 재현성은 새 target 디렉터리로, 이미지 deb 게시 시험; `2d29e3be` 웹 — 404 아닌 4xx에 끝없이 묻기, 1000개 넘는 선택은 폴더째, 흔들리던 시험).
- svc01 일반 시험(`08742fb1`, Rocky 10 aarch64, rustc 1.100): fmt·clippy·test·starve·scale·storage-dir·storage-api 모두 통과, **4277개 통과·0 실패**. 스테이지 4~6의 `restore-it` E2E도 svc01 TCG에서 통과(41·72초).
- GitHub 탐침: 세 번. 첫째(`f561252a`)에서 **실제 버그** — opld 시작의 남은 도우미 치우기가 같은 호스트의 다른 opld 도우미(scope)까지 멈춤 → 자기 디렉터리의 세션만(리뷰 1 커밋), 그리고 root에서 도는 단위 시험의 `run_as`. 둘째에서 덮개 지킴이 시험(부팅 중에도 지킴이가 돌아 1바이트 한도가 부팅에서 걸림 — 시험·문구 고침). 마지막(`b8a43a2f`) **12개 작업 모두 통과** — `file-restore`(x86_64 이미지 두 번 빌드 같은 해시, KVM 부팅 1.7~2.1초·481 MB/s, 강제 TCG 8.4~8.9초·35 MB/s, btrfs·ntfs·exfat 포함 17 볼륨, Uid 65534·Seccomp 2·scope), `package`(이미지 deb 빌드·Debian 13 설치), linux-kvm·ldap·ceph·ovn·backup-s3·sdn-multinode·install-media·iso-e2e 둘·catalog-links.
- 배포: (배포 후 채움)

## 다음 계획으로 넘길 사항

- **로드맵 다음**: **계획 70 — 2.5D 클러스터 보기**(3D처럼 보이는 2D, 아이소메트릭 SVG/CSS).
- 카탈로그(백업 뒤 색인 Task)·이름 검색·백업 사이 차이, LUKS·BitLocker·ZFS, 실행 중 VM·스냅샷 찾아보기, ISO로 꽂아 되돌리기, 권한·소유자 되돌리기, 빠른 부팅(microvm·따뜻한 도우미), arm64 deb·RPM.
