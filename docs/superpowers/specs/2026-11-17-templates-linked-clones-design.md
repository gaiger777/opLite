# 템플릿·연결 복제·이미지 라이브러리 설계 (계획 58)

작성일: 2026-11-17
상위 문서: `2026-11-17-install-media-cloud-init-design.md`(계획 57 — `volume-download`·cloud-init·`identity_warning`), `2026-10-20-opl-node-core-design.md`(VM 스펙·볼륨·드라이버), `2026-10-26-snapshots-design.md`(§5 복제 — `VmClone`), `2026-10-28-shared-storage-nfs-design.md`, `2026-10-30-live-migration-design.md`(로컬 디스크 마이그레이션 §3), `2026-11-04-backup-design.md`, `2026-11-11-availability-scale-design.md` §22(계획 55 로컬 디스크 복제)
기준 트리: 계획 57 끝(`fd9779c`). 이 문서는 구현 전 설계다 — 스테이지 보고는 구현하며 덧붙인다.

## 1. 목적

격차 분석의 둘째 항목은 "VM을 찍어 내는 길이 없다"이다. 계획 57로 클라우드 이미지를 받고 cloud-init으로 첫 설정을 할 수 있게 됐지만, VM 하나를 만들 때마다 이미지를 다시 받거나 `vm clone`(계획 11, `TaskOp::VmClone` → `tasks/clone.rs::clone`)으로 **디스크 전체를 복사**해야 한다. 템플릿이라는 개념(켤 수 없는 원본)도, 기반 디스크를 함께 쓰는 연결 복제도, 잘 알려진 이미지의 목록도 없다(`template`·`backing`·`linked` grep — 백업 대상의 `"backing": null`과 복제 겹층 `create_overlay`뿐).

| | Proxmox VE 8 | vSphere 8 | Orbivisor 계획 58 |
|---|---|---|---|
| 템플릿 | "Convert to template"(켤 수 없음, 되돌리기 없음) | VM ↔ 템플릿 바꾸기 | `VmSpec.template`, 바꾸기·되돌리기(연결 복제가 없을 때) (§3) |
| 연결 복제 | 템플릿에서만, 같은 저장소(qcow2 backing, RBD·ZFS·LVM-thin 네이티브) | Instant/linked clone(델타 디스크) | 템플릿에서만, 같은 풀, qcow2 backing·RBD·ZFS·LVM-thin, 안 되면 전체 복사 (§4·§5) |
| 배포 때 바꾸기 | 이름·노드·저장소, 나머지는 배포 뒤 | 사용자 지정 명세 | 이름·노드·풀·vCPU·메모리·NIC 네트워크·디스크 크기·cloud-init·켜기 (§6) |
| 평탄화 | 전체 복제로만 | "Consolidate" | `volume-flatten` Task (§7) |
| 이미지 목록 | 없음(appliance 템플릿은 LXC) | 콘텐츠 라이브러리(구독) | 내장 카탈로그 + 관리자 항목, 받기 → 템플릿 한 번에 (§8) |

## 2. 범위

| 넣는 것 | 넣지 않는 것(§13) |
|---|---|
| VM ↔ 템플릿 바꾸기, 템플릿 불변 규칙 | 템플릿을 다른 노드·풀로 옮기기(마이그레이션·저장소 이동) |
| 템플릿에서 연결 복제(같은 풀)·전체 복제, 백엔드별 네이티브 복제 | 연결 복제의 연결 복제(사슬 2단 이상), 다른 풀·다른 노드의 연결 복제 |
| 의존 기록(`VolumeSpec.base`), 기반 지우기 거절 | 실행 중 평탄화(`block-stream`) |
| 배포 덮어쓰기(이름·노드·풀·CPU·메모리·NIC·디스크 크기·cloud-init·켜기) | 서버 쪽 대량 배포·IP 풀 — 대량 배포는 CLI·웹이 단건 API를 되풀이 |
| 꺼진 VM 디스크의 평탄화 Task | 연결 복제 디스크가 있는 VM의 로컬 디스크 복제(계획 55) — 먼저 평탄화 |
| 이미지 카탈로그(내장 + Raft의 관리자 항목), 체크섬 파일 확인(SHA-256·SHA-512) | 카탈로그 원격 갱신·구독, GPG 서명 확인, OVF 템플릿 내보내기 |
| 카탈로그 이미지 받기 → 템플릿 한 번에 | Windows sysprep 일반화 |

**기능 이름 `templates`**(`opl_types::node::TEMPLATES`, `FEATURES` 끝). 모든 노드가 알릴 때만 다음을 받고, 아니면 409 `cluster_upgrading`: `template: true`인 VM 스펙 쓰기, `base`가 있는 볼륨 만들기, `VmClone`의 새 필드(`linked`·`overrides`), `VolumeDownload`의 새 필드(`sha512`·`checksum_url`·`template`), 새 Task `volume-flatten`·`vm-untemplate`, 비지 않은 `SettingsSpec.image_catalog`. 옛 노드는 `VmSpec`·`VolumeSpec`·`TaskOp`·`SettingsSpec`이 `deny_unknown_fields`라 새 필드를 풀지 못한다. 새 필드는 모두 기본값이면 쓰지 않으므로(`skip_serializing_if`) 기존 VM·볼륨·Task·설정의 JSON은 그대로이고, 새 필드 없는 `vm clone`은 계획 57과 바이트까지 같은 Task를 만든다. 계획 57 S6처럼 기능 검사는 검증보다 먼저다.

## 3. 템플릿

- `VmSpec.template: bool`(`#[serde(default, skip_serializing_if = "is_false")]`). 새 Kind를 만들지 않는다 — 템플릿도 `GET /vms`·watch·RBAC·감사·백업이 그대로 다루는 VM이다. 목록 API는 따로 없고 CLI·웹이 `template`으로 거른다(계획 57의 ISO 목록과 같은 방식).
- **바꾸기** `POST /vms/{key}/template` → 200 VM(동기, Raft 쓰기 하나, `If-Match`). 조건: 꺼짐(`is_stopped`, 아니면 409 `vm_running`), 마이그레이션·스냅샷·복제·백업 Task 없음(`check_not_busy`·`check_no_snapshot_task`), 스냅샷 0개(409 `vm_has_snapshots` — 템플릿의 되돌리기가 기반을 바꾸면 안 됨), `ha` 없음·`replication` 비움·`snapshot_schedule` 없음(400, 지우고 다시), 디스크 볼륨이 모두 `ready`·`content: disk`·`base` 없음(400 `"volume … is a linked clone; flatten it before making a template"` — 사슬은 1단). 저장소에는 아무것도 하지 않는다(기반 스냅샷은 첫 연결 복제가 만든다 — §4).
- **불변 규칙**(`template: true` 동안): `power: running`·`actions/start|reboot`·`migrate`·`replicate`·스냅샷 만들기·`cdrom` 넣기 → 409 **`vm_is_template`** `"VM \"tmpl\" is a template; deploy a VM from it or convert it back"`. `PUT`은 디스크 목록(`disks[].volume`·`format`·`bus`·개수)을 바꾸면 409 `vm_is_template`, `ha`·`replication`·`snapshot_schedule`은 400. CPU·메모리·NIC·cloud-init·CD 같은 나머지는 바꿀 수 있다(배포가 그때의 스펙을 복사할 뿐 기반 데이터와 무관). 템플릿 디스크 볼륨의 `resize`·`DELETE` → 409 **`volume_is_template`** `"volume … is a disk of template \"tmpl\""`. 다른 VM에 붙이기는 기존 "볼륨은 VM 하나에만" 검사가 막는다.
- **되돌리기** `DELETE /vms/{key}/template` → 202 Task **`vm-untemplate`**(`TaskOp::VmUntemplate`, 대상 = VM, 노드 = `vm_node`). 연결 복제가 남았으면 API가 409 **`template_in_use`** `"template \"tmpl\" has 3 linked clone(s) (web-1, web-2, …); delete or flatten them first"`. Task는 줄에서 기다린 뒤 다시 확인하고, 디스크마다 `Storage::release_base`(RBD `opl-base` 풀기·지우기, ZFS `@opl-base` 지우기, 파일·LVM-thin은 할 일 없음)를 한 뒤 마지막에 `template: false`를 쓴다. 중간에 죽으면 플래그가 남아 있으므로 다시 하면 된다(스냅샷이 없는데 플래그가 있으면 다음 연결 복제가 다시 만든다 — 늘 현재 데이터).
- **템플릿 지우기** `DELETE /vms/{key}`는 템플릿이면 409 `vm_is_template` `"convert template … back to a VM before deleting it"`. 되돌리기 Task를 거치게 해서 블록 백엔드에 오래된 `opl-base`가 남지 않게 한다(남은 스냅샷을 나중에 다시 템플릿이 된 같은 볼륨이 기반으로 쓰면 옛 데이터가 된다). CLI `template delete`·웹 "삭제"는 되돌리기 Task를 기다린 뒤 지운다.
- reconciler(`opl-reconciler`)는 `template`이면 스펙과 상관없이 QEMU를 띄우지 않고(방어, 로그 경고), 스케줄러·균형 조정기·HA·비우기(`drain`)는 템플릿을 후보에서 뺀다.

## 4. 연결 복제의 기반과 의존 기록

- `VolumeSpec.base: Option<Uuid>`(`skip_serializing_if = "Option::is_none"`): 이 볼륨이 연결 복제면 기반(템플릿 디스크) 볼륨의 id. 이름이 아니라 id인 것은 다시 만든 같은 이름의 볼륨을 기반으로 오인하지 않으려고. 기반과 복제는 **늘 같은 풀**이다(`base`의 풀 = 자기 풀, 검증 `VolumeError::BaseInOtherPool`).
- 기반 스냅샷 태그 상수 `opl_types::storage::BASE_TAG = "opl-base"`. UUID가 아니므로 남은 블록 스냅샷 정리(`tasks.rs`의 `tag.parse::<Uuid>()` 거름)와 VM 스냅샷 목록이 건드리지 않는다.
- **기록이 먼저**: 복제 Task는 `base`를 담은 볼륨 리소스(`pending`)를 저장소 작업 **전에** 만든다(`clone.rs::made`의 기존 순서). 그래서 끊겨도 기반을 지울 수 없고, 남은 `pending` 복제는 기존 고아 정리가 지운다(복제 쪽만 지우므로 기반은 안전).
- **지우기 거절**: `DELETE /volumes/{key}`는 `base == 이 id`인 볼륨이 있으면 409 **`volume_has_clones`** `"volume \"tmpl-disk0\" is the base of 2 linked clone(s) (web-1-disk0, …); delete or flatten them first"`. 계획 57 ISO와 같이 `vm_lock` 아래에서 확인하고, `VolumeDelete` Task가 지우기 직전에 다시 본다(다른 노드의 복제 Task가 그새 기록을 만들었으면 같은 문구로 실패). 백엔드도 막는다(§5 "백엔드 막이") — Raft 기록이 어긋나도 데이터를 잃지 않게.
- **기록이 풀리는 때**: 평탄화(§7), 그리고 디스크를 새 자리에 쓰는 모든 길(로컬 디스크 마이그레이션 확정, 백업 복원, 전체 복제)은 전체 데이터를 쓰므로 새 기록에 `base`가 없다(§9).
- 순수 함수 `opl_types::storage::pool_supports_linked_clones(backend, format) -> bool`: dir·NFS(원본 qcow2·raw 모두), LVM-thin·ZFS·RBD(raw)는 참, `LvmShared`는 거짓.

## 5. 백엔드별 연결 복제

새 `Storage` 트레이트 메서드(기본은 `InvalidSpec("this storage cannot make linked clones")`, `Storages`가 백엔드로 나눔, `MockStorage`·`call` 로그 시험):

- `link_clone(pool, base_id, base_format, clone_id) -> Result<(PathBuf, ImageFormat), _>` — 기반 스냅샷이 필요하면 없을 때만 만들고(멱등), 복제를 만든다. 대상이 이미 있으면 `VolumeExists`, 실패하면 만들다 만 것을 남기지 않는다(`copy_volume`과 같은 약속).
- `release_base(pool, id, format)` — 기반 스냅샷을 푼다. 자식이 있으면 실패, 없으면 성공(다시 해도 됨).
- `flatten_volume(pool, id, format)` — 기반과의 연결을 끊는다. 이미 독립이면 성공(멱등). 볼륨을 쓰는 QEMU가 없어야 한다.

| 백엔드 | 기반 준비 | 연결 복제(형식) | 평탄화 | 백엔드 막이 |
|---|---|---|---|---|
| dir·NFS | 없음 | `qemu-img create -f qcow2 -b {base-id}.{fmt} -F {fmt} {clone-id}.qcow2`(**상대 경로** — NFS 마운트 지점 `{mount_root}/{풀 id}`가 노드마다 달라도 열림), qcow2 | `qemu-img rebase -f qcow2 -b ""`(안전 모드: 다른 클러스터를 복사한 뒤 헤더를 마지막에 고침 — 끊겨도 옛 사슬 그대로) | 없음 → Raft 검사와 `VolumeDelete` 재확인 |
| LVM-thin | 없음(기반 LV는 쓰이지 않음) | `lvcreate -s -n opl-{clone} vg/opl-{base}`(기존 `LvmOps::snapshot`, 활성화 건너뛰기는 기존 `-ay -K`), raw | 할 일 없음(thin 스냅샷은 원본과 독립) | 없음(지워도 안전하지만 같은 규칙으로 거절) |
| ZFS | `zfs snapshot {ds}/opl-{base}@opl-base` | `zfs clone {ds}/opl-{base}@opl-base {ds}/opl-{clone}`, raw | `zfs send … \| zfs recv {ds}/opl-{clone}-flat` → 원본 destroy → rename(남은 `-flat`은 다시 할 때 지우고 처음부터; 원본이 없고 `-flat`만 있으면 rename만) | `zfs destroy`가 자식 clone이 있으면 실패 |
| RBD | `rbd snap create` + `rbd snap protect opl-{base}@opl-base` | `rbd clone …@opl-base opl-{clone}`(clone v1·v2 모두 되는 protect 방식), raw | `rbd flatten`(멱등) | `snap unprotect`가 `rbd children`이 있으면 실패; `delete_volume`은 `snap purge` 전에 `opl-base`를 푼다(지금은 protect된 스냅샷에서 purge가 실패) |
| 공유 LVM(iSCSI·FC) | — | 지원 안 함 → 전체 복사 | — | — |

- **QEMU 쪽은 바뀌지 않는다**: 디스크는 지금처럼 `-blockdev driver=qcow2,file=disk{i}-file`이고 `backing`을 적지 않으므로 QEMU가 헤더의 backing을 읽기 전용으로 연다. RBD clone·ZFS clone·thin 스냅샷은 보통 볼륨처럼 열린다. 실제 QEMU 시험이 이를 단언한다(§11).
- 기존 `check_import_source`(올린 qcow2의 backing 거절)는 그대로다 — backing 헤더는 Orbivisor가 만든 파일에만 있다.

## 6. 템플릿에서 배포

- 새 경로를 만들지 않고 **`POST /vms/{key}/clone`을 넓힌다**. `CloneRequest`(`snapshots.rs`)에 더하는 것: `linked: Option<bool>`, `node`, `vcpus`, `memory_mib`, `nics: Vec<Option<String>>`(NIC 순번별 네트워크, null은 그대로), `disks: Vec<Option<u64>>`(순번별 새 크기, 늘리기만 — 줄이면 400), `cloud_init: Option<CloudInit>`(주어진·비지 않은 필드만 바꿈), `start: bool`. 템플릿이 아닌 VM에도 덮어쓰기는 되지만 `linked: true`는 템플릿만(400 `"linked clones can be made only from templates"`). 템플릿에서 스냅샷 복제는 없다(템플릿은 스냅샷이 없음).
- `linked`: 없음 = 템플릿이면 **되는 디스크는 연결, 안 되는 디스크는 전체 복사**하고 디스크마다 Task 경고 `"disk1: pool \"san\" (lvm-shared) cannot hold linked clones; made a full copy"`; `true` = 하나라도 안 되면 400(`"disk0: …"` — 다른 풀(`pool`이 기반 풀과 다름), 백엔드 미지원); `false` = 전체 복사(계획 11 그대로).
- `TaskOp::VmClone`에 `linked: Option<bool>`·`overrides: Option<Box<CloneOverrides>>`를 더한다(둘 다 없으면 생략). `CloneOverrides`와 순수 함수 `apply_overrides(spec, &ov) -> Result<VmSpec, VmError>`은 새 `opl-types/src/template.rs`에 둔다 — API가 미리 적용해 `prepare`와 같은 검사(vCPU·메모리 상한, 네트워크 있음, `check_security_groups`, `CloudInit::validate(nics)`)를 하고, Task는 실행 때 다시 적용한다. cloud-init 비밀번호는 API가 계획 57처럼 SHA-512 crypt로 바꿔 Task에 넣고, Task를 내보내는 모든 길(`volumes::public_task`·`redact_task_json`)이 `overrides.cloud_init.password`를 `********`로 가린다.
- **노드**: Task는 지금처럼 `vm_node(템플릿)`에서 돈다. `node`는 새 VM의 `spec.node`(켤 곳)이고, 디스크가 닿아야 한다(공유 풀이거나 로컬 풀이면 그 노드 — 아니면 400 `"node \"b\" cannot reach pool \"local\" of template \"tmpl\"; deploy on node \"a\" and migrate the VM"`).
- **Task 흐름**(`tasks/clone.rs::clone`에 갈래 더하기): 다시 확인(템플릿인지, 기반 볼륨에 끝나지 않은 `VolumeDelete`가 없는지) → 디스크마다 연결/전체 결정 → 볼륨 기록(`base` 포함) → `link_clone` 또는 기존 `copy_volume` → 크기 덮어쓰기면 `resize_volume` → UEFI 변수 복사(기존) → `mark_ready` → `apply_overrides` → `create_vm`(새 VM은 `template: false`; `start`면 `power: running`으로 만들어 스케줄러가 배치). 경고는 연결 실패 대체·`identity_warning`(정적 IP·호스트 이름)을 모은다. 실패하면 기존 `discard`가 만든 복제 볼륨만 지운다.
- **대량 배포는 클라이언트에서**: `oplctl template deploy tmpl --count 5 --name 'web-{n}'`(`{n}`은 `--first`부터, 기본 1)와 웹 배포 창의 "개수"가 단건 API를 차례로 부르고 Task들을 함께 지켜본다. cloud-init `hostname`을 비우면 VM 이름이 호스트 이름이므로 겹치지 않는다. 개수가 2 이상인데 정적 IP·호스트 이름을 주면 CLI·웹이 거절한다(같은 값이 겹침). 서버 쪽 일괄 Task는 부분 실패·재시도·감사가 복잡해져 넣지 않는다(§12).

## 7. 평탄화 (`volume-flatten`)

- `POST /volumes/{key}/flatten` → 202 Task **`volume-flatten`**(`TaskOp::VolumeFlatten`, 대상 = 볼륨, 노드 = 볼륨을 쓸 수 있는 노드 — 로컬이면 그 노드). `base`가 없으면 400 `"volume … is not a linked clone"`, 쓰는 VM이 실행 중이면 409 `vm_running`(실행 중 평탄화는 §13), 그 VM에 스냅샷·백업·마이그레이션 Task가 있으면 409 `vm_busy`. 공간: 기반의 할당량만큼 풀 여유를 미리 보고(알 때) 모자라면 409 `no_capacity`.
- Task: `flatten_volume` → `base: None`을 쓴다(revision 확인). 저장소 작업이 끝났는데 기록 전에 죽으면 다시 할 때 `flatten_volume`이 할 일 없이 성공하고 기록만 고친다. 진행률은 qcow2·ZFS만(`qemu-img rebase -p`, `zfs send -v`) `TaskStatus.progress`에.
- 감사 `volume flatten web-1-disk0 (base tmpl-disk0)`.

## 8. 이미지 라이브러리

- **카탈로그 항목** `opl_types::image_catalog::CatalogImage { id, title, os, version, arch: x86_64|aarch64, url, checksum: Checksum, source_format: qcow2|raw, firmware: bios|uefi, default_user, min_size_bytes }`. `Checksum`(`tag = "kind"`): `sha256 { value }` / `sha512 { value }` / `sums { url }`.
- **내장 목록** `BUILTIN`(정적 데이터, Orbivisor 판마다 갱신): `ubuntu-24.04`·`ubuntu-22.04`(`cloud-images.ubuntu.com/releases/{v}/release/` + `SHA256SUMS`), `debian-12`(`cloud.debian.org/images/cloud/bookworm/latest/` genericcloud + `SHA512SUMS`), `rocky-9`·`alma-9`(GenericCloud latest + `CHECKSUM`, BSD 모양), `fedora`(Cloud Base, 판 번호가 URL에 있어 판마다 갱신 + `…-CHECKSUM`), `alpine-3.20`(nocloud cloudinit qcow2 + `.sha512`). 각각 x86_64와 aarch64(Alpine aarch64는 UEFI만). 내장 항목은 "latest/release" 경로와 **같은 디렉터리의 체크섬 파일**을 써서 고정 해시가 낡지 않는다.
- **관리자 항목**: `SettingsSpec.image_catalog: Vec<CatalogImage>`(비면 생략) — 새 Kind 대신 설정에 둔다(작은 목록, 기존 `PATCH /settings`·Raft·백업 그대로). id는 내장과 겹치면 400, 200개까지, URL은 `http`·`https`. 원격 갱신(피드 구독)은 하지 않는다(§13).
- **체크섬 확인**: `TaskOp::VolumeDownload`에 `sha512: Option<String>`, `checksum_url: Option<String>`을 더한다(셋 중 하나만, 없으면 확인 안 함 — 계획 57 그대로). `checksum_url`이면 Task가 새 단계 **`checksum`**(진행률 단계 `downloading` 앞)에서 그 파일을 같은 reqwest 클라이언트·SSRF 규칙(`PublicResolver`, `[download] allow_private`)으로 1 MiB까지 받아 순수 `parse_checksums(text, file_name)`로 줄을 찾는다: GNU `해시  이름`·`해시 *이름`, BSD `SHA256 (이름) = 해시`·`SHA512 (…)`, Fedora·Rocky의 clearsign 머리·꼬리는 건너뜀, 길이 64는 SHA-256·128은 SHA-512. 파일 이름은 이미지 URL의 마지막 경로 조각(퍼센트 디코딩). 줄이 없으면 `"checksum file {url} has no entry for {name}"`. 찾은 해시를 Task 메시지에 남기고 받는 동안 그 알고리즘으로 셈한다. 보증 범위: 손상·잘린 받기·미러 오류를 잡지만 배포처 자체의 변조는 못 잡는다(GPG는 §13). 배포처가 이미지와 SUMS를 바꾸는 사이에 받으면 불일치로 실패한다 — 다시 받으면 된다.
- **받기 → 템플릿 한 번에**: `POST /image-catalog/{id}/download { pool, name?, template? }` → 202 Task `volume-download`. `name` 기본은 id의 `.`을 `-`로(aarch64면 `-arm64`를 붙임, 예 `ubuntu-24-04`). `template: TemplateDraft { name, vcpus(2), memory_mib(2048), size_bytes?(디스크 늘리기), network, cloud_init? }`가 있으면 Task에 `template`으로 담고, 가져오기가 끝나면 크기를 늘리고 템플릿 VM을 만든다(머신은 아키텍처로 `q35`/`virt`, 펌웨어는 항목의 `firmware`, `virtio-blk` 디스크 `boot_order 1`, NIC 하나, cloud-init 기본 — 호스트 이름·정적 IP 없음, 비밀번호는 API가 해시). API는 VM 만들기와 같은 검사를 미리 한다(이름 409 `name_taken`, 네트워크·아키텍처 맞는 노드 400 `machine_arch_mismatch`). 끝에 VM 이름이 그새 쓰였으면 Task는 실패하되 **받은 볼륨은 지우지 않고** `"volume \"ubuntu-24-04\" is ready; create the template from it with vm create and template convert"`로 알린다.
- 목록 `GET /image-catalog?arch=` → 내장 + 관리자 항목(`source: builtin|custom`), 이미 받은 볼륨(같은 URL에서 받은 `volume-download` Task의 결과)을 `volumes`로 함께.

## 9. 다른 기능과의 관계

- **HA**: 템플릿은 HA를 가질 수 없다(§3). 공유 풀의 연결 복제는 기반도 같은 공유 풀이므로 HA 복구가 다른 노드에서 그대로 연다. 로컬 풀 연결 복제의 HA는 지금처럼 복제(계획 55) 없이는 안 되고, 아래처럼 복제는 평탄화 뒤에만 된다.
- **라이브 마이그레이션(공유 풀)**: 디스크를 복사하지 않으므로 양쪽 QEMU가 같은 기반을 읽기 전용으로 연다. NFS의 상대 backing 경로 덕에 노드마다 마운트 지점이 달라도 된다. RBD clone은 librbd가 부모를 읽는다.
- **로컬 디스크 마이그레이션**: 대상 노드에 새로 만든 볼륨으로 `blockdev-mirror sync: full`(기존)을 하면 사슬 전체를 읽어 **독립 볼륨**이 된다. 확정 때 볼륨 기록을 옮기는 쓰기(`tasks/migrate.rs`)가 `base: None`도 쓴다 — 마이그레이션이 곧 평탄화다. 공간 검사는 이미 원본의 가상 크기를 본다. 대상 노드에 같은 기반을 미리 두는 "기반 캐시"는 §13.
- **로컬 디스크 복제(계획 55)**: `replication`을 켤 때 디스크에 `base`가 있으면 400 `"volume … is a linked clone of …; flatten it before enabling replication"`. 복제 마이그레이션 확정 뒤 옛 원본이 복제본으로 남으면서 backing을 계속 가리키는 경우를 피한다. 템플릿은 복제 불가(§3).
- **백업·복원**: QEMU `backup` 잡은 `disk{i}` 노드에서 사슬 전체를 읽고, 꺼진 VM의 `export_volume`(`qemu-img convert`)도 backing을 따라가므로 **백업은 늘 전체 데이터**다. 증분 비트맵은 복제 볼륨에만 있으면 된다. 복원(원격·DR 포함)은 독립 볼륨을 만든다(`base` 없음). 템플릿의 백업은 매니페스트 `vm_spec.template: true`를 그대로 두어 복원하면 템플릿이다. 원격 클러스터가 받을 때(`backup_receive`) `templates`를 모르는 노드가 있으면 `template`을 버린다(계획 57의 `drop_install_media`와 같은 자리, `drop_templates`). `replicate::expected_spec`도 같은 규칙으로 견준다.
- **스냅샷**: 연결 복제 VM의 스냅샷은 백엔드마다 그대로 된다(qcow2 겹층의 내부 스냅샷, ZFS clone·RBD clone의 스냅샷, thin 스냅샷의 thin 스냅샷). 되돌리기는 복제 볼륨만 바꾼다. 스냅샷에서 복제(`--snapshot`)는 전체 복사다. 템플릿은 스냅샷이 없다(§3).
- **Ceph/RBD**: protect된 `opl-base`에 clone. 공유 풀이라 템플릿이 어느 노드에 있든 Task 노드는 `pool_node`. 복제 이미지가 많아도 지우기·평탄화는 자식 하나씩이다. 계획 56의 관리 Ceph와 바깥 Ceph 모두 같다.
- **LVM-thin**: thin 스냅샷이라 thin pool 과할당이 늘어난다 — 기존 풀 용량 표시(`available_bytes`)와 경고가 그대로 다룬다. 기반을 지워도 데이터는 안전하지만 규칙을 백엔드마다 다르게 하지 않는다.
- **ZFS**: clone이 기반 스냅샷에 묶인다. 평탄화는 send/recv라 복제 크기만큼 공간이 더 든다(§7 공간 검사). `zfs promote`는 쓰지 않는다(기반이 복제에 묶이는 반대 방향이 됨).
- **NFS**: 기반과 복제가 같은 내보내기 안이라 `root_squash`·권한 문제가 새로 생기지 않는다. `qemu-img rebase`는 NFS 위에서 오래 걸릴 수 있다(진행률).
- **iSCSI·FC 공유 LVM**: 두꺼운 LV라 연결 복제가 없다 → `linked` 없음이면 전체 복사 + 경고, `true`면 400.
- **유지 보수·비우기**: 비우기는 실행 중 VM만 옮기므로 템플릿(꺼짐)은 남는다. 로컬 풀 템플릿의 배포는 Task가 그 노드에서 돌아야 하므로 노드가 오프라인이면 기존처럼 409(`vm_node`가 없음). 유지 보수 중인 노드의 로컬 풀에 `start: true`로 배포하면 스케줄러가 놓을 곳이 없어 VM은 꺼진 채이고 Task 경고 `"node \"a\" is in maintenance; start web-1 after maintenance"`.
- **ISO·cloud-init(계획 57)**: 템플릿의 CD는 그대로 복사된다(같은 ISO를 가리킴). 템플릿 자체는 켜지지 않으므로 씨앗을 만들지 않는다. 배포한 VM은 새 id라 instance-id가 달라 cloud-init이 처음부터 돈다.
- **버전이 섞인 클러스터**: §2의 `templates` 기능. 새 Task 종류는 옛 노드가 풀지 못하므로 만들기 전에 막고, 실행기는 계획 57 S1처럼 뒤 스테이지까지 `"this node cannot … yet"`으로 실패한다.

## 10. API·CLI·웹

- **API**: `POST /vms/{key}/template`(200), `DELETE /vms/{key}/template`(202 `vm-untemplate`), `POST /vms/{key}/clone`의 새 필드, `POST /volumes/{key}/flatten`(202), `POST /volumes/download`의 `sha512`·`checksum_url`, `GET /image-catalog`, `POST /image-catalog`·`DELETE /image-catalog/{id}`(관리자 항목, `Action::Write` on `Settings`), `POST /image-catalog/{id}/download`(202). 새 오류 코드 `vm_is_template`·`volume_is_template`·`template_in_use`·`volume_has_clones`. 권한은 기존과 같다: 바꾸기·되돌리기 `Write` Vm, 배포 `Write` Vm + `Write` Volume(기존 `clone_vm`), 평탄화 `Write` Volume, 받기 `Write` Volume(+ 템플릿이면 `Write` Vm), 목록 `Read` Volume. 감사 설명 `vm template tmpl`·`vm untemplate tmpl`·`vm clone tmpl -> web-1 (linked)`·`volume flatten …`·`image download ubuntu-24.04 to pool local (template ubuntu-tmpl)`·`image catalog add/remove ID`. OpenAPI(utoipa)에 모두.
- **CLI**(oplctl 새 `template.rs`): `template list`(`NAME DISKS POOL CLONES`), `template convert VM`, `template unconvert TMPL`, `template delete TMPL`(되돌리기 → 지우기), `template deploy TMPL --name N [--count C --first F] [--linked|--full] [--node] [--pool] [--cpus] [--memory] [--network nic0=NET] [--disk-size disk0=20G] [--ci-* — 계획 57과 같은 이름] [--start] [--wait]`; `vm clone`에도 `--linked|--full`. `image list [--arch]`, `image get ID --pool P [--name] [--template NAME --network N --ci-user --ci-ssh-key --ci-password …]`, `image catalog add|remove`; `volume flatten V`, `volume download --sha512|--checksum-url`. `vm list`에 `TEMPLATE` 표시, `volume list`·`volume get`에 `base:`.
- **웹**: 메뉴 "템플릿"(`/templates`, 아이콘 `LayoutTemplate`) — 이름·디스크·풀·연결 복제 수·"배포"·"VM으로 되돌리기"·"삭제"(복제가 있으면 막음). 배포 창(`templates/DeployDialog.tsx`): 이름(개수 2 이상이면 `{n}` 패턴 미리 보기), 연결/전체(디스크마다 가능 여부 표시), 노드·풀, vCPU·메모리, NIC별 네트워크, 디스크 크기, 계획 57 `CloudInitSection` 재사용, "배포 뒤 켜기", 여러 Task 진행 표. VM 상세 "템플릿으로 바꾸기"(조건 미충족이면 이유), VM 목록의 템플릿 배지와 거름 칩, 볼륨 화면의 "기반" 열과 "평탄화". 메뉴 "이미지 라이브러리"(`/images`) — 아키텍처 거름, 항목별 "받기"·"템플릿 만들기"(이름·풀·네트워크·사용자·SSH 키), 받기 진행(`checksum`·`downloading`·`verifying`·`importing`), 관리자 항목 더하기·지우기. 오류·Task 이름 번역(`tasks.op.volume-flatten`·`vm-untemplate`).

## 11. 시험과 CI

- **순수(어디서나)**: serde(옛 VM·볼륨·Task·설정 JSON이 그대로 풀리고 다시 쓰면 같음, 새 필드가 기본이면 바이트가 같음), `apply_overrides`, `parse_checksums`(Ubuntu `SHA256SUMS`·Debian `SHA512SUMS`·Rocky `CHECKSUM`·Fedora clearsign·Alpine `.sha512` 실제 모양 고정 시험 값), `BUILTIN` 검사(id 유일, https, 아키텍처별, 체크섬 종류), `pool_supports_linked_clones`, 불변 규칙 판정, CLI 해석(`{n}` 패턴, `--network nic0=`), 웹 폼.
- **이 Mac(macOS, qemu-img, HVF)**: `storage-it` `dir_storage.rs` — 상대 backing 만들기, 다른 디렉터리 이름으로 옮겨 마운트한 척해도 열림, 복제에 쓰면 기반 해시 그대로, `rebase` 평탄화 뒤 기반을 지워도 내용 같음, 중간에 끊은 rebase 다시 하기, `copy_volume`·`export_volume`이 전체 데이터. `qemu-it` opl-driver `qemu_driver.rs` — 겹층 디스크로 QEMU를 켜고 `query-block`에 `backing-image`, 게스트 없이 `rd_bytes > 0`. API 시험(`MockStorage`·`MockHypervisor`): 바꾸기·되돌리기 조건, 409 네 가지, 기능 게이트, 지우기 경합 재확인, Task의 비밀번호 가림, 대체 경고. `storage-it` `downloads.rs` — 127.0.0.1 서버의 SUMS 파일로 `checksum` 단계, 없는 줄·틀린 해시 실패, SHA-512.
- **svc01(aarch64 실제 노드)**: 일반 묶음 전체와 `qemu-it`(virt + EDK2로 겹층 디스크 켜기), `migration-it`에 `a_linked_clone_migrates`(로컬 dir: 대상이 독립 볼륨·`base` 지워짐·기반 그대로; NFS 공유: 사슬 유지).
- **GitHub 러너 `linux-kvm`(시스템을 바꾸는 시험)**: opl-driver `lvm_storage.rs`·`zfs_storage.rs`·`rbd_storage.rs`에 `link_clone`·`release_base`·`flatten_volume`·백엔드 막이(ZFS destroy·RBD unprotect 실패), `lvm_shared_storage.rs`는 미지원 오류. opld `lvm.rs`·`zfs.rs`·`rbd.rs`·`nfs.rs`·`lvm_shared.rs`에 "템플릿 → 연결 배포 → 켜기 → 기반 지우기 409 → 평탄화 → 지우기" 한 흐름씩(lvm-shared는 전체 복사 + 경고).
- **실제 게스트(GitHub, `install-media` 작업에 단계 추가)**: 이미 받은 Ubuntu 24.04 클라우드 이미지와 `SHA256SUMS`를 127.0.0.1 HTTP 서버로 내놓고, opld 기능 `templates-it`의 `tests/templates.rs` `mod guest`가 관리자 카탈로그 항목(`checksum: sums`)으로 `image get --template ubuntu-tmpl`(사용자·SSH 키) → `template deploy --linked --count 2 --name 'ci-{n}' --disk-size disk0=8G --start` → 두 VM이 `vendor_data` `runcmd`로 10.0.2.2에 보고(호스트 이름 `ci-1`/`ci-2`, 사용자, 루트 파일 시스템이 growpart로 7 GiB 넘음) → 기반 파일 SHA-256이 배포 전과 같음 → `volume delete` 기반 409 → `ci-1` 끄고 `volume flatten` → 다시 켜 두 번째 보고 → 복제 지우기 → `template unconvert`. 이미지는 이미 받으므로 작업 시간 30 → 40분. 게스트 없는 부분은 `qemu-it`로 이 Mac·svc01에서도 돈다(작은 qcow2 기반).
- **카탈로그 링크 확인**: 새 `.github/workflows/catalog.yml`(주 1회 + 수동): `BUILTIN`의 URL마다 HEAD와 체크섬 파일에 그 줄이 있는지(받지 않음). 바깥 서버 사정으로 푸시 CI를 깨지 않게 나눈다. 새 작업이 `check`·`web`·`shell`에 줄을 더하면 프로브 `mk.py`도 고친다.

## 12. 견준 길

- **템플릿 = 새 Kind vs `VmSpec.template`(채택).** Kind를 새로 두면 목록·watch·RBAC·백업·웹을 모두 새로 이어야 한다. Proxmox·vSphere 모두 VM의 상태다.
- **바꿀 때 기반 스냅샷 만들기 vs 첫 연결 복제 때(채택).** 바꾸기를 동기 Raft 쓰기로 두고, 블록 백엔드 작업은 어차피 노드에서 도는 복제 Task가 멱등으로 한다. 되돌리기·지우기만 Task로 정리해 낡은 `opl-base`가 남지 않게 한다.
- **의존을 저장소에서만 보기(`rbd children`, `qemu-img info`) vs Raft `base` + 백엔드 막이(채택).** 저장소 질의는 노드·풀 상태에 따라 느리거나 불가능하고(파일 풀은 역참조가 없음), Raft 기록은 API에서 바로 409를 줄 수 있다. 백엔드 막이는 기록이 틀려도 데이터를 지킨다.
- **qcow2 절대 backing vs 상대(채택).** NFS 마운트 지점이 노드마다 다를 수 있고 dir 풀 경로를 바꿔도 열린다. 대가는 기반과 복제가 같은 디렉터리(같은 풀)여야 한다는 것 — 원래 규칙이다.
- **새 `/deploy` 경로 vs `clone` 넓히기(채택).** 템플릿 배포는 복제의 한 종류이고 같은 Task·정리(`discard`)·경고를 쓴다.
- **서버 쪽 대량 배포 vs 클라이언트 반복(채택).** 단건 Task가 이미 재개·정리·감사를 갖고 있어 N개를 한 Task로 묶으면 부분 실패 처리만 늘어난다.
- **고정 SHA-256 vs 체크섬 파일(내장 항목 채택).** 클라우드 이미지는 "release/latest"가 자주 바뀌어 고정 해시는 곧 낡는다. 관리자 항목은 둘 다 고를 수 있다.
- **카탈로그 = 새 Kind vs `SettingsSpec`(채택).** 수십 개의 작은 목록이고 관리자만 쓴다. 원격 피드 갱신은 신뢰 경로(서명)가 먼저라 미룬다.
- **ZFS 평탄화 `promote` vs send/recv(채택).** `promote`는 의존 방향을 뒤집어 기반이 복제에 묶인다.

## 13. 나중 할 일

실행 중 평탄화(`block-stream`, RBD는 온라인 `rbd flatten`), 노드마다 기반 사본을 두는 "기반 캐시"로 로컬 풀 다른 노드 연결 배포, 템플릿 옮기기(노드·풀), 연결 복제 사슬(템플릿의 연결 복제를 다시 템플릿으로), 연결 복제 VM의 로컬 디스크 복제, 체크섬 파일의 GPG 서명 확인(Ubuntu `SHA256SUMS.gpg`, Fedora clearsign), 카탈로그 원격 피드·원격 클러스터 간 템플릿 동기화(콘텐츠 라이브러리), 서버 쪽 대량 배포와 IP 풀, 배포 때 NIC 더하기·빼기, OVF/OVA 템플릿 내보내기, Windows sysprep·cloudbase-init 템플릿 안내.

## 14. 스테이지 나누기

각 스테이지는 커밋 하나이고 혼자 컴파일·시험된다. 앞 스테이지의 새 Task 실행기는 `"this node cannot … yet"`으로 실패하고(계획 57 S1), API는 마지막에 연다.

1. **feat(types): 템플릿·연결 복제 타입·검증·기능과 이미지 카탈로그·체크섬 해석(opl-types)** — `VmSpec.template`, `VolumeSpec.base`, `BASE_TAG`, `pool_supports_linked_clones`, `TEMPLATES` 기능, `TaskOp::VmClone{linked, overrides}`·`VolumeDownload{sha512, checksum_url, template}`·`VolumeFlatten`·`VmUntemplate`, `template.rs`(`CloneOverrides`·`apply_overrides`·`TemplateDraft`), `image_catalog.rs`(`CatalogImage`·`Checksum`·`BUILTIN`·`parse_checksums`), `SettingsSpec.image_catalog`, serde 하위 호환 시험.
2. **feat(driver): 백엔드별 연결 복제·기반 스냅샷·평탄화(opl-driver)** — `Storage::link_clone`·`release_base`·`flatten_volume`, dir·NFS(상대 backing, `rebase`), LVM-thin, ZFS(send/recv·rename 재개), RBD(protect·clone·flatten, `delete_volume`의 unprotect), `Storages` 나눔·모의 저장소, `storage-it`·`lvm-it`·`zfs-it`·`rbd-it` 시험, 겹층 디스크 실제 QEMU 시험(`qemu-it`).
3. **feat(reconciler,tasks): 템플릿을 켜지 않음, 연결 배포·평탄화·되돌리기 Task(opl-reconciler·opl-api)** — reconciler 방어, 스케줄러·균형·HA·비우기에서 템플릿 빼기, `tasks/clone.rs`의 연결 갈래·덮어쓰기·크기·`start`·대체 경고, `volume-flatten`·`vm-untemplate` 실행기, `VolumeDelete` 재확인, 마이그레이션 확정의 `base: None`, `drop_templates`.
4. **feat(api): 체크섬 파일 단계와 받기 → 템플릿(opl-api)** — `tasks/download.rs`의 `checksum` 단계·SHA-512, `template` 초안으로 템플릿 VM 만들기(실패 때 볼륨 보존), `downloads.rs` 시험.
5. **feat(api): 템플릿·배포·평탄화·이미지 카탈로그 API, 지우기 거절, 가림(opl-api)** — §10의 경로, 불변 규칙과 409 네 가지, `CloneRequest` 확장과 미리 검사, 복제 켜기 거절, 기능 게이트, Task 비밀번호 가림, 감사 설명, OpenAPI.
6. **feat(cli): template·image·volume flatten CLI, opld 실제 시험과 CI(oplctl·opld·CI)** — oplctl `template.rs`·`image`, 클라이언트 대량 배포, opld `templates.rs`(게스트 없는 부분 + `mod guest`), 백엔드별 opld 흐름, `migration-it` 시험, `install-media` 작업 단계, `catalog.yml`.
7. **feat(web): 템플릿 화면·배포 창, 이미지 라이브러리, VM·볼륨 화면의 템플릿·기반·평탄화(web)**.
8. **docs: README "템플릿과 연결 복제"·"이미지 라이브러리", 설계 문서 스테이지 보고, 계획 58 실행 기록과 배포 확인**.

## 15. 스테이지 보고 (설계와 다른 것·결정)

- **S1 `apply_overrides`의 오류 타입**: `VmError`는 없으므로 `template.rs`에 `TemplateError`(`TooManyNics`·`TooManyDisks`·`DiskShrink`·`Invalid(ValidationError)`)를 두었다. 디스크 크기는 볼륨에 있어 `apply_overrides`는 개수만 보고, 늘릴 크기는 순수 함수 `grown_disks(&ov, 원본 크기들)`가 정한다(같으면 None, 줄이면 `DiskShrink`). `vcpus` 덮어쓰기는 소켓 1 × 코어 N이다(`max_vcpus`는 그대로라 그보다 크면 검증 오류). `apply_overrides`는 `template` 플래그를 건드리지 않는다 — 새 VM의 `template: false`는 복제 Task가 쓴다.
- **S1 `VolumeError`**: 새로 두고 `VolumeSpec::check_base(기반 이름, 기반 스펙)`가 `BaseInOtherPool`·`BaseIsLinkedClone`(사슬 1단)·`BaseNotDisk`(ISO)를 판정한다.
- **S1 카탈로그 키는 `(id, arch)`**: 내장 항목은 같은 id를 아키텍처마다 한 줄씩 둔다(§8의 이름 규칙 `-arm64`와 맞음, `CatalogImage::default_volume_name`). 찾기는 `image_catalog::find(images, id, arch)` — S5의 `POST /image-catalog/{id}/download`는 `arch`(없으면 풀·노드의 아키텍처)를 받아야 한다. 관리자 항목은 내장 id를 아키텍처와 상관없이 쓸 수 없고(`CatalogError::Builtin`), 자기들끼리는 `(id, arch)`가 겹치면 안 된다. 검사는 `validate_custom`(S5가 `PATCH /settings`·`POST /image-catalog`에서 부른다).
- **S1 내장 목록 실제 확인(2026-11-17)**: URL·체크섬 파일·가상 크기(`min_size_bytes`)를 실제 서버로 확인했다. Fedora는 id `fedora-45`(`Fedora-Cloud-Base-Generic-45-1.4`), Alpine은 `3.20.10`(x86_64 BIOS, aarch64 UEFI)으로 판 번호가 URL에 있다. Rocky `CHECKSUM`은 `.latest.` 이름 줄을 함께 담는다. **Alpine `.sha512`는 파일 이름 없이 해시 하나만** 있어 `parse_checksums`가 "해시 하나뿐인 파일"을 그 해시로 받는다. 내장 항목의 펌웨어는 Alpine x86_64(BIOS 이미지)만 BIOS, 나머지는 UEFI다. `BUILTIN`은 `&'static str`의 `Builtin` 정적 표이고 `builtin()`이 `CatalogImage`로 바꾼다.
- **S1 `TemplateDraft`의 `machine`·`firmware`**: Task가 카탈로그 항목 없이도 템플릿을 만들 수 있게 초안에 선택 칸으로 두었다. API(S5)가 항목의 아키텍처·펌웨어로 채운다(없으면 이 노드의 머신, UEFI). `TemplateDraft::vm_spec(볼륨)`이 §8의 템플릿 VM 스펙을 만들고 검증한다.
- **S1 `TaskOp::needs_templates()`**: 새 Task 종류와 새 칸을 쓴 `vm-clone`·`volume-download`면 참. S5의 기능 게이트가 쓰고, S1~의 실행기는 이것이 참이면 `"this node cannot … yet"`으로 실패한다(뒤 스테이지가 갈래를 연다).
- **S1 해시 대소문자**: `parse_checksums`는 소문자로 돌려주지만 관리자 항목의 고정 `sha256`·`sha512`는 대소문자 모두 받는다 — S4·S5가 비교·저장 때 소문자로 바꾼다.
- **S2 `release_base`의 기본은 성공**: 연결 복제를 만들지 못하는 저장소(공유 LVM)에는 기반 스냅샷도 없으므로 되돌리기 Task가 백엔드를 가리지 않고 부를 수 있게 기본 구현은 할 일 없이 성공한다. `link_clone`·`flatten_volume`의 기본은 §5대로 `InvalidSpec`이다.
- **S2 파일 풀 평탄화는 내부 스냅샷이 있으면 거절**: `qemu-img rebase -b ""`는 지금 내용(활성 L1)만 채우므로 겹층의 내부 스냅샷에서 비어 있던 클러스터가 기반 대신 0으로 읽히게 된다(실제 qemu-img로 확인). `flatten_volume`은 `"volume … has snapshots; delete them before flattening it"`(`InvalidSpec`)로 거절한다 — S5 API는 파일 풀 디스크의 평탄화를 VM 스냅샷이 있으면 미리 409로 막는 것이 좋다. 다른 백엔드: ZFS는 스냅샷을 함께 옮기고(아래), LVM-thin은 할 일이 없고, RBD는 `rbd flatten` 뒤에도 복제의 스냅샷이 부모를 가리키므로 그 스냅샷이 남은 동안 기반의 `opl-base` 보호를 풀지 못한다(데이터는 안전, 되돌리기 Task가 백엔드 오류로 실패).
- **S2 ZFS 평탄화**: 보낼 시점으로 clone에 `@opl-flatten`을 찍고, clone의 첫 스냅샷(없으면 `@opl-flatten`)은 전체 스트림, 나머지는 `zfs send -I`로 `{zvol}-flat`에 받는다 — 복제 VM의 스냅샷이 평탄화 뒤에도 남는다. 받을 때 새 zvol과 같은 속성(`volmode=dev`·`snapdev=visible`·`com.sun:auto-snapshot=false`)을 `recv -o`로 준다. 원본 destroy → rename → `@opl-flatten` 지우기. 이미 clone이 아니면 남은 `@opl-flatten`만 지운다. 파이프는 `BlockTool::run_pipe_change`(앞 명령이 스스로 실패하면 그 오류, 아니면 뒤 명령의 오류)이고 시간 제한이 없다.
- **S2 평탄화 진행률은 없다**: `flatten_volume`에 진행률 콜백을 두지 않았다(`qemu-img rebase -p`·`zfs send -v`를 읽지 않음). S3의 `volume-flatten` Task는 단계만 알린다.
- **S2 RBD**: `RbdSnapshot.protected`(`rbd snap ls`의 `protected`)를 더했다. `link_clone`은 `opl-base`가 없으면 찍고 보호돼 있지 않으면 보호한다(끊긴 되돌리기 뒤에도 맞음). `release_base`·`delete_volume`은 `opl-base`의 보호를 먼저 푼다(clone이 있으면 `rbd snap unprotect`가 거절 — 백엔드 막이). `flatten_volume`은 `rbd info`의 `parent`가 없으면 할 일이 없고, 이미지를 연 클라이언트가 있으면 일시 오류다.
- **S2 LVM-thin `link_clone`**은 thin 스냅샷을 만든 뒤 켜고 활성화 건너뛰기 표시를 지운다(`snapshot_revert`와 같음, 켜지 못하면 만든 LV를 지움).
- **S2 실제 QEMU 시험**: QMP 소켓은 드라이버가 쥐고 있어 `query-block`을 따로 부를 수 없으므로 HMP `info block`의 `Backing file`과 "기반 파일에 다른 프로그램이 쓰기 잠금을 못 잡음"으로 QEMU가 backing을 연 것을 단언한다(`rd_bytes > 0`은 펌웨어가 읽은 것).
- **S2 모의 저장소**: opl-api 시험의 `FakeStorage`가 `link`(복제 볼륨 파일을 기반 크기로 두고 파일 풀이면 qcow2, 아니면 raw)·`release_base`·`flatten`을 호출 기록에 적는다.
- **S3 템플릿 빼기의 자리**: 따로 거르지 않고 두 곳에서 막는다 — `VmSpec::ha_active()`가 템플릿이면 거짓(HA 복구·옮기기·펜싱 조건이 모두 이것을 봄), `scheduler::node_fits_with`가 템플릿이면 `"the VM is a template and is never started"`(스케줄러·HA 대상 고르기·균형 조정기 대상·마이그레이션 대상이 모두 이것을 봄). 균형 조정기 `movable`은 `"template"`으로, 비우기는 대상 VM 목록에서 뺀다.
- **S3 reconciler 방어**: `power: running`인 템플릿을 켜지 않고 `phase: stopped`·`last_error = TEMPLATE_NOT_STARTED`를 쓴다. 원하는 상태(`power`)는 고치지 않는다(사용자 쓰기를 reconciler가 바꾸지 않는 규칙). 이미 도는 QEMU는 끄지 않는다(바꾸기가 꺼짐을 요구하므로 생길 수 없음).
- **S3 `start`의 배치**: 배경 스케줄러가 없으므로(스케줄은 API의 켜기·만들기가 함) "`power: running`으로 만들어 스케줄러가 배치"를 복제 Task가 직접 한다: 덮어쓰기의 `node`가 있으면 `node_fits`로 그 노드를 보고, 없으면 `scheduler::schedule`로 고른 뒤 `spec.node`와 `power: running`으로 VM을 만든다(그 노드의 reconciler가 켬). 놓지 못하면 꺼진 채 만들고 경고 `"{이유}; start \"web-1\" later"`(유지 보수면 `node "a" is in maintenance; start "web-1" later` — §9의 문구 대신). `start`가 없으면 `spec.node`는 덮어쓰기의 `node`(없으면 비움, 원본의 노드는 받지 않음). `create_vm`에 `node`·`start` 인자를 더했다(복원·가져오기는 그대로 없음·꺼짐).
- **S3 연결 여부 판정**(`clone.rs::unlinkable`): 다른 풀 `"disk0: pool \"fast\" is not the pool \"local\" of template disk \"root\""`, 백엔드 미지원 `"disk1: pool \"san\" (lvm-shared) cannot hold linked clones"`, 기반이 다시 연결 복제 `"disk0: volume … is a linked clone itself"`. `linked` 없음이면 뒤에 `; made a full copy`를 붙인 경고, `true`면 그 문구로 실패. 기반에 끝나지 않은 `VolumeDelete`가 있으면 `"volume … is being deleted"`로 실패. 볼륨 기록의 형식은 파일 풀 qcow2·블록 풀 raw로 미리 적고, `link_clone`이 돌려준 형식이 다르면 기록을 고친다. 늘린 크기는 `resize_volume` 뒤 `mark_ready`가 실제 크기로 적는다. 경고(대체·`identity_warning`·켜지 못함)는 `; `로 잇는다. `identity_warning`은 배포 때 새로 준 호스트 이름·네트워크는 원본 것이 아니므로 보지 않는다.
- **S3 줄과 시간 제한**: `volume-flatten`은 크기 늘리기처럼 그 볼륨을 쓰는 VM의 줄(없으면 볼륨의 줄)에 서고, 기반 데이터를 디스크 크기만큼 복사하므로 Task 시간 제한(`timeout`)을 두지 않는다. `vm-untemplate`은 템플릿 VM의 줄에 서서 먼저 받은 배포가 끝난 뒤 남은 연결 복제를 본다.
- **S3 평탄화 Task**: `base`가 이미 없으면 할 일 없이 성공(기록까지 마친 앞 시도의 재시도), 쓰는 VM이 꺼져 있지 않으면 `"VM \"db\" is running; stop it before flattening its disk"`. 기록을 고친 뒤 `mark_ready`로 할당량을 다시 적는다.
- **S3 되돌리기 Task의 복제 이름**: 쓰는 VM의 이름, 붙지 않은 복제는 볼륨 이름을 이름 순으로 셋까지 적는다(`volumes::template_in_use_message`·`has_clones_message` — S5 API가 같은 문구를 쓴다).
- **S3 `base: None`이 되는 길**: 로컬 디스크 마이그레이션 확정의 볼륨 기록 쓰기는 `opl-reconciler::resolve::move_volume`(복제본 승격도 씀)에 있어 거기서 `base`를 지운다(같은 풀·형식이어도 spec을 씀). 백업 복원·전체 복사·가져오기는 새 기록을 `base: None`으로 만든다(S1부터). 복원은 매니페스트의 `template`을 그대로 두되 `templates`를 모르는 노드가 있으면 `create_vm`이 `drop_templates`로 버린다(받기 `backup_receive`와 `replicate::expected_spec`도 같은 규칙).
- **S4 찾은 해시의 자리**: Task 상태에는 성공 메시지 칸이 없으므로(`error`는 경고로 읽힘) 체크섬 파일에서 찾은 해시는 opld 로그(`found the checksum of the download`)에 남기고, 불일치 오류에 `"checksum mismatch: expected {해시} (from {가린 체크섬 URL}), got {셈한 해시}"`로 적는다. 체크섬 파일이 1 MiB를 넘으면 `"checksum file {url} is larger than 1024 KiB"`, 이미지 URL에서 파일 이름을 얻지 못하면 `"cannot tell the file name of {url}"`. 체크섬 파일도 이미지와 같은 클라이언트·SSRF 규칙(IP로 적은 주소·이름 해석·리디렉션)으로 받는다. 고정 `sha256`·`sha512`는 Task가 소문자로 바꿔 비교한다.
- **S4 진행률 단계 `template`**: §10 웹의 단계(`checksum`·`downloading`·`verifying`·`importing`) 뒤에 템플릿을 만드는 동안 `template` 단계를 더 쓴다(크기 늘리기가 블록 풀에서 시간이 걸릴 수 있음) — S7 웹이 번역한다.
- **S4 받은 볼륨 남기기**: Task 정리(`tasks.rs::keeps_downloaded`)가 템플릿 초안이 있는 받기의 볼륨이 이미 `ready`면(가져오기를 마침) 지우지 않는다 — 템플릿 단계 실패와 그 단계 중의 opld 재시작(`interrupted`) 모두. 오류는 `"{원인}; volume \"ubuntu-24-04\" is ready; create the template from it with vm create and template convert"`(원인: `cannot create VM "x": …`·`cannot grow volume …`·`invalid template …`). 가져오기 전 실패(체크섬 등)는 계획 57처럼 볼륨을 지운다. 이미 같은 이름의 템플릿 VM이 그 볼륨을 쓰면 할 일 없이 성공한다(재시도).
- **S4 초안의 `size_bytes`**: 받은 이미지보다 클 때만 늘리고, 작거나 같으면 그대로 둔다(Task는 받기 전에 이미지 크기를 모른다). 늘리기는 `resize_volume`(꺼진·쓰지 않는 볼륨 경로)이다. S5 API가 카탈로그 항목의 `min_size_bytes`보다 작은 값을 400으로 막는 것이 좋다.
- **S4 시험**: 받기 API는 아직 새 칸을 받지 않으므로(S5) `downloads.rs`는 볼륨 기록과 Task를 저장소에 바로 만든다(`run_download`).
- **S5 템플릿 표시는 전용 경로로만 바뀐다**: `PUT /vms/{key}`는 저장된 `template`을 그대로 둔다(본문의 값은 버림) — 이 칸을 모르는 옛 클라이언트의 PUT이 템플릿을 보통 VM으로 만들지 않게. `POST /vms`의 `template: true`는 받되(기능 게이트) 바꾸기와 같은 스펙 규칙(켜짐 409 `vm_is_template`, `ha`·`replication`·`snapshot_schedule` 400, 연결 복제 디스크 400)을 `prepare`에서 본다. 바꾸기(`POST /vms/{key}/template`)는 같은 `write_spec` 길이라 준비되지 않은 디스크는 §3의 400 대신 기존 409 `volume_not_ready`다. 템플릿의 `stop`과 CD 빼기는 막지 않는다(넣기만 409). 템플릿 디스크 목록의 비교는 볼륨·버스·개수이고 `boot_order`는 바꿀 수 있다.
- **S5 배포 미리 검사**: `linked: true`의 디스크별 판정은 Task와 같은 `unlinkable` 문구를 400으로 낸다. `node`는 없으면 400, 위트니스면 409 `node_unavailable`, 아키텍처가 다르면 400 `machine_arch_mismatch`이고, 로컬 풀 디스크는 Task 노드(복제 볼륨을 만드는 `vm_node`)에서만 닿는다. 디스크 크기는 512의 배수여야 한다(400). cloud-init `password: "********"`는 원본의 비밀번호를 그대로 둔다(덮어쓰기에서 뺌). 새 칸이 모두 비면 `overrides`를 쓰지 않아 계획 57과 같은 Task다. 응답 Task의 비밀번호는 가린다.
- **S5 기능 게이트의 안전망**: 경로마다 검증 전에 `templates`를 보고, `tasks::create_task`도 `TaskOp::needs_templates()`면 다시 본다(빠뜨린 길이 옛 노드가 풀지 못하는 Task를 만들지 않게).
- **S5 평탄화 API**: 파일 풀(dir·NFS) 디스크는 쓰는 VM에 스냅샷이 있으면 409 `vm_has_snapshots`(S2 보고). 공간 검사는 기반의 `allocated_bytes`와 Task 노드가 알린 풀 상태(`pool_id`가 맞고 `total_bytes > 0`)의 `available_bytes`를 견주고, 어느 쪽이든 모르면 보지 않는다. Task 노드는 크기 늘리기와 같다(쓰는 VM의 디스크 노드, 없으면 볼륨 노드).
- **S5 카탈로그 API의 모양**: `GET /image-catalog?arch=` → `List<CatalogEntry>`(`CatalogImage` 칸 + `source: builtin|custom` + `volumes`), `volumes`는 같은 URL의 끝난 `volume-download` Task 가운데 대상 볼륨이 `ready`인 것(템플릿 단계가 실패해 남긴 볼륨 포함, URL이 같으면 아키텍처를 가리지 않음). `POST /image-catalog`(본문 `CatalogImage`, 201) — 같은 `(id, arch)`는 409 `already_exists`, 나머지 검사 오류는 400, 고정 해시는 소문자로 저장. `DELETE /image-catalog/{id}?arch=`(없으면 그 id의 모든 아키텍처) — 내장 id 400, 없으면 404. `PATCH /settings`에는 `image_catalog`를 넣지 않았다(전용 경로만 — `validate_custom`은 더하기에서 부른다). 받기(`POST /image-catalog/{id}/download`)는 `install-media`·`templates`가 모두 있어야 하고, `arch`가 없으면 풀에서 받을 노드(`pool_node`)의 아키텍처다. 템플릿 초안은 따로 `CatalogTemplate { name, vcpus, memory_mib, size_bytes, network, cloud_init }`로 받고 머신·펌웨어는 항목에서 채운다(사용자가 고르지 않음). 아키텍처 검사는 "풀을 쓸 수 있는 위트니스 아닌 노드 가운데 그 아키텍처(또는 모르는 옛 노드)가 있는가"다.
- **S5 감사 설명**: `vm template X`·`vm untemplate X`·`vm clone X -> Y`(템플릿이면 ` (linked)`, `linked: false`면 ` (full)`)·`volume flatten V (base B)`·`volume download …`에 ` (sha512 abcd…)`·` (checksum file URL)`·`image download ID to pool P (template T)`·`image catalog add ID`·`image catalog remove ID`. 받기 Task는 응답·watch에서 `checksum_url`도 `url`처럼 가린다.
- **S6 CLI 모양**: §10의 `image get`은 `image download`를 별칭으로 둔다(계획 57 `iso download`·`volume download`와 같은 말). `template deploy`는 다른 명령처럼 기본으로 기다리고 `--no-wait`을 둔다(§10의 `--wait` 대신). `--start`면 VM이 `running`이 될 때까지(5분) 기다려 `vm X: running on NODE`를 쓰고, Task 경고(대체 전체 복사·`identity_warning`·켜지 못함)는 stderr의 `warning: vm X: …`다. `--network`·`--disk-size`는 `nicN=`·`diskN=`이 없으면 준 순서의 자리다. `--count`는 1~100이고 하나가 API에서 거절되면 더 만들지 않고 이미 만든 Task를 마저 기다린 뒤 실패한다. `--ci-hostname`·정적 `--ci-ip`/`--ci-ip6`은 `--count` 2 이상에서 거절한다. `template delete`는 템플릿이 아니면 거절하고 디스크 볼륨은 남긴다(`vm delete`와 같음). `image catalog add`는 `--os`·`--version`이 필수(API가 빈 값을 거절)이고 체크섬은 `--sha256`·`--sha512`·`--checksum-url` 가운데 꼭 하나다.
- **S6 목록 표시**: `vm list`는 새 열 대신 템플릿의 PHASE 칸에 `template`을 쓴다(템플릿은 늘 꺼져 있어 PHASE가 뜻이 없음, 열 순서를 쓰는 스크립트를 깨지 않음). `volume list`에 `BASE` 열(기반 볼륨 이름), `vm get`의 stderr에 `template: N linked clone(s); …`과 `diskN: linked clone of …` 줄(볼륨을 읽을 권한이 없으면 빠짐)을 더했다. `volume get`은 JSON이라 `spec.base`(id)가 그대로 보인다.
- **S6 opld 시험**: 풀마다 같은 흐름은 `tests/common/mod.rs::linked_clone_flow`(템플릿 → `--disk-size`로 늘린 배포 `--start` → 템플릿 디스크 지우기 409 `volume_is_template` → 되돌리기 409 `template_in_use` → 실행 중 평탄화 409 `vm_running` → 끄고 평탄화 → 다시 켜기 → 지우기, 공유 LVM은 전체 복사·평탄화 400)이고 `templates.rs`(디렉터리)·`lvm.rs`·`zfs.rs`·`rbd.rs`·`nfs.rs`·`lvm_shared.rs`의 기존 시험 끝에서 부른다. 템플릿 디스크 지우기는 `volume_has_clones`보다 먼저 `volume_is_template`으로 거절된다(기반은 늘 템플릿 디스크이므로 `volume_has_clones`는 되돌린 뒤 남은 복제가 있을 때만 — 되돌리기가 그것을 막으므로 API 시험만 본다). 실제 게스트 시험은 기능 `templates-it`(OS 조건 없이 컴파일되어 이 Mac의 `clippy --all-features`가 본다)이고, 부팅마다 보고하도록 cloud-init `scripts/per-boot`를 vendor-data의 `write_files`로 둔다(§11의 `runcmd`는 첫 부팅만 돎). 루트 파일 시스템 기준은 7 GiB가 아니라 **6 GiB 초과**다 — Ubuntu 24.04 이미지는 /boot(913 MiB)·EFI 파티션이 있어 8 GiB 디스크의 루트가 7 GiB에 못 미친다. 받은 `SHA256SUMS`(`OPL_TEST_CLOUD_SUMS`)를 그대로 내놓고 이미지는 원래 파일 이름으로 내놓는다.
- **S6 마이그레이션 시험**: `migration.rs`에 `a_linked_clone_migrates_to_a_local_pool_of_another_node`(이 Mac에서 돎 — 대상은 backing 없는 독립 qcow2, `base` 지워짐, 원본 겹층 지워짐, 기반 그대로, 템플릿 되돌리기 됨)와 `nfs-it`의 `a_linked_clone_on_an_nfs_pool_migrates_with_its_chain`(왕복 뒤 `base` 그대로).
- **S6 카탈로그 확인**: `catalog.yml`(주 1회·수동·`ci-probe/**` 푸시)은 oplctl 기능 `catalog-it`의 `tests/catalog.rs`를 돌린다 — 항목마다 이미지 URL의 HEAD(안 되면 `Range: bytes=0-0` GET)와 체크섬 파일에 그 파일 이름의 줄(`parse_checksums`)을 본다(요청마다 세 번까지). 2026-11-18 이 Mac에서 14개 모두 통과했다. `check`·`web`·`shell` 작업에는 줄을 더하지 않았다.
- **S7 웹 배포 창의 진행**: §10의 "여러 Task 진행 표" 대신 VM마다 Task 알림(`trackTask`)을 띄우고 창을 닫는다 — 다른 화면의 Task와 같은 길이고, 성공했지만 `status.error`가 있는 배포(연결 실패 대체·`identity_warning`·켜지 못함)는 경고 알림으로 남는다. 요청은 차례로 보내고 거절되면(409 등) 멈춘다(CLI와 같음): 첫 요청이면 창에 오류, 아니면 만든 Task는 두고 "나머지 N개 배포 안 함" 오류 알림. 개수 2 이상에서 호스트 이름·정적 IP는 보내기 전에 거절한다. 디스크 복제 방식은 세 값(되는 디스크만 연결 = `linked` 생략, 모두 연결 = `true`, 전체 복사 = `false`)이고, 디스크마다 연결 가능 여부를 `pool_supports_linked_clones`와 같은 규칙(+ 고른 풀이 기반 풀과 같은지)으로 보인다. cloud-init은 계획 57 `CloudInitSection`을 그대로 쓰고(값 타입만 `Pick<VmFormValues, "name" | "nics" | "cloudInit">`로 좁힘) 켰을 때 넣은 칸만 보낸다. 긴 창을 위해 `FormDialog`에 `className`(너비)을 더했다.
- **S7 웹 템플릿 동작**: "템플릿으로 바꾸기"·"VM으로 되돌리기"는 VM 목록 메뉴와 VM 상세 머리글 둘 다에 두고 이유를 보인다(꺼짐·스냅샷·HA·복제 대상·예약 스냅샷·연결 복제 디스크 / 연결 복제 남음). 템플릿의 시작·재시작·마이그레이션·콘솔·삭제는 `vms.reason.template`로 막고, 복제 자리에 "배포"가 온다. 템플릿 화면의 삭제는 CLI처럼 되돌리기 Task를 기다린 뒤 VM을 지운다(디스크 볼륨은 남음). VM 목록의 거름 칩은 "템플릿만 보기"다. 볼륨 화면은 "기반" 열, 연결 복제 행의 평탄화(쓰는 VM이 실행 중이면 이유), 템플릿 디스크의 크기 바꾸기·삭제 이유, 기반 볼륨의 삭제 이유를 보인다.
- **S7 웹 이미지 라이브러리**: 카탈로그는 watch 종류가 아니므로 `useQuery(["image-catalog", arch])`로 읽고, 끝난 `volume-download` Task 수가 바뀌면 다시 읽어 `volumes`를 갱신한다. 받는 중 진행률은 같은 URL의 끝나지 않은 `volume-download` Task의 `describeProgress`(새 단계 `checksum`·`template` 번역)다. "받은 뒤 템플릿 만들기"는 템플릿 이름·네트워크·vCPU·메모리·디스크 크기·사용자·비밀번호·SSH 키를 받는다(VM 쓰기 권한이 있을 때만 보임). 항목 더하기·지우기는 `Settings` 쓰기(admin)에게만 보인다.

### 리뷰 1에서 고친 것

- **되돌리기와 배포의 엇갈림(노드 사이)**: 둘 다 "Raft에 쓰고 다시 보기"다. 배포 Task는 연결할 복제 볼륨을 기록한 뒤 템플릿인지 다시 보고 아니면 `"template \"tmpl\" was converted back to a VM"`로 실패한다(정리가 기록을 지움). 되돌리기 Task는 기반을 푼 뒤 `template: false`를 쓰고 연결 복제(만드는 중 포함)를 다시 보아 있으면 `true`로 되돌리고 `template_in_use` 문구로 실패한다 — Raft 순서에서 뒤에 쓴 쪽이 물러난다. 기반을 푼 뒤 생긴 복제는 `link_clone`이 기반 스냅샷을 다시 찍는다. 템플릿의 배포·되돌리기 Task 노드는 요청 노드와 상관없는 `snapshots::template_node`(`vm_placement(.., None, ..)`, 공유 풀이면 id가 가장 작은 노드)라 보통 같은 줄에 선다(§3의 `vm_node` 대신). 되돌리기 API의 `vm_lock`(배포가 잡지 않아 뜻이 없음)은 뺐다.
- **평탄화 중 켜기**: reconciler는 실행 중인 `volume-flatten` Task가 있는 디스크를 켜지 않고 `VolumeFlattening`으로 기다리며(크기 늘리기와 같음), API의 `check_no_resize_task`(마이그레이션·핫플러그)도 평탄화를 409 `vm_busy` `"a disk of VM … is being flattened"`로 막는다. ZFS 평탄화는 받은 뒤 `written@opl-flatten`이 0이 아니면 원본을 지우지 않고 `-flat`을 지운 뒤 실패한다(`ZfsOps::written`).
- **`POST /volumes`의 `base`**: 400 `"base is set only by deploying from a template"`. 연결 복제 기록은 배포 Task만 만든다(거짓 기록이 기반 지우기·되돌리기를 막고, 옛 노드가 풀지 못함).
- **템플릿 백업의 비트맵**: VM이 템플릿이거나 디스크에 연결 복제가 있으면 백업 Task가 비트맵을 만들지 않는다(꺼진 VM의 `set_backup_bitmap`이 연결 복제의 backing qcow2를 바꾸므로).
- **카탈로그 템플릿 초안**: cloud-init `hostname`·정적 IP(`networks`의 `static`)는 400(§8 — 배포할 때 준다).
- **RBD 기반 풀기 실패 문구**: `opl-base` 보호 풀기가 자식 때문에 실패하면(`(16) Device or resource busy`) 원래 오류 뒤에 "연결 복제, 또는 평탄화한 연결 복제가 평탄화 전에 찍은 스냅샷이 쓰고 있으니 먼저 지우라"는 말을 붙인다. 평탄화 뒤 남은 부모 참조는 Raft에 없어 API가 미리 409로 막지 못한다.
- 그 밖: 바꾸기 API가 `check_not_busy`·`check_no_snapshot_task`(§3대로), 스냅샷 되돌리기가 `check_not_template`, Task 응답·watch가 VM 쓰기 권한이 없으면 덮어쓰기·초안의 `user_data`·`vendor_data`도 가림, ZFS `delete_volume`이 끊긴 평탄화가 남긴 `-flat`도 지움, 볼륨 지우기 Task가 지우기 직전에 템플릿 디스크인지도 다시 봄.

### 리뷰 2에서 고친 것

- **지난 노드가 빠졌거나 꺼진 템플릿**: `template_node`는 `spec.node`·`status.node`가 클러스터에 없거나 오프라인이면 건너뛰고 디스크로 정한다(공유 풀은 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드, 로컬 풀은 볼륨이 있는 노드, 그 노드도 빠졌으면 요청 노드). 되돌리기 Task는 풀이 없어졌거나 로컬 풀의 볼륨이 있던 노드가 빠진 디스크의 `release_base`를 건너뛴다 — 지운 노드에 묶인 템플릿도 되돌려 지울 수 있다. 웹 배포 창도 같은 규칙(`templateNode`)으로 풀을 거른다. 바꾸기에서 `spec.node`를 지우지는 않았다.
- **되돌리기의 표시 되돌리기**: `template: false`를 쓴 뒤 연결 복제가 다시 보이면, VM 스펙이 이 Task가 쓴 것과 같을 때만 `true`로 되돌린다(상태 쓰기도 revision을 올리므로 revision 대신 스펙을 견줌). 그새 스펙이 바뀌었으면 덮지 않고 `"…; VM \"x\" was left as a regular VM because it may have changed meanwhile — make it a template again"`로 실패한다. 첫 쓰기가 오류여도 VM을 다시 읽어 표시가 지워졌으면 다시 보고, 이미 보통 VM인 VM의 되돌리기 Task와 끊긴(`interrupted`·시간 초과) 되돌리기 Task도 다시 보아 남은 복제를 오류에 적는다(표시는 되돌리지 않음). 끝나지 않은 `vm-untemplate` Task가 있으면 켜기·재시작·PUT·CD·바꾸기·지우기·둘째 되돌리기가 409 `vm_busy`다. `vm start`는 쓰기 직전의 스펙으로 템플릿인지 다시 본다.
- **카탈로그 URL**: 이미지·체크섬 파일 URL의 사용자 정보는 400(`urls must not contain a user name or password`)이고, 관리자(설정 쓰기 권한)가 아니면 `GET /image-catalog`·`GET /settings`가 URL을 `redact_url`로 가린다(쿼리 문자열). watch의 설정 이벤트는 받기 Task의 URL처럼 늘 가린다. URL은 `url` 크레이트로 해석하고(호스트 필요, 제어 문자·공백·`https:///` 거절), id는 `.`을 `-`로 바꾸면 리소스 이름이 되는 57자까지(`_`·끝의 `.` 안 됨 — 기본 볼륨 이름이 늘 유효), 제목 128자·배포판·버전 64자·기본 사용자 32자까지다.
