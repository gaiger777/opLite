# 게스트 CPU 모델·플래그 숨기기·클러스터 기준(EVC) 설계 (계획 63)

작성일: 2026-11-19
상위 문서: `2026-10-20-opl-node-core-design.md`(VM 스펙·QEMU 명령줄·드라이버), `2026-10-30-live-migration-design.md`, `2026-11-11-availability-scale-design.md`(계획 53 배치·계획 54 하드웨어 기록과 보류), `2026-11-17-templates-linked-clones-design.md`(계획 58), `2026-11-18-vtpm-secure-boot-guest-agent-design.md`(계획 59 `firmware_caps`), `2026-11-18-ui-ux-redesign-design.md`(계획 61), `2026-11-18-folders-permissions-quotas-2fa-design.md`(계획 62)
기준 트리: 계획 62 끝(`e04f9a8`). 이 문서는 구현 전 설계다. 스테이지 보고는 구현하면서 덧붙인다.

## 1. 목적

사용자가 Java4Shell(CVE-2026-114938, JDK 17.0.6 이상의 AVX-512 VBMI Base64 내장 함수 버그)을 물으며 "호스트가 게스트에게 `avx512vbmi`를 숨길 수 있나"를 물었다. 지금은 못 한다. 게스트 CPU는 `CpuModel::Host | Max` 둘뿐이고(`opl-types/src/vm.rs`), 명령줄은 `-cpu host` 또는 `-cpu max`다(`opl-qemu/src/cmdline.rs`, TCG에서 `host`는 `max`). 그래서 (1) CPU 기능을 숨길 수 없고, (2) 세대가 다른 노드 사이의 라이브 마이그레이션이 안전한지 알 수도 막을 수도 없고, (3) 클러스터에 옛 노드가 섞여도 공통 CPU를 정할 길이 없다.

이 계획은 **명명 CPU 모델(버전 고정)**, **플래그 켜기·끄기**, **노드마다 QEMU에 물어 알리는 CPU 카탈로그**, **클러스터 기준(EVC 같은 것)**, **마이그레이션·HA·배치의 CPU 확인**을 넣는다. 사용자와 정한 원칙: **모델 목록을 코드에 박지 않는다.** 각 노드가 자기 QEMU에 묻고 결과를 알린다(계획 59 `firmware_caps`와 같은 길). 그러면 QEMU를 올리는 것만으로 새 CPU 모델이 들어오고 Orbivisor는 바뀌지 않는다.

| | Proxmox VE 8 | vSphere 8 | OpenStack Nova | Orbivisor 계획 63 |
|---|---|---|---|---|
| 모델 | QEMU 모델 목록(버전 없는 이름), `x86-64-v2-AES`(기본)~`v4`, 사용자 정의 모델 | 호스트 CPU 그대로 + EVC 마스크 | `cpu_mode`: `host-passthrough`·`host-model`·`custom`(+ `cpu_models`) | `host`·`max`·`x86-64-v2`~`v4`·QEMU 모델(버전 고정)·`cluster`(§5) |
| 플래그 | VM은 허용 목록 몇 개(`pcid`·`spec-ctrl`·`aes` …), 사용자 정의 모델은 아무 플래그 | VM별 CPUID 마스크(옛 기능) | `cpu_model_extra_flags`(+/-) | VM `+flag`/`-flag`, 클러스터 숨김 목록(§11), QEMU가 아는 이름만 |
| 클러스터 공통 CPU | 없음(관리자가 모델을 고름) | EVC 모드(클러스터·VM별), 약한 호스트는 합류 거부 | 없음(trait로 노드 거르기) | `cpu.baseline` off·auto·pinned, 약한 노드는 경고만(§9) |
| 마이그레이션 확인 | 없음 | EVC·CPU 호환 검사 | 스케줄러가 CPU trait로 거름, libvirt 비교 | 대상 노드가 VM의 **해석된** 모델·플래그를 켤 수 있을 때만(§10) |
| 모델 정보 | 코드에 박힌 표 | 하드웨어 표 | libvirt `domcapabilities`(QMP) | 노드가 QMP로 읽어 Raft에 해시로 나눔(§6·§7) |

## 2. 범위

| 넣는 것 | 넣지 않는 것(§16) |
|---|---|
| `CpuModel` 넓히기(하위 호환), `CpuSpec.flags` | 사용자 정의 모델 객체(Proxmox `cpu-models.conf`), `phys-bits`·`hv-vendor-id`·Hyper-V enlightenment |
| 노드 CPU 탐침(QMP), 카탈로그 해시 공유, QEMU 바뀜 감지와 다시 읽기 | 머신 유형 버전 고정(`pc-q35-8.2`) |
| 별칭 → 버전 모델 고정(만들 때) | 실행 중 CPUID 바꾸기(불가능, 늘 전원 껐다 켜기) |
| 클러스터 기준 off·auto·pinned, 클러스터 숨김 목록 | aarch64 auto 기준(QEMU가 순위 정보를 주지 않음, §4) |
| 배치·마이그레이션·HA·비우기·균형 조정의 CPU 확인, `host` VM의 CPU 차이 경고 | `host` VM의 교차 CPU 마이그레이션 막기(오늘 동작 유지, 경고만) |
| 보류 `cpu`의 "전원 껐다 켜기" 표시, VM 목록 거름·일괄 재시작 | 게스트 안 JVM 옵션 관리 |
| API·CLI·웹(VM 폼·설정, 노드·클러스터 CPU 보기), 템플릿·복제·가져오기·복원 | VMware EVC 모드 이름 옮겨 오기(가져오기는 기본 모델) |

**기능 하나**(`opl_types::node`, `FEATURES` 끝): **`cpu-models`**(`CPU_MODELS`). `host`·`max`가 아니거나 플래그가 있는 `cpu`, 설정 `cpu`, 새 종류 `CpuCatalog`, `ClusterStatus.cpu_baselines`, 새 Task `cpu-baseline`·`node-cpu-probe`를 가린다. 모든 노드가 알릴 때만 쓰고 아니면 409 `cluster_upgrading`이다(`require_feature`, 검증보다 먼저 — 계획 58~62와 같다). 까닭은 §12.

## 3. 지금 있는 것 (계획 62 끝)

- **타입**: `CpuSpec { sockets, cores, model: CpuModel, max_vcpus }`는 `deny_unknown_fields`다. `CpuModel`은 `Copy`인 kebab 문자열 열거형(`host` 기본, `max`)이다. `VmSpec`·`SettingsSpec`도 `deny_unknown_fields`라 모르는 칸이나 모르는 모델 문자열이 있으면 옛 노드가 VM·설정을 풀지 못한다.
- **명령줄**: `match (model, accel)` → `(Host, Tcg) | (Max, _) => "max"`, `(Host, _) => "host"`. 플래그·`enforce`는 없다.
- **하드웨어 기록(계획 54)**: `RunningHardware.cpu`가 켤 때의 `CpuSpec`이다. 받는 쪽 마이그레이션은 스펙이 아니라 이 기록으로 켠다(`opl-reconciler/src/resolve.rs`의 `spec.cpu = hw.cpu.clone()`). `hotplug::cpu_changes`는 소켓·모델이 기록과 다르면 보류 `cpu`(`PendingReason::Restart`)를 만든다. `hotplug::can_receive(hw, node)`가 마이그레이션 대상의 핫플러그 능력을 본다.
- **재시작의 뜻**: 동작 `reboot`(Task `VmReboot`)은 ACPI로 끄고 QEMU를 다시 띄운다(`tasks.rs`의 `reboot` = 전원 껐다 켜기). 게스트 안의 재부팅은 QMP `RESET`일 뿐 같은 QEMU 프로세스다. 그래서 `Restart` 보류는 Orbivisor `reboot`·`stop`+`start`·HA 복구 켜기에서만 적용되고 게스트 안 재부팅으로는 적용되지 않는다. 화면 문구("다음 재시작 때 적용")는 이 차이를 말하지 않는다.
- **노드 능력의 본(계획 54·59)**: `Hypervisor::hotplug_caps()`·`firmware_caps()`를 드라이버를 만든 뒤 한 번 읽고(`opld/src/member.rs`), 등록 때 `NodeStatus`에 쓰며, 검사 루프(`opld/src/checks.rs`)가 저장된 값과 다를 때만 다시 쓴다. 스케줄러의 유일한 노드 조건 함수 `scheduler::node_fits_with`가 `firmware_caps`로 TPM·Secure Boot VM을 거른다. 없으면(옛 노드·위트니스) 못 하는 것으로 본다.
- **마이그레이션**(`vms.rs` `start_migration`): HVF 노드 막기, 위트니스·HA·온라인·아키텍처 확인, `hotplug::can_receive`, `node_fits_with`, 메모리, 배치 규칙. CPU는 보지 않는다. 비우기(`tasks/drain.rs`)·HA(`ha.rs`)·배치(`placement.rs`)·복제(`tasks/clone.rs`)도 `node_fits(_with)`만 본다.
- **QEMU 버전**: `Hypervisor::qemu_version()`·`running_qemu_version(id)`가 있다.
- **만드는 곳**: 템플릿 초안(`template.rs`)은 `CpuModel::default()`, 가져오기(`vm_imports.rs`)는 `CpuModel::Host`를 박는다. 웹 `GeneralSection`의 `vm-cpu-model` 선택은 `host|max`다. CLI `vm create|set`에는 `--sockets`·`--cores`·`--max-vcpus`만 있다.
- **CI**: 실제 게스트 OS가 필요한 시험(계획 57~59)은 클라우드 이미지를 받는 `install-media` 작업에, 게스트 없는 QEMU·마이그레이션 시험은 `linux-kvm` 작업에 있다.

## 4. QEMU에서 확인한 사실

이 Mac의 Homebrew QEMU 11.1.2로 QMP를 직접 불러 확인했다(`-machine q35|virt,accel=tcg|hvf -nodefaults -display none -qmp stdio`). 설계는 이 사실 위에 선다.

- **x86_64 `query-cpu-definitions`**: 188개. 항목마다 `name`, `typename`, `unavailable-features`(이 가속기에서 켤 수 없는 기능 이름), `alias-of`(별칭일 때), `static`, `migration-safe`, `deprecated`. 버전 모델은 `Icelake-Server-v1`~`-v9`, 별칭은 `Icelake-Server`(→ q35에서 `-v1`), `Icelake-Server-noTSX`(→ `-v2`). `-cpu help`는 별칭을 "alias configured by machine type"이라 적는다: **버전 없는 이름은 머신 유형이 정하고, 지금 `q35`에서는 `-v1`(가장 옛 정의)이다.**
- **`x86-64-v2|v3|v4`는 QEMU 모델이 아니다.** Proxmox가 `qemu64` + 플래그로 만든 것이다. `qemu64`의 정적 확장에는 `cx16`·`lahf-lm`·`pni`가 이미 있다.
- **`query-cpu-model-baseline`·`query-cpu-model-comparison`은 x86·Arm에서 쓸 수 없다**("CPU model baseline is not supported on this target", s390x 전용). 그래서 기준은 Orbivisor가 직접 셈한다(§9).
- **`query-cpu-model-expansion`**: x86은 `static`(기반 `base` + 불 415개, 그중 플래그 408개와 `vendor`·`family`·`model-id` 같은 값)과 `full`이 된다. `full` `max`는 이 가속기가 줄 수 있는 모든 것이다(TCG: 참 115개, `vendor` `AuthenticAMD`). `host`는 KVM에서만 된다(TCG: "CPU model 'host' not found").
- **aarch64**: `query-cpu-definitions`에 버전·`unavailable-features`·`alias-of`가 없다(`a64fx`, `cortex-a53`…`a76`·`a710`, `neoverse-n1|n2|v1`, `host`, `max` 등 38개). `static` 확장은 안 되고 `full`만 된다. 속성은 `sve`·`sve128`…`sve2048`·`pauth`·`pauth-impdef`·`pauth-qarma3|5`·`pmu`·`aarch64`뿐이다(`full max`, TCG). HVF에서 `full max`는 `pauth`·`pmu`·`aarch64` 셋, `host`는 "requires KVM". 이름 있는 모델은 TCG에서만 켜진다(`-machine virt,accel=hvf -cpu neoverse-n1` → "Invalid CPU model … valid models are: cortex-a53, cortex-a57, host, max"). KVM arm64도 `host`·`max`만이다. `neoverse-n1`에는 `sve` 속성이 없어 `sve=off`가 오류다.
- **플래그 이름**: `-cpu help`의 "Recognized CPUID flags"(`avx512vbmi`, `avx512-vpopcntdq`, `sse4.1`, `lahf-lm` …). QEMU는 `_`와 `-`를 같게 받는다. 모르는 이름은 켜기 오류다("Property 'Icelake-Server-v7-x86_64-cpu.nonexistentflag' not found").
- **`enforce`**: 기본은 가속기가 못 주는 기능을 경고만 하고 빼고 켠다("TCG doesn't support requested feature …"). `enforce`면 켜지 않는다. 명명 모델이 노드마다 다른 CPUID로 조용히 켜지면 마이그레이션이 깨지므로 Orbivisor는 명명 모델·수준에 늘 `enforce`를 붙인다.
- **`query-cpu-definitions` 원문 크기**: x86 TCG 64 KB.

## 5. 타입 (opl-types)

- **`CpuModel`**(문자열 하나로 직렬화, `Copy`를 잃는다):
  - `Host`(`"host"`, 기본), `Max`(`"max"`) — 지금 그대로.
  - `Cluster`(`"cluster"`) — 켤 때 클러스터 기준으로 바뀐다(§9).
  - `Level(X86Level)`(`"x86-64-v2"`·`"x86-64-v2-aes"`·`"x86-64-v3"`·`"x86-64-v4"`) — x86_64만. 정의는 Proxmox VE 8과 같게 `qemu64` + 플래그로 두고(v2: `popcnt`·`sse4.1`·`sse4.2`·`ssse3`, v2-aes: + `aes`, v3: v2-aes + `avx`·`avx2`·`bmi1`·`bmi2`·`f16c`·`fma`·`abm`·`movbe`·`xsave`, v4: v3 + `avx512f`·`avx512bw`·`avx512cd`·`avx512dq`·`avx512vl`; 스테이지 1에서 Proxmox 소스 `CPUConfig.pm`과 대조), **얼린다**. psABI 수준은 새 CPU가 나와도 바뀌지 않으므로 "모델 목록을 박지 않는다"의 예외로 둔다. 바꿔야 하면 새 이름을 만든다.
  - `Named(String)` — 그 밖의 모든 이름. QEMU 이름 그대로 저장한다(대소문자는 카탈로그의 표기로 맞춤). 문법 `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`, `base`는 받지 않는다. 존재 확인은 API에서 카탈로그로 한다(§8).
- **`CpuSpec.flags: Vec<CpuFlag>`**(`#[serde(default, skip_serializing_if = "Vec::is_empty")]`, 문자열 `"-avx512vbmi"`·`"+aes"`). 이름은 소문자, `_` → `-`로 맞춘다. 64개까지, 같은 이름 두 번·켜고 끄기 동시는 400. 이름 존재는 API가 카탈로그로 본다.
- **`ResolvedCpu { model: String /* QEMU -cpu 이름 */, flags: BTreeMap<String, bool>, enforce: bool, source: CpuSource (Spec|Cluster) }`**: 켤 때 실제로 쓴 것. `RunningHardware.cpu_resolved: Option<ResolvedCpu>`(없으면 계획 63 전 기록 = `host`/`max`로 켠 것)로 기록한다. 받는 쪽은 이 값을 그대로 쓴다.
- **순수 함수 `cpu::resolve(cpu: &CpuSpec, arch, accel, baseline: Option<&Baseline>, mask: &[String]) -> Result<ResolvedCpu, CpuError>`**: `Host`(TCG면 `max`), `Max`, `Level` → `qemu64-v1` + 정의 플래그, `Named` 그대로, `Cluster` → 기준 모델·플래그(기준 off면 `host`와 같다). 그다음 클러스터 숨김 목록(§11)을 끄고, 마지막에 VM 플래그를 얹는다. 숨김 목록과 VM `+flag`가 부딪치면 숨김이 이긴다(쓰기 때는 400 `cpu_flag_masked`, 나중에 숨김을 더한 경우만 켤 때 이긴다). 숨김 이름이 그 아키텍처 카탈로그에 없으면 건너뛴다(`sve`는 x86에서 무시).
- **`cpu::can_run(resolved: &ResolvedCpu, catalog: &CpuCatalogSpec) -> Result<(), CpuMismatch>`**: `host`·`max`는 늘 된다. 명명 모델은 카탈로그에 있고, `unavailable-features`가 모두 꺼진 플래그 안에 있으며(못 쓰는 까닭이 `avx512vbmi` 하나뿐인 모델은 `-avx512vbmi`로 켤 수 있다), `true`인 플래그가 모두 `supported`(`full max`)에 있어야 한다. 플래그 아닌 항목(`type` 등)이 `unavailable-features`에 있으면 못 쓴다. aarch64 KVM·HVF 노드에서 명명 모델은 못 쓴다. `CpuMismatch`는 빠진 플래그 목록을 든다.
- **노드**: `NodeStatus.cpu: Option<NodeCpu { catalog: String /* SHA-256 hex */, qemu: String, accel: String, vendor: Option<String>, model_id: Option<String>, probed_at: u64, error: Option<String> }>`(약 250바이트, 없으면 옛 노드·위트니스 = 명명 모델·수준을 못 켬).
- **`CpuCatalogSpec`**(새 종류 `Kind::CpuCatalog`, `"cpu-catalog"`, 이름 = 해시, 누구나 읽음, watch 불가): `{ arch, accel, qemu, machine, flags: Vec<String> /* QEMU가 아는 플래그, 정렬 */, supported: Bits /* full max의 참 */, vendor: Option<String>, models: Vec<CatalogModel> }`, `CatalogModel { name, alias_of: Option<String>, vendor: Option<String>, missing: Bits /* unavailable-features */, missing_other: Vec<String> /* 플래그 아닌 항목 */, features: Option<Bits> /* x86 static 확장의 참 */, migration_safe: bool, deprecated: bool }`. `Bits`는 `flags` 순번의 비트 집합을 base64url로(408비트 = 68자). 해시는 `probed_at`을 뺀 정규 JSON의 SHA-256이다.
- **설정**: `SettingsSpec.cpu: Option<CpuSettings { baseline: BaselineMode (off|auto, 기본 off), pinned: Vec<PinnedBaseline { arch, model: CpuModel, flags: Vec<CpuFlag> }>, auto_flags: bool (기본 참), mask: Vec<String> }>`. `pinned`에 있는 아키텍처는 auto 대신 그 모델이다. `cluster`·`host`·`max`는 고정 모델이 될 수 없다.
- **클러스터 상태**: `ClusterStatus.cpu_baselines: BTreeMap<Arch, Baseline { model: String, flags: BTreeMap<String,bool>, mode: auto|pinned, nodes: Vec<Uuid>, limited_by: Vec<Uuid>, since: u64, previous: Option<(String, BTreeMap<String,bool>)>, warnings: Vec<String> }>`. 리더가 쓴다(§9).
- **보류 `cpu`**: `hotplug::cpu_changes`가 `spec.cpu.model != hw.cpu.model` 대신 `resolve(spec…) != hw.cpu_resolved`(기록이 없으면 지금 규칙)로 견준다. `detail`은 바뀐 것(`"-avx512vbmi"`, `"Cascadelake-Server-v5 → Skylake-Server-v5 (cluster baseline changed)"`)과 "power cycle required; a reboot inside the guest does not apply it"이다. `PendingReason`에 새 변형을 더하지 않는다(옛 노드가 VM 상태를 풀지 못한다). 대신 `Restart`의 화면 문구를 모두 "다음 전원 껐다 켜기"로 바꾼다 — 이미 그 뜻이다(§3).

## 6. 노드 탐침

- **opl-qemu `cpu.rs`(순수)**: QMP 응답(`query-cpu-definitions`, 모델마다 `query-cpu-model-expansion static`, `full max`, KVM이면 `full host`)과 `-cpu help`의 플래그 줄을 받아 `CpuCatalogSpec`을 만든다. 이름 정규화, 별칭, 비트 집합, 해시, aarch64 규칙(KVM·HVF면 명명 모델 `missing_other = ["accel"]`), 기록된 QMP 응답 고정물로 시험한다.
- **opl-driver `Hypervisor::cpu_catalog() -> Result<CpuCatalogSpec, DriverError>`**: QEMU를 VM과 같은 머신(`q35`·`virt`)과 가속기로 `-S -nodefaults -display none -qmp stdio`로 한 번 띄워 위 질의를 하고 끈다. libvirt가 `domcapabilities`에 쓰는 것과 같은 길이다. x86 정적 확장 188번을 포함해 2초 안팎, 시간 제한 30초. `MockHypervisor`는 고정물을 돌려준다.
- **opld**: 등록 전 한 번(`member.rs`, `firmware_caps` 옆) 읽어 `NodeCpu`를 쓰고, 카탈로그는 §7대로 올린다. 검사 루프가 주기마다 QEMU 바이너리의 (경로, 크기, mtime)을 보고 바뀌면(apt로 QEMU를 올림) 다시 읽는다. 관리자 `POST /nodes/{key}/cpu/refresh`(Task `node-cpu-probe`)로도 다시 읽는다. 실패하면 `NodeCpu.error`를 쓰고 앞 카탈로그를 그대로 둔다(처음이면 없음 = `host`·`max`만).
- 실행 중 VM은 옛 QEMU 프로세스 그대로다. 버전 고정(§8) 덕에 새 QEMU로 켜도 같은 CPUID다.

## 7. 카탈로그 저장과 Raft 크기

- **해시로 나눈다.** `NodeStatus`는 해시만 들고, 카탈로그 본문은 이름이 해시인 `CpuCatalog` 리소스 하나다. 같은 QEMU·같은 CPU·같은 가속기인 노드는 같은 카탈로그를 쓴다. 노드는 그 이름이 없을 때만 만든다(이미 있으면 `name_taken`을 성공으로 본다).
- **크기**: x86 카탈로그는 모델 188 × 약 200바이트 + 플래그 이름 약 5 KB ≈ 40 KB(스테이지 2에서 측정해 보고), aarch64는 약 3 KB. 클러스터에는 (QEMU 버전 × CPU 세대 × 가속기) 수만큼, 보통 1~4개 있다. 노드 100대·세대 셋이면 약 120 KB를 **한 번** 쓴다. `NodeStatus`는 주기적으로 통째로 다시 쓰이므로(풀 상태 등) 카탈로그를 거기 두면 쓰기마다 40 KB가 로그에 쌓인다 — 그래서 따로 둔다.
- **정리**: 리더의 janitor가 어느 노드도 가리키지 않고 1시간 넘은 카탈로그를 지운다. 만들기와 상태 쓰기 사이에 지워지면 노드의 다음 검사가 다시 만든다.
- **peer RPC 대신 Raft인 까닭**: 스케줄러·HA 리더·마이그레이션 확인은 `Inventory` 위의 순수 함수다. 후보 노드마다 RPC를 부르면 HA 복구 경로가 다른 노드의 응답에 매이고, 오프라인 노드의 능력을 설명할 수 없다(§15).

## 8. 모델 이름 고정

- **만들 때·바꿀 때 API가 별칭을 버전 모델로 바꿔 저장한다.** `Icelake-Server` → `Icelake-Server-v1`(q35에서 QEMU의 뜻), `Icelake-Server-noTSX` → `-v2`. 이 아키텍처의 카탈로그들에서 `alias_of`를 찾는다. 카탈로그마다 답이 다르면(QEMU 버전이 섞여 별칭 뜻이 다름) 400 `cpu_model_ambiguous`와 후보를 돌려주고 버전 이름을 요구한다.
- 이름이 어느 카탈로그에도 없으면 400 `unknown_cpu_model`(비슷한 이름 셋), 다른 아키텍처 모델이면 400 `cpu_model_arch`. 어느 노드도 켤 수 없는 모델은 받되(꺼진 VM), 켜기는 409 `no_capacity`에 까닭이 붙는다.
- 플래그 이름은 이 아키텍처 카탈로그들의 `flags` 합집합에 있어야 한다(400 `unknown_cpu_flag`).
- `PUT`에서 모델이 그대로면 다시 해석하지 않는다. 응답과 감사 설명에 고정한 이름을 적는다(`vm create web cpu=Icelake-Server-v1 (from Icelake-Server)`). 웹은 별칭을 보이지 않고 버전 이름만 고르게 한다(§13).
- **까닭**: 버전 없는 이름의 뜻은 QEMU가 바꿀 수 있다. 한 노드의 QEMU를 올린 뒤 그 노드로 옮긴 VM이 다른 CPUID로 켜지면 마이그레이션이 깨진다. 버전 이름(`-v7`)의 정의는 QEMU가 바꾸지 않는다.
- `host`·`max`는 고정할 수 없다(§10의 경고). `max`는 QEMU 버전에 따라 바뀐다.

## 9. 클러스터 기준(EVC)

- **모드**: `off`(기본) — `cluster` 모델 VM은 `host`처럼 켠다. `auto` — 리더가 셈한다. `pinned` — 관리자가 아키텍처마다 모델·플래그를 고정한다. 기준은 `cluster` 모델 VM에만 쓰인다. 명명 모델·수준·`host` VM은 기준과 상관없다(클러스터 숨김 목록만 모든 VM에 쓰인다, §11).
- **auto 셈(x86_64, 순수 함수 `cpu::baseline(nodes, catalogs, settings)`)**:
  1. 대상: 위트니스가 아닌 이 아키텍처의 등록된 노드(온라인 여부와 유지보수는 따지지 않는다 — 잠깐 꺼진 노드 때문에 기준이 오르내리지 않게). 카탈로그가 없는 노드는 빼고 `warnings`에 적는다.
  2. 후보: 모든 대상 노드에서 `can_run`인 버전 모델(별칭 아님, `migration_safe`, `deprecated` 아님) 가운데 모델 `vendor`가 모든 노드의 호스트 `vendor`와 같은 것. 호스트 벤더가 섞였으면(Intel + AMD, 또는 TCG 노드) 명명 모델 후보는 없다.
  3. 고르기: 정적 확장 `features`의 비트 수가 가장 큰 것, 같으면 이름순 뒤(높은 버전). 후보가 없으면 모두 켤 수 있는 가장 높은 `x86-64-v*`, 그것도 없으면 `qemu64-v1`.
  4. 더할 플래그(`auto_flags`): 모든 노드의 `supported` 교집합 ∩ 카탈로그의 모든 명명 모델 `features` 합집합(QEMU가 어떤 모델 정의에든 쓰는 플래그만 — `vmx-*` 같은 세부 속성을 막을 표를 박지 않는다) − 고른 모델의 `features`.
  5. `pinned`·`auto`의 결과에 설정의 숨김 목록을 끈다.
- **aarch64**: auto는 셈하지 않는다(버전·`unavailable-features`·정적 확장이 없다, §4). `pinned`만 되고, 명명 모델은 모든 대상 노드가 TCG일 때만 받는다(svc01·svc02). KVM·HVF aarch64는 `host`뿐이라 `cluster` VM은 `host`로 켜고 화면이 그렇다고 적는다.
- **리더가 기록한다**: 노드·카탈로그·설정이 바뀌면 리더가 다시 셈해 `ClusterStatus.cpu_baselines`를 쓴다(달라졌을 때만). 바뀌면 기록 Task `cpu-baseline`(이전 → 새 모델·플래그, 원인 노드 `limited_by`, 영향 받는 실행 중 VM 수)을 남겨 활동·알림(계획 60 규칙)으로 보인다. 켜기는 이 기록만 읽으므로 모든 노드가 같은 기준을 쓴다.
- **약한 노드가 들어오면**: auto는 기준을 내린다. **실행 중 VM은 그대로**다(기록된 `cpu_resolved`). 그 VM들은 보류 `cpu`(`"Cascadelake-Server-v5 → Skylake-Server-v5 (cluster baseline changed)"`)를 얻고, 약한 노드로는 옮겨지지 않으며(§10) 나머지 노드 사이에서는 지금처럼 옮겨진다. 새로 켜는 `cluster` VM은 새 기준이다. 깨뜨리지 않고 경고한다(VMware EVC는 합류를 거부한다).
- **pinned에서 약한 노드**: 기준을 바꾸지 않는다. 그 노드는 `cluster` VM을 받지 않고, `warnings`·노드 화면에 "이 노드는 클러스터 기준 Haswell-v4를 켤 수 없다(빠진 것: …)"를 보인다. 고정 모델을 설정할 때 켤 수 없는 노드가 있으면 409 `cpu_baseline_unsupported`(노드와 빠진 것), `force`로 넘는다.
- **노드를 빼면**: auto는 기준을 올릴 수 있다. 실행 중 VM은 그대로이고 보류로 "다음 전원 껐다 켜기 때 더 나은 CPU"를 보인다. 내리기와 같은 기록 Task다.

## 10. 켜기·배치·마이그레이션·HA

- **켜기**(reconciler, VM이 놓인 노드): `resolve`로 `ResolvedCpu`를 만들어 명령줄에 넣고 `RunningHardware.cpu_resolved`에 기록한다. 명령줄은 `-cpu <model>[,enforce][,<flag>=on|off]…`이다(`host`·`max`에는 `enforce`를 붙이지 않는다). 받는 쪽 마이그레이션은 원본의 `cpu_resolved`를 그대로 쓴다 — 대상 노드의 기준·숨김 목록이 달라도 같은 CPUID다.
- **노드 조건**: `node_fits_with`에 CPU 확인을 더한다. 꺼진 VM(켜기·HA 복구·복제 배치)은 `resolve(spec, 지금 기준)`을, 실행 중 VM(마이그레이션·비우기·균형 조정)은 `cpu_resolved`를 `can_run`에 넣는다. `Inventory`가 카탈로그를 들고 다닌다. 실패 문구: `node "n3" cannot run CPU model Icelake-Server-v7 (missing avx512vbmi, gfni)`. `NodeCpu`가 없는 노드는 `host`·`max`만 된다.
- **HA 복구**는 다시 켜기이므로 스펙에서 새로 해석한다(`cluster`면 지금 기준, 숨김 목록 적용). 그 노드들 가운데 `can_run`인 노드만 후보다. `ha_capacity`는 바뀌지 않는다.
- **`host`·`max` VM**: 오늘처럼 어디로든 옮긴다. 다만 원본·대상의 카탈로그가 다르고 대상 `supported`에 원본 `supported`의 플래그가 빠져 있으면 **경고**를 만든다(`target node "n3" lacks avx512vbmi, amx-tile that the guest may be using`). 같은 해시면 경고가 없다. 위가 대상의 상위 집합이면 경고가 없다(게스트는 켤 때 본 CPUID를 쓴다). 경고는 마이그레이션 응답을 막지 않고 감사 detail(`cpu-warning`)에 남으며, 사전 확인(`GET /vms/{key}/cpu`)이 웹·CLI에 보인다. 마이그레이션을 막는 것은 §16.
- **비우기·균형 조정**은 `can_run`이 아닌 노드를 대상에서 뺀다. 모든 대상이 빠지면 지금처럼 그 VM을 "옮길 곳 없음"으로 보인다.
- **스냅샷 되돌리기·복원**은 켜기와 같다(새로 해석).

## 11. 플래그 숨기기와 전원 껐다 켜기 (보안 쓰임새)

- **두 길**: VM 하나는 `cpu.flags: ["-avx512vbmi"]`, 클러스터 전체는 설정 `cpu.mask: ["avx512vbmi"]`(모든 모델에 — `host`에도 `-cpu host,avx512vbmi=off`). 숨김 목록은 VM `+flag`보다 이긴다.
- **실행 중에는 바뀌지 않는다.** CPUID는 QEMU를 띄울 때 정해진다. 스펙·설정을 바꾸면 실행 중 VM마다 보류 `cpu`가 생기고 다음 **전원 껐다 켜기**(Orbivisor `reboot`, `stop` + `start`, HA 복구)에서 적용된다. **게스트 안 재부팅으로는 적용되지 않고**, 라이브 마이그레이션도 CPUID를 그대로 옮기므로 적용되지 않는다.
- **알리기**: API는 `status.hardware.pending`의 `cpu` 항목(위 `detail`), CLI `vm get`은 `cpu: … (power cycle pending: -avx512vbmi)`, 웹은 VM 상세의 "전원 껐다 켜기 필요" 표시와 "지금 전원 껐다 켜기" 단추(확인 대화상자가 게스트 안 재부팅으로는 안 된다고 적는다), VM 목록의 "CPU 변경 대기" 거름 칩과 일괄 재시작이다. 설정의 숨김 목록을 저장하면 응답이 영향 받는 실행 중 VM 수를 돌려주고 화면이 그 목록으로 가는 링크를 보인다.
- **보류를 누가 쓰나**: 기준·숨김 목록·카탈로그가 바뀌면 각 노드의 검사 루프가 자기 실행 중 VM의 `cpu` 보류를 다시 셈해 달라졌을 때만 쓴다(`checks.rs`의 "다를 때만 쓴다" 본). 스펙 변경은 지금처럼 핫플러그 Task가 쓴다.
- **한계(가이드에 적는다)**: CPUID 숨기기는 "이 기능이 있다고 알리지 않기"다. 하드웨어가 명령을 실행할 수는 있으므로 CPUID를 보지 않고 명령을 쓰는 코드는 막지 못한다. JDK는 CPUID를 보고 내장 함수를 고르므로 이 쓰임새에는 맞다. JVM 쪽 우회(`-XX:UseAVX=2` 등)는 게스트 안의 일이다.

## 12. 버전이 섞인 클러스터와 다른 기능

- **버전 섞임**: `CpuSpec`·`VmSpec`·`SettingsSpec`가 `deny_unknown_fields`이고 `CpuModel`이 닫힌 열거형이라, 옛 노드는 새 모델 문자열·`flags`·`cpu_resolved`가 든 VM을 풀지 못한다. 그래서 `host`·`max`(플래그 없음) 밖의 `cpu`, 설정 `cpu`, `CpuCatalog` 만들기, `cpu_baselines`, 새 Task는 모두 `require_feature(CPU_MODELS)` 뒤다. 노드는 모든 노드가 기능을 알릴 때까지 카탈로그를 올리지 않고 `NodeCpu`만 쓴다(`NodeStatus`는 모르는 칸을 무시한다). 업그레이드 중 `host`·`max` VM의 기록은 바이트까지 지금과 같다(`cpu_resolved`는 그때 쓰지 않는다). 옛 노드의 뒤늦은 합류는 `version_too_old`가 막는다.
- **템플릿(계획 58)**: 템플릿은 `VmSpec`이라 모델·플래그가 그대로 연결 복제·전체 복제로 간다. `CloneOverrides`에 `cpu_model`·`cpu_flags`를 더한다(별칭 고정 같음). 이미지 카탈로그 `TemplateDraft`에 `cpu: Option<CpuModel>`(없으면 지금처럼 `host`).
- **가져오기(OVA·ESXi)**: 지금처럼 `host`가 기본이고, 실행 요청에 `cpu_model`·`cpu_flags`를 더한다. 클러스터 기준이 auto·pinned면 웹 가져오기 화면이 `cluster`를 미리 고른다. VMX의 EVC·`featMask`는 읽지 않는다(§16).
- **백업 복원·원격 복제**: 스펙 그대로다. 받는 클러스터 카탈로그에 없는 모델이면 꺼진 VM으로 만들고 경고를 돌려준다(켜기는 409와 까닭).
- **핫플러그(계획 54)**: 붙이는 vCPU는 `query-hotpluggable-cpus`의 `type`(예: `Icelake-Server-v7-x86_64-cpu`)이고 `-cpu`의 플래그는 그 형식의 전역 속성이라 새 vCPU에도 똑같이 걸린다(시험으로 확인, §14). 실행 중 모델·플래그 바꾸기는 보류 `cpu`다. `can_receive`는 그대로다.
- **폴더·권한(계획 62)**: CPU 칸은 `vm.config` 권한이다. 설정 `cpu`는 루트 `cluster.admin`. 모델 목록 `GET /cpu/models`는 VM을 만들 수 있는 사용자 누구나 읽되, 루트 `cluster.read`가 없으면 노드 이름 없이 "n/m 노드"만 보인다.
- **macOS(HVF)**: `host`·`max`만. 마이그레이션은 지금처럼 막혀 있다.

## 13. API·CLI·웹

- **API**(태그 `cpu`·`vms`·`nodes`·`settings`):
  - VM: `cpu.model`(문자열), `cpu.flags`(`["-avx512vbmi","+aes"]`), `status.hardware.cpu_resolved`, 보류 `cpu`. `GET /vms/{key}/cpu`: 스펙·지금 해석·실행 중·보류, 노드마다 `{node, can_run, missing, warnings}`(마이그레이션 대화상자·CLI용).
  - 노드: `GET /nodes/{key}/cpu`(`NodeCpu` + 카탈로그 펼친 것: 모델마다 쓸 수 있음·빠진 플래그, 플래그 목록), `POST /nodes/{key}/cpu/refresh`(202, Task `node-cpu-probe`, admin).
  - 클러스터: `GET /cpu/models?arch=x86_64`(모델마다 별칭·벤더·켤 수 있는 노드·노드마다 빠진 것, 수준·`host`·`max`·`cluster` 의사 항목), `GET /cpu/flags?arch=`(이름과 지원 노드 수), `GET /cpu/baseline`(아키텍처마다 모드·해석 결과·`limited_by`·`warnings`·`cluster` VM 수).
  - 설정: `PATCH /settings {cpu: {baseline, pinned, auto_flags, mask}}` → 응답에 `affected_running_vms`. 409 `cpu_baseline_unsupported`(`force`).
  - 오류: 400 `unknown_cpu_model`·`unknown_cpu_flag`·`cpu_model_arch`·`cpu_model_ambiguous`·`cpu_flag_masked`·`invalid_cpu_flags`, 409 `cluster_upgrading`·`no_capacity`(CPU 까닭)·`migration_unsupported`(CPU 까닭)·`cpu_baseline_unsupported`. 감사: `vm set web cpu.flags=-avx512vbmi`, `settings cpu.mask=avx512vbmi`, `cpu baseline auto: Cascadelake-Server-v5 -> Skylake-Server-v5 (node n4)`.
- **CLI**:
  - `vm create|set --cpu-model Icelake-Server-v7 --cpu-flags=-avx512vbmi,+aes`, `--no-cpu-flags`. `vm get`은 `cpu: 1x4 Icelake-Server-v7 -avx512vbmi`, 실행 중이면 `cpu running: … (power cycle pending: …)`.
  - `vm cpu VM`(노드마다 켤 수 있음·빠진 것·경고), `vm migrate`는 `host` 경고를 먼저 찍는다.
  - `cpu models [--arch A] [--node N] [--usable]`, `cpu flags [--arch A]`, `cpu baseline`, `node cpu NODE [--refresh]`.
  - `settings set cpu.baseline=off|auto`, `cpu.pinned=x86_64:Haswell-v4,-hle`, `cpu.auto_flags=false`, `cpu.mask=avx512vbmi,…`(빈 값 = 없음).
  - `template create|vm clone|import … --cpu-model --cpu-flags`.
- **웹**(계획 61 규칙: 사이드바 항목을 더하지 않는다):
  - **VM 폼·VM 설정**(`GeneralSection`): 모델 선택을 찾기 상자 있는 묶음 목록으로 — "기본(host·max)", "클러스터 기준(cluster: 지금 Skylake-Server-v5)", "호환 수준(x86-64-v2 …)", "QEMU 모델(버전)". 항목마다 "n/m 노드에서 켤 수 있음", 고르면 못 켜는 노드와 빠진 것. 플래그는 칩 입력(`/cpu/flags` 자동 완성, +/− 전환). 실행 중 VM이면 "다음 전원 껐다 켜기 때 적용" 안내. 도움말 링크 "취약점 때문에 CPU 기능 숨기기". 기준이 auto·pinned면 새 VM 기본값은 `cluster`.
  - **VM 상세**: 하드웨어 카드의 CPU 줄(해석된 모델·플래그), 보류 `cpu`의 "전원 껐다 켜기 필요"와 단추. VM 목록 거름 칩 "CPU 변경 대기", 일괄 재시작.
  - **마이그레이션 대화상자**: 못 켜는 노드는 비활성과 까닭, `host` VM의 경고 띠.
  - **인프라 › 노드 › 노드 상세**: "CPU" 카드(QEMU 버전·가속기·벤더·`model-id`·읽은 시각·오류, 다시 읽기 단추)와 모델 표(쓸 수 있음/없음, 빠진 플래그, 찾기). 노드 목록에 "CPU 모델" 열(쓸 수 있는 수)과 기준을 못 켜는 노드 표시.
  - **인프라 › 노드 › 클러스터 보기**(`/cluster`): "CPU 기준" 카드(모드, 아키텍처마다 모델·플래그, 제한한 노드, 경고, 바뀐 기록 Task 링크)와 모델 × 노드 표.
  - **설정 › 일반**: "CPU" 칸(admin) — 기준 모드, 고정 모델, 자동 플래그, 숨김 목록. 저장 뒤 "실행 중 VM n개는 전원 껐다 켜기까지 그대로" 알림과 VM 목록 링크.
  - i18n en·ko `cpu.*`. `Restart` 보류 문구를 "다음 전원 껐다 켜기 때 적용"으로.
- **문서**: `docs/guide/vms.md`에 "CPU 모델과 플래그"와 짧은 따라 하기 **"취약점 때문에 CPU 기능 숨기기"**(숨김 목록에 `avx512vbmi` → 대기 VM 거름 → 일괄 재시작 → 게스트에서 `grep -c avx512vbmi /proc/cpuinfo`가 0, 한계), `docs/guide/cluster.md`에 "클러스터 CPU 기준".

## 14. 시험과 CI

- **순수(opl-types)**: serde 하위 호환 — 옛 `VmSpec`(`"model":"host"`)·`RunningHardware`·`SettingsSpec`·`NodeStatus` JSON이 바이트 그대로, `flags`·`cpu_resolved` 없음 = 지금. `CpuModel` 문자열 왕복(수준·`cluster`·명명, 대소문자), 플래그 문법·정규화·충돌·64개. `resolve` 표(모델 × 가속기 × 기준 × 숨김 × VM 플래그, 숨김이 이김), `can_run`(빠진 것을 끄면 됨, `+flag` 지원, aarch64 KVM·HVF), `baseline` 표(같은 노드, 약한 노드 합류, 벤더 섞임 → 수준, TCG 노드, 카탈로그 없는 노드, `auto_flags`, 고정), 보류 `cpu` 비교와 `detail`.
- **명령줄(opl-qemu)**: `-cpu Icelake-Server-v7,enforce,avx512vbmi=off`, `host,avx512vbmi=off`(KVM), TCG `host` → `max`에 숨김, 수준 → `qemu64-v1,enforce,popcnt=on,…`, aarch64 `max,sve=off`. 받는 쪽은 `cpu_resolved` 그대로.
- **탐침 파싱**: 기록한 QMP 고정물(`crates/opl-qemu/tests/fixtures/cpu/`) — 이 Mac의 QEMU 11.1.2 x86_64 TCG·aarch64 TCG·aarch64 HVF(스테이지 2에서 기록), GitHub `linux-kvm`의 Ubuntu 24.04 QEMU x86_64 KVM(시험이 로그에 남긴 것을 옮김), svc01 aarch64 TCG. 카탈로그 내용·해시 안정성·크기(보고)를 본다. 약한 노드는 KVM 고정물에서 모델을 못 쓰게 바꾼 합성 카탈로그다.
- **opl-api(`MockHypervisor`, 노드 셋 카탈로그 둘)**: 배치가 못 켜는 노드를 거름, 마이그레이션 409 까닭, HA 복구 후보, 비우기, 별칭 고정·모호, 기능 게이트(옛 노드 하나 → 409 `cluster_upgrading`), 리더 기준 기록과 약한 노드 합류 → 기준 내림·기록 Task·실행 중 VM 보류, 고정 기준 409·`force`, `host` VM 경고, 권한(계획 62 행렬에 `/cpu/*` 더하기).
- **실제 KVM — GitHub `linux-kvm`**(게스트 OS 없음, 새 `opld/cpu-it`): 실제 탐침(KVM 카탈로그를 로그로 남김), 러너 호스트에 있는 플래그 하나를 고른다(`avx512vbmi`가 있으면 그것, 없으면 `avx2`, 그다음 `sse4.2` — 러너 CPU를 모르므로). `-flag`로 켠 VM의 QMP `qom-get`(vCPU 객체의 그 속성 = 거짓), vCPU 핫플러그 뒤 새 vCPU도 거짓, 한 기계의 두 QEMU 사이 마이그레이션 뒤 받는 쪽도 거짓. 탐침이 못 쓴다고 한 모델로 켜면 `enforce`로 실패.
- **실제 게스트 — GitHub `install-media`**(클라우드 이미지가 그 작업에 있으므로 계획 57~59처럼 여기, 새 단계 "CPU flag masking in a real guest"): Ubuntu 24.04 클라우드 이미지를 같은 플래그를 숨긴 VM과 숨기지 않은 VM으로 켜고, 계획 59의 보고 길(cloud-init 스크립트가 보고)로 `/proc/cpuinfo`의 플래그 유무를 받는다. 숨긴 VM은 없음, 다른 VM은 있음. 실행 중 숨김 목록을 더하면 보류가 생기고 게스트 안 재부팅 뒤에도 그대로 있으며 `vm reboot`(전원 껐다 켜기) 뒤에야 사라지는지 본다.
- **Mac(aarch64 HVF)**: 탐침 파싱·HVF 카탈로그(`host`·`max`만), HVF 노드에서 명명 모델이 거부되는지. 마이그레이션은 HVF가 막혀 있어 시험하지 않는다. x86 시험은 하지 않는다.
- **svc01·svc02(aarch64, `qemu-tcg`)**: 일반 묶음 전체(위 순수·API 시험)와, 실제 aarch64 TCG 탐침, `neoverse-n1`·`max,sve=off`로 켜기, 고정 기준 `neoverse-n1`의 `cluster` VM을 svc01 ↔ svc02로 옮기기(배포 확인). **할 수 없는 것**: x86 KVM의 `unavailable-features`, `avx512vbmi` 숨기기, x86 auto 기준, `host` 교차 CPU 경고(두 노드가 같은 하드웨어) — 이것들은 GitHub와 합성 카탈로그로만 본다.
- **웹**: vitest(모델 선택·플래그 칩·보류 표시·거름 칩, msw), Playwright 한 줄기: 설정에서 숨김 목록 저장 → VM 목록 "CPU 변경 대기" → 일괄 재시작.

## 15. 견준 길

- **모델 목록: 코드의 표 vs 노드가 QEMU에 묻기(채택, 사용자 결정).** Proxmox는 표를 박아 QEMU를 올려도 새 모델이 Proxmox 업데이트를 기다린다. libvirt·Nova처럼 QMP로 읽으면 새 모델이 QEMU 업데이트로 들어온다. 예외는 얼린 `x86-64-v*` 넷이다(§5).
- **카탈로그: `NodeStatus`에 통째로 / peer RPC / 해시로 나눈 Raft 리소스(채택).** §7. 통째로는 상태 쓰기마다 40 KB, RPC는 HA·스케줄러를 다른 노드의 응답에 매고 오프라인 노드를 설명하지 못한다.
- **기준 셈: `query-cpu-model-baseline`(불가, x86에 없음) / 플래그 교집합만 / 공통 명명 모델 + 교집합 플래그(채택).** 플래그만 쓰면 모델 이름이 주는 `family`·`model`·캐시 정보를 잃고 명령줄이 수백 플래그가 된다.
- **약한 노드: 합류 거부(VMware EVC) / 기준을 내리고 경고(채택, 사용자 결정).** 실행 중 VM은 기록된 CPU로 지켜지므로 깨지지 않는다. 대가는 다음 전원 껐다 켜기에서 `cluster` VM의 CPU가 낮아지는 것이고, 기록 Task로 알린다.
- **기준을 켤 때마다 셈 / 리더가 기록(채택).** 기록하면 모든 노드가 같은 값을 쓰고 바뀐 때·까닭을 남긴다.
- **별칭: 그대로 저장 / 만들 때 고정(채택).** §8.
- **`enforce` 없음(QEMU 기본: 경고하고 뺌) / 늘 `enforce`(채택).** 조용히 다른 CPUID로 켜지는 것보다 켜지 않는 편이 낫다. 스케줄러가 먼저 거르므로 정상 경로에서는 실패하지 않는다.
- **보류 이유: 새 변형 `PowerCycle` / `Restart` 그대로 + 문구(채택).** 새 변형은 옛 노드가 VM 상태를 풀지 못하게 하고, `Restart`는 이미 전원 껐다 켜기다.
- **Proxmox식 VM 플래그 허용 목록 / QEMU가 아는 모든 플래그(채택).** 숨기기가 목적이라 허용 목록이 늘 뒤처진다. 켜기(`+`)는 `can_run`이 지원 여부를 본다.
- **OpenStack**: 노드 CPU 기능을 placement trait(`HW_CPU_X86_AVX512VBMI`)로 알리고 flavor가 요구·금지하는 방식은 우리 `can_run`과 같은 생각이다. `cpu_models` 순서 목록·`cpu_model_extra_flags`는 우리 `pinned`·`flags`에 해당한다.

## 16. 나중 할 일

`host` VM의 교차 CPU 마이그레이션 막기(선택 설정), aarch64 auto 기준(KVM arm64의 쓰기 가능한 ID 레지스터), 사용자 정의 CPU 모델 객체, `phys-bits`·`hv-vendor-id`·Hyper-V enlightenment, 머신 유형 버전 고정, VMX EVC·`featMask` 가져오기, VM별 기준(vSphere per-VM EVC), 숨김 목록의 폴더 범위, 노드 CPU 마이크로코드·취약점 상태(`/sys/devices/system/cpu/vulnerabilities`) 보이기, 보류 VM의 예약된 일괄 전원 껐다 켜기.

## 17. 스테이지 나누기

각 스테이지는 커밋 하나이고, 혼자 컴파일되고 시험된다. 새 API는 스테이지 5에서 연다. 그 전에는 `host`·`max` 밖의 모델이 기능 게이트와 API 검증에 막혀 쓰이지 않는다.

1. **feat(types): CPU 모델·플래그·카탈로그·클러스터 기준 타입과 해석, 기능 `cpu-models`(opl-types)** — `CpuModel`(`Cluster`·`Level`·`Named`, 문자열 serde), `CpuSpec.flags`·`CpuFlag`, `ResolvedCpu`·`RunningHardware.cpu_resolved`, `NodeCpu`, `CpuCatalogSpec`(`Kind::CpuCatalog`)·`Bits`, `CpuSettings`, `ClusterStatus.cpu_baselines`, 순수 `resolve`·`can_run`·`baseline`, 보류 `cpu` 비교, `x86-64-v*` 정의(Proxmox와 대조), serde 하위 호환.
2. **feat(qemu,driver,opld): 노드 CPU 탐침 — QMP 모델·플래그 읽기, 카탈로그 해시 공유, QEMU 바뀜 다시 읽기** — opl-qemu `cpu.rs` 파싱과 고정물, `Hypervisor::cpu_catalog`(QEMU·Mock), opld 등록·검사 루프·카탈로그 만들기(기능 게이트), janitor 정리, 크기 측정.
3. **feat(qemu,reconciler): `-cpu` 모델·플래그·enforce 명령줄, 켤 때 해석과 기록, 받는 쪽은 기록 그대로, CPU 보류 갱신** — `cmdline.rs`, `resolve.rs`, 검사 루프의 보류 다시 셈, `linux-kvm`의 `opld/cpu-it`(QMP `qom-get`·핫플러그·마이그레이션·`enforce`).
4. **feat(api): 배치·마이그레이션·HA·비우기·균형 조정의 CPU 확인, 리더의 클러스터 기준 기록과 `cpu-baseline` 기록 Task, `host` VM 경고** — `node_fits_with`·`Inventory` 카탈로그, `start_migration`, `ha.rs`, `drain.rs`, 기준 셈·기록, 시험(노드 셋, 약한 노드 합류).
5. **feat(api,cli): CPU 모델 API·별칭 고정·설정 `cpu`·노드 다시 읽기, `vm --cpu-model/--cpu-flags`·`cpu`·`node cpu` 명령** — VM 검증·오류 코드, `/vms/{key}/cpu`, `/nodes/{key}/cpu[/refresh]`, `/cpu/models|flags|baseline`, `PATCH /settings`, 템플릿·복제·가져오기 칸, 권한, OpenAPI·`schema.d.ts`, oplctl, `install-media`의 실제 게스트 단계.
6. **feat(web): VM 폼·설정의 CPU 모델과 플래그, 전원 껐다 켜기 보류 표시와 거름, 노드·클러스터 CPU 보기, 설정 › 일반의 CPU 기준과 숨김 목록(web)** — `GeneralSection`, VM 상세·목록·마이그레이션 대화상자, 노드 상세·클러스터 보기, 설정, i18n, vitest·Playwright.
7. **docs: README·가이드 "CPU 모델과 플래그"·"클러스터 CPU 기준"·"취약점 때문에 CPU 기능 숨기기", 설계 문서 스테이지 보고, 계획 63 실행 기록** — `docs/guide/vms.md`·`cluster.md`·`web-ui.md`, `docs/index.md`.

## 18. 스테이지 보고 (설계와 다른 것·결정)

### 스테이지 1 (opl-types)

- **`x86-64-v*` 정의를 Proxmox와 대조했다**(qemu-server `src/PVE/QemuServer/CPUConfig.pm`, GitHub 미러 커밋 `13007a1`, 2026-11-05). Proxmox는 모든 수준에 `+pni`를 넣는다(`qemu64`에 이미 있지만 같은 명령줄을 위해 그대로 둔다). §5의 목록에는 `pni`가 빠져 있었다. 나머지는 같다. Proxmox는 `-cpu qemu64`(버전 없음)이고 우리는 §8대로 `qemu64-v1`이다. Proxmox의 이름 `x86-64-v2-AES`도 받는다(수준·`host`·`max`·`cluster`는 대소문자를 가리지 않고 소문자로 저장).
- **`resolve`에 `catalog: Option<&CpuCatalogSpec>`를 더했다.** "숨김 이름이 그 아키텍처 카탈로그에 없으면 건너뛴다"를 하려면 켜는 노드의 카탈로그가 있어야 한다. 카탈로그가 없으면(탐침 실패) 숨김 목록을 모두 끈다 — 숨기기가 목적이라, 모르는 이름 때문에 켜기가 실패하는 편이 조용히 드러내는 것보다 낫다.
- **`Accel`을 opl-types(`cpu::Accel`)에 새로 두었다.** opl-qemu의 `Accel`은 그대로이고 스테이지 3이 잇는다. `NodeCpu.accel`·`CpuCatalogSpec.accel`은 설계대로 문자열이다(모르는 값이 와도 `NodeStatus`를 풀게).
- **`pinned`는 `baseline: off`여도 쓴다.** "pinned에 있는 아키텍처는 auto 대신"이라 했으나 off일 때를 말하지 않았다. 관리자가 고정한 것은 쓰는 것으로 했다. off는 auto 셈만 끈다.
- **`Baseline.previous`는 튜플 대신 `PreviousBaseline { model, flags }`**(JSON이 배열이 아니라 객체). `Baseline`에 `same_cpu`·`describe`를 두었다. `baseline()`은 `since`·`previous`를 비워 돌려주고 리더(스테이지 4)가 채운다.
- **auto의 `limited_by`**: "그 노드를 빼면 더 좋은 후보가 고를 수 있게 되는 노드"로 셈한다(벤더 섞임·TCG 노드도 원인으로 잡힌다). 후보는 노드마다 한 번만 `can_run`해 노드 100대에도 가볍다.
- **플래그 이름 맞추기**에 QEMU의 옛 별칭 `sse4-1`·`sse4-2` → `sse4.1`·`sse4.2`를 더했다(`_` → `-`만 하면 `sse4_1`이 `sse4-1`이 되어 카탈로그 이름과 어긋난다).
- **보류 `cpu`**: 순수 함수 `cpu::pending_change(spec, hw, wanted)`. 핫플러그 계획(`hotplug::plan`·`pending`)은 해석 문맥이 없어 `wanted = None`으로 부른다(스펙의 소켓·모델·플래그만 견줌). 이제 `cpu` 보류에는 늘 `detail`(바뀐 것 + "power cycle required; …")이 붙는다.
- **`CpuCatalogSpec`은 `deny_unknown_fields`가 아니다**(뒤 버전이 칸을 더해도 같은 기능의 노드가 읽게). `CatalogModel.migration_safe`는 기본 거짓, `deprecated`는 거짓이면 쓰지 않는다.
- **`CpuModel`·`CpuFlag`의 API 문서 스키마는 문자열**(손으로 적음). `schema.d.ts`에서 `cpu.model`이 `"host" | "max"` 대신 `string`이 된다.
- **명령줄(스테이지 3 전 임시)**: `cmdline.rs`는 `host`·`max` 밖의 모델을 `host`처럼 켠다(API가 막아 오지 않는다). `opld` 등록은 `NodeStatus.cpu`를 지우지 않고 둔다.
- **임시 API 막기**: 타입이 새 모델 문자열·`flags`를 풀게 되었으므로 `vms.rs` `prepare`가 `host`·`max` 밖이나 플래그가 있으면 `require_feature(CPU_MODELS)`(409) 뒤 400으로 거절한다(`tests/cpu_models.rs`). 스테이지 5가 이 400을 검증·별칭 고정으로 바꾼다.
- `schema.d.ts`를 다시 만들었다. 이 브랜치의 기준(`e04f9a8`)이 main의 `f83a523`(풀 설명 바뀜) 전이라 그 차이도 함께 들어갔다.

### 스테이지 2 (노드 CPU 탐침)

- **크기 측정**(이 Mac의 QEMU 11.1.2): x86_64 TCG 카탈로그는 모델 186개·플래그 407개(`max`가 주는 것 89개)로 **41,134바이트**(§7의 어림 40 KB와 같다). aarch64 TCG는 모델 12개·플래그 23개로 874바이트, aarch64 HVF는 12개·3개로 980바이트. 탐침은 x86 정적 확장 186번을 포함해 0.3초 안쪽이다.
- **카탈로그에 넣는 모델**: `host`·`max`·`base`와 `CpuModel::Named`로 풀리지 않는 이름은 넣지 않는다(`host`·`max`는 API가 의사 항목으로 보인다). aarch64는 모델마다 `full` 확장을 묻고 `aarch64` 속성이 참이 아닌 모델(32비트 `cortex-a15`·`arm926`, M·R 프로필)을 뺀다 — `virt` aarch64 게스트로 켤 수 없고, 표를 박지 않고 QMP로 가린다. 38개 정의 가운데 12개가 남는다.
- **플래그 이름**: x86은 설계대로 `-cpu help`의 "Recognized CPUID flags"(맞춘 뒤 407개 — `_`·`-` 짝이 하나가 된다), aarch64는 `full max`의 불 속성(거짓인 `pauth-impdef`도 이름이다). `supported`는 `full max`의 참을 맞춘 이름으로 셈한다(`lahf_lm`·`sse4-1` 같은 옛 이름도 들어간다).
- **x86 별칭**: 정적 확장을 묻지 않고(`features` 없음) 벤더는 가리키는 버전 모델의 것을 쓴다. 별칭은 기준 후보가 아니고 스테이지 5가 버전 이름으로 바꾼다.
- **고정물**은 탐침이 실제로 모으는 것(`CpuProbeReplies`) 그대로다. 탐침이 받을 때 카탈로그에 쓰지 않는 값(거짓인 불, `vendor` 밖의 값)을 버리므로 x86 고정물이 220 KB다(버리지 않으면 2.2 MB). `cargo test -p opl-qemu --features qemu-it --test cpu_probe`가 `OPL_RECORD_CPU_FIXTURES=DIR`이면 다시 기록하고, KVM이면 응답을 로그에 찍는다(GitHub `linux-kvm`의 KVM 고정물은 그 로그에서 옮긴다 — 아직 없다, 스테이지 3). KVM `full host`는 기록한 TCG 응답에 더한 합성 시험으로 본다. 고정물 x86 카탈로그의 해시는 시험에 고정했다(직렬화가 바뀌면 같은 노드가 새 카탈로그를 올리므로 알아차리게).
- **탐침 결과에 호스트 CPU**: `Hypervisor::cpu_catalog()`는 `CpuCatalogSpec`이 아니라 `CpuProbe { catalog, vendor, model_id }`를 돌려준다. `NodeCpu.vendor`·`model_id`는 `full host`(KVM만)의 것이고, 카탈로그 `vendor`는 설계대로 `max`의 것이다. 실패는 새 `DriverError::CpuProbe`다. QMP는 `-qmp stdio` 대신 실행 디렉터리의 유닉스 소켓(`cpu-probe-{pid}-{n}.qmp`)으로 기존 `QmpClient`를 쓴다.
- **`NodeCpu`도 기능 게이트 뒤다.** §12는 "카탈로그는 올리지 않고 `NodeCpu`만 쓴다"였으나, 옛 리더가 노드 상태를 다시 쓰며 모르는 `cpu`를 지우면 검사 루프가 회차마다 다시 쓰게 되고, 업그레이드 중 노드 상태도 바이트 그대로 두는 편이 낫다. 그래서 다른 모든 노드가 `cpu-models`를 알릴 때까지 탐침은 읽기만 하고 아무것도 쓰지 않는다(이 노드는 이 빌드라 게이트에서 뺀다 — 등록이 자기 기능을 함께 쓰므로).
- **등록·검사 루프**: `member.rs`가 `firmware_caps` 옆에서 한 번 읽고(`CpuWatch::probe_now`) 등록(`record_node`)이 쓴다. 검사 루프는 회차(5초)마다 바이너리(이름이면 `PATH`에서 찾고 심볼릭 링크를 따라간 실제 파일)의 (경로, 크기, mtime)을 보고 바뀌면 탐침을 **뒤에서** 띄운다(검사 회차를 늦추지 않게, 다음 회차가 거둔다). 실패한 탐침은 바이너리가 그대로여도 10분마다 다시 한다(apt가 바꾸는 중에 읽어 실패한 경우). 카탈로그 리소스는 검사 회차가 만든다(없으면 만들고 `name_taken`은 성공) — janitor가 지운 뒤에도 다음 회차가 되살린다. 관리자 다시 읽기(Task `node-cpu-probe`)는 스테이지 5다.
- **janitor**: 어느 노드의 `NodeStatus.cpu.catalog`도 아니고 만든 지(UUIDv7 id의 시각) 1시간이 넘은 카탈로그를 지운다(읽은 revision에서만).

### 스테이지 3 (명령줄·켤 때 해석·보류)

- **`VmRuntimeSpec.cpu_resolved`**(없으면 쓰지 않음)를 더했다. 작업자가 새로 켤 때 해석해 `RunningHardware.cpu_resolved`에 적고 `apply_hardware`가 런타임 정의로 옮긴다 — 받는 쪽·다시 붙기는 기록을 그대로 쓴다. 명령줄은 이것을 `-cpu <model>[,enforce][,<flag>=on|off]…`(플래그 이름순)로 쓰고, 실행 중 붙인 vCPU 장치도 `<model>-x86_64-cpu`다. 없으면(계획 63 전 기록, 업그레이드 중) `resolve(spec.cpu, 이 노드, 기준·숨김 없음)`으로 편다 — `host`·`max`는 지금과 바이트까지 같다.
- **해석 문맥은 opl-reconciler `cpu::CpuContext`**: 노드의 `NodeStatus.arch`·`hypervisor`(`qemu-kvm` → `Accel::parse`), `ClusterStatus.cpu_baselines[arch]`, 설정 `cpu.mask`, 노드 카탈로그. 모든 노드가 `cpu-models`를 알리지 않으면 문맥이 없고 `cpu_resolved`를 쓰지 않는다(§12 "업그레이드 중 기록은 바이트 그대로"). 노드의 아키텍처·가속기를 모를 때도 같다.
- **켤 때 확인**: `host`·`max` 밖의 모델은 이 노드에서 `can_run`이어야 켠다. `NodeStatus.cpu`가 없거나 빈 해시면 `node "n1" has no CPU catalog; only the host and max CPU models can run there`, 못 켜면 `node "n1" cannot run CPU model … (missing …)`로 켜지 않는다(전원 끔, 다시 시도 없음 — 방화벽 없는 노드와 같은 길). 노드가 알린 해시의 카탈로그 리소스가 아직 없으면 실패로 세지 않고 기다린다(검사 회차가 곧 만든다). 스케줄러가 먼저 거르는 것은 스테이지 4다.
- **메모리 스냅샷 불러오기는 스냅샷의 하드웨어 그대로**(담을 때의 `cpu_resolved`, 없으면 모델만 편 것)다. §10의 "되돌리기는 새로 해석"은 디스크만 되돌리는 경우다 — 메모리 상태는 담을 때의 CPUID를 전제한다.
- **aarch64 명명 모델에는 `enforce`를 붙이지 않는다**(`resolve`의 `enforce = arch == x86_64`). aarch64 QEMU에는 `enforce` 속성이 없어 `-cpu neoverse-n1,enforce`가 켜기 오류다(이 Mac의 QEMU 11.1.2로 확인). 같은 까닭으로 aarch64에서 숨김 목록의 이름이 명명 모델에 없는 속성이면(`neoverse-n1`의 `pauth`) QEMU가 켜지 않는다. aarch64 카탈로그는 모델별 기능 표가 없어 `can_run`이 이것을 미리 알지 못한다 — 가이드에 적을 한계다.
- **보류 `cpu` 다시 셈**: 검사 루프(`checks::run_once` 끝)가 `opl_reconciler::cpu::refresh_pending`으로 이 노드에서 도는 VM마다 `pending_change(spec, hw, 지금 해석)`을 셈해 `cpu` 항목만 바꾸고(없으면 빼고, 새로 생기면 앞에) 달라졌을 때만 revision 확인으로 쓴다. 핫플러그 Task의 보류 쓰기(`hotplug::pending`)도 이제 `wanted`를 받아 그 노드의 지금 해석을 넘긴다 — 검사 루프가 쓴 기준·숨김 보류를 지우지 않는다. `hotplug::plan`·`plan_resources`(API의 미리 보기, 켜는 중 바뀐 스펙)는 그대로 `None`이다(단계만 쓴다).
- **`pending_change`의 계획 63 전 기록**: 해석 없는 기록은 이제 "`host`·`max`를 플래그 없이 편 것"으로 견준다(스테이지 1은 스펙만 견줬다). 그래야 업그레이드 뒤 숨김 목록을 더하면 업그레이드 전에 켠 VM에도 보류가 생긴다.
- **Hypervisor `vcpu_props(id, names)`**: 도는 VM의 vCPU마다 QMP `query-cpus-fast` + `qom-get`으로 CPU 속성을 읽는다(Mock은 켠 스펙의 해석 플래그). 지금은 E2E가 쓰고, 스테이지 5의 `GET /vms/{key}/cpu`가 "실제로 걸린 것"을 보일 때 쓸 수 있다.
- **E2E `opld/cpu-it`**(`tests/cpu.rs`, CI `linux-kvm`의 FEATURES): API가 스테이지 5 전까지 모델·플래그·설정 `cpu`를 막으므로, 시험 빌드에만 있는 제어 소켓 고리(`/v1/test/vm-cpu`·`cpu-mask`·`vcpu-props`·`cpu-catalog`, `cpu_hook.rs`)로 클러스터에 바로 쓴다. 숨길 플래그는 `host` vCPU에 있는 것(`avx512vbmi` → `avx2` → `sse4.2`, aarch64 `sve` → `pauth`)을 고른다. 노드 하나: VM `-flag` → 모든 vCPU 거짓, x86_64는 vCPU 핫플러그 뒤 새 vCPU도 거짓, 숨김 목록을 더하면 검사 루프가 보류를 쓰고 전원 껐다 켜기에 적용, 명명 모델(x86_64: 카탈로그가 켤 수 있다는 버전 모델, aarch64 TCG: `neoverse-n1`). x86_64만: 빠진 기능이 있는 모델은 QEMU `enforce`로 켜지지 않고 그 기능을 끄면 켜지며, Orbivisor는 카탈로그로 먼저 거절한다. 노드 둘: 켠 뒤 숨김 목록을 바꾸고 옮기면 받는 쪽 기록·vCPU가 원본 그대로다. 이 Mac은 TCG로 노드 하나·마이그레이션 시험이 돈다(x86 시험은 GitHub). KVM 카탈로그 고정물은 아직 없다.

### 스테이지 4 (배치·마이그레이션·HA·비우기·균형 조정의 CPU 확인, 리더 기준 기록)

- **작업자와 같은 규칙을 나눠 쓴다.** opl-reconciler `CpuContext::from_parts(nodes, node, 카탈로그 찾기, 기준, 숨김 목록)`(순수)를 새로 두고 `CpuContext::load`가 그것을 부른다. `Inventory`가 카탈로그(`cpu_catalogs`, 해시 → `Arc<CpuCatalogSpec>`)·`cpu_baselines`·`cpu_mask`를 들고(모든 노드가 `cpu-models`를 알릴 때만 읽음) `Inventory::cpu_context(node)`로 같은 문맥을 만들어 `resolve`·`check`를 그대로 쓴다. 그래서 업그레이드 중·아키텍처나 가속기를 모르는 노드는 작업자처럼 CPU를 보지 않고, `NodeStatus.cpu`가 없거나 빈 해시이거나 카탈로그 리소스가 아직 없는 노드는 `host`·`max`만 받는다(작업자는 "아직 없음"을 기다리지만 스케줄러는 고르지 않는다). 카탈로그를 노드마다 베끼지 않게 `NodeCatalog::Found`를 `Box`에서 `Arc`로 바꿨다.
- **켜기와 옮기기를 나눴다.** `node_fits(_with)`·`replica_fits`는 새로 켜는 것(켜기·HA 복구·복제 배치·마이그레이션 아닌 모든 곳)이라 스펙을 대상 노드에서 해석한다. 도는 VM을 옮기는 곳은 새 `migration_fits`(마이그레이션 API·비우기)·`replica_migration_fits`(균형 조정·HA 되돌아가기·균형 조정기의 되돌아가기 기다림)·`cpu_receives`(HA 검사의 옮기기 후보)로 켤 때 기록한 `cpu_resolved`를 본다(없으면 계획 63 전 기록이라 명령줄처럼 모델만 편다, 하드웨어 기록이 없으면 스펙). 그래서 약한 노드가 들어와 기준이 내려가도 HA 복구는 새 기준으로 어디든 켜고, 앞 기준으로 도는 VM은 약한 노드로 옮겨지지 않는다.
- **고정된 노드에서 켜기**: 꺼진 VM이 고정된 노드(`spec.node`)가 CPU를 켤 수 없으면 `start`가 hard 규칙 때와 같이 켤 수 있는 다른 노드를 고르고, 없으면 409 `no_capacity`에 까닭(`node "n4" cannot run CPU model …`)이다(§8 "켜기는 409 `no_capacity`에 까닭"). 작업자가 켜지 않고 끄는 것보다 낫다.
- **리더 기록은 정리 작업(janitor)의 리더 tick**(10초, 장벽 뒤)에서 한다(`cpu_baseline.rs`). 새 반복 작업을 띄우지 않았다. `merge(앞, 새, 지금)`(순수): 모델·플래그가 같으면 앞 `since`·`previous`를 잇고, 다르면 `since = 지금`·`previous = 앞 모델·플래그`. **노드·`limited_by`·경고만 바뀌어도 기록은 고쳐 쓴다**(`same_cpu`가 참이면 쓰지 않는다는 스테이지 1 메모와 다르다 — 그러면 "카탈로그가 아직 없다" 경고가 남고 새 노드가 `nodes`에 들지 않는다). 기록 Task·감사는 모델·플래그가 바뀐 때만이다. 쓰기는 `update_status_checked`(관찰 노드 기록과 겨루면 다음 tick).
- **기록 Task `cpu-baseline`**: `TaskOp::CpuBaseline { arch, mode?, previous?, current?, limited_by(노드 이름), running_vms }`. 모델·플래그는 `describe()` 문자열이다. 대상은 클러스터 리소스, 노드는 리더이고 처음부터 `succeeded`다. 기준이 없어지면(off로 바꿈·고정을 뺌·그 아키텍처 노드가 모두 빠짐) `current`가 없는 기록이다. 감사는 `cpu.baseline`(대상 `cpu`/아키텍처, detail `cpu baseline x86_64 auto: Icelake-Server-v7 -> Skylake-Server-v5 (node n4)`). 알림 규칙 조건은 더하지 않았다(활동 목록에는 보인다, 웹 문구는 스테이지 6).
- **`host`·`max` 경고**: `scheduler::host_cpu_warning(vm, inv, from, to)`. 원본 `supported`에서 VM이 끈 플래그를 뺀 것 가운데 대상 `supported`에 없는 것이다. 마이그레이션 API만 감사 detail `cpu-warning: …`에 남기고(force 규칙과 함께면 `; `로 잇는다), 비우기·균형 조정·HA는 로그(`warn`)에만 남긴다(`start_migration_warned`). 스테이지 5의 `GET /vms/{key}/cpu`가 같은 함수를 쓸 수 있다.
- `ScheduleError::NoNode` 문구에 "can run its CPU model"을 더했다.
- **스테이지 3 GitHub 탐침 고침**: 임시 API 막기(스테이지 1)가 저장된 VM의 모델·플래그를 그대로 두는 바꾸기(`vm set --cores 2`가 보내는 전체 스펙)도 400으로 막아 `opld/cpu-it`의 `masked_flags_are_off_in_every_vcpu`가 실패했다. 저장된 VM과 모델·플래그가 같으면 받도록 좁혔다(바꾸면 그대로 400, 스테이지 5가 검증으로 바꾼다).
