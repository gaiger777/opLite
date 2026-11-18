# vTPM·Secure Boot·게스트 에이전트 설계 (계획 59)

작성일: 2026-11-18
상위 문서: `2026-10-20-opl-node-core-design.md`(VM 스펙·QEMU 명령줄·드라이버, §5 UEFI 변수), `2026-10-26-snapshots-design.md`, `2026-10-30-live-migration-design.md`, `2026-11-04-backup-design.md`, `2026-11-11-availability-scale-design.md`(§21 계획 54 하드웨어 기록, §22 계획 55 로컬 디스크 복제, 계획 51 게스트 감시), `2026-11-17-install-media-cloud-init-design.md`(계획 57), `2026-11-17-templates-linked-clones-design.md`(계획 58)
기준 트리: 계획 58 끝(`07d0bb7`). 이 문서는 구현 전 설계다. 스테이지 보고는 구현하면서 덧붙인다.

## 1. 목적

격차 분석에서 Windows 11과 보안 부팅 쪽 빈칸은 셋이다. (1) TPM이 없다(`tpm`·`swtpm` grep 0건). (2) UEFI는 있지만 펌웨어는 `HostProfile.uefi_code`·`uefi_vars_template` 한 쌍뿐이어서 Secure Boot를 켤 수 없다(`opl-qemu/src/host.rs`, `OVMF_CODE_4M.fd`). (3) 게스트 에이전트 채널은 HA 게스트 감시(`ha.guest_watch`, 계획 51)를 둔 VM에만 붙는다(`resolve.rs`의 `guest_agent:` 계산). 쓰임새도 `guest-ping` 하나뿐이다(`opl_qemu::qga::ping`). 그래서 게스트 정보(IP·OS)를 보여 주지 못하고, 에이전트로 끄지 못하고, fs-freeze도 하지 못한다. README의 복제 절도 "게스트 에이전트 fs-freeze는 아직 없다"고 적고 있다.

| | Proxmox VE 8 | vSphere 8 | Orbivisor 계획 59 |
|---|---|---|---|
| vTPM | `tpmstate0` 볼륨(저장소 위) + swtpm | vTPM(VM 암호화 필요, 키 공급자) | VM마다 swtpm 하나, 상태는 노드 로컬 `state_dir`, `firmware_node`가 따라다님, 사본 밀어 두기(§4·§5) |
| Secure Boot | `efidisk0` `pre-enrolled-keys=1` | "Secure Boot" 체크 | `VmSpec.secure_boot`, QEMU 펌웨어 descriptor로 찾은 `secure-boot`+`enrolled-keys` 펌웨어(§6) |
| 게스트 정보 | `qm guest cmd`·요약의 IP | VMware Tools(IP·OS·호스트 이름) | `GET /vms/{key}/guest`, 노드 메모리 캐시(§8) |
| 에이전트 종료 | agent가 켜져 있으면 `guest-shutdown` 먼저 | Tools로 Shut Down Guest | `StopMode::Agent`, 안 되면 ACPI(§9) |
| fs-freeze | 백업 때 `freeze-fs-on-backup` | 정지(quiesce) 스냅샷 | 디스크 스냅샷·백업·복제 회차(§9) |
| guest-exec | 있음(`qm guest exec`) | Guest Operations API | 넣지 않음(§13) |

## 2. 범위

| 넣는 것 | 넣지 않는 것(§14) |
|---|---|
| TPM 2.0(`tpm-crb` q35, `tpm-tis-device` virt), VM마다 swtpm | TPM 1.2, 호스트 TPM 통과(passthrough) |
| TPM 상태가 노드를 따라다니기(가져오기·사본·해시 기록), 엄격한 시작 규칙 | 공유 풀 위의 TPM 상태 볼륨(Proxmox 방식) |
| Secure Boot(MS 키 등록 vars), x86_64 SMM, 펌웨어 descriptor로 찾기 | 사용자 PK/KEK 등록, aarch64에서 배포판이 주지 않는 secboot 펌웨어 |
| UEFI 변수·TPM 초기화 Task(`vm-firmware-reset`) | Hyper-V enlightenment(`hv_*`), VirtIO 드라이버 ISO 묶음 |
| `VmSpec.guest_agent`(채널, 종료 방법, fs-freeze) | `guest-exec`·`guest-file-*` |
| 게스트 정보(OS·호스트 이름·NIC/IP·파일 시스템), 에이전트 종료, fs-freeze, 비밀번호 설정 | VM 목록의 IP 열(노드마다 묻는 일이 커짐), `guest-fstrim` |
| 노드 능력 `firmware_caps`(swtpm·Secure Boot), 스케줄러 거르기 | swtpm 상태 암호화(`--key`), 측정 부팅 증명 |
| 웹·CLI "Windows 11" 미리 채우기(클라이언트만) | `VmSpec.os_type` |

**기능 둘**(`opl_types::node`, `FEATURES` 끝): **`vtpm`**(`VTPM`)은 `tpm`·`secure_boot`, Task `vm-firmware-reset`, peer RPC `TpmState`·`PushFirmware`를 가린다. **`guest-agent`**(`GUEST_AGENT`)는 `VmSpec.guest_agent`와 peer RPC `GuestAgent`를 가린다. 모든 노드가 그 기능을 알릴 때만 해당 필드가 든 VM 스펙 쓰기, 백업 받기·복원, 새 엔드포인트를 받는다. 아니면 409 `cluster_upgrading`이다. 옛 노드는 `VmSpec`이 `deny_unknown_fields`라서 새 필드가 든 VM을 풀지 못한다. 새 필드는 모두 기본값이면 쓰지 않으므로(`skip_serializing_if`) 기존 VM의 JSON과 QEMU 명령줄은 바이트 단위로 그대로다. 계획 58처럼 기능 검사는 검증보다 먼저 하고, `tasks::create_task`도 `TaskOp::needs_vtpm()`으로 다시 본다.

## 3. 타입 (opl-types)

- `VmSpec.tpm: bool`, `VmSpec.secure_boot: bool`(둘 다 `skip_serializing_if = "is_false"`), `VmSpec.guest_agent: Option<GuestAgentSpec>`(없으면 생략).
- `GuestAgentSpec { freeze_fs: bool = true, shutdown: AgentShutdown::{Agent(기본), Acpi} }`는 `deny_unknown_fields, default`이고 기본값이면 쓰지 않는다. JSON `{}`은 기본값 그대로 켠다는 뜻이다.
- 검증(`VmSpec::validate`):
  - `tpm`·`secure_boot`는 `firmware: uefi`일 때만 쓸 수 있다: `"tpm needs UEFI firmware"`, `"secure_boot needs UEFI firmware"`.
  - `secure_boot`와 `machine: q35`는 SMM을 쓴다. 템플릿(`template: true`)도 `tpm`을 가질 수 있다. 배포한 VM이 새 TPM을 받는다(§10).
- `VmStatus`에 새 칸 셋(모두 생략 가능)을 더한다. 기존 `firmware_node`는 그대로 쓴다.
  - `tpm_sha256: Option<String>`: 지금 TPM 영구 상태(`tpm2-00.permall`)의 SHA-256. 비밀이 아니다.
  - `firmware_copies: Vec<Uuid>`: 그 해시와 같은 사본(vars+TPM)을 가진 다른 노드.
  - `firmware_epoch: u32`: 초기화할 때마다 1 올린다. 0이면 생략한다.
- `RunningHardware`에 `tpm`·`secure_boot`·`guest_agent: bool`(켤 때 단 장치, false면 생략)을 더한다. `devices_match`·`hardware_changed`가 이 셋을 견준다.
- `VmRuntimeSpec`:
  - 새 칸 `tpm: Option<RuntimeTpm { fresh: bool, epoch: u32 }>`, `secure_boot: bool`, `freeze_fs: bool`. 모두 `#[serde(default)]`이다.
  - `guest_agent` 채널 조건을 `spec.guest_agent.is_some() || ha.guest_watch.is_some()`로 넓힌다.
- `NodeStatus.firmware_caps: Option<FirmwareCaps { tpm: bool, secure_boot: bool }>`. `hotplug: Option<NodeHotplug>`와 같은 자리이고 옛 노드는 None이다.
- 새 Task `TaskOp::VmFirmwareReset { uefi_vars: bool, tpm: bool }`.
- `BackupManifest.tpm_state: bool`, `BackupStatus.tpm_state`(false면 생략). 매니페스트 형식 버전은 그대로이고, `BackupManifest`는 `deny_unknown_fields`가 아니므로 옛 노드는 이 칸을 무시한다.
- 게스트 정보 타입(`guest.rs`):
  - `GuestInfo { collected_at, agent_version, os: GuestOs { id, name, version, kernel }, hostname, interfaces: Vec<GuestNic { name, mac, addresses: Vec<GuestAddr { ip, prefix }> }>, filesystems: Vec<GuestFs { mountpoint, fstype, used_bytes?, total_bytes? }> }`
  - 상한: NIC 64, NIC마다 주소 32, 파일 시스템 64, 문자열 256바이트. 넘으면 자른다. 게스트가 보낸 값은 믿지 않는다.

## 4. vTPM: swtpm 수명과 파일

- **누가 띄우나**: QEMU처럼 `QemuHypervisor::launch`가 띄우고 opld에서 떼어 놓는다. 그래서 opld를 다시 시작해도 TPM은 VM과 함께 계속 돈다.
  - 명령: `swtpm socket --tpm2 --tpmstate dir={state}/vm/{id}/tpm,mode=0600 --ctrl type=unixio,path={run}/vm/{id}/tpm.sock,mode=0600 --pid file={run}/vm/{id}/swtpm.pid --log file={run}/vm/{id}/swtpm.log,level=1 --daemon --terminate`
  - `systemd_scope`이면 QEMU의 `scope_args`와 같은 모양으로 `opl-tpm-{id}.scope`에서 띄운다. QEMU의 scope와 나눈 것은, QEMU scope를 정리(`clear_stale_scope`)해도 띄운 순서가 꼬이지 않게 하려는 것이다.
  - `--terminate` 덕분에 QEMU가 제어 채널을 닫으면(VM 끝, 마이그레이션 원본 끝) swtpm도 스스로 끝난다. QEMU를 띄우지 못하면 `kill_and_cleanup`이 swtpm을 끝낸다. pidfile의 pid와 TPM 디렉터리의 `.lock`(swtpm의 fcntl 잠금)으로 주인을 확인한 pid만 죽인다(QEMU `liveness`와 같은 원칙).
  - 띄우는 순서: swtpm 먼저 띄우고, 소켓이 생길 때까지(5초 `SWTPM_START_TIMEOUT`) 기다린 뒤 QEMU를 띄운다.
- **소켓**: `RuntimePaths.tpm_socket = {run_dir}/vm/{id}/tpm.sock`. 이름 길이가 `qga.sock`과 같아서 `MAX_SOCKET_PATH` 검사가 그대로 맞다. `RuntimePaths.tpm_pidfile`도 더한다.
- **상태 파일**: `vm_tpm_dir(state_dir, id) = {state_dir}/vm/{id}/tpm/`(0700)이고, swtpm이 `tpm2-00.permall`(0600)을 쓴다. 새 TPM은 이렇게 만든다.
  - `RuntimeTpm.fresh`이고 파일이 없으면 `swtpm_setup --tpm2 --tpm-state dir --createek --create-ek-cert --create-platform-cert --lock-nvram --not-overwrite`로 EK와 인증서를 만든다.
  - `fresh`가 아닌데 파일이 없으면 만들지 않는다. `DriverError::InvalidSpec("the TPM state of VM … is missing on this node")`로 실패한다. TPM을 몰래 새로 만들지 않기 위해서다.
  - `RuntimeTpm.epoch`가 `{state}/vm/{id}/firmware.epoch`(없으면 0)와 다르면, 그 노드의 vars·TPM은 초기화 전의 것이다. 지우고 새로 만든 뒤 epoch를 적는다.
- **명령줄**(`build_cmdline`·`build_incoming_cmdline`의 끝, CD 뒤): `-chardev socket,id=chrtpm,path=…tpm.sock -tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-crb,tpmdev=tpm0`(q35). virt에서는 마지막 장치가 `tpm-tis-device,tpmdev=tpm0`이다. 끝에 붙이므로 디스크·NIC의 PCI 주소는 바뀌지 않는다. `tpm: None`이면 명령줄은 계획 58과 같다.
- **없을 때**:
  - 노드 능력은 opld가 시작할 때와 노드 검사(`checks.rs`, `hotplug`처럼 바뀔 때만 씀)에서 정한다. `Hypervisor::firmware_caps()`가 셋을 확인한다. (a) `[hypervisor] swtpm`(기본은 `PATH`의 `swtpm`)·`swtpm_setup`이 있어야 한다. (b) `swtpm socket --print-capabilities`에 `tpm-2.0`이 있어야 한다. (c) `qemu-system-* -tpmdev help`에 `emulator`가 있어야 한다.
  - 스케줄러 `node_fits_with`는 `spec.tpm && !caps.tpm`이면 `"node {name:?} has no swtpm for the VM's TPM (install swtpm and swtpm-tools)"`, Secure Boot도 같은 모양으로 거른다. 이 검사는 스케줄러·HA 대상·균형 조정기·마이그레이션 대상이 모두 보는 자리다.
  - 노드를 정해 둔 VM이면 기존 `schedule_error`가 된다. 실행 직전에 바이너리가 사라졌으면 `DriverError::StartFailed("swtpm: …")`다.
  - 패키지 `Recommends`에 `swtpm, swtpm-tools`를 더한다. `Depends`에는 넣지 않는다. TPM을 쓰지 않는 노드도 있기 때문이다.

## 5. TPM 상태가 노드를 따라다니기

UEFI 변수의 방식을 그대로 넓힌다. `firmware_node`, reconciler `FirmwareFetcher`, `opl-api/src/firmware.rs::UefiVarsFetcher`, peer RPC `UefiVars`(`opl-peer/src/firmware.rs`의 1 MiB 조각 `UefiVarsChunk`, 0 조각 압축)가 그 방식이다. 다른 점은 TPM은 **엄격하다**는 것이다. vars를 잃으면 부팅 항목만 잃지만, TPM을 잃으면 BitLocker·LUKS 봉인 키를 잃는다.

- **가져오기**: 새 RPC `TpmState { vm }`는 `UefiVars`와 같은 조각 스트림이고 `tpm2-00.permall` 하나를 보낸다. `FirmwareFetcher`에 `fetch_tpm(vm, node, sha256)`을 더한다. 받은 파일의 해시가 `tpm_sha256`과 다르면 버린다. 쓰기는 임시 파일에 쓰고 rename하며, 권한은 0600이다.
- **켜기 규칙**: reconciler `fetch_firmware`를 `prepare_firmware`로 바꾼다. 이 노드를 N이라 할 때 차례대로 본다. TPM이 없는 VM은 지금처럼 경고만 남기고 vars 규칙을 따른다.
  1. `firmware_node`가 N이거나 N의 파일 해시가 `tpm_sha256`과 같으면, N의 파일로 켠다.
  2. 아니면 `firmware_node`에서 받는다.
  3. 그 노드가 오프라인이거나 받기가 실패하면, `firmware_copies` 가운데 온라인 노드에서 받는다.
  4. `tpm_sha256`이 없고 `firmware_node`도 없으면(처음 켜기·복제·초기화 뒤) `fresh: true`로 켠다.
  5. 그 밖이면 켜지 않는다. `last_error` `"the TPM state of VM \"win\" is on node \"a\" (offline) and no other node holds a current copy; start it when \"a\" is back, or reset the TPM with `vm firmware reset win --tpm`"`을 남기고 `retry_interval`마다 다시 본다.
- **해시와 사본**:
  - 해시 기록: VM을 돌리는 노드의 작업자는 VM이 멈출 때와 실행 중 60초마다(`firmware_sync_interval`, 파일 mtime이 바뀌었을 때만) 해시를 셈한다. 해시가 바뀌면 `tpm_sha256`을 쓰고 `firmware_copies`를 비운다.
  - 사본 대상(`firmware_copy_nodes`, 셋까지): 계획 55 복제 대상이 먼저다. 그다음은 HA VM일 때 TPM 능력이 있는 다른 HA 가능 노드이고, HA 그룹 순서를 따른다. HA가 아니고 복제도 없는 VM은 사본을 두지 않는다.
  - 밀어 두기: 새 RPC `PushFirmware { vm, sha256, vars, tpm }`로 vars+TPM을 보내고, 성공한 노드를 `firmware_copies`에 더한다. 받는 쪽은 그 노드에서 VM의 QEMU가 돌면 거절한다.
  - 크기: TPM 상태는 수십 KiB이고, vars는 0 조각 압축을 쓴다.
  - 가용성: 유지 보수에 들어가는 노드는 꺼진 TPM VM의 사본도 밀어 둔다.
- **HA 복구**(`ha.rs::recovery_pick`): TPM VM의 `firmware_node`가 죽었으면 `firmware_copies`의 노드만 대상으로 본다. 그런 노드가 없으면 `VmHaStatus`에 위 문구를 남기고 켜지 않는다. 새 TPM으로 몰래 켜면 게스트가 복구 키를 묻고 멈추거나 봉인한 비밀을 영영 잃으므로, 사람이 고르게 한다.
- **초기화**: `POST /vms/{key}/firmware/reset {uefi_vars, tpm}`은 꺼진 VM에만 되고, 202로 `vm-firmware-reset` Task를 만든다. Task는 `firmware_node`(없거나 오프라인이면 요청 노드)에서 돈다. 할 일은 다음과 같다.
  - 그 노드의 파일을 지운다.
  - `firmware_epoch += 1`, `tpm_sha256 = None`, `firmware_copies = []`, `firmware_node = None`을 쓴다. vars만 지우는 초기화면 TPM 칸은 그대로 둔다.
  - 다른 노드의 낡은 사본은 다음 켜기의 epoch 비교와 정리(§10)가 지운다.
  - 다시 해도 안전하다. epoch는 Task id를 기록한 첫 쓰기에서만 올린다.

## 6. Secure Boot

- **펌웨어 찾기**(`opl_qemu::firmware::discover`): QEMU firmware descriptor(`docs/interop/firmware.json`)를 `/etc/qemu/firmware`, `/usr/share/qemu/firmware` 순으로 읽는다. 앞 디렉터리의 같은 이름 파일이 뒤의 것을 가리고, 파일 이름 순서가 우선순위다(QEMU 규칙). 고르는 조건은 다음과 같다.
  - `interface-types`에 `uefi`가 있다.
  - `mapping.device: flash`이고 `mode`가 없거나 `split`이다.
  - `targets`에 이 아키텍처와 머신 glob(`pc-q35-*`·`virt-*`)이 있다.
  - `features`에 `secure-boot`·`enrolled-keys`가 있다(x86_64면 `requires-smm`도).
  - 고른 결과는 `HostProfile.secure_boot: Option<FirmwarePair { code, vars_template, smm }>`에 담는다. Debian·Ubuntu `ovmf`라면 `OVMF_CODE_4M.secboot.fd`·`OVMF_VARS_4M.ms.fd`가 걸린다.
  - `[hypervisor] secure_boot_code`·`secure_boot_vars`(둘 다 주거나 둘 다 생략)로 덮어쓸 수 있다.
  - 일반 UEFI의 `uefi_code`·`uefi_vars_template`은 **바꾸지 않는다**. 기존 VM의 펌웨어가 그대로여야 한다.
  - Homebrew의 `50-edk2-x86_64-secure.json`에는 `enrolled-keys`가 없으므로 걸리지 않는다. aarch64는 배포판 descriptor에 `enrolled-keys`가 있을 때만 된다. 그것이 없는 노드는 `firmware_caps.secure_boot = false`이고, 스케줄러가 그 노드를 거른다(S2에서 svc01의 `/usr/share/qemu/firmware`를 읽어 확인한다).
- **명령줄**: x86_64는 `-machine q35,smm=on,…`과 `-global driver=cfi.pflash01,property=secure,value=on`, 그리고 secboot code를 쓴다. vars는 처음 켤 때 `vars_template`(MS 키 등록)에서 복사한다(`launch`의 `uefi_vars` 복사 자리). virt에는 SMM이 없어 code·vars만 바꾼다. `UefiVarsFetcher`의 크기 검사는 VM의 `secure_boot`에 맞는 템플릿과 견준다.
- **기존 VM에서 켜고 끄기**: vars 파일은 키 없이 만들어졌다. 키가 없으면 Secure Boot는 설정 모드로 남아 아무것도 막지 않는다. 그래서 다음 규칙을 둔다.
  - VM에 vars가 있는데(`firmware_node`가 있음) `secure_boot`가 바뀌면 409 `firmware_reset_required`: `"VM \"win\" has UEFI variables made for Secure Boot off; reset them (vm firmware reset win --uefi-vars, boot entries are lost) before turning Secure Boot on"`. 끌 때도 같다(MS 키가 남아 계속 막음).
  - 실행 중이면 먼저 409 `vm_running`이다(SMM과 펌웨어가 바뀌므로).
  - CLI `vm set --secure-boot on --reset-uefi-vars`와 웹 확인 창이 초기화 Task를 기다린 뒤 PUT을 한 번에 보낸다. 몰래 초기화하지 않는다.
- **Windows 11**: OS 종류 필드는 없다(`os_type` grep 0건, OVF 가져오기의 `osType`은 버림). 그래서 스펙 필드를 더하지 않고 클라이언트 미리 채우기로 한다. CLI `vm create --preset windows11`과 웹 새 VM 폼의 "Windows 11" 단추가 다음을 채운다: q35, UEFI, `secure_boot`, `tpm`, `guest_agent: {}`, vCPU 2, 4 GiB, 64 GiB SATA 디스크, e1000e NIC. 드라이버 없이 설치하기 위해서다. 웹은 설치 뒤 VirtIO로 바꾸라는 안내도 보인다.

## 7. 실행 중 바꾸기와 하드웨어 기록

- `tpm`을 실행 중에 더하거나 빼면 계획 54의 보류 `PendingChange { field: "tpm" }`(Restart)가 된다. `guest_agent`의 채널 유무도 보류 `guest_agent`로 같고, 마이그레이션 받는 쪽은 `status.hardware`의 값으로 켠다(`apply_hardware`). `ha.guest_watch`의 409 규칙은 손대지 않는다. `secure_boot`는 §6대로 409다. `guest_agent`의 `freeze_fs`·`shutdown`은 장치가 아니라서 바로 적용된다.
- `tpm`을 빼도 상태 파일은 지우지 않는다. 다시 켜면 같은 TPM이다. 상태를 지우는 길은 초기화와 VM 지우기뿐이다.
- 메모리 스냅샷의 `snapshot_mismatch` 검사에 `tpm`·`secure_boot`·채널 유무를 더한다. 장치가 다르면 vmstate를 불러올 수 없다.

## 8. 게스트 에이전트: 클라이언트와 게스트 정보

- **클라이언트**(`opl_qemu::qga`): 지금의 `ping`을 `Session`으로 넓힌다.
  - 연결: `connect`가 `guest-sync-delimited`로 맞춘다(기존 `exchange`의 구분 바이트 0xFF와 연결마다 다른 id). `execute(cmd, args, timeout)`은 한 줄 JSON을 받는다.
  - 상한: 답 한 줄 1 MiB(`MAX_REPLY_BYTES`, 넘으면 연결을 끊고 오류), 동기화 2초, 명령 5초(`AGENT_TIMEOUT`), freeze 10초.
  - 멈추거나 시간이 넘으면 연결을 버린다. 다음 세션이 다시 동기화한다.
  - 직렬화: 채널 소켓은 한 번에 한 연결뿐이다. 그래서 VM마다 `VmHandle.agent: tokio::sync::Mutex<()>`를 두고 ping·정보·종료·freeze가 모두 이것을 거친다. 정보 수집 한 번이 최대 20초라서 게스트 감시의 최소값(30초)보다 짧다.
- **드라이버**: 새 메서드 `Hypervisor::guest_info(id) -> GuestInfo`를 더한다. `guest-info`, `guest-get-osinfo`, `guest-get-host-name`, `guest-network-get-interfaces`, `guest-get-fsinfo`를 부르고, 실패한 명령(옛 에이전트, freeze 중)은 그 칸만 비운다. `MockHypervisor::set_guest_info`도 더한다.
- **캐시**: Raft에 두지 않는다. 계획 44 지표처럼 VM이 도는 노드의 메모리에 둔다.
  - 저장: `opld/src/guest.rs`의 `GuestInfoStore`(`HashMap<Uuid, GuestInfo>`)에 둔다. VM이 노드를 떠나거나 꺼지면 지운다.
  - 수집: 작업자가 에이전트가 이 QEMU에서 한 번 답한 VM만 60초마다 모은다.
  - 읽기: API는 `status.node`가 이 노드면 바로 읽고, 아니면 새 RPC `GuestAgent { vm, op: Info { refresh } | SetPassword {..} }`로 읽는다. 지표의 `read_node`와 같은 3초 제한이다.
  - 새로 고치기: `?refresh=true`면 지금 묻는다. VM마다 5초에 한 번이다.

## 9. 에이전트 종료·fs-freeze·비밀번호

- **종료**: `StopMode::Agent`를 더한다. 드라이버가 `guest-shutdown {mode: powerdown}`을 보낸다. 이 명령은 성공하면 답이 없으므로, 시간 초과는 보낸 것으로 본다. 연결 거부·오류 답·채널 없음이면 곧바로 `system_powerdown`(ACPI)으로 넘어간다. reconciler의 종료 단계(`worker.rs`의 ACPI 요청 자리)는 `guest_agent.shutdown == Agent`이고 에이전트가 이 QEMU에서 답한 적이 있으면 `Agent`를 쓴다. 나머지는 지금처럼 `acpi_timeout` 뒤 강제 종료다. `vm reboot`(끄고 켜기)도 같은 길이다.
- **fs-freeze**:
  - 대상: 시점을 찍는 드라이버 메서드 넷(`snapshot_save`(메모리 없음), `snapshot_save_with_bitmap`, `incremental_backup`, `replicate_disks`)에 `quiesce: bool` 인자를 더한다. 결과에는 얼림 결과 `Quiesced::{Yes, Skipped(이유)}`를 더한다.
  - 차례: 시점을 고정하는 QMP 직전에 `guest-fsfreeze-freeze`, 직후에 `thaw`를 보낸다. 전송 내내 얼리지 않는다.
  - 표시 파일: 얼리기 전에 `{run_dir}/vm/{id}/fsfrozen`을 쓰고 녹인 뒤 지운다. opld가 그사이 죽어도 다시 붙을 때(`attach`) 표시 파일이 있으면 녹인다.
  - 실패: 얼리지 못하면 Task를 실패시키지 않는다. 경고 `"guest agent did not freeze file systems ({이유}); the snapshot is crash-consistent"`을 남긴다(Task `status.error`의 경고 읽기).
  - 인자는 VM의 `guest_agent.freeze_fs`와 실행 중 여부로 정한다.
- **비밀번호**: `POST /vms/{key}/guest/password {user, password}`이고 권한은 `Write` Vm이다(게스트 root 권한과 같은 무게라 `Operate`보다 높게 둔다).
  - 처리: `guest-set-user-password {username, password: base64, crypted: false}`를 보낸다. user는 1~64바이트이고 제어 문자는 받지 않는다. password는 1~256바이트다.
  - 저장·기록: 저장하지 않는다. 로그·감사·오류 문구에도 넣지 않는다. 감사에는 `vm guest password win user Administrator`만 남긴다.
  - 응답은 200이다. 에이전트가 답하지 않으면 409 `guest_agent_unavailable`이다.

## 10. 다른 기능과의 관계

- **HA(다른 노드에서 켜기)**: TPM 상태는 늘 노드 로컬 `{state_dir}/vm/{id}/tpm/`에 있다. HA 복구는 `firmware_copies`의 노드에서만 켠다(§5). 공유 풀 HA VM은 HA 가능 노드에 사본을 두므로 노드 하나가 죽어도 이어 간다. 사본이 낡았으면(마지막 밀어 두기 뒤에 TPM이 바뀜) 켜지 않는다. README의 "죽은 노드에만 있던 UEFI 변수는 잃는다"는 TPM VM에 한해 사본으로 줄어든다.
- **라이브 마이그레이션**: QEMU `tpm-emulator`는 swtpm의 상태 묶음(permanent·volatile·savestate)을 제어 채널(`CMD_GET_STATEBLOB`/`SET_STATEBLOB`)로 꺼내 마이그레이션 스트림에 실어 보낸다.
  - swtpm의 `--migration release-lock-outgoing,incoming`은 상태가 **공유 저장소**에 있을 때만 필요하다. 여기서는 노드 로컬이라 쓰지 않는다.
  - 받는 노드: `start_incoming` 전에 빈 TPM 디렉터리(epoch 적용)로 swtpm을 먼저 띄운다. 받은 상태는 swtpm이 자기 디렉터리에 쓴다. 대상 노드에 TPM 능력이 없으면 `node_fits_with`가 막는다.
  - 확정: 마이그레이션 확정(`migrate.rs`의 `firmware_node = migration.node`)이 `firmware_copies`를 비운다. 대상의 작업자가 곧 해시를 다시 적는다.
  - 원본: 원본 swtpm은 `--terminate`로 끝난다. 원본 파일은 낡은 사본으로 남았다가 정리가 지운다.
  - Secure Boot의 pflash(secure)도 지금처럼 함께 옮겨진다. 그래서 TPM·Secure Boot VM도 라이브 마이그레이션된다.
- **로컬 디스크 마이그레이션**: 디스크 미러(계획 55의 따라잡기 포함)와 같은 마이그레이션 스트림이라 위와 같다.
- **로컬 디스크 복제(계획 55)**: 복제 대상은 사본 대상의 첫 자리다. 회차가 끝날 때 해시가 바뀌었으면 `PushFirmware`를 보낸다. 복제본으로 하는 HA 복구는 그 노드의 사본으로 켠다. 디스크는 회차 시점이고 TPM은 마지막 밀어 두기 시점이라 둘이 다를 수 있다. TPM 영구 상태는 드물게 바뀌므로 받아들인다(README 한계에 적음).
- **백업·복원**:
  - 백업: `tpm2-00.permall`을 백업 디렉터리에 같은 이름(0600)으로 넣고 `tpm_state: true`로 적는다(`copy_vars`와 같은 자리, 필요하면 `fetch_tpm`). 증분 백업에도 늘 넣는다. 크기가 작기 때문이다. 넣지 않으면 BitLocker VM을 복원했을 때 복구 키가 필요하다(Proxmox도 `tpmstate0`을 백업한다). 백업 저장소 권한 밖의 보호(암호화)는 §14로 미룬다.
  - 복원: 새 VM에 파일을 두고 `firmware_node`를 이 노드로, `tpm_sha256`을 그 파일의 해시로 적는다.
  - 원격 클러스터: 받기(`backup_receive`)·복원은 `vtpm`이 없는 노드가 있으면 409 `cluster_upgrading`이다. 계획 57·58의 `drop_*`처럼 버리지 않는다. TPM을 버리는 것은 데이터를 버리는 일이다.
  - 수동 복원 문서(README "백업 파일 형식")에 swtpm `--tpmstate dir=`을 더한다.
- **스냅샷**:
  - 디스크 스냅샷: vars 복사본(`snapshot.rs::vars_copy` → `{snap}.vars`) 옆에 `{snap}.tpm`(0600)을 둔다. 되돌리기가 둘을 함께 돌려놓고 `set_firmware_node`와 `tpm_sha256`을 쓴다.
  - 메모리 스냅샷(`snapshot-save`): vmstate에 `tpm-emulator`의 상태가 들어 있으므로 불러오기가 swtpm에 되돌린다. 파일 복사본은 꺼진 뒤 켜기를 위한 것이다.
  - 스냅샷을 찍은 노드가 아닌 곳에서 되돌리면, TPM VM은 vars처럼 경고하고 지금 것을 두지 않는다. Task를 `"the TPM copy of snapshot … is on node …"`로 실패시킨다.
- **템플릿·복제(계획 58)**: 복제(`vm clone`, 템플릿 배포, 스냅샷에서 복제)는 **늘 새 TPM**이다.
  - 새 VM의 `tpm_sha256`은 None이다. EK와 봉인한 비밀이 겹치지 않아야 하기 때문이다. 템플릿은 일반화된 원본이다.
  - vars는 지금처럼 복사한다(`clone.rs`의 `uefi_vars_path` 복사). Secure Boot 키도 함께 간다.
  - 원본에 TPM 상태가 있으면 Task 경고 `"VM \"web-1\" got a new TPM; secrets sealed to the TPM of \"tmpl\" do not carry over"`를 남긴다. 복사가 필요하면 백업 → 복원을 쓴다(§13).
- **VM 지우기와 정리**: 지금은 거쳐 간 노드의 vars가 남는다(README 한계). 이제 각 노드의 janitor가 정리한다.
  - 대상: `{state_dir}/vm/{id}` 가운데 Raft에 VM이 없다.
  - 조건: 디렉터리 mtime이 1시간보다 오래됐고, 끝나지 않은 Task의 대상이 아니다. 복원·복제는 VM을 만들기 전에 파일을 쓰기 때문이다.
  - 처리: 셋을 모두 만족하면 지운다(`remove_dir_all`). 사용자가 VM을 지운 뜻대로 하는 것이고 비밀을 남기지 않기 위해서다.
  - epoch가 다른 사본은 다음 켜기가 지운다.
- **버전이 섞인 클러스터**: §2의 기능 둘로 가린다. 새 Task와 새 RPC는 기능이 있는 노드만 받는다. `GuestAgent` RPC를 모르는 노드에서 도는 VM의 정보는 `unsupported`다(지표의 `NodeRead::Unsupported`와 같음).
- **유지 보수·비우기**: 비우기는 TPM VM도 라이브 마이그레이션으로 옮긴다(대상 능력 거르기). 들어가기 전에 꺼진 TPM VM의 사본을 밀어 둔다(§5). 노드를 지우기 전에는 비우기를 권한다. 노드 지우기는 `firmware_node`가 그 노드인데 사본이 없는 TPM VM이 있으면 경고를 남긴다.
- **cloud-init(계획 57)**: 바뀌는 것이 없다. 에이전트는 게스트 안에 `qemu-guest-agent`가 있어야 하고 cloud-init `vendor_data`의 `packages`로 넣을 수 있다(README 예).

## 11. API·CLI·웹

- **API**:
  - VM 스펙의 `tpm`·`secure_boot`·`guest_agent`.
  - `POST /vms/{key}/firmware/reset`(202 `vm-firmware-reset`, `Write` Vm, 꺼진 VM만).
  - `GET /vms/{key}/guest[?refresh=true]`(`Read` Vm, 200 `GuestInfoResponse { state: answered|no_agent|not_running|unreachable|unsupported, info? }`).
  - `POST /vms/{key}/guest/password`(`Write` Vm).
  - `Node.status.firmware_caps`.
  - 새 오류 코드: `firmware_reset_required`·`guest_agent_unavailable`·`tpm_state_unavailable`(켜기 API가 미리 보는 경우).
  - 감사 설명: `vm firmware reset win (uefi vars, tpm)`, `vm guest password win user X`.
  - OpenAPI(utoipa)에 모두 넣는다.
- **CLI**:
  - VM 만들기·바꾸기: `vm create|set --tpm on|off --secure-boot on|off [--reset-uefi-vars] --guest-agent on|off [--agent-freeze on|off --agent-shutdown agent|acpi] [--preset windows11]`.
  - 펌웨어·게스트: `vm firmware reset VM [--uefi-vars] [--tpm] [--yes]`(TPM이면 복구 키 경고를 보이고 확인을 받는다), `vm guest VM [--refresh]`(OS·호스트 이름·NIC/IP·파일 시스템 표), `vm guest password VM --user U`(비밀번호는 표준 입력이나 묻기로 받고 인자로는 받지 않는다).
  - 표시: `vm get`의 stderr에 `tpm: on (state on node a, copies: b)`를 더한다. `node list`에 `TPM`·`SB` 열을 더한다.
- **웹**:
  - VM 폼: "보안" 카드(TPM 2.0, Secure Boot, 노드 능력이 없으면 이유)와 "게스트 에이전트" 카드(켜기, freeze, 종료 방법)를 둔다. 새 VM 폼에 "Windows 11" 미리 채우기를 둔다.
  - VM 상세: "게스트" 탭(OS·호스트 이름·IP·파일 시스템 사용량, 새로 고치기, 수집 시각)을 둔다. "UEFI 변수·TPM 초기화" 확인 창(VM 이름을 입력해야 함)과 "게스트 비밀번호 설정" 창을 둔다.
  - 노드 화면의 능력 배지와 번역(`tasks.op.vm-firmware-reset`)도 더한다.

## 12. 시험과 CI

- **순수(어디서나)**:
  - serde: 옛 VM·노드·매니페스트 JSON이 그대로 풀리고 다시 쓰면 바이트가 같다.
  - 검증과 하드웨어: 검증 문구, `devices_match`·보류 규칙.
  - firmware descriptor 해석: 이 Mac의 실제 Homebrew `50-edk2-x86_64-secure.json`(`enrolled-keys` 없음 → 고르지 않음)과 Debian `ovmf` 모양의 고정 값.
  - 명령줄: TPM·SMM·secure pflash 인자와, TPM이 없으면 계획 58과 같은 명령줄.
  - 켜기 규칙 표(§5의 1~5)를 순수 함수 `tpm_source(vm, node, local_hash)`로 시험한다. `firmware_copy_nodes`도 시험한다.
  - qga `Session`: 가짜 에이전트로 지난 답 섞기, 멈춤, 1 MiB 넘는 줄, 잘못된 JSON, freeze 실패, 성공하면 답 없는 `guest-shutdown`을 시험한다. `GuestInfo`의 자르기도 시험한다.
- **이 Mac(macOS, HVF, swtpm 없음)**:
  - API 시험(`MockHypervisor`·가짜 `FirmwareFetcher`): 기능 게이트, 초기화 Task와 epoch, HA가 사본 노드만 고르기, 복제의 새 TPM 경고, 백업·복원의 `tpm_state`, 게스트 정보 캐시·RPC·`refresh` 제한, 비밀번호 가림, fs-freeze 경고.
  - swtpm 수명: 가짜 `swtpm` 스크립트(`opl-driver/tests/fixtures/fake-swtpm`가 pidfile·`.lock` 잠금·소켓을 만들고 `--terminate`를 흉내 냄)로 시험한다. `[hypervisor] swtpm`이 그 경로를 가리키게 해서 띄우기·주인 확인·정리·opld 재시작 뒤 다시 붙기를 본다. QEMU는 붙이지 않는다.
  - `opl-qemu` `qemu-it`: x86_64 TCG로 `edk2-x86_64-secure-code.fd` + `smm=on` + secure pflash가 켜지는지 본다(QMP `qom-get /machine smm`).
  - 피어 시험: `TpmState`·`PushFirmware`의 해시 불일치를 거절하는지 본다.
- **svc01(aarch64 실제 노드, 패키지 설치 금지)**: 일반 묶음 전체(fmt·clippy·test·scale·storage)와 `qemu-it`를 돈다. 능력 탐지가 swtpm 없음 → `tpm: false`로 나오는지 확인하고, 있으면 그 결과를 실행 기록에 적는다. 실제 swtpm 시험은 새 기능 `vtpm-it`(opl-driver·opld)으로 가두고 svc01에서는 켜지 않는다.
- **GitHub `linux-kvm`**:
  - apt에 `swtpm swtpm-tools`를 더하고 `FEATURES`에 `opl-driver/vtpm-it,opld/vtpm-it`를 더한다.
  - opl-driver `qemu_driver.rs`: TPM VM을 켜면 QMP `query-tpm`이 `emulator`를 보이고, `swtpm_setup`이 permall을 만들고, 끄면 swtpm이 끝난다.
  - `qemu_migration.rs`: 한 호스트의 QEMU 둘 사이에서 TPM VM을 옮긴 뒤 대상 permall 해시가 원본과 같고 원본 swtpm이 끝났는지 본다.
  - opld `migration.rs`: `a_tpm_vm_migrates_with_its_state`(두 opld 사이 라이브, `firmware_node`·`tpm_sha256`)와 `tpm_state_follows_a_stopped_vm`(꺼진 VM을 다른 노드에서 켜기, 그 노드가 꺼졌으면 `tpm_state_unavailable`, 사본이 있으면 켜짐).
  - opld `ha.rs`(`ha-it`): 사본 노드로 복구되는지 본다.
- **GitHub `install-media`(실제 게스트)**:
  - apt에 `swtpm swtpm-tools`를 더한다(`ovmf`는 이미 있음). 새 단계 `cargo test --features opld/vtpm-it,opld/qemu-it -p opld --test vtpm`를 돈다. 받아 둔 Ubuntu 24.04 클라우드 이미지를 쓴다.
  - 부팅 확인: VM `{secure_boot, tpm, guest_agent: {}}`를 cloud-init `vendor_data`로 `packages: [qemu-guest-agent, tpm2-tools]`와 함께 켠다. `runcmd`가 10.0.2.2에 보고하는 것은 셋이다: `/dev/tpm0` 있음, efivar `SecureBoot-8be4df61-…`의 값 1(`mokutil --sb-state`가 있으면 그 줄도), EK 공개 키(`tpm2_readpublic -c 0x81010001`)의 해시.
  - 에이전트 확인: `GET /vms/{key}/guest`가 호스트 이름·`Ubuntu`·10.0.2.x IP를 보이는지 본다. 실행 중 디스크 스냅샷 Task에 freeze 경고가 없는지 본다. `vm stop`이 `acpi_timeout`보다 훨씬 빨리 에이전트로 꺼지는지 본다.
  - 다시 켜기: 다시 켜면 EK 해시가 같다.   - 복제: `vm clone` VM의 EK 해시는 다르다. 두 노드: 두 노드 틀(`migration.rs`의 `Pair`를 `tests/common`으로 옮김)로 실행 중인 VM을 옮긴 뒤 다시 보고하게 해서 EK 해시가 같은지 본다.
  - 시간: 작업 시간을 40분에서 55분으로 늘린다. 게스트 안 apt가 1~2분 걸린다.

## 13. 견준 길

- **TPM 상태 = 공유 풀의 볼륨(Proxmox `tpmstate0`) vs 노드 로컬 + `firmware_node` + 사본(채택).** 볼륨으로 두면 백엔드마다 swtpm이 열 블록 장치가 있어야 한다. RBD는 krbd 매핑이 필요한데 Orbivisor는 librbd만 쓴다. 라이브 마이그레이션에 swtpm `--migration` 잠금 넘기기도 필요하다. 이미 있는 vars 경로를 넓히는 편이 작다. 대가로 HA는 사본에 기대고 사본이 낡으면 켜지 않는다.
- **상태를 Raft에 vs 노드 파일(채택).** Raft에 두면 모든 노드·위트니스·Raft 스냅샷에 비밀이 퍼진다. Raft에는 해시와 사본 위치만 둔다.
- **swtpm = opld 자식 vs 떼어 낸 데몬 + `--terminate`(채택).** 자식으로 두면 opld 재시작이 도는 VM의 TPM을 죽인다. QEMU의 `-daemonize`와 같은 원칙이다.
- **사본이 없을 때 새 TPM으로 켜기 vs 켜지 않기(채택).** 새 TPM으로 켜면 게스트가 복구 키를 묻고 멈춘다. 그래서 가용성도 얻지 못하고 봉인한 비밀만 버린다. 초기화는 명시적 동작으로 둔다.
- **복제 TPM 복사(vSphere 기본 copy) vs 새로(채택).** EK가 같은 두 VM이 생기지 않는다. 템플릿은 일반화된 원본이다. 복사는 백업·복원으로 할 수 있다.
- **Secure Boot 전환 때 vars 몰래 초기화 vs 409 + 명시적 초기화(채택).** 부팅 항목을 몰래 지우지 않는다.
- **`os_type` 필드 vs 클라이언트 미리 채우기(채택).** 서버가 OS 종류로 바꾸는 것이 아직 없다(Hyper-V enlightenment는 §14).
- **게스트 정보를 Raft·VM status에 vs 노드 메모리(채택).** IP는 자주 바뀌고 모든 VM의 정보를 60초마다 Raft에 쓰는 것은 낭비다. 지표와 같은 읽기 길을 쓴다.
- **`guest-exec` 넣기 vs 빼기(채택).** API 권한만으로 게스트 안 root 명령이 되므로 위험이 크다. 콘솔과 cloud-init이 이미 있다. 관리자 전용으로 넣는 것도 감사·출력 크기·시간 제한 설계가 따로 필요하다.
- **기능 하나 vs 둘(채택).** 게스트 에이전트는 swtpm 없는 노드에도 유용하고 따로 쓸 수 있다.

## 14. 나중 할 일

TPM 1.2, swtpm 상태 암호화(`--key`, 키 관리)와 백업 암호화, 사용자 Secure Boot 키(PK/KEK/db) 등록·dbx 갱신, TPM 없는 UEFI VM의 vars 사본(같은 `PushFirmware`), 복제 때 TPM 복사 선택, Hyper-V enlightenment·Windows 11 미리 채우기의 VirtIO 드라이버 ISO(카탈로그), VM 목록의 IP 열(노드마다 `GuestAgent` 일괄 읽기), `guest-fstrim`(평탄화·이동 뒤), 관리자 전용 `guest-exec`, 측정 부팅 증명(EK 인증서 체인 내보내기), aarch64 Secure Boot 펌웨어 직접 빌드 안내.

## 15. 스테이지 나누기

각 스테이지는 커밋 하나이고, 혼자 컴파일되고 시험된다. 앞 스테이지에서 새 Task 실행기와 RPC는 `"this node cannot … yet"`으로 실패하고(계획 57 S1), API는 마지막에 연다.

1. **feat(types): vTPM·Secure Boot·게스트 에이전트 타입·검증·기능(opl-types)**
   - 스펙·상태: `VmSpec.tpm`·`secure_boot`·`guest_agent`(`GuestAgentSpec`), `VmStatus.tpm_sha256`·`firmware_copies`·`firmware_epoch`, `RunningHardware`의 세 칸과 `devices_match`, `VmRuntimeSpec.tpm`·`secure_boot`·`freeze_fs`.
   - 노드·Task·백업: `NodeStatus.firmware_caps`, `VTPM`·`GUEST_AGENT` 기능, `TaskOp::VmFirmwareReset`·`needs_vtpm`, `BackupManifest.tpm_state`.
   - 게스트·시험: `guest.rs`(`GuestInfo`와 자르기), 순수 함수 `tpm_source`·`firmware_copy_nodes`, serde 하위 호환 시험.
2. **feat(qemu,driver): swtpm·Secure Boot 펌웨어·명령줄과 게스트 에이전트 세션(opl-qemu·opl-driver)**
   - QEMU 쪽: firmware descriptor `discover`, `HostProfile.secure_boot`, 명령줄(TPM 장치·SMM·secure pflash), `RuntimePaths.tpm_socket`.
   - swtpm: 띄우기·`swtpm_setup`·epoch·주인 확인·정리, `firmware_caps`.
   - 게스트 에이전트: `qga::Session`, `guest_info`·`StopMode::Agent`·`set_user_password`·`quiesce` 인자와 `fsfrozen` 표시.
   - 모의·시험: `MockHypervisor`, 가짜 swtpm 시험, `vtpm-it`·`qemu-it` 시험.
3. **feat(reconciler,peer): TPM 따라다니기·사본, 노드 능력과 스케줄러, 에이전트 종료·게스트 정보 캐시(opl-reconciler·opl-peer·opld·opl-api)**
   - 펌웨어 상태: `prepare_firmware`의 켜기 규칙, 해시 기록 주기, peer RPC `TpmState`·`PushFirmware`·`GuestAgent`, `UefiVarsFetcher`의 `fetch_tpm`과 템플릿 고르기.
   - 노드·배치: opld 능력 보고(`checks.rs`), `node_fits_with`·HA `recovery_pick`의 사본 노드.
   - 게스트: 작업자의 에이전트 종료, `GuestInfoStore`와 60초 수집.
4. **feat(tasks): 스냅샷·백업·복원·복제·마이그레이션의 TPM 상태와 fs-freeze, 초기화 Task, 상태 디렉터리 정리(opl-api)**
   - 상태 파일: `{snap}.tpm`, 백업의 `tpm2-00.permall`, 복원의 해시, 복제의 새 TPM 경고, 마이그레이션 확정과 복제 회차의 사본.
   - 새 동작: `vm-firmware-reset` 실행기, `quiesce` 경고, janitor의 `vm/{id}` 정리.
5. **feat(api): 펌웨어 초기화·게스트 정보·비밀번호 API, 기능 게이트·검사(opl-api)**
   - §11의 경로, `firmware_reset_required`·실행 중 규칙·보류, 받기·복원의 `vtpm` 409, 권한·감사·가림, OpenAPI.
 6. **feat(cli): vm firmware·guest CLI, Windows 11 미리 채우기, opld 실제 시험과 CI(oplctl·opld·CI)**
   - CLI: oplctl 명령과 표시.
   - 시험: opld `vtpm.rs`(게스트 없는 부분 + `mod guest`), `migration.rs`·`ha.rs` 시험, `Pair`를 `tests/common`으로.
   - CI: `ci.yml`의 `linux-kvm`·`install-media` 단계와 apt, 패키지 `Recommends`.
7. **feat(web): 보안·게스트 에이전트 카드, 게스트 탭, 초기화·비밀번호 창, Windows 11 미리 채우기, 노드 능력(web)**.
8. **docs: README "vTPM과 Secure Boot"·"게스트 에이전트", 마이그레이션·HA·백업 절의 TPM, 설계 문서 스테이지 보고, 계획 59 실행 기록**.

## 16. 스테이지 보고 (설계와 다른 것·결정)

- **S1 켜기 규칙 4**: `tpm_sha256`이 없으면 `firmware_node`와 상관없이 `TpmSource::Fresh`다(설계는 둘 다 없을 때만). UEFI 변수만 있던 VM(`firmware_node` 있음)에 TPM을 더하면 어느 노드에도 TPM 상태가 없는데, 설계대로면 `firmware_node`에서 받으려다 실패해 켜지 못한다. `RuntimeTpm.fresh`는 파일이 없을 때만 만들고 있으면 그대로 쓰므로(`--not-overwrite`) `firmware_node`에서 켜면 있던 TPM을 지킨다. 대가: 첫 해시 기록(멈출 때·60초) 전에 노드가 죽으면 다른 노드는 새 TPM으로 켠다.
- **S1 vars만 초기화**: TPM VM에서 `uefi_vars`만 초기화할 때 `firmware_node = None`으로 쓰면 TPM 상태의 위치를 잃는다(규칙 2가 볼 노드가 없음). S4 실행기는 TPM VM이면 vars 파일만 지우고 `firmware_node`를 남겨야 한다(vars가 없는 노드는 지금처럼 템플릿에서 새로 만든다).
- **S1 `devices_match`·보류의 에이전트 채널**: 계획 59 전 노드가 쓴 `RunningHardware`에는 `guest_agent`가 없다. 스펙에 `guest_agent`가 없고 기록이 거짓이면 `ha.guest_watch`(실행 중 바꿀 수 없음)가 정한 채널 그대로라 같다고 본다(`agent_channel_differs`). 그러지 않으면 게스트 감시 VM이 업그레이드 뒤 옮겨지지 않는다.
- **S1 보류**: `tpm`·`guest_agent`뿐 아니라 `secure_boot`도 보류(`Restart`)를 만든다. API는 실행 중 `secure_boot` 변경을 409로 막지만, `devices_match`가 다르다고 보는데 보류가 없으면 이유 없이 옮기지 못하는 VM이 되므로 방어로 둔다.
- **S1 `firmware_copy_nodes`**: 복제 대상과 HA 노드 모두 온라인·유지 보수 아님·위트니스 아님·`firmware_caps.tpm`인 노드만 고른다(능력 없는 노드의 사본은 HA 복구에 쓸 수 없다). HA 그룹이 있으면 우선순위 큰 그룹 노드 → (`restricted`가 아니면) 그룹 밖 노드, 같으면 이름순이다.
- **S1 더한 것**: `opl_types::guest`에 `GuestInfoState`·`GuestInfoResponse`(§11)와 비밀번호 검사 `validate_guest_password`(§9의 길이·제어 문자 규칙)를, `opl_types::backup::TPM_STATE_FILE`을 미리 둔다. `resolve.rs`는 채널을 `VmSpec::agent_channel()`로 넓히고 `secure_boot`·`freeze_fs`를 스펙에서 채운다. `tpm`은 S3의 `prepare_firmware`가 채울 때까지 늘 None이다. `bootstrap.rs`의 `firmware_caps`는 S3까지 None이다.
- **S2 확인 (a) swtpm 잠금**: swtpm의 `.lock`(TPM 디렉터리 fcntl 잠금)은 TPM을 초기화할 때(QEMU의 `CMD_INIT`) 잡고 `--migration release-lock-outgoing`으로 놓을 수도 있어, 프로세스가 사는 내내 잡혀 있다고 믿을 수 없다. 그래서 주인 확인은 잠금을 보지 않는다. pid 파일의 pid가 살아 있고(`kill 0`) 그 명령줄(Linux `/proc/{pid}/cmdline`, 그 밖 `ps -o command=`)에 이 VM의 제어 소켓 경로가 있어야 주인이다(`opl_driver::tpm::alive`). 가짜 swtpm은 잠금도 흉내 낸다.
- **S2 확인 (b) Homebrew QEMU 11.1.2**: 이 Mac의 `qemu-system-x86_64 -tpmdev help`와 `qemu-system-aarch64 -tpmdev help` 모두 `emulator`를 보인다. 이 Mac의 TPM 능력이 없는 까닭은 swtpm이 없는 것뿐이다.
- **S2 확인 (c) `/opt/homebrew/share/qemu/firmware`**: `50-edk2-i386-secure.json`, `50-edk2-x86_64-secure.json`(features `acpi-s3`·`amd-sev`·`requires-smm`·`secure-boot`·`verbose-dynamic`, `enrolled-keys` 없음), `60-edk2-{aarch64,arm,i386,loongarch64,riscv64,x86_64}.json`. `discover`는 아무것도 고르지 않는다(시험 `homebrew_descriptors_have_no_enrolled_keys`). 그래서 macOS 노드는 `firmware_caps.secure_boot = false`다. `qemu-it` 시험은 discover를 거치지 않고 Homebrew의 `edk2-x86_64-secure-code.fd`로 `smm=on`·secure pflash 부팅을 본다(Linux는 descriptor로 찾은 쌍).
- **S2 `FirmwarePair`**: `smm` 칸을 두지 않았다. x86_64는 `requires-smm`이 있어야 고르므로 SMM은 명령줄이 `secure_boot && machine == q35`로 정한다. descriptor 디렉터리의 빈 파일은 뒤 디렉터리의 같은 이름 파일을 끈다(QEMU 규칙). `HostProfile::native`가 찾는다(macOS `{share}/firmware`, Linux `/etc/qemu/firmware`·`/usr/share/qemu/firmware`). `[hypervisor] secure_boot_code`·`secure_boot_vars`와 `swtpm`·`swtpm_setup` 설정은 S3(opld)에서 더한다. 그때까지 `QemuConfig.swtpm`은 `PATH`의 이름이다.
- **S2 회차(epoch)**: 노드의 `firmware.epoch`가 `RuntimeTpm.epoch`와 다르면 UEFI 변수는 늘 지우지만 TPM 상태는 `fresh`일 때만 지운다. `fresh`가 아니면 그 파일은 해시가 맞는 지금 것이거나 막 받아 온 것이기 때문이다. 다른 노드에서 vars·TPM을 받아 둔 쪽(S3 `fetch_tpm`·vars 받기, S5 `PushFirmware` 받기)은 `opl_driver::write_firmware_epoch`로 VM의 `firmware_epoch`를 적어야 한다. 그러지 않으면 켤 때 받아 온 vars를 지운다. 마이그레이션 받는 쪽(`start_incoming`)은 상태를 만들지 않고 빈 디렉터리로 swtpm을 띄운다.
- **S2 얼리기**: `Quiesced`에 `No`(묻지 않음·메모리 스냅샷)를 더했다. 복제 회차는 게스트를 잠깐 멈추기(`stop`) 직전에 얼리고 이어서 실행한(`cont`) 직후에 녹인다. 멈춘 게스트의 에이전트는 답하지 않기 때문이다. 증분 백업은 작업을 시작하는 transaction 앞뒤, 디스크 스냅샷은 transaction 앞뒤다. freeze가 실패해도(시간 초과면 얼었을 수 있으므로) thaw를 보내고, 녹이지 못하면 새 세션으로 세 번까지 다시 해 본다. 그래도 안 되면 `fsfrozen` 표시를 남겨 다시 붙을 때 녹인다. `replicate_disks`는 `ReplicateRequest.quiesce`를 받고 `(보낸 바이트, Quiesced)`를 돌려준다. S2의 opl-api 호출자는 모두 `quiesce: false`이고 S4가 `guest_agent.freeze_fs`로 정한다.
- **S2 오류·종료**: 새 `DriverError::GuestAgent(String)`(다시 시도하지 않는 오류, API는 409 `guest_agent_unavailable`). `StopMode::Agent`는 `guest-shutdown`의 답을 5초(`AGENT_TIMEOUT`)까지 기다리고, 그동안 답이 없거나 연결이 끊기면 보낸 것으로 본다. swtpm 실패 문구는 `swtpm: …`이고 swtpm 자신의 `swtpm: ` 머리말이 이어 붙는다. swtpm은 QEMU 모니터가 실행 디렉터리를 지우기 전, `kill_and_cleanup`, opld 재시작의 죽은 실행 디렉터리 정리에서 주인 확인 뒤 끝낸다.
- **S2 시험 기능**: `vtpm-it`는 지금 opl-driver에만 있다(`vtpm-it = ["qemu-it"]`, `qemu_driver.rs`의 `a_tpm_vm_runs_with_its_own_swtpm`, `qemu_migration.rs`의 `a_tpm_vm_migrates_with_its_state`). opld의 `vtpm-it`과 CI는 S6이다.
- **S3 받기·회차**: `FirmwareFetcher`에 `fetch_tpm(vm, node, sha256, epoch)`와 `push(&FirmwareCopy)`를 더했다(기본 구현은 `"this node cannot …"` 오류라 옛 시험용 가짜도 그대로 컴파일된다). `UefiVarsFetcher::fetch`는 VM을 읽어 `secure_boot`면 Secure Boot 변수 템플릿과 크기를 견주고, 받은 뒤 VM의 `firmware_epoch`를 노드에 적는다(S2 회차 규칙 때문에 적지 않으면 켤 때 지운다). `fetch_tpm`도 해시를 확인해 0700/0600으로 둔 뒤 회차를 적는다.
- **S3 켜기 규칙의 변수**: TPM 상태를 이 노드의 파일로 켜는 경우(규칙 1)와 새로 만드는 경우에도 변수는 지금처럼 `firmware_node`에서 받아 온다(받지 못하면 경고). 다른 노드에서 TPM 상태를 받으면 변수도 그 노드에서 받는다(사본은 둘을 함께 둔다). TPM 상태를 얻지 못하면 실패로 세지 않고 디스크를 끈 채 `retry_interval`마다 다시 본다(phase `Starting`, `last_error`는 `tpm_unavailable` 문구).
- **S3 해시·사본은 작업자가**: `firmware_node`가 이 노드인 TPM VM의 작업자가 켤 때·멈출 때 곧바로, 도는 동안 `firmware_sync_interval`(60초)마다 `tpm_state_path`의 SHA-256을 본다(크기·mtime이 같으면 다시 셈하지 않음). 해시가 바뀌면 `tpm_sha256`을 쓰고 `firmware_copies`를 비운 뒤, 사본이 없는 `firmware_copy_nodes` 대상에 `PushFirmware`로 밀어 두고 성공한 노드를 더한다. 꺼진 VM도 사본이 모자라면 같은 주기로 다시 해 보므로 "유지 보수에 들어가는 노드가 꺼진 TPM VM의 사본을 밀어 두기"는 따로 두지 않았다(그 노드에 배정된 VM이면 이미 밀어 둔다). 작업자의 status 쓰기는 `tpm_sha256`·`firmware_copies`를 바꾸지 않았으면 저장소의 값을 지키고, 그 사이 회차가 바뀌었으면(초기화) 늘 저장소의 값이며, `firmware_epoch`는 늘 저장소의 값이다.
- **S3 `PushFirmware`**: 이진 메시지 하나(vm, epoch, secure_boot, sha256 64자, TPM 바이트, 변수 조각 목록 — 0 조각은 길이만)다. 받는 쪽은 그 노드가 VM의 `firmware_node`이거나 VM의 QEMU가 그 노드에서 돌면 `failed_precondition`, 해시가 다르면 `invalid_argument`, 변수 크기가 VM에 맞는 템플릿(Secure Boot면 Secure Boot 템플릿, 없으면 거절)과 다르면 거절한다. 변수 → TPM 상태(0600) → 회차 순으로 둔다. TPM 상태는 4 MiB(`MAX_TPM_BYTES`)까지다.
- **S3 HA 복구 문구**: `firmware_node`가 죽었거나 오프라인인 TPM VM(해시 있음)은 `firmware_copies`의 노드만 대상이고, 없으면 `Pick::NoNode(tpm_unavailable(..))`라 다른 "놓을 곳 없음"처럼 `last_error`에 남는다(`VmHaStatus.error`는 placement 규칙 막힘에만 쓰는 지금 관례를 따랐다). 꺼진 채 두는 VM(`state: stopped`)은 거르지 않는다(켤 노드가 받아 온다).
- **S3 노드 능력**: opld가 드라이버를 만든 뒤 `Hypervisor::firmware_caps()`를 한 번 읽어 등록 때 `firmware_caps`로 쓰고, 노드 검사기(`checks.rs`)는 핫플러그처럼 노드 상태의 값이 다르면(옛 리더가 지움) 다시 쓴다. swtpm을 나중에 설치하면 opld를 다시 시작해야 능력이 바뀐다. 스케줄러는 `firmware_caps`가 없는 노드(옛 노드)를 능력 없음으로 본다. 새 설정 `[hypervisor] swtpm`·`swtpm_setup`·`secure_boot_code`·`secure_boot_vars`(뒤 둘은 함께).
- **S3 게스트 정보와 에이전트 종료**: 채널이 있는 VM(`agent_channel()`, 게스트 감시만 있는 VM 포함)이면 작업자가 `guest_info_interval`(60초)마다 묻고 답하면 opld `GuestInfoStore`에 둔다(답하지 않으면 앞 정보를 둔다). QEMU가 바뀌거나 꺼지거나 노드를 떠나면 지운다. 끌 때 `StopMode::Agent`는 `guest_agent.shutdown == agent`이고 이 QEMU에서 에이전트가 답한 적(정보 수집이나 게스트 감시 ping)이 있을 때만이다. `GuestInfoStore`는 정보가 한 번도 없으면 `no_agent`, 이 노드에서 돌지 않으면 `not_running`이다. `refresh`는 VM마다 5초에 한 번 지금 묻는다. 드라이버 오류는 `opld::guest::agent_error`로 `GuestAgentError::{NotRunning, NoAgent(채널 없음), Unavailable}`가 된다(S5는 `Unavailable` → 409 `guest_agent_unavailable`). 노드 RPC `GuestAgent`의 답은 `{info?, error?}` JSON이라 오류 종류가 그대로 간다. 비밀번호는 받는 쪽도 `validate_guest_password`로 다시 본다.
- **S3 `apply_hardware`**: 인자에 `&VmSpec`을 더했다. 기록의 `tpm`·`secure_boot`·`guest_agent`를 쓰되, 계획 59 전 기록(채널 칸 없음)은 S1의 `agent_channel_differs` 규칙처럼 스펙에 `guest_agent`가 없으면 스펙의 채널(게스트 감시)을 쓴다. 기록에만 TPM이 있으면(실행 중에 뺌) 이 노드의 상태로(`fresh: false`) 붙는다. 마이그레이션 받기는 장치가 다르면 S1의 `devices_match`가 이미 막는다.
- **S4 스냅샷의 TPM 사본 노드**: `SnapshotStatus.tpm_node`(없으면 쓰지 않음)를 더했다. 되돌리기가 "the TPM copy of snapshot … is on node …"를 노드 이름과 함께 알리고, VM을 끄기 전에 그만두려면 사본이 어느 노드에 있는지 알아야 한다. 찍을 때 TPM 상태가 없었으면(한 번도 켜지 않은 VM) 없고, 그런 스냅샷의 되돌리기는 지금 TPM 상태를 그대로 둔다.
- **S4 담을 TPM 상태 맞추기**: 스냅샷·백업은 담기 전에 TPM 상태를 이 노드에 맞춘다(`tasks/firmware.rs::current_tpm`, 켜기 규칙과 같은 `tpm_source`). 이 노드에서 QEMU가 돌면 이 노드의 파일, `firmware_node`이거나 해시가 같으면 이 노드의 파일(해시가 기록됐는데 파일이 없으면 오류), 아니면 `firmware_node`·사본 노드에서 받아 오고 그 노드의 변수도 받는다. 받지 못하면 TPM 없이 담지 않고 `tpm_unavailable` 문구로 실패한다(TPM 없이 담은 백업을 복원하면 복구 키를 묻는다).
- **S4 되돌리기**: TPM 사본이 다른 노드에 있으면 디스크·전원을 건드리기 전에 실패한다. TPM 사본을 되돌리면 변수 사본이 없어도 지금 변수를 이 노드로 받아 두고(이 노드가 `firmware_node`가 되므로), 스냅샷에 TPM 사본이 없는데 변수 사본을 되돌리는 TPM VM은 디스크를 건드리기 전에 지금 TPM 상태를 이 노드로 가져온다(못 가져오면 되돌리지 않는다 — 아니면 `firmware_node`가 옮겨 가며 TPM 상태의 자리를 잃는다). 되돌린 파일을 둔 뒤 이 노드의 회차 파일을 VM의 `firmware_epoch`로 적는다(S2 회차 규칙 때문에 적지 않으면 켤 때 변수를 지운다).
- **S4 초기화 Task**: VM status를 먼저 쓰고(회차 +1, 사본 비움, TPM이면 해시 지움) 이 노드의 파일을 지운 뒤 이 노드의 회차 파일을 새 회차로 적는다. 쓰기만 하고 파일 지우기가 실패해도 회차가 달라 켤 때 지워진다. `firmware_node`는 남는 파일의 자리일 때만 지킨다: 변수만이면 TPM VM은 지킨다(S1 보고), TPM만이면 이 노드가 `firmware_node`일 때만 지킨다(이 노드의 변수는 새 회차로 남는다. 다른 노드의 변수는 회차가 달라 켤 때 지워지므로 자리를 지운다). 사본은 변수만 초기화해도 비운다(사본의 변수가 낡았다 — 작업자가 다시 밀어 둔다). "Task id를 기록한 첫 쓰기에서만 회차를 올린다"는 타입 칸을 더하지 않고, 쓰기 결과를 모를 때(시간 초과) 다시 읽어 회차가 이 Task가 쓴 값까지 올랐으면 다시 올리지 않는 것으로 했다(중단된 Task는 다시 이어 하지 않고 실패로 끝나므로 Task 안에서만 지키면 된다). 한계: `firmware_node`가 오프라인이라 다른 노드에서 변수만 초기화한 TPM VM은, 나중에 다른 노드가 그 노드에서 TPM 상태와 변수를 받아 오면 낡은 변수가 돌아온다(받기가 새 회차를 적는다) — S5는 TPM VM의 변수만 초기화를 `firmware_node`에서 돌리는 것이 좋다.
- **S4 fs-freeze**: 디스크 스냅샷(실행 중, 메모리 없음, 디스크 있음)·백업의 임시 스냅샷·증분 백업·복제 회차가 `guest_agent.freeze_fs`로 `quiesce`를 넘기고 `Quiesced::warning()`을 Task `status.error`(성공한 Task의 경고)에 남긴다. 블록 스냅샷 디스크(LVM·ZFS·RBD)는 VM을 멈추고 저장소로 찍으므로 얼리지 않고 경고 `"… ({kind} disks are snapshotted with the VM paused); the snapshot is crash-consistent"`를 남긴다.
- **S4 복제 회차의 사본**: 회차가 끝나면 대상이 `firmware_copy_nodes`에 있고 지금 해시의 사본이 없을 때만 `FirmwareFetcher::push`로 밀어 두고 `firmware_copies`에 더한다(이 노드가 `firmware_node`일 때). 해시가 기록 뒤에 바뀌었으면 `push`가 거절하고, 작업자가 60초 안에 해시를 다시 적고 복제 대상부터 다시 밀어 둔다.
- **S4 마이그레이션 확정**: 확정 쓰기가 `firmware_node`를 대상으로 바꿀 때 `firmware_copies`를 비운다. 대상의 작업자가 확정보다 먼저 넘겨받아 `firmware_node`를 쓰는 경우(`own_firmware`)도 같게 비운다.
- **S4 상태 디렉터리 정리**: 리더만 도는 `ClusterJanitor`가 아니라 노드마다 도는 Task 실행기가 시작할 때와 `tag_sweep`(30분)마다 `{state_dir}/vm/{id}`를 본다. Raft에 VM이 없고(다른 노드가 맡은 VM도 VM이 있으므로 두고), 그 id를 대상으로 하는 끝나지 않은 Task가 없고, 디렉터리 mtime이 `state_dir_min_age`(새 `TaskRunnerConfig` 칸, 기본 1시간)보다 오래됐으면 지우기 바로 전에 VM을 한 번 더 보고 지운다. VM id 모양이 아닌 디렉터리는 두고, 목록을 읽지 못하면 아무것도 지우지 않는다.
- **S4 받기**: `backup_receive`는 매니페스트에 `tpm_state`가 있거나 VM 스펙에 `tpm`·`secure_boot`가 있으면 `vtpm`, `guest_agent`가 있으면 `guest-agent`를 모든 노드가 알아야 받는다(409 `cluster_upgrading`, 버리지 않는다). `tpm2-00.permall`은 받을 파일이고(4 MiB까지) `BackupStatus.tpm_state`·끝낸 매니페스트에 남는다. 원격 복제(`replicate.rs`)도 그 파일을 보내고 사본의 `tpm_state`를 견준다. 복원 Task도 `vtpm`을 모르는 노드가 있으면 실패한다(API의 409는 S5).
- **S4 메모리 스냅샷 검사**: `snapshot_mismatch`는 게스트 감시만이 아니라 QGA 채널(`agent_channel`: 스냅샷의 `guest_agent`가 있으면 지금 게스트 감시와 상관없이 채널이 있다)을 견주고, 기록된 하드웨어(`SnapshotSpec.hardware`)의 `tpm`·`secure_boot`가 스냅샷의 스펙과 다르면 막는다. 되돌리기는 스냅샷의 스펙을 쓰므로 TPM·Secure Boot는 기록이 어긋날 때만 걸린다.
- **S5 Secure Boot 바꾸기는 초기화와 함께**: 설계는 "변수가 있으면(`firmware_node`) PUT은 409 `firmware_reset_required`, 초기화 Task를 기다린 뒤 PUT"이었으나, TPM VM의 변수만 초기화는 TPM 상태의 자리로 `firmware_node`를 남기므로(S1·S4) 그 뒤의 PUT도 늘 409가 된다. 그래서 `POST /vms/{key}/firmware/reset`에 `secure_boot`(선택, `uefi_vars`가 있어야 함)를 더해 초기화 Task를 만든 다음 같은 잠금(`vm_lock`) 안에서 스펙의 `secure_boot`를 바꾼다. Task가 끝날 때까지 켜기(`start`·`reboot`, 꺼진 VM을 켜는 PUT)는 409 `vm_busy`이므로 낡은 변수로 켜지지 않는다. 409 문구는 `vm set … --secure-boot on --reset-uefi-vars`를 알린다(S6 CLI는 이 한 요청으로 한다). TPM 없는 VM은 변수만 초기화한 뒤 PUT해도 된다.
- **S5 초기화 노드**: 온라인 `firmware_node`, 없거나 오프라인이면 `vm_node`(디스크로 정한 노드 — 요청 노드보다 위트니스를 피한다). TPM 상태가 있는 VM(`tpm` 또는 `tpm_sha256`)의 변수만 초기화는 `firmware_node`가 오프라인이면 409 `node_unavailable`(S4 한계를 막음). 꺼져 있지 않으면 409 `vm_running`, 전원·마이그레이션·스냅샷·복제·백업 Task가 있으면 409 `vm_busy`.
- **S5 켜기 미리 보기 `tpm_state_unavailable`**: 꺼진 TPM VM의 `start`가 고른 노드로 `tpm_source`를 미리 보고 `Unavailable`이면 409(작업자 문구와 같음). 고른 노드가 `firmware_copies`에 있으면 그 노드의 파일이 지금 해시라고 본다. 실행 중이거나 끄는 중인 VM, PUT으로 켜는 길은 보지 않는다(작업자가 기다린다).
- **S5 게스트 API**: `GET /vms/{key}/guest`에는 기능 게이트가 없다(옛 노드의 VM은 `unsupported`, §10). 돌지 않으면(`status.node` 없음 또는 phase가 Running·Stopping이 아님) `not_running`, 스펙·도는 하드웨어 모두 채널이 없으면 `no_agent`로 묻지 않고 답한다. 다른 노드는 3초, `refresh`면 25초 한도다(에이전트에 다섯 명령을 묻는 데 20초까지 걸려 3초로는 늘 `unreachable`이 된다). 비밀번호는 `guest-agent` 게이트, 응답 200 `{user}`, 다른 노드가 닿지 않으면 503 `node_unreachable`, 그 노드가 RPC를 모르면 409 `cluster_upgrading`, 채널이 없어도 409 `guest_agent_unavailable`. 감사 설명은 비밀번호 검사가 통과한 뒤 붙인다.
- **S6 CLI**: `vm create|set --tpm on|off --secure-boot on|off --guest-agent on|off --agent-freeze on|off --agent-shutdown agent|acpi`(끄면 필드를 뺀다, `--agent-*`는 에이전트가 있어야 함). `vm set --secure-boot … --reset-uefi-vars`는 S5의 `POST /vms/{key}/firmware/reset {uefi_vars, secure_boot}` 한 요청으로 바꾸고 Task를 기다린 뒤, 다른 변경이 있을 때만 PUT한다(초기화를 먼저 보내 꺼져 있지 않으면 아무것도 바꾸기 전에 실패). `--reset-uefi-vars` 없이 Secure Boot를 바꾸면 서버의 `firmware_reset_required` 문구를 그대로 보인다. `vm create --preset windows11`은 머신·펌웨어를 q35·UEFI로(다르게 주면 거절), 디스크 버스 SATA·NIC e1000e, TPM·Secure Boot·`guest_agent: {}`, 코어 2·4096 MiB(따로 주지 않았을 때)를 채운다. CLI는 볼륨을 만들지 않으므로 "64 GiB 디스크"는 `--disk`로 준 볼륨의 버스만 바꾼다. 그래서 `--sockets`·`--cores`·`--memory`·`--firmware`는 기본값을 둔 옵션에서 생략 가능한 옵션(기본값은 같음)으로 바꿨다.
- **S6 `vm guest`**: `vm guest VM [--refresh]`과 하위 명령 `vm guest password VM --user U`를 한 명령에 둔다(`args_conflicts_with_subcommands`, 그래서 VM 이름 `password`는 `vm guest`로 볼 수 없다 — id로 준다). 비밀번호는 터미널이면 두 번 묻고, 아니면 표준 입력의 첫 줄이며 인자·출력에 없다. `vm firmware reset VM --uefi-vars|--tpm [--yes] [--no-wait]`는 `--tpm`일 때만 VM 이름을 쳐서 확인한다(터미널이 아니면 `--yes`). 표시: `vm get` stderr의 `tpm:`·`secure boot:`·`guest agent:` 줄, `node list`의 `TPM`·`SB` 열, `node get` stderr의 `firmware: tpm yes, secure boot no`.
- **S6 시험 고리**: `[test] firmware_sync_secs`(`qemu-it` 빌드만)로 TPM 해시·사본 주기(60초)를 줄인다. HA 시험이 사본이 지금 해시와 같아질 때까지 기다려야 하기 때문이다.
- **S6 시험**: `vtpm.rs`는 `qemu-it`(swtpm 없이도 되는 CLI 흐름: `no_agent`·`not_running`, 실행 중 초기화 409, `firmware_reset_required`, 변수 초기화와 회차, 능력 없는 노드의 거절), `vtpm-it`의 `mod swtpm`(상태 만들기·해시·swtpm 정리·`--tpm` 초기화 뒤 새 상태), `vtpm-it`+`install-media-it`의 `mod guest`(실제 게스트)다. 두 노드 시험은 `migration.rs`의 `a_tpm_vm_migrates_with_its_state`(라이브로 옮기기, 꺼진 VM을 다른 노드에서 켜면 받아 옴)와 `ha.rs`의 `an_ha_tpm_vm_recovers_on_the_node_with_its_copy`에 두었다. 설계의 "두 노드 실제 게스트로 옮긴 뒤 EK 비교"와 "`firmware_node`가 꺼졌을 때 `tpm_state_unavailable`"은 넣지 않았다: 앞의 것은 install-media 작업에 두 노드 틀(`Pair`)을 옮겨야 하고 상태 파일 옮기기는 `migration.rs`가 보며, 뒤의 것은 두 노드 클러스터에서 한 노드가 꺼지면 과반을 잃어 API가 답하지 않는다(S5 API 시험이 본다). 그래서 `Pair`는 `tests/common`으로 옮기지 않았다.
- **S6 발견 (a) 정해진 노드의 능력**: 노드가 이미 정해진 VM(`spec.node`, 한 번 켠 VM)의 `start`는 스케줄러를 거치지 않으므로 swtpm·Secure Boot 펌웨어가 없는 노드에서는 409가 아니라 노드가 켜다 실패해 `last_error`에 이유를 남긴다. 시험은 그 동작을 본다(바꾸려면 켜기 API가 `node_fits`의 능력 검사를 그 노드에도 하면 된다).
- **S6 발견 (b) AppArmor**: Ubuntu 24.04의 swtpm 꾸러미는 `/usr/bin/swtpm` AppArmor 프로필(강제)을 싣고, 허용 경로는 `/tmp/**`·`/var/lib/libvirt/swtpm/**`·`/var/lib/swtpm/**`·`/run/libvirt/qemu/swtpm/*` 등이다. 시험 디렉터리(/tmp)는 되지만 배포 경로 `/var/lib/orbivisor/vm/{id}/tpm`·`/run/orbivisor/vm/{id}/tpm.sock`은 막힐 수 있다. 패키지에 `/etc/apparmor.d/local/usr.bin.swtpm` 덧붙임(그 두 경로 `rwk`)을 두는 일을 S8(또는 다음 계획)에서 확인한다. Debian 13도 AppArmor가 기본으로 켜져 있다.
- **S6 CI**: `linux-kvm`에 `swtpm swtpm-tools`와 `opl-driver/vtpm-it,opld/vtpm-it`(시간 55 → 65분), `install-media`에 `swtpm swtpm-tools`, root가 아닌 사용자의 `swtpm_setup --create-config-files skip-if-exist`(없으면 `swtpm_localca`가 tss 소유 `/var/lib/swtpm-localca`에 쓰지 못한다)와 새 단계(시간 40 → 55분). 패키지 `Recommends`에 `swtpm, swtpm-tools`(`ovmf`는 이미 Depends).
- **S7 Secure Boot 바꾸기(웹)**: VM 고치기 폼은 `status.firmware_node`가 있는 VM의 Secure Boot 스위치를 막고 "VM 화면의 펌웨어 초기화에서 바꾸라"고 알린다(멈추지 않은 VM은 "정지하라"). 폼이 PUT과 초기화를 함께 보내지 않는 것은 실패했을 때 둘 중 무엇이 적용됐는지 알리기 어렵기 때문이다. VM 요약의 "보안" 카드가 `firmware_node`가 있으면 "Secure Boot 켜기/끄기" 버튼을 두고, 이것은 초기화 창을 Secure Boot 바꾸기(+UEFI 변수, 끌 수 없음)를 고른 채 연다(`{uefi_vars: true, secure_boot}`). 변수가 없는 VM은 지금처럼 폼(PUT)으로 바꾼다.
- **S7 초기화 창**: 설계대로 VM 이름을 입력해야 하고(`ConfirmDialog`의 `typeToConfirm`), 아무것도 고르지 않거나 VM이 멈추지 않았으면 보내지 않는다. TPM 상태 칸은 `tpm` 또는 `tpm_sha256`이 있는 VM에만, UEFI 변수·Secure Boot 칸은 UEFI VM에만 보인다.
- **S7 노드 능력**: 노드 상세 화면이 없어 노드 목록에 "펌웨어" 열(`TPM 예, Secure Boot 아니요`)을 더했다. `firmware_caps`가 없으면 "모름", 위트니스는 "—"다. 폼은 스케줄러처럼 능력을 모르는 노드를 능력 없음으로 보고, 켤 수 없으면 스위치를 막고 이유를 보인다(이미 켠 것은 끌 수 있고, 켠 채 쓸 수 없으면 오류색으로 알린다 — HA와 같은 관례).
- **S7 게스트 정보**: 요약의 "게스트 에이전트" 카드는 채널이 있는 VM(`agent_channel` 또는 `status.hardware.guest_agent`)만 묻고, "게스트" 탭은 열 때 묻는다. 둘 다 60초마다 다시 읽고(노드의 수집 주기), 쿼리 키에 `status.node`·`phase`를 넣어 켜고 끄거나 옮기면 다시 묻는다. "새로 고치기"는 누른 뒤 5초 동안 막는다. 비밀번호 창은 사용자 이름을 Windows 게스트(`os.id == "mswindows"`)면 `Administrator`, 아니면 `root`로 채우고, 두 번 입력하게 하며, 닫으면 입력을 버린다.

### 리뷰 1에서 고친 것

- **C1 에이전트 잠금 범위**: VM의 에이전트 잠금은 백업·복제 작업 전체가 아니라 얼리기부터 녹이기까지만 쥔다(`Freezer`가 잠금을 쥐고 `thaw`에서 놓는다). ping·정보 수집·에이전트 종료·비밀번호는 잠금을 2초(`AGENT_LOCK_WAIT`)까지만 기다리고, 넘으면 답 없음(ping) 또는 `GuestAgent("… busy …")`(정보·종료 — 종료는 ACPI로 넘어간다)이다.
- **C2 펌웨어 회차 옮기기**: 다른 노드에서 파일을 받은 뒤 회차를 적는 일은 이제 옮기기다(`opl_driver::adopt_firmware_epoch`). 이 노드의 회차가 다르면 받지 않은 파일을 먼저 지운다: 변수만 받으면 TPM 상태(해시가 VM의 지금 `tpm_sha256`과 같으면 남김), TPM만 받으면 변수, `PushFirmware`에 변수가 없으면 변수. 그래서 초기화 뒤 사본 노드의 낡은 TPM이 되살아나지 않고, Secure Boot를 바꾼 뒤 낡은 변수가 지금 것으로 보이지 않는다.
- **I1 TPM 없는 VM의 회차**: `VmRuntimeSpec.firmware_epoch`(serde 기본 0)를 더했다. TPM이 없는 UEFI VM도 켤 때 노드의 회차와 다르면 그 노드의 변수를 지우고 템플릿에서 만든다(`tpm::prepare_vars`).
- **I2 TPM을 끈 채 옮긴 VM**: 켜기 규칙의 Local은 이 노드의 해시가 같거나, `firmware_node`가 이 노드이고 파일이 있을 때다(파일이 없으면 사본에서 받는다). `firmware_node`를 옮길 때(`own_firmware`, 마이그레이션 확정) TPM이 같이 오지 않았으면(`spec.tpm`·`hardware.tpm` 모두 거짓이고 해시가 있음) 앞 `firmware_node`를 `firmware_copies`의 첫 자리로 남긴다(`firmware::move_firmware_node`). 그 노드에서 TPM을 받을 때 이 노드가 `firmware_node`면 변수는 받지 않는다(이 노드의 것이 지금 것). 켜기 API의 미리 보기는 `firmware_node`도 파일이 있다고 본다.
- **I3 상태 디렉터리 정리**: 확인했다 — 정리는 실행기가 시작하자마자 로컬 클러스터 상태로 돌았다. 이제 시작할 때는 후보만 모으고 다음 차례(`tag_sweep`, 30분 뒤)에도 후보이고 이 노드가 받은 기록을 다 적용했을 때(`revision() >= known_revision()`)만 지운다. 지운 VM은 기록이 없으므로 "마지막 `firmware_node`" 검사는 둘 수 없다.
- **I4 남은 얼림**: 녹이기는 적어도 세 번, `FREEZE_TIMEOUT`+5초가 지날 때까지 다시 해 본다. 그래도 남은 `fsfrozen` 표시는 다음 ping·정보 수집(잠금을 쥔 뒤)이 먼저 녹이고, 얼린 뒤 녹이기 전에 버려진 `Freezer`(취소)는 `Drop`에서 녹이기를 띄운다.
- **I5 남은 TPM 비밀**: 새 노드 사이 RPC `PurgeFirmware`(JSON, `{vm, epoch?, uefi_vars, tpm, snapshot?}`)를 더했다. 초기화 Task는 이 노드의 파일을 지운 뒤 다른 온라인 노드(위트니스 빼고) 모두에 회차와 함께 보낸다 — 받는 노드는 회차가 다를 때만 지우고 VM이 거기서 돌면 거절한다. 어느 노드를 거쳐 갔는지 모두 기록하지 않으므로 모든 노드에 보낸다. 닿지 않는 노드의 파일은 회차가 달라 켤 때 지워진다. 스냅샷 지우기는 `tpm_node`가 다른 노드면 그 노드에 `snapshot`을 보내 사본을 지운다(오프라인이면 경고, 온라인인데 실패하면 Task 실패).
- **I6 AppArmor**: 패키지가 conffile `/etc/apparmor.d/abstractions/orbivisor-swtpm`(기본 `/var/lib/opl/vm/*/tpm/**`·`/run/opl/vm/*/{tpm.sock,swtpm.pid,swtpm.log}`)을 싣고, postinst가 swtpm 꾸러미의 `/etc/apparmor.d/local/usr.bin.swtpm`에 `include <abstractions/orbivisor-swtpm>`을 한 번만 더한 뒤 프로필이 있고 AppArmor가 켜져 있으면 `apparmor_parser -r -T -W`로 다시 읽힌다. purge는 그 줄만 뺀다. 다른 `data_dir`·`run_dir`은 관리자가 local 파일에 더한다. TPM 능력 확인은 `--print-capabilities`·`-tpmdev help`에 더해 실제 상태·실행 디렉터리(`vm/{버릴 id}`)에서 swtpm을 VM과 같은 인자로(떼어 내지 않고) 5초까지 띄워 소켓·pid 파일이 생기는지 보고, 끝낸 뒤 디렉터리를 지운다. AppArmor가 막으면 VM 켜기 실패가 아니라 `firmware_caps.tpm = false`(경고 로그)다.
- **M1 Secure Boot와 초기화**: `TaskOp::VmFirmwareReset`에 `secure_boot`(선택)를 더해 Task가 스펙의 `secure_boot`와 초기화를 함께 쓴다(스펙 먼저, 다시 해도 됨). API는 Task를 만든 뒤 스펙을 쓰지 않는다 — 스펙은 Task가 끝나면 바뀐다.
- **M2 TPM 해시 캐시**: 열쇠에 inode와 ctime(나노초)을 더했다.
- **M3 HA와 낡은 사본(문서만)**: 사본은 마지막 밀어 두기(`firmware_sync_interval`, 60초) 시점이라, 그 사이 TPM에 쓴 것(NV 카운터, 새로 만든 키)은 다른 노드로 복구하면 없어진다. 복제 회차의 디스크와 TPM 시점이 다를 수 있는 것과 같은 설계 결과로 받아들이고 README 한계에 적는다(S8).
- **M4 실제 게스트 시험**: 보고 스크립트는 파이썬 표준 라이브러리만 쓴다 — TPM은 sysfs(`/sys/class/tpm/tpm0`, `tpm_version_major`), EK는 `/dev/tpmrm0`에 보낸 `TPM2_ReadPublic`(swtpm_setup이 EK를 0x81010001 등에 영구로 둔다)의 답 해시. `qemu-guest-agent`만 cloud-init `packages`로 넣고, 설치되지 않으면 게스트 정보·얼린 스냅샷의 경고 없음·에이전트 종료 검사를 건너뛴다고 알린다. CI 시간: `linux-kvm` 65 → 80분, `install-media` 55 → 60분.

### GitHub 탐침에서 고친 것

- **TPM 능력 거짓 음성**: QEMU는 `-tpmdev help`를 표준 오류로 찍는다(`error_printf`, 8.2와 11.1 같음). 능력 확인이 표준 출력만 읽어 swtpm이 있는 Ubuntu 노드도 `firmware_caps.tpm = false`였다(S2 확인 (b)는 터미널에서 둘을 함께 봤다). 이제 둘 다 읽고, 실패 이유를 경고로 남긴다. CLI 시험은 노드가 알린 능력대로 TPM VM이 켜지는지(능력 있음) 또는 이유를 남기고 켜지지 않는지(없음) 본다.
- **Linux에서만 컴파일하는 시험**: `opl-driver/tests/qemu_bridge.rs`의 `VmRuntimeSpec`에 계획 59 칸을 더했다. 이 Mac에서 `target_os` 조건을 바꾼 사본으로 모든 기능을 검사해 다른 곳은 없음을 확인했다.

### 리뷰 2에서 고친 것

- **모든 세대 기록에 같은 규칙(#1a·1b·1c)**: 노드의 `firmware.epoch`를 쓰는 길은 이제 모두 `opl_driver::adopt_firmware_epoch` 하나를 거친다 — 받기(`UefiVars`·`TpmState`), 사본 받기(`PushFirmware`), 초기화 Task, 스냅샷 되돌리기, 켜기(`prepare_vars`·`prepare_state`). 이 노드의 회차가 VM의 회차와 다르면 막 받았거나 되돌린 파일만 남기고 나머지를 지운 뒤 회차를 적는다. TPM 상태는 지금 것이면 남긴다: 이 노드가 `tpm_node`이거나 파일 해시가 `tpm_sha256`과 같다(`firmware::tpm_is_current`). 유일한 지금 사본을 지우지 않기 위해서다. 초기화 Task는 status를 바꾸기 전에 이 노드의 회차가 초기화 전 VM의 회차와 같았는지와 TPM이 지금 것인지를 보고, 고르지 않은 파일도 그 규칙으로 지운다. 켜기는 작업자가 정한 `VmRuntimeSpec.keep_tpm`(serde 기본 거짓)으로 TPM을 남길지 정하고, `RuntimeTpm.fresh`는 이제 "없으면 새로 만든다"만 뜻한다.
- **TPM 보관 노드 따로(#2·#3)**: `VmStatus.tpm_node`(없으면 쓰지 않음)를 더했다. TPM VM을 켠 노드, 마이그레이션 대상, TPM을 되돌리거나 복원한 노드가 쓰고, TPM을 초기화하면 지운다. `firmware_node`는 이제 UEFI 변수의 노드일 뿐이다. 켜기 규칙의 Local은 해시가 같거나 `tpm_node`가 이 노드이고 파일이 있을 때다. 그래서 TPM을 끈 채 다른 노드에서 켜 변수의 노드가 된 노드의 낡은 TPM 파일이 지금 것으로 보이지 않는다. 받는 순서는 `tpm_node`, 그다음 사본이다. 리뷰 1 I2의 "앞 `firmware_node`를 사본 첫 자리로" 우회는 뺐다. TPM 해시 기록과 HA 복구의 보유 노드, 켜기 API 미리 보기, `PushFirmware` 거절도 `tpm_node`를 본다. 사본 밀어 두기는 이 노드가 `firmware_node`이기도 할 때만 한다. 변수만 초기화하면 `firmware_node`는 지우지만 `tpm_node`와 사본은 그대로 둔다. 그 사본 노드의 변수는 회차가 달라 `UefiVars`를 주는 노드가 거절한다(VM의 회차와 자기 회차를 견줌, 회차 파일을 못 읽으면 준다). TPM 상태를 받은 뒤 변수는 온라인인 `firmware_node`에서, 없거나 오프라인이면 TPM을 준 노드에서 받는다(`firmware::vars_source`).
  - 업그레이드: 계획 59 전 VM은 `tpm_node`와 `tpm_sha256`이 없다. TPM VM은 처음 켤 때 `fresh`이므로 문제가 없다. 이 단계 사이의 개발 기록(해시는 있고 `tpm_node`가 없음)은 배포한 적이 없어 `firmware_node`로 대신하지 않았다.
- **동결 풀기 잠금(#4)**: `VmHandle.agent`는 이제 `Arc<Mutex<()>>`이고, `Freezer`는 `OwnedMutexGuard`를 쥔다. 버려진 얼리기의 `Drop`이 띄우는 녹이기가 그 잠금을 넘겨받아 끝날 때까지 쥔다. 그래서 다음 회차가 그 사이 얼리지 못하고, 앞 회차의 녹이기가 새 회차의 얼림을 풀거나 그 표시를 지우지 않는다.
- **세대 파일 오류 안전(#5)**: 회차 파일을 읽지 못하면(깨짐, 빈 파일, 입출력 오류) 모르는 것으로 본다. 경고를 남기고 아무것도 지우지 않은 채 VM의 회차를 다시 적는다(`adopt_firmware_epoch`, `firmware_epoch_differs`). `PurgeFirmware`도 지우지 않는다. `write_firmware_epoch`는 임시 파일을 fsync한 뒤 rename하고 디렉터리도 fsync한다. 계획 59 전 UEFI VM(회차 파일 없음, VM 회차 0)은 첫 켜기에 변수를 지키고 회차 파일을 쓰지 않는다(`upgraded_uefi_vms_keep_their_vars`).
- **정리의 리더 확인(#6)**: 새 리더 요청 `LeaderRequest::ReadIndex`를 더했다. 리더가 `ensure_linearizable`로 과반의 리더인지 확인하고 그 인덱스를 돌려준다. `Cluster::read_index(timeout)`은 그 인덱스까지 이 노드에도 적용되기를 기다린다(팔로워·관찰 노드에서도 쓴다). 상태 디렉터리 정리는 `revision() >= known_revision()` 대신 이것이 성공해야 지운다. 리더에 닿지 못하는 노드(혼자 떨어짐, 비었거나 낡은 Raft 상태)는 후보만 모은다. 옛 리더는 이 요청을 읽지 못해 실패하므로 섞인 클러스터에서는 지우지 않는다.
- **낡은 TPM 비밀 주기 정리(#7)**: 같은 정리 차례가 VM이 있는 디렉터리도 본다. 이 노드의 회차를 읽었고 VM의 회차와 다르며, TPM 상태가 지금 것이 아니고, VM이 이 노드에서 돌지 않고, 끝나지 않은 Task의 대상이 아니면 두 차례 연속(같은 VM 회차) 본 뒤 `tpm/`만 지운다. 변수는 켤 때 지운다. 초기화와 함께 돈 `push_copies`가 퍼지 뒤 옛 TPM을 다시 둔 경우도 회차가 달라 이것이 지운다.
- **TPM 담기(#8)**: 스냅샷·백업의 `current_tpm`은 기록된 TPM 상태가 없을 때(`Fresh`) 이 노드의 회차가 VM의 회차와 같아야 이 노드의 파일을 담는다.
- **탐침 swtpm(#9)**: 능력 확인의 swtpm은 `--terminate`를 단다. 확인한 뒤 제어 소켓에 붙었다 끊어 스스로 끝나게 하고, 남으면 죽인다. Linux에서는 `PR_SET_PDEATHSIG`(SIGKILL)로 opld가 그 사이 죽어도 남지 않는다. 능력은 여전히 opld가 시작할 때만 본다. swtpm을 나중에 설치하면 opld를 다시 시작해야 한다(S8 README 한계에 적는다).
- **TPM만 초기화(#10)**: `firmware_node`가 오프라인이면 TPM만 초기화도 409 `node_unavailable`이다. 회차가 오르면 그 노드의 변수와 부팅 항목도 함께 잃기 때문이다. 둘 다 초기화하거나 그 노드가 돌아온 뒤 한다.
