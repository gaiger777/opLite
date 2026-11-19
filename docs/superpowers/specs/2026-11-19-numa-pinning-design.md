# NUMA 구조 수집·vNUMA·전용 CPU 고정·hugepages 설계 (계획 64)

작성일: 2026-11-19
상위 문서: `2026-10-20-opl-node-core-design.md`(VM 스펙·QEMU 명령줄·드라이버), `2026-10-30-live-migration-design.md`, `2026-11-11-availability-scale-design.md`(계획 50 배치 규칙·계획 53 배치·계획 54 핫플러그와 하드웨어 기록), `2026-11-17-templates-linked-clones-design.md`(계획 58), `2026-11-18-ui-ux-redesign-design.md`(계획 61), `2026-11-18-folders-permissions-quotas-2fa-design.md`(계획 62 권한·할당량), `2026-11-19-cpu-models-design.md`(계획 63 노드 탐침·`cpu_resolved`)
기준 트리: 계획 63 끝(`c8df968`). 이 문서는 구현 전 설계다. 스테이지 보고는 구현하면서 덧붙인다.

## 1. 목적

사용자가 "CPU NUMA·고정이 있나"를 물었다. 없다. 게스트는 `-smp sockets=S,cores=C,threads=1`과 `-m`뿐이고 NUMA 노드는 `maxmem`이 있을 때 QEMU가 저절로 만드는 하나뿐이다(`opl-qemu/src/cmdline.rs`). vCPU·에뮬레이터 스레드는 어디서든 돌고, 게스트 메모리는 호스트 NUMA 노드에 묶이지 않으며, hugepages가 없고, 노드는 CPU 수(`NodeStatus.cpus`)와 메모리 합만 알린다. 관련된 것은 계획 50의 VM↔노드 배치 규칙뿐이다. 그래서 (1) 호스트 노드보다 큰 VM은 게스트가 NUMA를 모른 채 원격 메모리를 쓰고, (2) 지연에 민감한 VM(DB·NFV·실시간)이 다른 VM과 pCPU를 나눠 쓰며, (3) 큰 메모리 VM이 TLB 비용을 줄일 길이 없다.

이 계획은 **노드 토폴로지 수집**, **게스트 vNUMA**, **전용 CPU 정책(pCPU 독점 고정)과 스레드·에뮬레이터 정책**, **hugepages(2M·1G)**, **호스트 노드 메모리 묶기**를 넣고, 전용 pCPU·hugepages를 **노드 자원으로 Raft에서 세어** 동시 배치가 같은 자원을 두 번 잡지 않게 한다. 원칙 넷:

- **호스트에 매인 결정(어느 pCPU, 어느 호스트 노드)은 QEMU가 도는 노드가 켤 때 정하고 기록한다**(계획 63의 `cpu_resolved`와 같은 길). 마이그레이션·HA는 대상에서 새로 정한다.
- **게스트가 보는 것(vNUMA 모양, 페이지 크기)은 켤 때 정해 기록하고, 라이브 마이그레이션 동안 바꾸지 않는다.**
- **Orbivisor는 커널 명령줄을 고치지 않는다.** hugepages·isolcpus는 보고만 하고, 노드마다 고를 수 있는 실행 중 hugepages 예약만 한다(§7).
- **이 기능을 쓰지 않는 VM·노드는 바이트까지 지금과 같다**(JSON·명령줄·띄우는 방법).

| | OpenStack Nova | Proxmox VE 8 | vSphere 8 | Orbivisor 계획 64 |
|---|---|---|---|---|
| CPU 정책 | `hw:cpu_policy=shared\|dedicated\|mixed` | 없음(`affinity`: QEMU 프로세스 전체의 CPU 목록) | Latency Sensitivity High(100% CPU 예약 → pCPU 독점), `sched.cpu.affinity` | `cpu.policy` `shared`(기본)·`dedicated`(§5) |
| 스레드·에뮬레이터 | `hw:cpu_thread_policy=prefer\|isolate\|require`, `hw:emulator_threads_policy=share\|isolate` | 없음 | 호스트가 정함 | `cpu.thread_policy` 같은 셋, `cpu.emulator` `shared`·`isolate` |
| 노드 CPU 나누기 | `cpu_dedicated_set`·`cpu_shared_set`(nova.conf) | 없음 | 없음 | 노드 `host_resources`: 예약·전용, 나머지가 공유(§7) |
| vNUMA | `hw:numa_nodes`, `hw:numa_cpus.N`·`hw:numa_mem.N` | `numa: 1`, `numaN: cpus=,hostnodes=,memory=,policy=` | 9 vCPU 이상이면 저절로, 소켓당 코어로 모양 | `numa.nodes` `auto`·개수·직접 목록(§8) |
| 호스트 메모리 묶기 | NUMA VM은 늘 strict | `policy=bind\|preferred\|interleave` | NUMA 스케줄러의 home node, `numa.nodeAffinity` | `numa.memory` `strict`·`preferred`(전용·hugepages면 strict) |
| hugepages | `hw:mem_page_size=small\|large\|any\|2MB\|1GB`, 커널 명령줄로 예약 | `hugepages: 2\|1024\|any`, 켤 때 노드마다 실행 중 할당 | 2M 기본, 1G는 `sched.mem.lpage.enable1GPage` | `hugepages` `2M`·`1G`·`any`, memfd hugetlb + prealloc(§10) |
| 자원 세기 | placement `PCPU`·`VCPU` 클레임 + NUMATopologyFilter | 없음(사람이 고름) | DRS가 예약을 지킴 | Raft 받아들이기 검사 + 스케줄러 NUMA 맞춤 + 노드 할당기(§9) |
| 마이그레이션 | 대상에서 고정을 다시 셈(Train부터) | 같은 설정 그대로 | vMotion이 home node를 다시 고름 | 게스트 모양은 기록 그대로, 대상이 고정을 다시 셈(§11) |

## 2. 범위

| 넣는 것 | 넣지 않는 것(§16) |
|---|---|
| 노드 토폴로지 탐침(소켓·코어·스레드·NUMA 노드·노드 메모리·hugepages·거리·L3 묶음·isolcpus) | 커널 명령줄 고치기(`isolcpus`·`hugepagesz`), 호스트 서비스 CPU 묶기 |
| 노드 `host_resources`(예약·전용 CPU, 실행 중 hugepages 예약) | Proxmox식 켤 때마다 hugepages 할당·반납 |
| VM `cpu.policy`·`thread_policy`·`emulator`, `numa`, `hugepages` | `mixed` 정책(VM 안에서 전용·공유 vCPU 섞기), 실시간 vCPU(`hw:cpu_realtime`) |
| `-numa`·노드마다 메모리 백엔드(memfd hugetlb, `host-nodes`, `policy`, `prealloc`), vCPU·에뮬레이터 스레드 고정, 스코프 `AllowedCPUs` | 게스트에 SMT 보이기(`threads=2`), iothread, vhost 스레드 따로 묶기 |
| 전용 pCPU·hugepages의 Raft 받아들이기 검사, 스케줄러 NUMA 맞춤, 노드 할당기 | 호스트 예약 메모리·HA 용량 예약의 전용 CPU 셈 |
| 마이그레이션·HA·비우기의 NUMA 확인, 대상의 다시 고정 | 균형 조정기가 전용 VM을 옮기기(건너뜀) |
| 보류 `numa`, 핫플러그(계획 54)와 CPU 모델(계획 63)의 맞물림 | VMX `numa.*`·Latency Sensitivity 가져오기 |
| API·CLI·웹, 템플릿·복제·가져오기·복원, 권한 `vm.dedicated`, 할당량 `dedicated_vcpus` | PCI 장치 NUMA 친화(장치 통과가 아직 없음) |

**기능 하나**(`opl_types::node`, `FEATURES` 끝): **`numa`**(`NUMA`). VM의 `cpu.policy`·`cpu.thread_policy`·`cpu.emulator`·`numa`·`hugepages`, 노드 `host_resources`, `NodeStatus.topology` 쓰기, 할당량 `dedicated_vcpus`, 노드 사이 RPC `ReadTopology`를 가린다. 모든 노드가 알릴 때만 쓰고 아니면 409 `cluster_upgrading`이다(`require_feature`, 검증보다 먼저 — 계획 58~63과 같다). 까닭은 §12.

## 3. 지금 있는 것 (계획 63 끝)

- **명령줄**(`cmdline.rs`): `-name guest=…,debug-threads=on`(QEMU 스레드에 `CPU 0/KVM` 같은 이름이 이미 붙는다), `-smp sockets=S,cores=C,threads=1`, 핫플러그면 `-smp B,sockets=S,cores=max/S,threads=1,maxcpus=max`이고 켤 때 vCPU는 소켓 0부터 채운다(`i / per_socket`). 메모리는 `-m M` 또는 `-m M,maxmem=X` + `memory-backend-ram,id=vmem0` + `virtio-mem-pci,…,block-size=2M`(`VIRTIO_MEM_BLOCK`). 부팅 메모리에는 백엔드 객체가 없고 `-numa`도 없다("NUMA 노드는 `maxmem`이 있으면 QEMU가 만든다").
- **드라이버**(`opl-driver/src/qemu.rs`): Linux는 `systemd-run --scope --unit=opl-vm-<id>`(기본 슬라이스 `system.slice`, `Before=opld.service`)로 감싸고 `-daemonize`로 띄운다(`START_TIMEOUT` 30초). `AllowedCPUs`·CPU 친화는 없다. macOS는 스코프가 없다. vCPU 핫플러그는 x86_64만이고 `device_add <model>-x86_64-cpu,socket-id,core-id,thread-id=0`이다. QMP `query-cpus-fast`는 계획 63의 `vcpu_props`가 이미 부른다.
- **스케줄러**(`scheduler.rs` `fits`): 템플릿·위트니스·온라인·유지보수·종료 중·HA 펜싱·아키텍처·저장소·브리지·방화벽·펌웨어 능력·CPU 모델. **메모리는 거르지 않는다**("메모리가 모자라도 고른다") — `free_memory_mib`는 순위와 균형 조정에만 쓴다. HA 복구는 `Assign`으로 같은 회차의 앞 배치를 센다. `state.vm_lock`은 API 노드 하나 안의 잠금이라 두 API 노드의 동시 켜기는 서로 모른다.
- **상태 머신 검사**(`opl-cluster`): 클러스터에서 하나만 쓸 값(`exclusive.rs`, `InUse` — 볼륨·MAC·스냅샷 이름·폴더 형제 이름)과 폴더 할당량(`folders.rs` `QuotaCheck`, `QuotaExceeded` — "쓰기 뒤 상한을 넘고 쓰기 전보다 늘었다")을 커밋 순서대로 본다. 여러 노드의 API가 동시에 써도 하나만 성공한다. 이번 자원 세기의 본이다.
- **하드웨어 기록**: `RunningHardware`(켤 때의 `cpu`·`memory_mib`·`layout`·`cpu_resolved` …)를 받는 쪽이 `apply_hardware`로 그대로 쓴다. `PendingChange.field`는 문자열이다. 마이그레이션 받기 응답은 `MigrateIncomingResponse { port, nbd_port, refused }`이고 받는 쪽은 `opl-api/src/incoming.rs`(대상 노드의 opld 안)다.
- **노드 능력의 본**(계획 54·59·63): `member.rs`가 등록 전에 읽고 검사 루프(`checks.rs`, 5초)가 저장된 값과 다를 때만 쓴다. 계획 63은 모든 노드가 기능을 알릴 때까지 쓰지 않는다(옛 리더가 모르는 칸을 지워 회차마다 다시 쓰는 것을 피함).
- **노드 스펙**: `NodeSpec`은 `deny_unknown_fields`이고 `roles`가 기능 게이트 뒤 새 칸의 본이다. `PUT /nodes/{key}`가 쓴다.
- **권한·할당량**(계획 62): 권한 비트 목록(`vm.config`·`vm.create` …, 새 권한은 operator에 넣지 않는다), 할당량 차원 `vcpus`·`memory_mib`·`disk_bytes`·`vms`(할당 기준, 전원과 무관).
- **CI**: `linux-kvm` 작업은 root·KVM·systemd 스코프(`OPL_TEST_SYSTEMD`)로 돈다. 러너는 4 vCPU·NUMA 노드 하나다.

## 4. QEMU·커널에서 확인한 사실

이 Mac의 Homebrew QEMU 11.1.2로 QMP를 직접 불러 확인했다(`-nodefaults -display none -S -qmp stdio`). Linux 전용(memfd·virtio-mem·`host-nodes`)은 QEMU 문서에서 옮겼고 스테이지 4가 GitHub `linux-kvm`에서 확인한다.

- **`-numa`**: `-object memory-backend-ram,id=m0,size=512M -numa node,nodeid=0,memdev=m0 … -numa cpu,node-id=0,socket-id=0 -numa dist,src=0,dst=1,val=21`이 q35·virt(TCG·HVF) 모두 켜진다. `-numa node,mem=`은 q35에서 오류("use -numa node,memdev instead"). 노드 메모리 합은 `-m`과 같아야 한다("total memory for NUMA nodes (0x30000000) should equal RAM size (0x40000000)"). 크기 0 백엔드는 안 된다. `maxcpus`까지 모든 CPU를 노드에 매지 않으면 경고("ability to start up with partial NUMA mappings is obsoleted and will be removed") — **늘 모든 소켓을 맨다.** 한 소켓을 두 노드에 나누는 매핑(`cpus=0`·`cpus=1-3`)은 오류가 아니지만 게스트 토폴로지가 어긋나므로 **노드는 소켓 단위**로 한다.
- **핫플러그 자리**: `-numa cpu,node-id,socket-id`가 있으면 `query-hotpluggable-cpus`의 빈 자리에도 `node-id`가 붙는다. `device_add`는 `node-id`를 주지 않아도 QEMU가 매핑에서 채운다 — 계획 54의 핫플러그 코드는 바뀌지 않는다.
- **`-machine memory-backend=`와 `-numa memdev`는 함께 못 쓴다**("mutually exclusive"). `memory-backend=`는 `maxmem`(저절로 생기는 노드)과 함께 된다. 이 계획은 메모리를 바꾸는 VM에 늘 `-numa`를 쓰고 `memory-backend=`는 쓰지 않는다(§10).
- **이 Mac QEMU**: `host-nodes`·`policy`는 "NUMA node binding are not supported by this QEMU", `thread-context,node-affinity`도 같다. `memory-backend-memfd`·`virtio-mem-pci`가 없다(Linux 빌드에만). 그래서 macOS는 호스트 묶기·hugepages·전용 CPU를 못 하고 게스트 vNUMA만 된다.
- **`query-cpus-fast`의 `thread-id`**: macOS에서는 TCG·HVF 모두 vCPU마다 같은 값(프로세스 id)이다 — 스레드를 고를 수 없다. Linux는 KVM vCPU마다, TCG도 MTTCG(같은 아키텍처 손님의 기본)면 vCPU마다 스레드가 있다(svc01 aarch64 TCG에서 스테이지 4가 확인한다, §14). vCPU 스레드는 `-S`로 멈춘 동안에도 있으므로 `cont` 전에 고정한다. 받는 쪽 QEMU도 마이그레이션 전에 고정한다. `props`에 `socket-id`·`core-id`·`node-id`가 있어 `cpu-index` 대신 자리로 찾는다.
- **memfd·virtio-mem(QEMU 문서)**: `memory-backend-memfd,hugetlb=on,hugetlbsize=2M|1G`는 hugetlbfs 마운트 없이 hugepages를 쓴다. 모든 메모리 백엔드에 `host-nodes`·`policy=default|preferred|bind|interleave`·`prealloc`·`prealloc-threads`가 있다. `virtio-mem-pci`에는 `node=`와 실행 중 꽂을 때 미리 잡는 `prealloc=on`이 있고 `block-size`는 백엔드 페이지 크기 이상이어야 한다 — 지금 2M 고정이라 **1G hugepages와 virtio-mem은 함께 못 쓴다.**
- **prealloc은 `-daemonize`의 부모가 끝나기 전에 한다**(객체 만들 때). 큰 VM은 30초 `START_TIMEOUT`을 넘을 수 있다(§10).
- **커널**: `/sys/devices/system/node/node*/{cpulist,meminfo,distance,hugepages/hugepages-<kB>kB/{nr,free,surplus}_hugepages}`, `/sys/devices/system/cpu/cpu*/topology/{physical_package_id,core_id,die_id,cluster_id,thread_siblings_list}`, `cpu*/cache/index*/{level,type,shared_cpu_list}`, `/sys/devices/system/cpu/{online,isolated,nohz_full}`, `/sys/kernel/mm/hugepages/`, `/proc/meminfo`의 `Hugepagesize`. `CONFIG_NUMA`가 없으면 `node` 디렉터리가 없다. root는 `nr_hugepages`를 노드마다 실행 중 바꿀 수 있다 — 2M은 대개 되고 1G는 부팅 뒤 조각 때문에 모자라기 쉽다. `sched_setaffinity`는 cgroup `cpuset` 안에서만 되고, root는 친화를 넓힐 수 있다(상한은 cpuset). systemd `AllowedCPUs=`(cgroup v2 `cpuset.cpus`)는 스코프에 `systemd-run --property`로, 도는 스코프에는 `systemctl set-property --runtime`으로 건다.
- **시험 장비**: svc01·svc02는 aarch64 TCG·4 CPU·NUMA 노드 하나·`HugePages_Total 0`·`Hugepagesize 2048 kB`(호스트 설정은 바꾸지 않는다). GitHub 러너는 x86_64 KVM·4 vCPU·노드 하나·root.

## 5. 타입 (opl-types)

새 모듈 `opl_types::numa`. 새 칸은 모두 `#[serde(default, skip_serializing_if = …)]`라 쓰지 않으면 JSON이 지금과 바이트까지 같다.

- **`CpuSet`**: Linux cpulist 문자열(`"0-3,8,10-11"`)로 직렬화하는 `BTreeSet<u16>`. 맞춘 꼴(오름차순·범위 합침)로 쓴다. 4096까지.
- **VM CPU**(`CpuSpec`에 더함):
  - `policy: CpuPolicy` — `shared`(기본, 쓰지 않음)·`dedicated`.
  - `thread_policy: Option<ThreadPolicy>` — `prefer`(기본: 형제 스레드를 먼저 채움)·`isolate`(vCPU마다 코어 하나, 형제는 비워 둠)·`require`(vCPU를 형제 짝으로, SMT 호스트만). `dedicated`일 때만(아니면 400).
  - `emulator: Option<EmulatorPolicy>` — `shared`(기본: 노드의 공유 CPU)·`isolate`(전용 pCPU 하나를 더 잡음). `dedicated`일 때만.
- **`VmSpec.numa: Option<NumaSpec { nodes: NumaNodes, memory: Option<MemoryPolicy> }>`**:
  - `NumaNodes`는 JSON에서 `"auto"`, 정수 1~8(`MAX_NUMA_NODES`), 또는 `[{ "cpus": 8, "memory_mib": 16384 }, …]`. 개수는 `sockets`를 나눠야 한다. 직접 목록의 `cpus`는 vCPU **자리** 수(핫플러그 상한 `max_vcpus` 기준)이고 소켓당 자리 수의 배수, 합은 상한, `memory_mib` 합은 `memory_mib`(400 `invalid_numa`). 노드는 앞 소켓부터 차례로 갖는다.
  - `MemoryPolicy` — `strict`(QEMU `policy=bind`)·`preferred`. 주면 공유 VM도 호스트 노드에 묶는다. 전용·hugepages VM은 주지 않으면 `strict`.
  - `numa`가 없고 `dedicated`나 `hugepages`가 있으면 `auto`로 본다(OpenStack도 전용·큰 페이지 VM에 NUMA 토폴로지를 준다). 셋 다 없으면 지금 그대로(vNUMA 없음).
- **`VmSpec.hugepages: Option<PageSize>`** — `"2M"`·`"1G"`·`"any"`(노드가 가진 가장 큰 것부터). `1G` + 메모리 핫플러그(`max_memory_mib > memory_mib`)는 400(§4).
- **노드 설정 `NodeSpec.host_resources: Option<HostResources>`**: `{ reserved_cpus: CpuSet, dedicated_cpus: CpuSet, hugepages: Vec<HugepageReserve { size: PageKind /* 2M|1G */, count: u32, host_node: Option<u16> }> }`. 공유 CPU는 `online − reserved − dedicated`로 셈한다(§7).
- **노드 상태 `NodeStatus.topology: Option<NodeTopology>`**(§6): `{ sockets: u16, threads_per_core: u8, cpus: CpuSet /* online */, nodes: Vec<HostNode { id: u16, cpus: CpuSet, memory_mib: u64, hugepages: Vec<HugepagePool { size_kib: u32, total: u32 }> }>, distances: Vec<Vec<u8>>, siblings: Vec<CpuSet> /* 2개 이상인 묶음만 */, l3: Vec<CpuSet>, page_sizes_kib: Vec<u32>, isolated: CpuSet, nohz_full: CpuSet, pinning: bool, cpuset: bool, warnings: Vec<String>, error: Option<String>, probed_at: u64 }`. 빈 값(`free`)은 넣지 않는다 — VM을 켤 때마다 Raft 쓰기가 생긴다. 빈 값은 기록된 클레임에서 셈하고(§9), 화면은 노드에 직접 묻는다(`ReadTopology`, §13).
- **켤 때 기록**(`RunningHardware`에 더함, `cpu_resolved`와 같은 뜻):
  - `numa: Option<ResolvedNuma>` — 게스트가 보는 것과 마이그레이션 양쪽이 같아야 하는 것: `{ nodes: Vec<GuestNode { sockets: Vec<u16>, memory_mib: u32 }>, distances: Vec<Vec<u8>> /* 비면 QEMU 기본 */, page: Option<PageKind>, policy: CpuPolicy, thread_policy, emulator, memory: Option<MemoryPolicy> }`. 없으면 계획 64 전 기록이거나 이 기능을 쓰지 않는 VM이다.
  - `host_placement: Option<HostPlacement>` — 이 노드에만 맞는 것: `{ node: Uuid, vcpus: Vec<u16> /* 자리(상한까지)마다 pCPU, 공유면 빔 */, emulator: CpuSet, host_nodes: Vec<u16> /* 게스트 노드마다, 묶지 않으면 빔 */ }`. `node`가 지금 노드와 다르면 낡은 기록이다.
- **순수 함수**(§8): `numa::layout`(게스트 모양), `numa::demand(spec, topology) -> Demand { pcpus, pages: BTreeMap<PageKind, u64> /* MiB */, any_mib }`, `numa::fit(layout, demand, topology, sets, free) -> Result<Fit, NumaRefusal>`, `numa::usage(vms, node)`, `numa::pending_change(spec, hw)`.
- **클러스터 거절**(`opl-cluster` `Rejection`): `HostResourcesExceeded { node: Uuid, resource: HostResource /* dedicated_cpus|hugepages_2m|hugepages_1g */, limit: u64, used: u64 }`(§9).
- **권한** `VmDedicated = "vm.dedicated"`, **할당량** `FolderQuota.dedicated_vcpus`·`QuotaDimension::DedicatedVcpus`(§12).
- **노드 사이 RPC**(opl-peer): `ReadTopology` → `TopologyLive { nodes: [{ id, memory_free_mib, hugepages: [{ size_kib, free, surplus }] }], affinity_ok: bool }`. `MigrateIncomingResponse.host_placement: Option<HostPlacement>`(없으면 쓰지 않음).
- **보류**: `PendingChange.field` `numa`(게스트 모양·페이지·정책이 기록과 다름, `PendingReason::Restart`, `detail`은 바뀐 것 + 계획 63의 `POWER_CYCLE` 문구). 새 `PendingReason`은 더하지 않는다.

## 6. 노드 토폴로지 탐침

- **opld `topology.rs`**: `probe(sys: &Path, proc: &Path) -> Result<NodeTopology, String>`(뿌리 경로를 받아 고정물로 시험한다). 읽는 것은 §4의 목록이다. 온라인 CPU만 센다. `node` 디렉터리가 없으면 노드 0 하나(모든 CPU, 메모리는 `/proc/meminfo`의 `MemTotal`). 형제 묶음은 `thread_siblings_list`를 겹치지 않게 모으고 둘 이상인 것만 둔다. L3는 `level` 3·`type Unified`의 `shared_cpu_list`. `threads_per_core`는 가장 큰 형제 묶음이다. 거리는 노드마다 `distance` 한 줄.
- **능력**: `pinning`은 Linux이고 `sched_setaffinity`를 쓸 수 있음(늘 참, macOS는 탐침이 없음), `cpuset`은 cgroup v2에 `cpuset` 컨트롤러가 있고 `[hypervisor] systemd_scope`가 켜짐. 전용 정책은 `pinning`이 있는 노드만, `AllowedCPUs`는 `cpuset`이 있을 때만 건다(§10).
- **경고**(문자열, 화면에 그대로): 전용 CPU가 `isolated`도 아니고 PID 1의 `Cpus_allowed_list`에 들어 있음("host processes may run on the dedicated CPUs; see the host preparation guide"), 전용·예약 CPU가 오프라인, 실행 중 hugepages 예약이 모자람("node 1: 2M hugepages 1024 wanted, 812 reserved"), 전용 집합이 형제 짝을 나눔.
- **언제**: `member.rs`가 `firmware_caps` 옆에서 한 번, 검사 루프는 여섯 회차(30초)마다 읽고 다를 때만 쓴다(`probed_at`은 바뀐 때의 시각). CPU를 온라인·오프라인으로 바꾸거나 hugepages 수가 바뀌면 30초 안에 보인다. 계획 63 스테이지 2처럼 **모든 노드가 `numa`를 알릴 때까지는 읽기만 하고 쓰지 않는다.**
- **크기**: 노드 상태는 주기적으로 통째로 다시 쓰이므로 작게 둔다. 2소켓 × 64코어 × 2스레드(256 CPU)에서 형제 묶음 128개 ≈ 1.2 KB, L3 16개 ≈ 0.2 KB, 나머지 ≈ 0.3 KB로 2 KB 안쪽이다(스테이지 2에서 고정물로 재어 보고). 4 KB를 넘으면 형제 묶음을 "모두 `i`·`i+N`" 꼴로 줄이는 것을 그때 정한다.
- **macOS·위트니스**: `None`. vNUMA만 되는 노드로 본다.

## 7. 호스트 자원 설정과 호스트 준비

- **세 집합**(노드마다 `host_resources`, `cluster.admin`): **예약**(호스트와 Orbivisor — VM 스레드는 두지 않는다), **전용**(전용 VM의 vCPU와 `isolate` 에뮬레이터만), **공유**(나머지 — 공유 VM 전체와 전용 VM의 `shared` 에뮬레이터). 검증(API, 그 노드의 토폴로지로): 온라인 CPU의 부분집합, 서로 겹치지 않음, 공유가 비지 않음(400 `invalid_host_resources`). **설정이 없으면 예약·전용이 비고 공유 = 모든 CPU — 지금과 같다**(친화·`AllowedCPUs`를 걸지 않는다, 띄우는 인자가 바이트까지 같다).
- **바꾸기**: 실행 중 VM의 기록된 pCPU가 새 전용 집합 밖으로 나가면 409 `host_cpus_in_use`(VM 목록), `force`면 쓰되 그 VM들은 경고와 보류 `numa`("pinned outside the dedicated CPUs")를 얻는다. 공유 집합이 바뀌면 검사 루프가 도는 공유 VM의 스레드 친화·`AllowedCPUs`를 다시 건다.
- **Orbivisor 자신**: 예약이 비지 않으면 opld가 자기 모든 스레드의 친화를 예약 집합으로 둔다(시작할 때와 바뀔 때). swtpm 같은 자식은 물려받고, QEMU는 §10이 따로 건다.
- **호스트의 다른 프로세스는 보고만 한다.** 가이드가 권하는 길: (a) `/etc/systemd/system.conf`의 `CPUAffinity=`를 예약 ∪ 공유로(모든 서비스, 재부팅 뒤), (b) 지연이 중요하면 커널 `isolcpus=domain,managed_irq,<전용>`·`irqaffinity=<예약>`(선택 `nohz_full=`·`rcu_nocbs=`). Orbivisor는 이것을 쓰지 않고 §6의 경고로 알린다. VM 스코프를 새 슬라이스로 옮기고 `system.slice`를 묶는 길은 §15.
- **hugepages**: 노드가 크기·호스트 노드마다 전체 수를 알린다(§6). 선택 기능으로 `host_resources.hugepages`가 있으면 opld가 시작할 때와 설정이 바뀔 때 `/sys/devices/system/node/node<N>/hugepages/hugepages-<kB>kB/nr_hugepages`(호스트 노드를 안 정하면 `/sys/kernel/mm/hugepages/…`)에 쓰고, 얻은 수를 알린다(모자라면 경고). Orbivisor VM이 잡은 페이지 아래로 줄이는 설정은 409 `hugepages_in_use`. 설정에서 빼면 Orbivisor는 더 쓰지 않는다(지금 수를 그대로 둔다 — 다른 쓰임새일 수 있다). 1G는 커널 명령줄(`hugepagesz=1G hugepages=16` 또는 노드별 `hugepages=0:8,1:8`)을 가이드가 권한다.
- **memfd hugetlb를 쓰므로 hugetlbfs 마운트는 필요 없다.**
- **메모리 셈**: hugepages 풀은 보통 VM이 쓸 수 없으므로 `free_memory_mib`(순위·균형 조정)는 노드 메모리에서 풀을 빼고, hugepages VM의 메모리는 풀에서 센다.

## 8. 해석: vNUMA·NUMA 맞춤·고정 (opl-types `numa.rs`, 순수)

- **게스트 모양 `layout(spec, topology)`**:
  - `numa` 없음 + 공유 + hugepages 없음 → 없음(지금 그대로).
  - 개수 N(또는 직접 목록)이면 그대로. `auto`면 `sockets`의 약수 d(오름차순, d ≤ 호스트 노드 수, d ≤ 8) 가운데 처음 맞는 것: 묶는 VM은 `fit`이 되는 d, 묶지 않는 VM은 노드마다 vCPU 자리가 호스트 노드 CPU 수 이하이고 메모리가 호스트 노드 메모리 이하인 d(VMware처럼 호스트 노드보다 클 때만 나눔). 호스트 노드가 하나면 d = 1. 그래서 **`auto`로 노드를 나누려면 소켓이 여럿이어야 한다** — 화면과 가이드가 알린다.
  - 게스트 노드 i는 소켓 `i·S/N … (i+1)·S/N − 1`을 갖고, 핫플러그 자리는 소켓의 노드를 따른다. 메모리는 고른 나눔을 단위 u(1G면 1024, 그 밖은 2 MiB)로 내리고 남는 것은 마지막 노드에 준다. 노드마다 u의 배수가 아니면 400 `invalid_numa`(1G VM은 `memory_mib`가 `N × 1024`의 배수여야 한다).
- **호스트 맞춤 `fit`**(묶는 VM: 전용, hugepages, 또는 `numa.memory`를 준 VM):
  - 게스트 노드마다 **서로 다른** 호스트 노드 하나(OpenStack과 같다 — 한 호스트 노드에 게스트 노드 둘을 두지 않는다).
  - 게스트 노드 i의 요구: 전용이면 vCPU 자리 수만큼 그 호스트 노드의 빈 전용 pCPU(`isolate`는 코어마다 하나 + 형제를 잡음, `require`는 형제 짝 — SMT가 없거나 자리 수가 홀수면 안 됨, `prefer`는 형제를 먼저), hugepages면 `memory_mib_i / page` 페이지. 에뮬레이터 `isolate`는 게스트 노드 0의 호스트 노드에서 pCPU 하나를 더.
  - 고르기: 호스트 노드 순열을 (최대 거리 작은 것 → 맞춘 뒤 남는 것이 적은 것(채우기, 조각을 줄임) → id) 순으로 본다. 호스트 노드가 16개를 넘으면 순열 대신 욕심쟁이 고르기. 한 호스트 노드 안에서는 L3 묶음 수가 적고 번호가 낮은 pCPU를 고른다.
  - `any`는 1G로 먼저, 안 되면 2M으로(VM 전체가 한 크기).
  - 결과는 `ResolvedNuma` + `HostPlacement`. 게스트 거리 = 맞춘 호스트 노드 사이 거리(묶지 않으면 비움 — QEMU 기본 10·20).
  - 묶는 공유 VM은 CPU를 잡지 않는다. vCPU는 게스트 노드의 (공유 ∩ 호스트 노드 CPU)에, 에뮬레이터는 공유 전체에 둔다.
  - 실패 `NumaRefusal`은 까닭 하나: `node "n1" has 2 and 3 free dedicated CPUs on host NUMA nodes 0 and 1; the VM needs 4 on one`, `node "n2" has no 1G hugepages`, `node "n3" cannot pin vCPUs (no topology)`.
- **이미 켠 VM의 맞춤 `fit_fixed(hw.numa, …)`**: 게스트 모양·페이지는 기록 그대로 두고 호스트 노드와 pCPU만 고른다(마이그레이션·스냅샷 메모리 불러오기).
- **`demand`**: 전용 pCPU 수(자리 × 스레드 정책의 몫 + 에뮬레이터 `isolate` 1)와 페이지 크기마다 MiB(`any`는 `any_mib`). Raft 검사(§9)가 이것만 본다.
- 같은 함수를 스케줄러(어림), 노드 할당기(정확), 상태 머신(합계)이 쓴다 — 그래야 고른 노드가 켜기를 거절하는 일이 드물다.

## 9. 자원 장부와 스케줄러

세 층이다. 위가 빠르고 거칠며, 아래가 정확하고 마지막에 정한다.

1. **Raft 받아들이기 검사**(opl-cluster, 계획 62 할당량과 같은 자리): VM 스펙 쓰기에서, 쓰기 뒤 VM이 `power = running`이고 `node = n`이고 `demand > 0`이면 노드 n에 세는 VM(`spec.node == n`·running·템플릿 아님)의 전용 pCPU 합 ≤ 전용 집합 크기, 크기마다 hugepages MiB 합 ≤ 그 노드 풀 전체(`any`는 두 크기 합에 센다)를 본다. 상한은 `NodeSpec.host_resources`와 `NodeStatus.topology`에서 셈한다(결정적). **쓰기 뒤 넘고 쓰기 전보다 늘었을 때만** `HostResourcesExceeded`다 — 상한이 줄어 넘친 노드에서도 다른 쓰기는 된다. 마이그레이션 확정 쓰기(`spec.node`가 `status.migration.node`로 바뀜)는 보지 않는다(받는 쪽 할당기가 이미 잡았다). 이것으로 두 API 노드(또는 API와 HA 리더)가 같은 노드의 마지막 전용 CPU에 동시에 VM을 놓는 경쟁을 커밋 순서로 막는다.
2. **스케줄러**(`Inventory`): `HostUsage`를 노드마다 셈한다 — 그 노드에서 도는 VM의 `hardware.host_placement`(정확한 pCPU·호스트 노드·페이지), 그 노드로 오는 마이그레이션(`status.migration.node`, 합계로만), 놓였으나 아직 켜지 않은 VM(합계로만), HA 회차의 `Assign`. `fits`에 더한다: (a) 능력 — 전용은 `topology.pinning`과 비지 않은 전용 집합, hugepages는 그 크기 풀, 묶는 VM은 토폴로지, (b) `numa::fit`을 빈 자원 어림에 대본다. 꺼진 VM(켜기·HA 복구·복제 배치)은 스펙의 모양으로, 도는 VM을 옮길 때(`migration_fits`·`replica_migration_fits`·`cpu_receives` 자리)는 `fit_fixed(hw.numa)`로. `ScheduleError::NoNode` 문구에 "has room for its dedicated CPUs, hugepages and NUMA layout"을 더하고, 계획 63처럼 노드마다의 까닭을 `details.nodes`에 준다. 순위(`placement::compare`)는 그대로다.
3. **노드 할당기**(opl-reconciler `host_alloc.rs`, opld마다 하나, 작업자들과 `IncomingMigrations`가 `Arc`로 나눔): 켜기·받기 때 정확한 pCPU·호스트 노드·페이지를 잠금 안에서 고르고 메모리 예약을 만든 뒤 QEMU가 뜨면 기록으로 넘긴다. 보는 것은 이 노드의 Raft 기록(도는 VM의 `host_placement`) + 메모리 예약(켜는 중·받는 중) + `/sys`의 실제 빈 hugepages(다른 프로세스가 쓴 것). 한 노드의 QEMU는 그 노드의 opld만 띄우므로 이 잠금 하나로 pCPU가 두 번 잡히지 않는다. 받는 중 예약은 실행 디렉터리의 받기 표시(`INCOMING_MARK`) 옆 `placement.json`에도 써서 opld가 다시 떠도 잃지 않는다.

- **거절의 길**: API 켜기는 `HostResourcesExceeded`면 그 노드를 빼고 한 번 다시 고르고, 그래도 안 되면 409 `no_capacity`(까닭). HA는 그 VM을 다음 회차로 미룬다. 할당기가 거절하면 작업자는 계획 63의 `CpuRefusal`처럼 둘로 나눈다: 켜는 중인 다른 VM의 예약 때문이면 `Waiting`(실패로 세지 않고 다시), 아니면 `Cannot`(꺼짐, `failed to start: …`, 노드 이름은 숨김).
- **보기**: `GET /topology/usage`(노드마다 전용 전체·사용, 크기마다 hugepages 전체·사용)는 위 2의 셈이다.

## 10. 켜기·명령줄·스레드 고정 (핫플러그·CPU 모델 포함)

- **작업자**(`worker.rs` `start`): `cpu_at_start` 다음에 `numa_at_start` — 스펙에서 `layout`을 만들고 할당기로 `fit`해 `hardware.numa`·`host_placement`를 채운다. 메모리 스냅샷 불러오기는 스냅샷의 `numa` 그대로 `fit_fixed`(호스트 쪽만 새로). 작업자가 스스로 다시 켜는데 새 스펙이 이 노드에 맞지 않으면 계획 63의 `restart_fallback`처럼 앞 기록의 모양으로 켠다. `apply_hardware`가 둘을 `VmRuntimeSpec.numa`·`host_placement`로 옮긴다.
- **명령줄**(`cmdline.rs`, `numa`가 있을 때만 — 없으면 지금과 바이트까지 같다): `-m` 다음에 게스트 노드마다
  `-object memory-backend-<ram|memfd>,id=ram-node<i>,size=<m_i>M[,hugetlb=on,hugetlbsize=<2M|1G>][,host-nodes=<h_i>,policy=<bind|preferred>][,prealloc=on,prealloc-threads=<k>]` + `-numa node,nodeid=<i>,memdev=ram-node<i>`, 모든 소켓의 `-numa cpu,node-id=<i>,socket-id=<s>`, 거리가 있으면 i<j마다 `-numa dist,src=<i>,dst=<j>,val=<d>`. 백엔드는 hugepages면 `memfd`, 아니면 `ram`이고, `host-nodes`·`policy`는 묶는 VM(전용·hugepages·`numa.memory`)에만 붙는다. `prealloc`은 hugepages와 전용 VM에 켜고(`prealloc-threads` = 그 VM의 vCPU 수, 16까지) 공유 vNUMA VM에는 켜지 않는다. 백엔드 id가 RAMBlock 이름이므로 마이그레이션 양쪽이 같은 모양이어야 한다 — 그래서 모양은 기록이다. `-smp`·`-cpu`는 그대로다(게스트 스레드는 늘 1, CPU 모델·플래그는 vCPU 형식의 속성이라 노드 매핑과 상관없다). 장치 순서·PCI 주소는 바뀌지 않는다(백엔드·`-numa`는 장치가 아니다).
- **virtio-mem**(계획 54)과 vNUMA: 게스트 노드마다 장치 `vmem<i>`(`node=<i>`, 백엔드 `vmem<i>`는 그 노드의 묶기·`hugetlb 2M`을 따르고 hugepages면 장치에 `prealloc=on`). 상한 몫과 `requested-size`는 블록 단위로 고르게 나누고 남는 것은 노드 0에. `HotplugLayout.plugged_mib`는 합 그대로이고 핫플러그 Task가 나눠 건다. `numa`가 없으면 지금처럼 `vmem0` 하나.
- **vCPU 핫플러그**: 노드 매핑은 QEMU가 채운다(§4). 전용 VM은 **상한(`max_vcpus`)까지 pCPU를 켤 때 잡는다** — 빈 자리도 전용 CPU를 쓴다(화면·가이드가 알림). `device_add` 뒤 `query-cpus-fast`로 새 스레드를 찾아 그 자리의 pCPU에 고정하고, 실패하면 `device_del`로 되돌리고 핫플러그 단계를 실패로 남긴다. 빼면 스레드만 없어지고 pCPU는 그대로 잡혀 있다. 켤 때 vCPU가 소켓 0부터 채워지므로(계획 54 배치) 핫플러그 VM의 켤 때 vCPU는 앞 게스트 노드에 몰린다 — 한계로 적는다.
- **드라이버**(`VmRuntimeSpec.host_placement`가 있거나 노드에 전용 집합이 있을 때만):
  1. 스코프에 `--property=AllowedCPUs=<그 VM이 쓸 CPU 합>`(`cpuset`이 있을 때): 전용 VM = 자기 pCPU ∪ 에뮬레이터, 공유 VM = 공유 집합.
  2. 자식 프로세스의 친화를 `pre_exec`의 `sched_setaffinity`로 에뮬레이터 집합(전용 VM)이나 공유 집합으로 — QEMU의 모든 스레드(나중 일꾼 스레드, vhost 일꾼 포함)가 물려받는다.
  3. QMP 붙은 뒤(`-S`) `query-cpus-fast`의 `props`(소켓·코어)로 자리를 찾아 vCPU 스레드마다 `sched_setaffinity(tid, {pCPU})`, `/proc/<pid>/task/<tid>/status`의 `Cpus_allowed_list`로 확인. 틀리면 QEMU를 끄고 `failed to start: cannot pin vCPU 3 to CPU 7: …`. 그다음 `cont`.
  4. `START_TIMEOUT`은 30초 + prealloc하는 GiB마다 1초.
- **어긋남 고치기**: 검사 루프가 여섯 회차마다 도는 VM의 스레드 친화를 기록과 견주어 다르면(누가 `taskset`을 씀) 다시 걸고 경고 로그를 남긴다.
- **보류**: 실행 중 `cpu.policy`·`thread_policy`·`emulator`·`numa`·`hugepages`를 바꾸면 보류 `numa`(전원 껐다 켜기). 게스트 안 재부팅으로는 적용되지 않는다(계획 63과 같은 문구).

## 11. 마이그레이션·HA·비우기·균형 조정

- **라이브 마이그레이션**: 게스트 모양·페이지는 `hardware.numa` 그대로이고 대상은 `fit_fixed`로 호스트 쪽만 새로 고른다. API(`start_migration`)는 `migration_fits`로 미리 보고(409 `migration_unsupported`, `node "n2" cannot place the VM: needs 4 dedicated CPUs on one host NUMA node (free: 2, 3)`), 받는 쪽 `IncomingMigrations::receive`가 할당기로 예약하고 받는 QEMU를 그 배치로 띄운 뒤 `MigrateIncomingResponse.host_placement`로 돌려준다. 원본 Task가 확정할 때 `hardware.host_placement`를 대상의 것으로 쓴다. 취소·실패는 예약을 푼다.
- **hugepages + prealloc**: 받는 QEMU가 마이그레이션 전에 모두 잡으므로 옮기는 중 페이지가 모자라 게스트가 죽는 일이 없다. 대신 받는 QEMU가 뜨는 시간이 길다(§10의 시간 제한). Orbivisor는 postcopy를 쓰지 않는다(auto-converge만). 페이지 크기는 기록 그대로 — 다른 크기로 받으면 RAMBlock 페이지 크기가 달라진다.
- **HA 복구**는 다시 켜기라 스펙에서 새로 `layout`·`fit`하고(`any`·`auto`도 다시 고름) `Assign`을 센다. Raft 검사가 거절하면 그 VM은 다음 회차다. `ha_capacity`(메모리)는 그대로이고 전용 CPU의 HA 예약은 §16.
- **비우기·호스트 종료(migrate)**는 `migration_fits`로 대상을 고르고, 모두 안 되면 계획 63의 `cpu_blocks_every_move`처럼 첫 까닭을 보인다.
- **균형 조정기는 전용 VM을 옮기지 않는다** — 자기 pCPU에서 돌아 다른 VM과 다투지 않으므로 옮겨도 얻는 것이 없고 다시 고정하는 비용만 든다. 공유 vNUMA VM은 지금처럼 옮긴다.
- **배치 규칙(계획 50)**은 그대로 먼저 거른다.

## 12. 버전이 섞인 클러스터와 다른 기능

- **버전 섞임**: `CpuSpec`·`VmSpec`·`NodeSpec`이 `deny_unknown_fields`라 옛 노드는 새 칸이 든 VM·노드를 풀지 못한다. 그래서 §2의 모든 칸은 `require_feature(NUMA)` 뒤이고, 탐침은 모든 노드가 알릴 때까지 쓰지 않는다. 기록 `numa`·`host_placement`·응답 `host_placement`는 이 기능을 쓰는 VM에만 생기므로 업그레이드 중에는 없다. Raft 검사는 `demand > 0`인 VM에만 걸리고 그런 VM은 모든 노드가 새 빌드일 때만 생기므로 복제본마다 같은 판정이다(계획 62 할당량과 같은 논리). 옛 노드의 뒤늦은 합류는 `version_too_old`가 막는다. **이 기능을 쓰지 않는 VM은 스펙·기록 JSON, 명령줄, `systemd-run` 인자가 바이트까지 같다**(시험).
- **템플릿(계획 58)·복제**: `VmSpec`이라 그대로 간다. `CloneOverrides`·`ImportRun`·템플릿 초안에 `cpu_policy`·`cpu_thread_policy`·`cpu_emulator`·`numa`·`hugepages`를 더한다(CLI 이름이 세 곳에서 같다, 계획 63과 같은 본).
- **가져오기(OVA·ESXi)**: 기본은 공유·vNUMA 없음. VMX의 `numa.*`·`sched.cpu.latencySensitivity`는 읽지 않는다(§16).
- **백업 복원·원격 복제**: 스펙 그대로다. 받는 클러스터에서 맞출 노드가 없으면 꺼진 VM으로 만들고 경고한다(켜기는 409와 까닭).
- **스냅샷**: 메모리 스냅샷은 `hardware.numa`를 담고 불러오기는 그 모양이 맞는 노드에서만 된다. 디스크만 되돌리기는 켜기와 같다.
- **CPU 모델(계획 63)**: `-numa cpu`는 소켓 id로 매므로 모델과 상관없다. aarch64 `virt`도 `socket-id`가 있다(확인).
- **권한(계획 62)**: 새 권한 **`vm.dedicated`** — VM 스펙에 `dedicated`나 `hugepages`가 있는 만들기·바꾸기(그 칸을 바꿀 때)·복제·배포·가져오기·복원에 그 폴더에서 요구한다. 내장 operator에는 넣지 않는다(계획 62의 새 권한 규칙, 드문 노드 자원이다). 노드 `host_resources`는 `cluster.admin`, `/nodes/{key}/topology`·`/topology/usage`는 루트 `cluster.read`, `/vms/{key}/numa`는 VM 읽기(루트 `cluster.read`가 없으면 노드 이름·pCPU 번호를 숨긴다 — 계획 63의 `without_node_name`).
- **할당량**: 폴더 할당량 `dedicated_vcpus` — 전용 VM의 `max_vcpus`(+ 에뮬레이터 `isolate` 1)를 전원과 상관없이 센다(계획 62의 할당 기준). hugepages 메모리는 이미 `memory_mib`에 든다.
- **아키텍처·OS**: aarch64 Linux(KVM·TCG)도 x86_64와 규칙이 같다. macOS(HVF)는 토폴로지가 없어 게스트 vNUMA만 되고, 묶기·전용·hugepages VM은 노드 조건에서 빠진다.

## 13. API·CLI·웹

- **API**(태그 `numa`·`vms`·`nodes`):
  - VM: `cpu.policy`, `cpu.thread_policy`, `cpu.emulator`, `numa`(`{"nodes":"auto"|2|[{"cpus":8,"memory_mib":16384}],"memory":"strict"}`), `hugepages`, `status.hardware.numa`·`host_placement`, 보류 `numa`. `GET /vms/{key}/numa`: 스펙, 지금 모양, 기록, 노드마다 `{node, fits, reason, plan: {host_nodes, pcpus, page}}`(마이그레이션 대화상자·CLI용).
  - 노드: `PUT /nodes/{key}`의 `host_resources`(409 `host_cpus_in_use`·`hugepages_in_use`, `?force=true`). `GET /nodes/{key}/topology`: `NodeTopology` + 세 집합 + 호스트 노드마다 사용(어느 VM이 어느 pCPU·페이지) + 노드에 직접 물은 빈 메모리·빈 hugepages·다른 프로세스 사용(`ReadTopology`, 답이 없으면 `live: null`) + 경고. `GET /topology/usage`: 노드 목록 열용 요약.
  - 오류: 400 `invalid_numa`·`invalid_host_resources`, 403(`vm.dedicated`), 409 `cluster_upgrading`·`no_capacity`(까닭)·`migration_unsupported`(까닭)·`host_cpus_in_use`·`hugepages_in_use`·`quota_exceeded`(`dedicated_vcpus`). 감사: `vm create db cpu.policy=dedicated numa=auto hugepages=1G`, `node set n1 host_resources dedicated=4-31 reserved=0-1 hugepages=2M:1024@0`.
- **CLI**:
  - `vm create|set --cpu-policy shared|dedicated --cpu-threads prefer|isolate|require --emulator-threads shared|isolate --numa none|auto|N|8:16384,8:16384 --numa-memory strict|preferred --hugepages none|2M|1G|any`. `vm get`은 `numa: 2 nodes x (8 vCPU, 16 GiB), dedicated, hugepages 1G`과 실행 중이면 `placement: host nodes 0,1, vCPU 0-7 -> CPU 4-11, emulator 1-3`(모두 기본이면 줄 없음 — 지금 출력 그대로).
  - `vm numa VM`(노드마다 맞음·까닭·계획). `vm migrate`는 대상의 까닭을 먼저 찍는다.
  - `node topology NODE [--live]`, `node resources NODE --reserved-cpus 0-1 --dedicated-cpus 4-31 --hugepages 2M:1024@0,2M:1024@1 [--force]`, `node resources NODE --clear`.
  - `template deploy|vm clone|import run|image get --template`의 같은 플래그, `folder quota set --dedicated-vcpus N`.
- **웹**(계획 61 규칙: 사이드바 항목을 더하지 않는다):
  - **VM 만들기·편집 › 일반**: CPU 칸 아래 접힌 "CPU 배치와 NUMA" — 정책(공유·전용, `vm.dedicated`가 없으면 막고 까닭), 전용이면 스레드·에뮬레이터, NUMA(끔·자동·노드 수·직접 표 — 소켓 수에 맞는 선택만), 메모리 정책, hugepages(없음·2M·1G·아무것). `/topology/usage`로 "n1·n3에 들어감"을 보이고, 실행 중 VM이면 "다음 전원 껐다 켜기 때 적용".
  - **VM 상세 › 요약**: 하드웨어 카드에 "CPU 배치"(정책·NUMA 노드·페이지) 줄과 펼치는 vCPU → pCPU 표, 보류 `numa`는 계획 63의 "전원 껐다 켜기 필요" 띠를 쓴다.
  - **마이그레이션 대화상자**: `/vms/{key}/numa`로 맞지 않는 노드를 막고 까닭(계획 63의 CPU 까닭과 같은 자리).
  - **인프라 › 노드 › 노드 상세 "토폴로지"**(루트 `cluster.read`): 소켓 › NUMA 노드 › L3 › 코어 › 스레드 격자(색: 예약·공유·빈 전용·쓰는 전용, 마우스를 올리면 VM 이름), 호스트 노드 표(CPU·메모리·크기마다 hugepages 전체/빈/Orbivisor/다른 것), 거리 표, 경고 띠. "호스트 자원 설정"(admin) 대화상자: cpulist 입력과 격자 눌러 고르기, hugepages 예약 행, 409 `host_cpus_in_use`면 VM 목록과 "그래도 저장".
  - **노드 목록**: "전용 CPU"(사용/전체)·"Hugepages" 열(`/topology/usage` 한 번).
  - **폴더 상세 › 할당량**: "전용 vCPU". **설정 › 역할**: 새 권한 이름.
  - i18n en·ko `numa.*`.
- **문서**: `docs/guide/vms.md` "NUMA·전용 CPU·hugepages"(언제 쓰나, `auto`와 소켓, 핫플러그 상한이 전용 CPU를 잡음, 마이그레이션), `docs/guide/cluster.md` "호스트 준비: 예약·전용 CPU와 hugepages"(세 집합, `CPUAffinity=`·`isolcpus`, 1G는 커널 명령줄, 실행 중 예약), `web-ui.md` "NUMA 화면", `docs/architecture.md`(세 층의 자원 세기), `docs/index.md`.

## 14. 시험과 CI

- **순수(opl-types)**: serde 하위 호환 — 옛 `VmSpec`·`CpuSpec`·`NodeSpec`·`NodeStatus`·`RunningHardware` JSON이 바이트 그대로, 새 칸 왕복. `CpuSet` 파싱·맞춤 꼴. `layout` 표(없음·개수·직접·`auto`, 소켓 나눔, 메모리 나눔과 단위, 핫플러그 자리), `fit` 표(노드 하나·둘·넷, 스레드 정책 셋 × SMT 있음·없음, 에뮬레이터 `isolate`, hugepages 2M·1G·`any`, 조각난 노드, 거리 순서, L3 고르기), `demand`, `pending_change`.
- **탐침 고정물**(`crates/opld/tests/fixtures/topology/`, `/sys`·`/proc` 일부를 옮긴 디렉터리 트리): x86 2소켓(소켓 2 × 코어 8 × 스레드 2, 노드 2, 거리 10·21, L3 2, 노드마다 1G 4·2M 512 — 실제 2소켓 Xeon 덤프의 모양을 손으로 만든 것, 그렇다고 고정물에 적는다), aarch64 노드 하나(svc01에서 읽기만 해서 기록 — `find /sys/devices/system/{cpu,node} … | cpio`로 값 파일만), x86 노드 하나(GitHub 러너, 시험이 로그에 남긴 것), `node` 디렉터리 없는 커널, 오프라인 CPU가 있는 트리. 내용·경고·크기(보고)를 본다.
- **명령줄(opl-qemu)**: 기능을 쓰지 않는 VM은 모든 기존 시험이 그대로 통과(바이트), vNUMA 둘(공유) → `memory-backend-ram`·`-numa node`·`-numa cpu`, 전용 + 1G → `memory-backend-memfd,hugetlb=on,hugetlbsize=1G,host-nodes=1,policy=bind,prealloc=on`, 거리, 핫플러그 + vNUMA의 `vmem<i>,node=<i>`, aarch64 `virt`. 드라이버의 `systemd-run` 인자(`AllowedCPUs`)는 단위 시험.
- **opl-cluster**: Raft 검사 — 두 쓰기가 같은 마지막 pCPU를 노릴 때 하나만 성공, 늘지 않는 쓰기는 상한을 넘어도 됨, 마이그레이션 확정은 보지 않음, `demand 0` VM은 늘 됨.
- **opl-api(`MockHypervisor`, 토폴로지 있는 노드 셋)**: 스케줄러가 NUMA로 거름, 고정 노드 켜기의 까닭, 마이그레이션 409, HA 복구와 `Assign`, 비우기, 균형 조정기가 전용 VM을 건너뜀, `HostResourcesExceeded` 뒤 다시 고르기, 기능 게이트(옛 노드 하나 → 409), `host_resources` 검증과 409·`force`, 권한 행렬(`vm.dedicated`, `/topology/*`), 할당량 `dedicated_vcpus`.
- **실제 KVM — GitHub `linux-kvm`**(게스트 OS 없음, 새 `opld/numa-it` `tests/numa.rs`, root): 러너는 4 vCPU·노드 하나다. 시험 앞에서 2M hugepages 256개를 실행 중 예약하고(`host_resources.hugepages`로 — Orbivisor의 예약 길도 함께 본다) 끝에 되돌린다. `host_resources { reserved: 0, dedicated: 2-3 }`(공유 = 1). (1) 2 vCPU 전용 VM: vCPU 스레드마다 `/proc/<pid>/task/<tid>/status`의 `Cpus_allowed_list`가 2·3 하나씩, 나머지 스레드는 1, 스코프 `cpuset.cpus` 확인. (2) 공유 VM의 모든 스레드는 1. (3) 2M hugepages 256 MiB VM: `HugePages_Free`가 128 줄고 QMP `query-memdev`가 hugetlb·prealloc. (4) 공유 vNUMA 둘(노드 하나 위): `query-cpus-fast`의 `node-id`. (5) 전용 VM 둘을 동시에 켬(각 2 vCPU) → 하나는 409 `no_capacity`. (6) 한 기계의 opld 둘(n1 전용 2, n2 전용 3)로 1 vCPU 전용 hugepages VM을 라이브 마이그레이션 → 받는 쪽 스레드가 3, 기록이 n2의 것. (7) x86_64 vCPU 핫플러그 → 새 스레드가 잡아 둔 pCPU.
- **실제 게스트 — GitHub `install-media`**(계획 57~59의 보고 길): Ubuntu 클라우드 이미지를 `--numa 2 --sockets 2`로 켜고 cloud-init 스크립트가 `/sys/devices/system/node/node*/cpulist`·`meminfo`를 보고 → 노드 둘, 소켓마다 CPU.
- **Mac(aarch64 HVF)**: 토폴로지 없음, 공유 vNUMA VM이 켜짐(`-numa`는 HVF에서 됨, §4), 전용·hugepages·`numa.memory`는 노드 조건으로 거절. x86 시험은 하지 않는다.
- **svc01·svc02(aarch64 TCG, 4 CPU, 노드 하나)**: 일반 묶음 전체와 배포 확인 — 실제 탐침(노드 하나·hugepages 0·2048 kB가 그대로 보임), 공유 vNUMA VM, 그리고 **Orbivisor 설정만으로**(`host_resources` — 호스트 설정이 아니며 끝에 지운다) 전용 1 vCPU VM의 MTTCG 스레드 고정과 svc01 ↔ svc02 마이그레이션 뒤 다시 고정. hugepages VM은 "node has no 2M hugepages" 거절만 본다. **할 수 없는 것**: 호스트 노드 둘 이상의 맞춤·거리·묶기, 1G, SMT 스레드 정책 — GitHub의 고정물·Mock 시험으로만 본다.
- **웹**: vitest(배치 칸·토폴로지 격자·호스트 자원 대화상자·마이그레이션 까닭, msw), Playwright 한 줄기: 노드 상세 "토폴로지"가 실제 탐침을 보이고 VM 폼의 NUMA 칸이 저장된다.

## 15. 견준 길

- **노드 CPU 나누기: VM마다 아무 pCPU / 정적 세 집합(채택, OpenStack `cpu_dedicated_set`·`cpu_shared_set`).** 정적이면 공유 VM이 전용 pCPU를 피하는 규칙이 단순하고(공유 집합 하나) 전용 VM이 켜질 때 공유 VM을 다시 묶지 않는다. 대가는 관리자가 나눔을 정해야 한다는 것 — 설정이 없으면 지금과 같다.
- **고정 수단: cgroup 하위 그룹마다 vCPU(libvirt) / `sched_setaffinity` + 스코프 `AllowedCPUs`(채택).** 스코프는 VM마다 이미 있고, vCPU마다 하위 cgroup을 만들면 systemd 스코프 안에 위임이 필요하다. 친화는 root가 넓힐 수 있으므로 상한은 스코프의 cpuset이 쥔다.
- **호스트 격리: VM 스코프를 새 슬라이스로 옮기고 `system.slice`를 예약 CPU로 묶기 / 보고만(채택).** 지금 VM 스코프는 `system.slice` 안이라 그것을 묶으면 VM도 묶인다. 옮기면 모든 VM의 띄우는 방법이 바뀌고 도는 VM은 전원 껐다 켜기까지 옛 슬라이스에 남는다. `CPUAffinity=`·`isolcpus`가 표준 길이고 Orbivisor는 어긋남을 경고한다.
- **hugepages: 켤 때마다 할당·반납(Proxmox) / 커널 명령줄만(OpenStack) / 보고 + 선택적 정적 예약(채택).** 켤 때 할당은 1G가 조각으로 실패하기 쉽고 마이그레이션 받는 쪽에서 시간이 든다. 정적 예약이면 스케줄러가 셀 수 있다.
- **페이지 백엔드: hugetlbfs `memory-backend-file` / memfd hugetlb(채택).** 마운트 경로·파일 정리가 없다. 둘 다 Linux만이다.
- **자원 세기: API 잠금만 / peer RPC로 노드에 묻기 / Raft 합계 검사 + 노드 할당기(채택).** API 잠금은 노드 하나 안이고, RPC는 HA 경로를 다른 노드 응답에 맨다(계획 63 §7과 같은 까닭). 정확한 pCPU 번호를 Raft 상태 머신이 고르게 하면 커밋마다 NUMA 맞춤을 돌려야 하고 노드의 실제 빈 hugepages를 모른다. 합계만 Raft가, 번호는 그 노드가 정한다.
- **고정 기록: 스펙에 pCPU 적기 / 켤 때 기록(채택, `cpu_resolved`와 같음).** pCPU는 노드마다 다르고 마이그레이션·HA가 바꾼다.
- **vNUMA 자동: 늘 호스트 노드 수만큼 / 크기를 보고(VMware) / 소켓의 약수(채택).** 노드를 소켓 단위로 두어야 게스트 토폴로지가 어긋나지 않는다(§4). 그래서 소켓 하나인 VM은 나누지 않는다.
- **전용 + 핫플러그: 막기(OpenStack은 vCPU 핫플러그가 없음) / 상한까지 잡기(채택).** 핫플러그를 쓰는 사람이 이미 상한을 정했고, 실행 중 pCPU를 새로 구하다 실패하는 일이 없다.
- **균형 조정기의 전용 VM: 옮김 / 건너뜀(채택).** §11.

## 16. 나중 할 일

`mixed` 정책, 실시간 vCPU, 게스트에 SMT 보이기(`require`일 때 `threads=2`), iothread와 그 고정, vhost 일꾼 따로 묶기, 장치 통과의 NUMA 친화, HA 용량 예약에 전용 CPU 세기, 호스트 예약 메모리, VM 스코프 슬라이스 옮기기와 호스트 격리 관리, 1G hugepages + virtio-mem(블록 1G), VMX `numa.*`·Latency Sensitivity 가져오기, 메모리 대역폭·캐시 나누기(Intel RDT·AMD PQoS), 자동 NUMA 균형(`numa_balancing`) 상태 보이기, 노드 안 조각 모음(전용 VM 다시 고정).

## 17. 스테이지 나누기

각 스테이지는 커밋 하나이고, 혼자 컴파일되고 시험된다. 새 API는 스테이지 5에서 연다. 그 전에는 새 칸이 기능 게이트와 API 검증에 막혀 쓰이지 않는다(시험은 계획 63처럼 시험 빌드의 제어 소켓 고리로 클러스터에 바로 쓴다).

1. **feat(types): NUMA 토폴로지·vNUMA·CPU 정책·hugepages 타입과 맞춤 계산, 기능 numa(opl-types)** — `CpuSet`, `CpuSpec.policy`·`thread_policy`·`emulator`, `VmSpec.numa`·`hugepages`, `NodeSpec.host_resources`, `NodeStatus.topology`, `RunningHardware.numa`·`host_placement`, 순수 `layout`·`fit`·`fit_fixed`·`demand`·`usage`·`pending_change`, 권한 `vm.dedicated`, 할당량 차원, serde 하위 호환.
2. **feat(opld): 노드 토폴로지 탐침 — /sys 읽기, NodeStatus.topology, 호스트 자원 설정 적용과 hugepages 예약** — `topology.rs`와 고정물, 등록·검사 루프(기능 게이트), 경고, opld 자기 친화, `nr_hugepages` 쓰기, 크기 측정.
3. **feat(cluster,api): 전용 CPU·hugepages 자원 장부 — Raft 받아들이기 검사, 스케줄러 NUMA 맞춤, 마이그레이션·HA·비우기의 확인** — `HostResourcesExceeded`, `Inventory`의 `HostUsage`, `fits`·`migration_fits`, `start`의 다시 고르기, `ha.rs`·`drain.rs`, 균형 조정기 건너뛰기, `free_memory_mib`의 hugepages.
4. **feat(qemu,driver,reconciler): vNUMA·메모리 백엔드·hugepages 명령줄, vCPU·에뮬레이터 스레드 고정, 켤 때 배치와 기록, 받는 쪽 다시 고정** — `cmdline.rs`, 드라이버 `AllowedCPUs`·친화·확인·시간 제한, `host_alloc.rs`, 작업자·`incoming.rs`·`MigrateIncomingResponse.host_placement`, virtio-mem 노드마다, 핫플러그 고정, 검사 루프의 어긋남 고치기, `linux-kvm`의 `opld/numa-it`.
5. **feat(api,cli): NUMA·전용 CPU·hugepages API와 검증, 노드 토폴로지·호스트 자원 설정, vm --numa/--cpu-policy/--hugepages·node topology·node resources 명령** — VM 검증·오류 코드·권한·할당량, `/vms/{key}/numa`, `/nodes/{key}/topology`, `/topology/usage`, `ReadTopology` RPC, `PUT /nodes`의 `host_resources`, 템플릿·복제·가져오기 칸, OpenAPI·`schema.d.ts`, oplctl, `install-media`의 게스트 vNUMA 단계.
6. **feat(web): VM 폼의 CPU 배치와 NUMA, VM 상세의 vCPU 고정 보기, 노드 상세 토폴로지와 호스트 자원 설정, 노드 목록의 전용 CPU·hugepages 열(web)** — 일반 칸, 요약 카드, 마이그레이션 대화상자, 노드 상세·목록, 폴더 할당량, i18n, vitest·Playwright.
7. **docs: README·가이드 "NUMA·전용 CPU·hugepages"·"호스트 준비", 설계 문서 스테이지 보고, 계획 64 실행 기록** — `docs/guide/vms.md`·`cluster.md`·`web-ui.md`·`development.md`, `docs/architecture.md`, `docs/index.md`.

## 18. 스테이지 보고 (설계와 다른 것·결정)
