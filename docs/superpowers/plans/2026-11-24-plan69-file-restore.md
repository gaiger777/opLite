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

- [ ] **Task 1: 파일 복원 타입 — 세션·게스트 되돌려 쓰기 TaskOp, 경로·볼륨·항목 보기, 노드 file_restore 상태, 권한 backup.file_restore, 기능 file-restore(opl-types·opl-cluster)** — `file_restore.rs`(`EntryPath`·`PathPart`·`VolumeView`·`EntryView`·`ListPage`·`FileRestoreSessionView`·`GuestRestoreCaps`·`GuestConflict`), `TaskOp::FileRestoreSession`·`FileRestoreToGuest`, `NodeStatus.file_restore`, `Privilege::BackupFileRestore`(operator 제외), 순수 `guest_path_ok`·`guest_target_name`·`content_disposition`·`choose_session_node`, 역할·grant 쓰기의 기능 게이트, 임시 API 막기, serde 하위 호환, `schema.d.ts`.
- [ ] **Task 2: opl-restore — 통신 규약, 에이전트 몸통(Fs trait·탐지 해석·마운트 옵션·나열·읽기·걷기), zip·tar.gz 흘려 쓰기, 에이전트 바이너리(opl-restore)** — 새 crate, `flate2`(`rust_backend`)와 `deny.toml`, 프레임 해석기 proptest, blkid·lvm 고정물 해석, 호스트 임시 디렉터리 위 몸통 시험(심볼릭 링크 탈출 막기·동시 흐름·취소), `storage-it`에서 `unzip -t`·`bsdtar -tf`로 견줌, musl 빌드에 C 빌드 스크립트 없음 검사.
- [ ] **Task 3: 도우미 이미지 빌더와 deb, 고정물 디스크 스크립트(packaging·ci)** — `packaging/restore-image/{build.sh,debian.lock}`(snapshot.debian.org·SHA-256, ELF `NEEDED` 검사, 재현), `orbivisor-restore-image` deb(`Depends: orbivisor (= 판)`, `orbivisor`의 `Recommends`), `packaging/tests/restore-image-test.sh`, `crates/opl-restore/tests/fixtures/make-disks.sh`(루트 없이, LVM만 root), `package` 작업, 커널 설정 탐침을 §18에.
- [ ] **Task 4: 도우미 VM 띄우기 — 명령줄·덮개·NBD/qcow2 원본·scope·권한 내림·sandbox, 에이전트 채널 클라이언트, 남은 것 치우기, 이미지 검사와 노드 알림(opl-qemu·opl-driver·opld)** — `build_restore_helper_cmdline`, `opl-driver/src/file_restore.rs`(`start_helper`·`HelperClient`·`stop_helper`·`sweep_leftovers`·덮개 지킴이), 가짜 하이퍼바이저의 가짜 도우미, `[file_restore]` 설정·이미지 해시 검사·`NodeStatus.file_restore`, `restore-it` 기능, `qemu-it` 탐침(부팅 시간·MB/s·저널 재생·`nouuid`·LVM thin·uid·sandbox)을 §18에.
- [ ] **Task 5: 파일 복원 세션 API — 세션 Task·노드 고르기·유휴/최대 시간, 나열·내려받기 토큰과 zip/tar.gz 흐름, 노드 사이 FileRestore RPC, 백업 지우기·보존과의 관계, 감사, oplctl backup files(opl-api·opl-peer·oplctl·opld)** — `file_restore.rs`·`tasks/file_restore.rs`, `FileRestoreCall`·`FileRestoreDownload`, `backup_in_use`·보존 건너뜀, 경로표·오류·OpenAPI·`schema.d.ts`, CLI `open|volumes|ls|get|sessions|close`, 권한 행렬(`insta`), opld E2E(암호화 저장소·풀 증분 체인·도우미 `kill -9`·opld 다시 시작).
- [ ] **Task 6: 게스트 에이전트로 되돌려 쓰기와 GitHub file-restore 작업(opl-qemu·opl-driver·opl-peer·opl-api·oplctl·ci)** — QGA `file_open`·`file_write`·`file_flush`·`file_close`·`exec`·`exec_status`·`supported_commands`, 하이퍼바이저 `guest_file_*`(잠금은 쓰기마다), `GuestAgentOp` 새 갈래, `FileRestoreToGuest` Task(keep·skip·overwrite·한도·취소), `GET /vms/{key}/guest/restore-caps`, CLI `backup files restore`, 이미지 빌더 `--test-guest`와 실제 QGA E2E, `.github/workflows/ci.yml`의 `file-restore`, 측정값을 §18에.
- [ ] **Task 7: 파일 찾아보기 화면 — 볼륨 나무·파일 표·부팅 상태·내려받기·VM으로 복원 대화상자, 백업 표 동작(web)** — `web/src/fileRestore/`(늦게 읽음), `BackupTable.tsx`·`VmBackups.tsx` 동작, 경로 `/backups/:id/files`, i18n en·ko, vitest·axe·Playwright, 번들 크기.
- [ ] **Task 8: 백업 가이드의 파일 단위 복원·도우미 이미지 설치·보안 모형·RHEL 에이전트 설정, 아키텍처, 설계 문서 스테이지 보고, 계획 69 실행 기록(docs)** — `docs/guide/backup.md`(지원 파일시스템 표, 안 되는 것, PBS·Veeam 견주기, "계획 69가 아직 하지 않는 것"·한계), `docs/architecture.md`, `web-ui.md`·`development.md`, `docs/index.md`, README 한 줄, 이 문서.

## 완료 기준 (계획 69)

- 옛 JSON(`TaskOp`·`NodeStatus`·역할·grant)이 바이트 그대로이고, 백업 형식·매니페스트·VM 명령줄이 계획 68과 같다. 업그레이드 중 새 쓰기는 409 `cluster_upgrading`, 옛 노드는 `backup.file_restore`를 버린다.
  - **결과:** (채움)
- 저장소 백업(평문·암호화, 로컬·S3)·옛 풀 백업(증분 체인 포함)을 열어 MBR·GPT·LVM(선형·thin) 위의 ext4·XFS·Btrfs·NTFS·FAT·exFAT 고정물 파일을 나열하고, 내려받은 파일·zip·tar.gz를 풀면 원본과 SHA-256이 같다. 더러운 저널은 재생된 내용을 보이고, LUKS는 "지원 안 함"으로 보인다. 백업 데이터는 세션 앞뒤로 바이트 그대로다.
  - **결과:** (채움)
- 도우미 VM은 네트워크 장치가 없고, Linux에서 QEMU가 `nobody`로 돌며(seccomp QEMU면 sandbox), 세션을 닫거나 도우미·opld가 죽으면 scope·덮개·소켓이 남지 않는다. 프레임 해석기는 무작위 입력에 패닉하지 않고 한도를 넘는 답은 세션을 닫는다.
  - **결과:** (채움)
- 세션은 같은 사용자·백업이면 다시 쓰이고, 노드 한도·유휴·최대 시간·덮개 한도가 지켜지며, 세션 노드가 아닌 API 노드에서도 나열·내려받기가 되고 클라이언트가 끊으면 에이전트 흐름이 취소된다. 열린 세션의 백업 지우기는 409 `backup_in_use`.
  - **결과:** (채움)
- 게스트 에이전트로 되돌려 쓰기가 실제 QGA에 keep·skip·overwrite로 파일·디렉터리를 쓰고, 에이전트가 막은 명령은 409 `guest_agent_restricted`, `exec` 없는 디렉터리·덮어쓰기는 400, 취소가 임시 파일을 지운다.
  - **결과:** (채움)
- API: 권한 행렬(`backup.file_restore` 없음 403, operator 기본 403, 되돌려 쓰기는 `vm.config`도), 토큰(만료·두 번째·다른 사용자), 감사 문구(경로·바이트·항목 수), 409들(`file_restore_unavailable`·`file_restore_busy`·`session_not_ready`·`volume_not_mountable`).
  - **결과:** (채움)
- 웹: 볼륨 나무·파일 표·부팅 칩·내려받기·VM으로 복원 대화상자가 vitest로, 찾아보기 → 파일 내려받기가 브라우저 E2E로 보인다.
  - **결과:** (채움)
- 부팅 시간(`Hello`까지)과 흐름 MB/s를 이 Mac HVF·CI KVM·CI 강제 TCG·svc01 TCG에서 재 §18에 적는다.
  - **결과:** (채움)
- `cargo fmt`·`clippy -D warnings`·`cargo test --workspace`, `pnpm typecheck`·`lint`·`test`, 로컬 브라우저 E2E가 통과하고 svc01 일반 시험과 GitHub `file-restore`·`package`·`linux-kvm`을 포함한 모든 시스템 작업이 통과한다.
  - **결과:** (채움)

## 알고 받아들인 제한

- **부팅 시간:** (채움 — TCG 노드의 첫 세션 대기, 세션 다시 쓰기·유휴 15분으로 덜어 냄)
- **TCG 대역폭:** (채움 — virtio-serial이 CPU에 묶임, svc01 측정값)
- **파일시스템:** (채움 — LUKS·BitLocker·ZFS·ReFS·mdraid·동적 디스크 없음, 겹친 LVM VG UUID)
- **게스트로 되돌려 쓰기:** (채움 — 권한·소유자·ACL 없음, RHEL·Rocky 에이전트 기본 막힘, `exec` 없으면 파일만, Windows는 손 확인)
- **zip:** (채움 — 심볼릭 링크·장치 빠짐(보고서 항목), 읽기 오류 항목)
- **이미지 배포:** (채움 — deb는 amd64만, Rocky·macOS는 빌드 스크립트)
- **macOS 노드:** (채움 — 권한 내림·sandbox 없음, 개발용)

## 실행 후 기록

- 리뷰: (채움)
- svc01 일반 시험: (채움)
- GitHub 탐침: (채움)
- 배포: (배포 후 채움)

## 다음 계획으로 넘길 사항

- **로드맵 다음**: **계획 70 — 2.5D 클러스터 보기**(3D처럼 보이는 2D, 아이소메트릭 SVG/CSS).
- 카탈로그(백업 뒤 색인 Task)·이름 검색·백업 사이 차이, LUKS·BitLocker·ZFS, 실행 중 VM·스냅샷 찾아보기, ISO로 꽂아 되돌리기, 권한·소유자 되돌리기, 빠른 부팅(microvm·따뜻한 도우미), arm64 deb·RPM.
