# Orbivisor 계획 58: 템플릿·링크드 클론·이미지 라이브러리 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 꺼진 VM을 켤 수 없는 템플릿으로 바꾸고, 템플릿에서 기반 디스크를 함께 쓰는 연결 복제(링크드 클론)나 전체 복사로 이름·노드·CPU·메모리·NIC·디스크 크기·cloud-init을 바꿔 여러 VM을 배포하며, 연결 복제를 평탄화하고, 내장·관리자 이미지 카탈로그에서 클라우드 이미지를 체크섬 파일로 확인해 받아 한 번에 템플릿으로 만든다.

**Architecture:**
- **템플릿은 VM의 상태:** 새 Kind 대신 `VmSpec.template`. 바꾸기는 동기 Raft 쓰기, 되돌리기는 Task `vm-untemplate`(블록 백엔드의 기반 스냅샷 정리). 템플릿인 동안 켜기·마이그레이션·스냅샷·디스크 목록 바꾸기를 409 `vm_is_template`로 막고, reconciler·스케줄러·HA·비우기가 템플릿을 켜거나 옮기지 않는다.
- **연결 복제와 의존 기록:** `VolumeSpec.base`(기반 볼륨 id, 같은 풀, 사슬 1단)를 저장소 작업 전에 기록하고, 기반 지우기를 API(409 `volume_has_clones`)·지우기 Task 재확인·백엔드(ZFS destroy·RBD unprotect)에서 막는다. 백엔드별 `Storage::link_clone`·`release_base`·`flatten_volume` — dir·NFS는 상대 경로 qcow2 backing, LVM-thin은 thin 스냅샷, ZFS는 `@opl-base` clone, RBD는 보호한 `opl-base` clone, 공유 LVM은 전체 복사로 대체.
- **배포는 `vm clone`의 확장:** `POST /vms/{key}/clone`에 `linked`·덮어쓰기·`start`를 더하고, 대량 배포는 CLI·웹이 단건 API를 되풀이한다. 평탄화는 Task `volume-flatten`.
- **이미지 라이브러리:** 내장 `BUILTIN`(7개 배포판 × 2 아키텍처) + `SettingsSpec.image_catalog` 관리자 항목. `volume-download`에 SHA-512·체크섬 파일 단계(`checksum`)와 템플릿 초안을 더해 받기 → 템플릿을 한 Task로.

**Tech Stack:** Rust 2024(MSRV 1.88), `qemu-img create -b`·`rebase`, `lvcreate -s`, `zfs snapshot|clone|send|recv`, `rbd snap protect|clone|flatten`, reqwest 받기, axum·utoipa, React. 새로 받는 크레이트는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-17-templates-linked-clones-design.md`
**선행 계획:** 계획 1~57 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| VM ↔ 템플릿 바꾸기, 템플릿 불변 규칙, 템플릿 지우기(되돌린 뒤) | 템플릿 옮기기(노드·풀) |
| 템플릿에서 연결 복제(같은 풀)·전체 복사, 백엔드별 네이티브 복제 | 연결 복제의 연결 복제, 다른 풀·다른 노드의 연결 복제(기반 캐시) |
| 의존 기록(`VolumeSpec.base`)과 기반 지우기 거절 | 실행 중 평탄화(`block-stream`) |
| 배포 덮어쓰기(이름·노드·풀·CPU·메모리·NIC·디스크 크기·cloud-init·켜기), 클라이언트 대량 배포 | 서버 쪽 대량 배포·IP 풀, 배포 때 NIC 더하기·빼기 |
| 꺼진 VM 디스크의 평탄화 Task | 연결 복제 VM의 로컬 디스크 복제(먼저 평탄화) |
| 이미지 카탈로그(내장 + 관리자), 체크섬 파일·SHA-512 확인, 받기 → 템플릿 | 카탈로그 원격 피드, GPG 서명, OVF 내보내기, Windows sysprep |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 변경과 단위·통합 시험, `storage-it`(상대 backing·rebase 평탄화), `qemu-it`(겹층 디스크로 QEMU 켜기, opld `templates` 게스트 없는 흐름), `migration-it`(로컬 dir 연결 복제 마이그레이션), 카탈로그 링크 확인, 웹 시험.
- **배포 서버 svc01(Linux aarch64):** 같은 일반 시험 묶음.
- **GitHub(Ubuntu 24.04 x86_64):** `linux-kvm`(LVM-thin·ZFS·RBD·공유 LVM·NFS의 연결 복제 흐름과 마이그레이션), `install-media` 작업의 실제 게스트 단계(Ubuntu 클라우드 이미지 템플릿 → 연결 복제 2대 → 부팅 → 평탄화), 그 밖에 ceph·ldap·package, 새 `catalog.yml`(`catalog-links`).
- **테스트 VM:** 두 노드에 배포한 뒤 `templates` 기능과 화면만 확인한다.

## 설계 결정

- **옛 노드가 읽을 수 있는 것만 쓴다:** `template: true` VM, `base`가 든 볼륨, `vm-clone`의 `linked`·`overrides`, `volume-download`의 `sha512`·`checksum_url`·`template`, `volume-flatten`·`vm-untemplate`, 관리자 카탈로그 항목은 기능 `templates`가 가린다(경로마다, 그리고 `create_task`가 `TaskOp::needs_templates()`로 다시). 새 칸은 기본값이면 쓰지 않아 기존 JSON과 새 옵션 없는 `vm clone` Task는 바이트까지 같다.
- **기반 스냅샷은 첫 연결 복제가 만든다:** 바꾸기는 Raft 쓰기 하나이고, 블록 백엔드 작업은 노드에서 도는 배포 Task가 멱등으로 한다. 되돌리기·지우기만 Task로 정리해 낡은 `opl-base`가 남지 않게 한다.
- **의존은 Raft `base` + 백엔드 막이:** API가 바로 409를 주고, 기록이 어긋나도 백엔드가 데이터를 지킨다. `base`는 배포 Task만 쓴다.
- **qcow2 backing은 상대 경로:** NFS 마운트 지점이 노드마다 달라도 열린다(기반과 복제는 늘 같은 풀).
- **새 `/deploy` 대신 `clone` 넓히기, 서버 쪽 일괄 Task 대신 클라이언트 반복:** 같은 Task·정리·감사·경고를 쓴다.
- **내장 항목은 고정 해시 대신 같은 디렉터리의 체크섬 파일:** "release·latest" 이미지가 바뀌어도 낡지 않고, 주 1회 링크 확인이 URL 변화를 알린다.
- **ZFS 평탄화는 `promote` 대신 send/recv:** 의존 방향을 뒤집지 않는다.

## Task

각 Task는 하나의 커밋이며, 앞 Task의 커밋 위에서 순서대로 한다. 단계마다 `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 바뀐 크레이트의 시험을 통과한 뒤 커밋한다. 앞 스테이지의 새 Task 실행기는 `"this node cannot … yet"`으로 실패하고 API는 Task 5에서 연다.

- [x] **Task 1: 템플릿·연결 복제 타입·검증·기능과 이미지 카탈로그·체크섬 해석(opl-types)** — `VmSpec.template`, `VolumeSpec.base`와 `check_base`(`BaseInOtherPool`·`BaseIsLinkedClone`·`BaseNotDisk`), `BASE_TAG = "opl-base"`, `pool_supports_linked_clones`, 기능 `templates`, `TaskOp::VmClone{linked, overrides}`·`VolumeDownload{sha512, checksum_url, template}`·`VolumeFlatten`·`VmUntemplate`와 `needs_templates()`, `template.rs`(`CloneOverrides`·`apply_overrides`·`grown_disks`·`TemplateError`·`TemplateDraft`), `image_catalog.rs`(`CatalogImage`·`Checksum`·실제 서버로 확인한 `BUILTIN` 14줄·`(id, arch)` 키·`validate_custom`·`parse_checksums` — GNU·BSD·clearsign·해시 하나뿐인 Alpine 파일), `SettingsSpec.image_catalog`, serde 하위 호환 시험.
- [x] **Task 2: 백엔드별 연결 복제·기반 스냅샷·평탄화(opl-driver)** — `Storage::link_clone`·`release_base`(기본 성공)·`flatten_volume`, dir·NFS(상대 backing, 안전 모드 `rebase`, 내부 스냅샷이 있으면 평탄화 거절), LVM-thin(thin 스냅샷 켜기), ZFS(`@opl-base` clone, `@opl-flatten` 기준 send/recv로 스냅샷까지 옮기고 rename, 재개), RBD(`protected` 읽기, protect·clone·flatten, `delete_volume`의 unprotect), `Storages` 나눔과 모의 저장소, `storage-it`·`lvm-it`·`zfs-it`·`rbd-it`·`lvm-shared-it` 시험, HMP `info block`으로 겹층 디스크를 여는 실제 QEMU 시험.
- [x] **Task 3: 템플릿을 켜지 않음, 연결 배포·평탄화·되돌리기 Task(opl-reconciler·opl-api)** — reconciler 방어(`TEMPLATE_NOT_STARTED`), `ha_active()`·`node_fits_with`·균형 조정기·비우기에서 템플릿 빼기, `tasks/clone.rs`의 연결 갈래(`unlinkable` 판정과 대체 경고)·덮어쓰기·디스크 늘리기·`start` 배치, `volume-flatten`·`vm-untemplate` 실행기, `VolumeDelete` 재확인, 로컬 디스크 마이그레이션 확정(`move_volume`)의 `base: None`, 복원·받기·복제 확인의 `drop_templates`.
- [x] **Task 4: 체크섬 파일 단계와 받기 → 템플릿(opl-api)** — `tasks/download.rs`의 `checksum` 단계(같은 클라이언트·SSRF 규칙, 1 MiB 상한)와 SHA-512, 진행률 단계 `template`, 템플릿 초안으로 크기 늘리기와 템플릿 VM 만들기, 템플릿 단계 실패·재시작 때 받은 볼륨 남기기(`keeps_downloaded`), `downloads.rs` 시험.
- [x] **Task 5: 템플릿·배포·평탄화·이미지 카탈로그 API, 지우기 거절, 가림(opl-api)** — `POST|DELETE /vms/{key}/template`, `POST /vms/{key}/clone`의 새 칸과 미리 검사(노드·아키텍처·풀 닿음·512 배수), `POST /volumes/{key}/flatten`(파일 풀 스냅샷 409·공간 검사), `GET|POST /image-catalog`·`DELETE /image-catalog/{id}`·`POST /image-catalog/{id}/download`, 불변 규칙과 409 `vm_is_template`·`volume_is_template`·`template_in_use`·`volume_has_clones`, `PUT`이 `template`을 지키기, 복제 켜기 거절, 기능 게이트와 `create_task` 안전망, Task 비밀번호·URL 가림, 감사 설명, OpenAPI.
- [x] **Task 6: template·image·volume flatten CLI, opld 실제 시험과 CI(oplctl·opld·CI)** — `orbivisorctl template list|convert|unconvert|delete|deploy`(`--count`·`{n}`·`--linked|--full`·덮어쓰기·`--start`), `vm clone --linked|--full`, `image list|get(download)|catalog add|remove`, `volume flatten`, `volume download --sha512|--checksum-url`, `vm list`·`vm get`·`volume list`의 표시, opld `templates.rs`(게스트 없는 흐름 + 기능 `templates-it`의 `mod guest`), 풀마다 `linked_clone_flow`, `migration-it`·`nfs-it`의 연결 복제 마이그레이션, `install-media` 작업 단계, `catalog.yml`과 oplctl `catalog-it`.
- [x] **Task 7: 템플릿 화면·배포 창, 이미지 라이브러리, VM·볼륨 화면의 템플릿·기반·평탄화(web)** — 메뉴 "템플릿"(`/templates`)과 배포 창(개수·`{n}` 미리 보기, 디스크 복제 방식 세 값과 디스크별 가능 여부, `CloudInitSection` 재사용, VM마다 Task 알림), VM 목록·상세의 템플릿으로 바꾸기·되돌리기(이유)·"템플릿만 보기" 칩, 볼륨 화면의 "기반" 열·평탄화, 메뉴 "이미지 라이브러리"(`/images`, 아키텍처 거름, 받기·받은 뒤 템플릿 만들기, 진행률, 관리자 항목), 번역.
- [x] **Task 8: README와 설계 문서, 계획 문서** — README "템플릿과 링크드 클론"·"이미지 라이브러리"와 개발 시험 명령, 설계 문서 `2026-11-17-templates-linked-clones-design.md`의 스테이지 보고·리뷰 기록(§15), 이 계획 문서와 실행 기록.

## 완료 기준 (계획 58)

- `orbivisorctl template convert`로 꺼진 VM을 템플릿으로 바꾸고, 템플릿을 켜거나 옮기거나 디스크를 지우려 하면 409이며, `template unconvert`가 연결 복제가 남았으면 409 `template_in_use`, 없으면 보통 VM으로 되돌린다.
- `template deploy --linked`가 dir·NFS·LVM-thin·ZFS·RBD 풀에서 연결 복제를, 공유 LVM에서 전체 복사와 경고를 만들고, 배포한 VM이 켜지며, 기반 지우기가 409이고, 끈 뒤 `volume flatten`하면 독립 볼륨이 된다(이 Mac `qemu-it`, GitHub `linux-kvm`).
- 로컬 풀 연결 복제를 다른 노드로 마이그레이션하면 독립 볼륨이 되고, NFS 풀 연결 복제는 사슬째 옮겨진다.
- `image get --template`이 체크섬 파일로 확인해 받은 Ubuntu 24.04 클라우드 이미지로 템플릿을 만들고, 연결 복제 2대가 cloud-init(호스트 이름·사용자·늘어난 루트)으로 부팅해 보고하며, 평탄화 뒤 다시 켜진다(GitHub `install-media` 작업).
- 웹에서 템플릿 화면·배포 창, 이미지 라이브러리, VM·볼륨 화면의 템플릿·기반·평탄화를 쓸 수 있다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, 웹 시험이 이 Mac과 svc01에서, linux-kvm·install-media·ceph·ldap·package·catalog-links가 GitHub에서 통과한다.

## 알고 받아들인 제한

- 평탄화는 꺼진 VM만, 진행률은 단계만이다. 파일 풀 연결 복제는 VM 스냅샷이 있으면 평탄화하지 못한다(`qemu-img rebase`가 내부 스냅샷의 빈 클러스터를 0으로 만듦).
- RBD는 평탄화한 복제가 평탄화 전에 찍은 스냅샷이 남은 동안 템플릿의 기반 보호를 풀지 못한다(되돌리기 Task가 안내 문구와 함께 실패, 데이터는 안전).
- 로컬 풀 템플릿은 그 노드에서만 연결 배포한다(다른 노드는 배포 뒤 마이그레이션 — 그러면 독립 볼륨).
- 체크섬 파일은 손상·미러 오류만 잡고 배포처 변조는 못 잡는다(GPG 없음). 배포처가 이미지와 SUMS를 바꾸는 사이에 받으면 실패한다.
- Fedora·Alpine 내장 항목은 판 번호가 URL에 있어 Orbivisor 판마다 고친다(주 1회 링크 확인이 알림).

## 실행 후 기록 (2026-11-18)

- 8개 Task를 단계 커밋으로 구현했고, 리뷰를 두 번 받아 고친 내용은 각 단계 커밋에 합쳤다(설계 문서 §15).
  - **리뷰 1:** 노드 사이 되돌리기와 배포의 엇갈림(둘 다 Raft에 쓰고 다시 보아 뒤에 쓴 쪽이 물러남, 배포·되돌리기 Task 노드를 `template_node`로 통일), 평탄화 중 켜기·핫플러그·마이그레이션 막기(`VolumeFlattening`, 409 `vm_busy`)와 ZFS 평탄화의 `written@opl-flatten` 확인, `POST /volumes`의 `base` 거절, 템플릿·연결 복제 VM 백업의 비트맵 만들지 않기, 카탈로그 템플릿 초안의 호스트 이름·정적 IP 거절, RBD 기반 풀기 실패 안내 문구, 바꾸기 API의 바쁨 검사·스냅샷 되돌리기의 템플릿 검사·권한 없는 사용자에게 덮어쓰기의 `user_data`·`vendor_data` 가림·ZFS `-flat` 정리·지우기 Task의 템플릿 디스크 재확인.
  - **리뷰 2:** 지난 노드가 빠졌거나 꺼진 템플릿의 Task 노드 고르기와 되돌리기의 `release_base` 건너뛰기(웹 배포 창도 같은 규칙), 되돌리기의 표시 되돌리기를 스펙이 같을 때만 하기(바뀌었으면 보통 VM으로 두고 안내), 끊긴·이미 보통 VM인 되돌리기 Task의 남은 복제 보고, 끝나지 않은 `vm-untemplate` 동안 켜기·PUT·CD·바꾸기·지우기 409 `vm_busy`, `vm start`의 템플릿 재확인, 카탈로그 URL의 사용자 정보 거절·비관리자 가림·`url` 크레이트 해석·id·칸 길이 상한.
- 이 Mac에서 `cargo test --workspace` 2912개, 웹 시험 781개가 통과했다.
- svc01에서 일반 시험 묶음이 통과했다(시험 2988개). Linux에서만 나는 시험 하나가 한 번 흔들려 뒤에 고쳤다.
- GitHub 탐침에서 linux-kvm(LVM-thin·ZFS·RBD·공유 LVM·NFS의 연결 복제 흐름과 마이그레이션), install-media(실제 Ubuntu 24.04 게스트: 템플릿 → 연결 복제 2대 → 부팅·보고 → 평탄화), ceph·ldap·package·catalog-links가 통과했다. iso-e2e uefi는 한 번 Debian netinst 설치 중 시간 초과가 났다 — 이 계획과 상관없다.
- 배포: (배포 후 채움)

## 다음 계획으로 넘길 사항

- 실행 중 평탄화(`block-stream`, RBD 온라인 `rbd flatten`)와 평탄화 진행률.
- 노드마다 기반 사본을 두는 "기반 캐시"로 로컬 풀의 다른 노드 연결 배포, 템플릿 옮기기(노드·풀), 연결 복제 사슬.
- 연결 복제 VM의 로컬 디스크 복제(계획 55), RBD 평탄화 뒤 남는 스냅샷의 부모 참조를 Raft에 기록해 미리 409.
- 카탈로그 원격 피드·원격 클러스터 사이 템플릿 동기화(콘텐츠 라이브러리), 체크섬 파일의 GPG 서명 확인.
- 서버 쪽 대량 배포와 IP 풀, 배포 때 NIC 더하기·빼기, OVF/OVA 템플릿 내보내기, Windows sysprep·cloudbase-init 템플릿 안내.
- 격차 분석 다음 항목(계획 59~).
