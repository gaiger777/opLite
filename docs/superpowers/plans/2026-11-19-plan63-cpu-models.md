# Orbivisor 계획 63: 게스트 CPU 모델·플래그 숨기기·클러스터 CPU 기준 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 게스트 CPU가 `host`·`max` 둘뿐이던 것을 **명명 CPU 모델(버전 고정)·`x86-64-v*` 수준·`cluster`**로 넓히고, VM과 클러스터 전체에서 **CPU 기능을 숨기며**(Java4Shell CVE-2026-114938의 `avx512vbmi`), 세대가 섞인 클러스터에 **클러스터 CPU 기준(EVC)**을 두고, 배치·마이그레이션·HA·비우기·균형 조정이 대상 노드가 VM의 CPU를 켤 수 있는지 보게 한다.

**Architecture:**
- **모델 목록을 코드에 박지 않는다(사용자 결정).** 노드마다 자기 QEMU에 QMP로 묻고(`query-cpu-definitions`·모델마다 정적 확장·`full max`·`-cpu help`) 결과를 이름이 SHA-256인 `CpuCatalog` 리소스 하나로 Raft에 올린다. `NodeStatus.cpu`는 해시만 든다. QEMU 바이너리가 바뀌면 다시 읽는다. 예외는 얼린 `x86-64-v*` 넷(Proxmox와 같은 정의).
- **순수 함수 셋**(`opl_types::cpu`): `resolve`(모델 펴기 → 클러스터 숨김 목록 → VM 플래그, 숨김이 이김), `can_run`(카탈로그로 켤 수 있는지), `baseline`(auto·고정 기준). 작업자·스케줄러·API가 같은 `CpuContext`로 부른다.
- **켤 때 해석해 기록한다.** `RunningHardware.cpu_resolved`가 실제로 쓴 `-cpu`이고 마이그레이션 받는 쪽은 그대로 쓴다. 명명 모델·수준은 x86_64에서 `enforce`. 차이는 보류 `cpu`(`Restart` = 전원 껐다 켜기)이고 기준·숨김 목록 변경은 각 노드의 검사 루프가 다시 셈한다.
- **리더가 기준을 기록한다.** janitor 틱(10초)이 `ClusterStatus.cpu_baselines`를 쓰고, 바뀌면 기록 Task `cpu-baseline`과 감사 `cpu.baseline`. 약한 노드가 들어오면 auto는 기준을 내리고 경고한다(합류 거부 없음, 사용자 결정).
- **별칭은 만들 때 버전 모델로 고정한다**(`Icelake-Server` → `-v1`, 모호하면 400).
- **기능 하나** `cpu-models`가 `host`·`max` 밖의 CPU·설정 `cpu`·카탈로그·기준·새 Task를 모두 가린다(섞인 클러스터는 409 `cluster_upgrading`, 노드는 아무것도 쓰지 않는다).

**Tech Stack:** Rust(opl-types·opl-qemu·opl-driver·opl-reconciler·opl-api·oplctl·opld), QEMU QMP, React 19·TanStack Query, Vitest·msw, Playwright(opld `web-it`). 새 의존성 없음.

**설계 문서:** `docs/superpowers/specs/2026-11-19-cpu-models-design.md`
**선행 계획:** 계획 1~62 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| `CpuModel` 넓히기(`cluster`·`x86-64-v2|v2-aes|v3|v4`·QEMU 이름, 하위 호환), `CpuSpec.flags`(`+`/`-`, 64개) | 사용자 정의 모델 객체(Proxmox `cpu-models.conf`), `phys-bits`·`hv-vendor-id`·Hyper-V enlightenment |
| 노드 CPU 탐침(QMP), 해시로 나눈 카탈로그, QEMU 바뀜 다시 읽기, 관리자 다시 읽기 Task `node-cpu-probe` | 머신 유형 버전 고정(`pc-q35-8.2`) |
| 별칭 → 버전 모델 고정, 모호·모름·다른 아키텍처 400 | 실행 중 CPUID 바꾸기(불가능, 늘 전원 껐다 켜기) |
| 클러스터 기준 off·auto·고정, 클러스터 숨김 목록, 리더 기록과 `cpu-baseline` 기록 Task | aarch64 auto 기준, VM별 기준(per-VM EVC), 폴더 범위 숨김 목록 |
| 배치·마이그레이션·HA·비우기·균형 조정의 CPU 확인, `host` VM의 CPU 차이 경고 | `host` VM의 교차 CPU 마이그레이션 막기 |
| 보류 `cpu`의 "전원 껐다 켜기" 표시, VM 목록 거름·일괄 재시작 | 예약된 일괄 전원 껐다 켜기, 게스트 안 JVM 옵션 관리 |
| API·CLI·웹(VM 폼·상세·목록·마이그레이션, 노드·클러스터 CPU, 설정), 템플릿·복제·가져오기 칸 | VMware VMX의 EVC·`featMask` 가져오기, 노드 목록의 "CPU 모델" 열 |

## 실행 환경

- **이 Mac(aarch64 HVF, Homebrew QEMU 11.1.2)에서 하는 것:** 모든 Rust·웹 변경과 `cargo test --workspace`, 탐침 고정물 기록(x86_64 TCG·aarch64 TCG·aarch64 HVF), `cargo test -p opld --features cpu-it --test cpu`(TCG로 aarch64 `neoverse-n1`·노드 둘 마이그레이션), `pnpm -C web typecheck`·`lint`·`test`, 로컬 브라우저 E2E.
- **배포 서버 svc01(Linux aarch64, `qemu-tcg`):** 일반 시험 묶음 전체와 실제 aarch64 TCG 탐침. x86 KVM의 못 쓰는 기능·`avx512vbmi` 숨기기·x86 auto 기준·`host` 교차 CPU 경고는 여기서 볼 수 없다.
- **GitHub(Ubuntu 24.04 x86_64 KVM):** `linux-kvm` FEATURES에 `opld/cpu-it`(실제 KVM 탐침, vCPU `qom-get`, 핫플러그, `enforce`), `install-media`의 기존 단계가 실제 게스트 시험 `a_hidden_cpu_flag_is_absent_in_a_real_guest`를 돌린다.
- **테스트 VM:** 두 노드(svc01·svc02)에 배포해 기능 `cpu-models`, 노드 카탈로그, 기존 VM이 그대로 도는지 본다.

## 설계 결정

- **코드의 모델 표 대신 노드가 QEMU에 묻기(사용자 결정):** QEMU를 올리면 새 모델이 Orbivisor 업데이트 없이 들어온다. 얼린 `x86-64-v*`만 예외.
- **카탈로그를 `NodeStatus`에 통째로·peer RPC 대신 해시로 나눈 Raft 리소스:** 상태 쓰기마다 40 KB를 싣지 않고, 스케줄러·HA가 다른 노드의 응답에 매이지 않으며 오프라인 노드도 설명한다.
- **기준 셈은 공통 명명 모델 + 교집합 플래그:** x86·Arm에는 `query-cpu-model-baseline`이 없고, 플래그만 쓰면 모델의 family·model·캐시 정보를 잃는다.
- **약한 노드: 합류 거부 대신 기준을 내리고 경고(사용자 결정):** 실행 중 VM은 기록된 CPU로 지켜지고, 대가는 다음 전원 껐다 켜기의 낮은 CPU다.
- **켤 때마다 셈 대신 리더 기록:** 모든 노드가 같은 값을 쓰고 바뀐 때·까닭이 남는다.
- **늘 `enforce`:** 조용히 다른 CPUID로 켜지는 것보다 켜지 않는 편이 낫다(aarch64는 속성이 없어 뺀다).
- **보류 이유 새 변형 대신 `Restart` + 문구:** 옛 노드가 VM 상태를 풀지 못하는 일을 피하고, `Restart`는 이미 전원 껐다 켜기다.
- **Proxmox식 허용 목록 대신 QEMU가 아는 모든 플래그:** 숨기기가 목적이라 허용 목록이 늘 뒤처진다.

## Task

각 Task(스테이지)는 하나의 커밋이며, 앞 Task의 커밋 위에서 순서대로 한다. 스테이지마다 혼자 컴파일되고 시험된다. 새 API는 스테이지 5에서 연다(그 전에는 기능 게이트와 임시 막기가 `host`·`max` 밖을 막는다). 설계와 다른 것·결정은 설계 문서 §18에 스테이지마다 남긴다. 설계 문서 커밋은 `8ed4a59`.

- [x] **Task 1: CPU 모델·플래그·카탈로그·클러스터 기준 타입과 해석, 기능 `cpu-models`(opl-types)** — `cpu.rs`(`CpuModel` 문자열 serde, `X86Level`(Proxmox `CPUConfig.pm` `13007a1`과 대조, `pni` 포함), `CpuFlag`·`normalize_flag`·`validate_flags`·`normalize_mask`, `ResolvedCpu`, `NodeCpu`, `Bits`, `CpuCatalogSpec`(`Kind::CpuCatalog`), `CpuSettings`, `Baseline`, `Accel`), 순수 `resolve`·`can_run`·`baseline`·`pending_change`, serde 하위 호환, API 임시 막기. (`768296b`)
- [x] **Task 2: 노드 CPU 탐침 — QMP 모델·플래그 읽기, 카탈로그 해시 공유, QEMU 바뀜 다시 읽기(opl-qemu·opl-driver·opld)** — `opl_qemu::cpu`(`build_catalog`, 고정물 셋), `Hypervisor::cpu_catalog`(QEMU·Mock), opld `CpuWatch`(등록·검사 루프·10분 다시 시도·기능 게이트), 카탈로그 만들기, janitor 정리(1시간), 크기 측정. (`8110e5e`)
- [x] **Task 3: `-cpu` 모델·플래그·enforce 명령줄, 켤 때 해석과 기록, 받는 쪽은 기록 그대로, CPU 보류 갱신(opl-qemu·opl-reconciler)** — `cpu_option`, `VmRuntimeSpec.cpu_resolved`, `opl_reconciler::cpu::CpuContext`·`refresh_pending`, 켤 때 확인(못 켜면 전원 끔), 핫플러그 Task의 `wanted`, `Hypervisor::vcpu_props`, `opld/cpu-it`(`tests/cpu.rs`, CI `linux-kvm`). (`2b29ea6`)
- [x] **Task 4: 배치·마이그레이션·HA·비우기·균형 조정의 CPU 확인, 리더의 클러스터 기준 기록과 `cpu-baseline` 기록 Task, `host` VM 경고(opl-api)** — `Inventory` 카탈로그·기준·숨김 목록, `migration_fits`·`replica_migration_fits`·`cpu_receives`, 고정된 꺼진 VM 다시 배치, `cpu_baseline.rs`(`merge`·기록 Task·감사), `host_cpu_warning`, 스테이지 3 탐침 고침(임시 막기 좁히기). (`b2841f6`)
- [x] **Task 5: CPU 모델 API·별칭 고정·설정 `cpu`·노드 다시 읽기, `vm --cpu-model/--cpu-flags`·`cpu`·`node cpu` 명령(opl-api·oplctl·opld)** — `opl_api::cpu`(§13 오류 코드, `pin_model`, `check_cpu`), `/vms/{key}/cpu`, `/nodes/{key}/cpu[/refresh]`(Task `node-cpu-probe`), `/cpu/models|flags|baseline`, `PATCH /settings` `cpu`(`affected_running_vms`, 409 `cpu_baseline_unsupported`·`force`), 템플릿·복제·가져오기 칸, 경로표 권한, `no_node`의 노드마다 까닭, `oplctl/src/cpu.rs`, E2E를 CLI로, 실제 게스트 시험, 스택 넘침 둘 미리 막기. (`57666a0`)
- [x] **Task 6: VM 폼·설정의 CPU 모델과 플래그, 전원 껐다 켜기 보류 표시와 거름, 노드·클러스터 CPU 보기, 설정 › 일반의 CPU 기준과 숨김 목록(web)** — `web/src/cpu/`(`CpuModelPicker`·`CpuFlagsInput`·`NodeCpuSection`·`ClusterCpuSection`·`CpuSettingsSection`), VM 상세 띠·"지금 전원 껐다 켜기", VM 목록 `cpu=pending`, 마이그레이션 대화상자, Task 문구, i18n, vitest 17개, Playwright 몇 줄. (`4eb032a`)
- [x] **Task 7: 가이드·README·아키텍처·계획 문서** — "CPU 모델과 플래그", 따라 하기 "취약점 때문에 CPU 기능 숨기기", "클러스터 CPU 기준", "CPU 모델 화면", 개발 문서의 새 시험 명령, README 상태·기능 표, `docs/index.md`, `docs/architecture.md` §4·§9.1·§9.6·§14(main의 아키텍처 커밋 둘을 옮겨 옴), 설계 §18 스테이지 7, 이 계획 문서와 실행 기록.

## 완료 기준 (계획 63)

- 옛 JSON(`VmSpec` `"model":"host"`·`RunningHardware`·`SettingsSpec`·`NodeStatus`)이 바이트 그대로이고, 업그레이드 중(`cpu-models`를 모르는 노드가 있음) `host`·`max` VM의 명령줄·기록이 계획 62와 같으며 노드는 카탈로그·`NodeStatus.cpu`를 쓰지 않는다. 새 쓰기는 409 `cluster_upgrading`이다.
- `resolve`·`can_run`·`baseline` 표 시험이 통과한다: 숨김이 VM `+`보다 이기고, 빠진 기능을 끄면 켤 수 있으며, 약한 노드 합류·벤더 섞임·TCG 노드·카탈로그 없는 노드·`auto_flags`·고정이 맞다.
- 탐침 고정물(x86_64 TCG·aarch64 TCG·aarch64 HVF)로 만든 카탈로그의 해시가 고정이고, 실제 QEMU 탐침이 이 Mac·GitHub KVM에서 된다.
- 실제 QEMU에서 `-flag`인 VM의 모든 vCPU(실행 중 붙인 vCPU 포함)가 그 속성이 거짓이고, 숨김 목록을 더하면 검사 루프가 보류를 쓰고 전원 껐다 켜기에 적용되며, 마이그레이션 받는 쪽은 기록된 CPU 그대로이고, 못 주는 기능이 있는 모델은 `enforce`와 카탈로그 확인으로 켜지 않는다.
- 실제 Ubuntu 게스트의 `/proc/cpuinfo`에 숨긴 플래그가 없고, 실행 중 지우면 보류로 남다가 `vm reboot` 뒤 보인다.
- API: 별칭이 버전 모델로 고정되고 모호·모름·다른 아키텍처가 400이며, 노드 셋에 약한 노드가 들어오면 auto 기준이 내려가고 기록 Task·감사가 남으며, 실행 중 `cluster` VM은 약한 노드로 옮길 수 없고(409) 다른 노드로는 옮기며, `host` VM은 경고와 함께 옮긴다. 고정 기준을 켤 수 없는 노드가 있으면 409, `force`로 넘는다. 폴더 사용자 권한 행렬(`/cpu/*`)이 맞다.
- 웹: VM 폼의 모델 고르기·플래그 칩, 보류 띠·"CPU 변경 대기" 거름, 마이그레이션 대화상자의 막힌 노드, 설정의 CPU 칸(409·"그래도 고정"·영향 VM 링크)이 vitest로, 노드 상세 "CPU 모델"과 설정 기준 칸이 브라우저 E2E로 보인다.
- `cargo fmt`·`clippy -D warnings`·`cargo test --workspace`, `pnpm typecheck`·`lint`·`test`, 로컬 브라우저 E2E가 통과하고 svc01 일반 시험과 GitHub `linux-kvm`·`install-media`가 통과한다.

## 알고 받아들인 제한

- `host`·`max` VM을 CPU가 다른 노드로 옮기는 것은 막지 않고 경고만 한다(마이그레이션 API는 감사 `cpu-warning`, 비우기·균형 조정·HA는 로그에만).
- aarch64는 auto 기준이 없고 고정만 되며, QEMU 모델은 모든 대상 노드가 TCG일 때만 켠다(KVM·HVF aarch64는 `host`·`max`). aarch64 모델에는 `enforce`가 없고, 숨김 이름이 그 모델에 없는 속성이면(`neoverse-n1`의 `pauth`) QEMU가 켜지 않는데 aarch64 카탈로그에 모델별 기능 표가 없어 `can_run`이 미리 알지 못한다.
- 메모리 스냅샷 되돌리기는 찍을 때의 CPU로 켠다(새 기준·숨김 목록을 쓰지 않는다).
- `GET /vms/{key}/cpu`의 "실행 중"은 기록(`cpu_resolved`)이고 QMP로 읽은 실제 vCPU 속성이 아니다.
- 리더 기록은 janitor 틱이라 노드·설정이 바뀐 뒤 10초 안팎 늦다. 기준 바뀜에 걸리는 경보 규칙 조건은 없다(활동·감사에만).
- 노드 다시 읽기는 검사 회차를 기다려 10초 안팎 걸린다(시간 제한 75초 — 도는 탐침과 새 탐침 둘 + 회차 셋, 리뷰 1 M3).
- 웹은 별칭을 보이지 않고, 노드 목록에 "CPU 모델" 열이 없으며, CPU 화면에서 가이드로 가는 도움말 링크가 없다. 일괄 동작에 따로 "전원 껐다 켜기"는 없고 "재시작"이 그것이다. CLI에는 보류 VM 거름이 없다.
- KVM 탐침 고정물(`x86_64-kvm.json`)은 기록하지 않았다. 약한 노드는 합성 카탈로그(Mock)로만 본다.
- 이 Mac(HVF)의 Playwright는 숨길 안전한 플래그가 없어 "숨김 목록 저장 → 대기 거름 → 일괄 재시작" 흐름을 브라우저로 돌리지 않는다(vitest가 본다).

## 실행 후 기록 (2026-11-19)

- 7개 스테이지를 단계 커밋으로 구현했다(설계 `8ed4a59`, `768296b` 타입 … `4eb032a` 웹, 이 문서 커밋). 설계와 다른 것·결정은 설계 문서 §18에 스테이지마다 남겼다. `docs/architecture.md`가 이 브랜치의 기준(`e04f9a8`) 뒤 main에 들어와 그 커밋 둘(`104324e`·`6695eae`)을 이 브랜치에 옮긴 뒤 고쳤다.
- 새 시험(브랜치 diff의 `#[test]`·`#[tokio::test]` 수): opl-types 20, opl-qemu 16, opl-driver 2, opl-reconciler 9, opl-api 20, oplctl 6, opld 14 — Rust 87개. 웹 vitest 17개(`src/cpu/cpu.test.tsx`)와 Playwright `vm.spec.ts` 몇 줄. 워크스페이스 시험 총수는 svc01 일반 시험 줄에 적는다.
- 카탈로그 크기(이 Mac의 QEMU 11.1.2): x86_64 TCG 모델 186개·플래그 407개(`max`가 주는 것 89개) **41,134바이트**(설계 어림 40 KB), aarch64 TCG 모델 12개·플래그 23개 874바이트, aarch64 HVF 12개·3개 980바이트. aarch64는 정의 38개 가운데 64비트 `virt`로 켤 수 있는 12개만 남는다. x86 고정물은 220 KB(거짓 값을 버리지 않으면 2.2 MB).
- 탐침 시간: x86 정적 확장 186번을 포함해 **0.3초 안쪽**(설계 어림 2초, 시간 제한 30초).
- 묶음 크기(`index.html`이 받는 JS gzip 합): 392,618 → 397,616 B(**+4,998 B**), CSS 14,461 → 14,477 B(+16).
- **이른 GitHub 탐침(스테이지 3 뒤):** `linux-kvm`의 `opld/cpu-it` `masked_flags_are_off_in_every_vcpu`가 스테이지 1의 임시 API 막기에 걸렸다 — `vm set --cores 2`가 보내는 전체 스펙이 저장된 모델·플래그를 그대로 담아도 400이었다. 저장된 VM과 같으면 받도록 좁혔다(스테이지 4 커밋, 시험 `an_update_that_keeps_the_stored_cpu_model_and_flags_is_accepted`). 같은 회차에 `install-media`의 vTPM 시험 `a_cloud_image_boots_with_secure_boot_a_tpm_and_the_guest_agent`가 "no report 1 in 900s"로 실패했다. 코드 원인은 찾지 못했다 — 그 VM의 기록은 `cpu_resolved {model: host}`(enforce·플래그 없음)라 `-cpu host`로 스테이지 3 전과 바이트가 같고, 펌웨어·SMM·TPM 코드는 건드리지 않았으며 TPM 상태가 두 번 기록되어 swtpm·OVMF는 돌았다. 게스트 보고 스크립트가 cloud-init이 네트워크에서 `qemu-guest-agent`를 설치한 뒤 돌므로 apt 미러가 느리거나 실패한 흔들림으로 보고 다시 돌려 가리기로 했다. **풀림:** 스테이지 4 뒤 탐침에서 같은 `install-media` 시험이 통과했다 — 흔들림이었다(코드 원인 없음).
- **이른 GitHub 탐침(스테이지 4 뒤):** `opld/cpu-it` `enforce_refuses_a_model_with_missing_features`의 기대가 옛것이었다. 시험은 노드를 고정하지 않은 VM이라 스케줄러의 일반 `no_node` 문구만 받았고 CPU 까닭을 찾지 못했다. 스테이지 5에서 `no_node`가 이 아키텍처 노드마다의 까닭(`node "n1" cannot run CPU model Broadwell-v1 (missing hle, rtm)`)과 `details.nodes`를 주게 하고, 시험이 고정된 꺼진 VM(409 `no_capacity`)과 고르는 켜기(`no_node`) 둘 다 보게 했다.
- **스택 넘침 둘을 미리 막음(스테이지 5):** 디버그 빌드에서 opl-api `replica_receive`와 opld `member::tests::a_restart_with_new_links_signs_the_node_cert_again`이 시험 스레드 스택(2 MiB)을 넘었다. 계획 62처럼 `Env::start`·`Member::start`를 `Box::pin`으로 힙에 두었다. CLI 새 명령은 `run_cli` 안에서 dispatch해 `templates` 시험의 넘침이 다시 나지 않는다.
- 리뷰 1(서버·웹 나눠서): 서버 중대 1·중요 3·경미 5 — 노드 QEMU가 모르는 끈 플래그도 `can_run`에서 확인(클러스터 숨김에서 온 것만 그 노드에서 빼고, 그때 `host` VM이 그 이름을 아는 노드로 옮겨 숨김이 풀리는 길을 막음), 카탈로그를 해시보다 먼저 기록, 껐다 켜기는 지금 노드가 새 CPU를 못 돌리면 끄기 전에 409, HA 검사 옮기기는 켠 CPU로, 노드 이름 숨기기·다시 읽기 75초·옛 스냅샷 CPU 확인·시험 경로 Box::pin(`0dc89f1`). 웹 중대 1·중요 3·경미 7 — 새 VM은 기준이 있을 때만 `cluster`(서버도 기준 없으면 `current`를 뺌), `+플래그` 입력, 설정 저장의 동시 바꿈 확인(412), 지정 기준의 플래그 유지 외(`2942685`).
- 리뷰 2(리뷰 1 수정 대부분 옳다고 확인): 서버 중요 4·경미 5 — 켤 때 뺀 숨김 이름(`dropped_mask`)을 기록해 그 이름을 아는 노드로의 라이브 이동·메모리 복원을 막음, 기준 보류는 그 노드가 온라인이고 10분 안일 때 그 아키텍처만, 비관리자 `+플래그`는 Proxmox 허용 목록 12개만·`invtsc`는 라이브 이동 막힘으로 표시, 일꾼 재시작은 새 CPU를 못 돌리면 기록된 CPU로 켜고 보류 유지, 비우기의 CPU 까닭·감사 보강·실제 게스트 시험의 플래그 고르기·카탈로그 해시 캐시(`8a8771e`→리베이스 `f87c64a`). 웹 중요 3·경미 5 — 바꾼 모드만 보내기(리뷰 1 #5 회귀), 내 저장 뒤 판 갱신, 복제·템플릿 배포·가져오기의 CPU 칸, 아키텍처 전환 구석, 비관리자의 켤 수 있는 플래그 안내(`5ef203c`).
- 리베이스: main의 `f83a523`(schema.d.ts)·아키텍처 문서 두 커밋 위로 옮겼다(브랜치에 cherry-pick해 둔 둘은 같은 패치라 빠짐).
- svc01 일반 시험: 처음(`5ef203c`)에 clippy·시험 빌드가 rustc 1.99의 "queries overflow the depth limit"로 실패 — opl-api 시험 공용 `start_runner_config`의 `TaskRunner::spawn` future 타입이 깊어 그 async 본문의 레이아웃 셈이 쿼리 깊이 한도를 넘음(이 Mac의 rustc는 괜찮음). 그 future를 `Box::pin`으로 나눠(`66a54bc`, 시험 코드만) fmt·clippy·시험 3537개·starve·규모 100·저장소 시험 모두 통과.
- GitHub 탐침: 스테이지 3·4·5마다 미리 돌림(위 기록) → 마지막 `5ef203c`에서 linux-kvm(`cpu-it` 넷: 숨긴 플래그가 모든 vCPU에서 꺼짐, `enforce` 거절, 받는 쪽은 기록 그대로 둘)·install-media(실제 게스트에서 `avx512vbmi`를 숨기고 `/proc/cpuinfo`에서 사라짐·껐다 켜면 돌아옴)·브라우저 E2E 둘·ceph·ldap·package·iso-e2e(bios·uefi)·catalog-links 모두 통과. 중간의 linux-kvm 실패 두 번은 ZFS 연결 복제(알려진 udev 경쟁)와 Ceph apt 미러 15분 제한이었다.
- **테스트 VM 배포(svc01·svc02):** main `1361773`을 svc01에서 릴리스로 빌드(웹 index-DXVGJBv_.js 포함)해 svc02 → svc01 순서로 바이너리만 바꿨다. orbivisord sha256 `b80020a6…573`, orbivisorctl `6faa6a78…5a9`. 두 노드 헬스 `ok`, 두 voter online, 두 노드 모두 기능 `cpu-models`를 알리고 같은 카탈로그 해시(`43928290…`, QEMU 8.2.2, TCG)를 낸다. `/cpu/baseline`은 `off`(aarch64, cluster VM 0), `/cpu/models`가 답한다. svc02의 test-vm은 QEMU PID 916 그대로 실행 중이고 `/vms/test-vm/cpu`가 spec `host` → TCG에서 `max`로 풀리며 보류 바뀜은 없다(업그레이드 뒤 거짓 "껐다 켜기" 없음). 저널에 경고·오류 없음.

## 다음 계획으로 넘길 사항

- **계획 64: NUMA·vCPU 고정·hugepages** — 노드 CPU 탐침(계획 63의 `CpuWatch`·카탈로그 길)을 다시 써서 노드의 NUMA 토폴로지·hugepage 크기를 알리고, VM의 게스트 NUMA 노드·vCPU 고정·hugepage 메모리와 그것을 보는 배치를 넣는다.
- KVM 탐침 고정물 `x86_64-kvm.json`을 GitHub `linux-kvm` 로그에서 옮겨 KVM 카탈로그 파싱을 이 Mac 시험에 넣기.
- aarch64 숨김 이름을 모델의 `full` 확장 속성으로 걸러 `neoverse-n1`의 `pauth` 같은 켜기 실패를 미리 막기.
- `GET /vms/{key}/cpu`의 "실행 중"을 노드 RPC + `vcpu_props`로 실제 vCPU 속성에서 읽기.
- 기준 바뀜·켤 수 없는 노드의 경보 규칙 조건, 웹 노드 목록의 "CPU 모델" 열, 웹 문서 도움말 링크, CLI의 보류 VM 거름(`vm list --cpu-pending`)과 일괄 전원 껐다 켜기.
- 설계 §16의 나중 일: `host` VM의 교차 CPU 마이그레이션 막기(선택 설정), aarch64 auto 기준(KVM arm64의 쓰기 가능한 ID 레지스터), 사용자 정의 CPU 모델 객체, `phys-bits`·`hv-vendor-id`·Hyper-V enlightenment, 머신 유형 버전 고정, VMX EVC·`featMask` 가져오기, VM별 기준, 숨김 목록의 폴더 범위, 노드 마이크로코드·취약점 상태(`/sys/devices/system/cpu/vulnerabilities`) 보이기, 보류 VM의 예약된 일괄 전원 껐다 켜기.
