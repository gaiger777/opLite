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

### 스테이지 1 (opl-types)

- **순수 함수의 모양**: `layout(spec, topology) -> Result<Option<Layout>, NumaError>`는 게스트 모양 **후보**(`splits`, 묶는 VM의 `auto`만 소켓 수의 약수마다 여럿)와 정책·페이지 요구를 돌려주고, `fit(&Layout, Option<&HostView>)`가 후보 × 페이지 크기(`any`는 1G 먼저)를 앞부터 시도한다. 노드 쪽 입력은 `HostView { node, topology, sets: HostSets, free: HostFree }`로 묶었다. `HostSets::new(host_resources, topology)`(세 집합, 온라인으로 자름), `usage(records, node) -> HostUsage`(기록의 pCPU·페이지), `HostFree::new(topology, sets, usage)`. 묶지 않는 VM(공유, 페이지·`numa.memory` 없음)은 노드 없이 `fit(&l, None)`이 되고 `placement`가 없다.
- **`HostPlacement.siblings`를 더했다**(`isolate`가 비워 둔 형제 스레드). 기록만으로 잡힌 pCPU를 정확히 세려면 필요하다(토폴로지가 바뀌어도). `emulator`가 비면 "그때의 공유 집합"이라 적지 않는다(공유 집합이 바뀌면 검사 루프가 다시 건다, §7).
- **`ResolvedNuma`는 기본을 채운 값**을 적는다: 전용이면 `thread_policy`·`emulator`가 늘 있고(`prefer`·`shared`), `memory`는 실제 묶기(전용·hugepages면 `strict`)다. 그래서 `memory.is_some()` = 묶는 VM이다.
- **`fit_fixed(numa, cpu, host)`는 거리까지 기록 그대로** 둔다(게스트 ACPI SLIT도 게스트가 보는 것이라 마이그레이션 양쪽이 같아야 한다). `cpu`는 `hw.cpu`(소켓당 자리 수).
- **메모리 나눔**: "노드마다 단위 u의 배수(0 아님)"만 본다. 1G VM 3072 MiB를 둘로 나누면 1024 + 2048이 된다(§8의 "N × 1024의 배수"보다 느슨하다 — 남는 것을 마지막 노드에 주는 규칙과 맞춤). `any`의 검증 단위는 2 MiB이고, 1G로 나눌 수 없으면 그 후보는 2M으로 떨어진다.
- **`require` 검증**: 소켓당 vCPU 자리 수가 홀수면 400(`RequireOdd`) — 게스트 노드는 소켓 단위라 이것이 "자리 수가 홀수"와 같고 맞춤까지 가지 않는다.
- **`auto`(묶지 않는 VM)**: 가장 작은 호스트 노드(CPU 수, hugepages 풀 밖 메모리)와 견준다. 어떤 d도 들지 않으면 가장 큰 d.
- **호스트 노드 고르기**: 순열이 5만 개 이하면 모두 보고(8 × 8도 4만), 넘으면 욕심쟁이(§8의 "16개를 넘으면" 대신 — 16P8은 5억이다). 순서 키는 (최대 거리, 남는 전용 CPU 합, 남는 페이지 MiB 합, 호스트 노드 id들).
- **L3**: 하나에 다 들면 번호가 낮은 묶음, 아니면 **큰 묶음부터 채우고** 나머지를 다음 묶음에서(그래서 vCPU 순서가 `4,5,6,7,2`처럼 될 수 있다).
- **거절 까닭**(`NumaRefusal`, `for_node(name)`이 `node "n1" …`을 붙임): 토폴로지 없음·고정 못 함·전용 집합 없음·SMT 없음·그 크기 풀 없음(`any`가 둘 다 없으면 "has no hugepages")·호스트 노드 수·전용 CPU(§8 문구 그대로)·페이지·공유 CPU·메모리·나눔·조합. 여러 후보가 모두 안 되면 **첫 후보의 마지막 페이지 시도**의 까닭이다.
- **`demand`의 hugepages는 `memory_mib`**(부팅 메모리)만 센다. virtio-mem으로 꽂는 hugepages는 꽂을 때 prealloc이 실패하면 핫플러그 실패로 남는다(설계 그대로).
- **`HostResources::validate(topology)`를 지금 두었다**(§7 검증: 온라인 부분집합·겹침·공유 비지 않음, 그리고 hugepages의 호스트 노드 있음·같은 (크기, 노드) 두 번). 스테이지 5 API가 쓴다. `NodeTopology.distances[i][j]`는 `nodes`의 **순번**으로 센다.
- **`PageKind::from_kib`**: 2M·1G만 안다(aarch64 64K 커널의 512M 풀은 무시).
- **`pending_change(spec, hw)`는 `hw.numa`만** 견준다(`hw.cpu`에도 정책 칸이 생기지만 기록은 `numa`가 정본). `auto`는 기록의 어느 모양이든, `any`는 기록의 어느 크기든 받는다. 설명은 `shared → dedicated`, `threads prefer → isolate`, `emulator threads …`, `hugepages 1G → 2M`, `memory none → strict`, `NUMA 2 nodes → 1 node` + `POWER_CYCLE`.
- **권한 `vm.dedicated`는 `vm.clone` 바로 뒤**(비트 순서; `PrivSet`은 이름 목록으로 저장되므로 저장 값은 바뀌지 않는다). 웹의 권한 목록(`web/src/auth/permissions.ts`)은 스테이지 6.
- **할당량 `dedicated_vcpus`는 임시 400 없이 기능 게이트만**(`folders.rs`, 만들기·바꾸기에서 `require_feature(NUMA)`): 상태 머신의 `of_vm_value`가 이미 `cpu.policy`·`max_vcpus`·`emulator`로 세므로 지금 받아도 맞게 동작한다. `FolderUsage.dedicated_vcpus`는 0이면 쓰지 않는다(지금 응답 그대로).
- **임시 API 막기(스테이지 5 전)**: `vms.rs` `prepare`가 `numa::in_use(spec)`(정책·스레드·에뮬레이터·`numa`·`hugepages` 가운데 하나)면 `require_feature(NUMA)`(409) 뒤 400 "dedicated CPUs, NUMA and hugepages are not available yet". `PUT /nodes/{key}`(`resources.rs` `update_node`)도 `host_resources`가 있으면 같은 409 → 400. 시험 `crates/opl-api/tests/numa.rs`. 스테이지 5가 이 400을 검증·권한·할당량으로 바꾼다.
- **opld 등록**은 `NodeStatus.topology`를 지우지 않고 둔다(`bootstrap.rs`, 계획 63의 `cpu`와 같다).
- `schema.d.ts`를 다시 만들었다.

### 스테이지 2 (opld 탐침)

- **`topology::probe(sys, proc)`는 설계 그대로**이고 `warnings`·`probed_at`은 비워 돌려준다. `cpuset`은 탐침이 cgroup v2 컨트롤러만 보고, 검사 루프의 `TopologyWatch`가 `[hypervisor] systemd_scope`와 AND 한다. `pinning`은 늘 참(Linux에서만 띄움 — macOS·위트니스는 `TopologyWatch`가 없다).
- **노드 상태를 작게**: `HostNode.hugepages`에는 0이 아닌 풀만 둔다(크기 목록은 `page_sizes_kib`에, aarch64 svc01은 `64·2048·32768·1048576`). `node` 디렉터리가 없으면 노드 0의 풀은 `/sys/kernel/mm/hugepages`의 것이고 거리는 비운다. `distance` 줄 하나라도 노드 수와 어긋나면 거리 전체를 비운다. 패키지 번호가 없거나 -1이면 소켓 0(svc01 aarch64는 `physical_package_id` 36 — 소켓 수는 서로 다른 번호의 수라 1이다).
- **크기 측정**: 2소켓 × 64코어 × 2스레드(256 CPU, 노드 2, L3 16, 풀 둘) JSON 1,924바이트, x86 2소켓 고정물 571바이트. 4 KB 안쪽이라 형제 묶음을 줄이지 않는다(시험 `a_large_topology_stays_small`이 4 KB를 넘으면 실패).
- **경고 문구**: `dedicated CPUs 7 are offline`(예약도 같은 꼴), `host processes may run on the dedicated CPUs 4-5; see the host preparation guide`(전용 − `isolated`, PID 1 `Cpus_allowed_list`와 겹침, PID 1을 못 읽으면 생략), `the dedicated CPUs split SMT sibling groups 0,4 1,5`, `node 0: 2M hugepages 100 wanted, 64 reserved`(호스트 노드를 안 정하면 `node …:` 없이 전체와 견줌). 더해 `nr_hugepages`를 쓰지 못한 까닭과 opld 친화를 걸지 못한 까닭도 경고로 둔다.
- **hugepages 예약은 설정이 바뀔 때만** 쓴다(시작할 때 + `host_resources`가 바뀐 회차). 커널이 모자라게 잡은 수는 회차마다 다시 쓰지 않고(1G의 조각 모음을 30초마다 부르지 않게) 경고로만 남는다 — 다시 하려면 설정을 바꾸거나 opld를 다시 시작한다. 지금 수와 같은 예약은 쓰지 않는다.
- **탐침 주기**: 검사 회차(5초)마다 노드 설정만 읽고, 설정이 바뀐 회차와 여섯 회차(30초)마다 `/sys`를 읽는다. 읽기·쓰기는 `spawn_blocking`. 값이 같으면 `probed_at`을 바꾸지 않아(`keep_time`, 저장된 값과도 견줌 — 다시 시작해도) 노드 상태를 다시 쓰지 않는다. 읽지 못하면 앞 값에 `error`만 단다. 노드 설정을 읽지 못한 회차는 건너뛴다(없는 것으로 보고 친화를 되돌리지 않게).
- **opld 자기 친화**: 예약 CPU(온라인과 교집합)가 있으면 `/proc/self/task`의 모든 스레드에 `sched_setaffinity`, 예약을 빼면 온라인 CPU 전체로 되돌린다(바뀔 때만). **QEMU·swtpm은 opld의 친화를 물려받으므로** 스테이지 4 드라이버는 예약이 있을 때 `pre_exec`에서 공유 집합(또는 전용 VM의 에뮬레이터 집합)으로 늘 다시 걸어야 한다 — 아니면 공유 VM이 예약 CPU에 갇힌다(지금은 API가 `host_resources`를 막으므로 생기지 않는다).
- **등록**: `member.rs`가 `firmware_caps`·CPU 탐침 다음에 저장된 `host_resources`로 한 번 적용·읽고 `Host.topology`에 둔다. `record_node`는 모든 노드가 `numa`를 알 때만 그것을 쓰고(`probed_at`만 다르면 저장된 것), 아니면 저장된 것을 둔다.
- **고정물**(`crates/opld/tests/fixtures/topology/`, 각 디렉터리의 `SOURCE`에 출처): `x86-2s`(손으로 만든 2소켓 Xeon 모양), `aarch64-1n`(svc01에서 읽기만 해서 기록), `x86-no-numa`(`node` 없음·cpuset 없음), `x86-offline`(CPU 7 오프라인·`isolcpus`·`nohz_full` `(null)`·PID 1 친화). **GitHub 러너의 x86 노드 하나는 아직 없다**: `numa-it`의 `probes_this_host`가 러너의 탐침 JSON을 로그에 찍으므로 그것으로 나중에 옮긴다.
- **`opld/numa-it`를 스테이지 2에서 만들었다**(스테이지 4가 VM 시험을 더한다): 이 호스트 읽기(JSON 찍기), 노드 0에 2M 16개를 더 예약했다가 되돌리기(끝에 원래 글을 다시 씀), `sleep` 자식의 스레드 친화 걸기. `/sys`에 쓰는 시험은 root와 `OPL_TEST_NUMA_SYS=1`일 때만 돈다(배포 서버에서 기능을 켜고 돌려도 호스트 설정을 바꾸지 않게). CI `linux-kvm`의 `FEATURES`와 `env`에 더했다.

### 스테이지 3 (장부·스케줄러)

- **장부의 판정은 opl-types에 함께 둔다**: `numa::HostLimits::new(host_resources, topology)`(전용 집합 크기, 크기마다 풀 MiB, 토폴로지가 없으면 0), `numa::exceeded(limits, before, after) -> Option<HostExceeded>`("쓰기 뒤 넘고 쓰기 전보다 늘었다", 전용 → 2M → 1G → 두 크기 합 순), `Demand::add`·`within`·`mib`, `HostUsage::add`. 상태 머신과 스케줄러가 같은 함수를 쓴다(계획 63의 `CpuContext`처럼). 스테이지 4 할당기는 `fit`·`fit_fixed`·`usage`·`HostFree`를 그대로 쓴다.
- **`HostResource`에 `hugepages`(두 크기 합)를 더했다**(§5의 셋에 더해): `any` VM은 한 크기에 셀 수 없어 "2M + 1G + any ≤ 두 풀 합"을 따로 본다. 단위는 전용 CPU는 개수, hugepages는 MiB(`limit`·`used`). 거절은 `HostResourcesExceeded { node, resource, limit, used }`(문구 `node <id> would use 5 of 4 dedicated CPUs`), API는 409 `no_capacity`에 `details {node, resource, limit, used}`.
- **상태 머신(`opl-cluster/src/hosts.rs`)**: VM 만들기·스펙 바꾸기만 본다. 쓰기 뒤 `power` running·템플릿 아님·`node` 있음이고 `demand(spec, None)`이 0이 아니며 그 노드에서 이 VM의 몫이 쓰기 전보다 늘 때만 VM을 훑는다(`check_exclusive`의 훑기에 붙여 할당량과 함께 한 번). 노드의 `host_resources`·`topology`는 그 두 칸만 푼다(노드 스펙·상태의 다른 칸이 바뀌어도 판정이 같게). 마이그레이션 확정(옛 `status.migration.node` = 새 `spec.node`)은 보지 않는다. 노드 설정 바꾸기(전용 집합 줄이기)는 보지 않는다 — 스테이지 5 API의 409 `host_cpus_in_use`가 막는다.
- **스케줄러(`Inventory::numa_fits`)**: 노드마다 `host_usage` — 그 노드에 자리 잡은 VM(`placement::occupied`, `Assign`을 따름, 오는 마이그레이션 포함) 가운데 기록된 `host_placement`(그 노드의 것)는 정확히, 아직 켜지 않은 VM은 스펙 모양을, 오는 마이그레이션은 기록된 모양을 지금 빈 자원에 차례로 `fit`해 본 배치로 센다(§9의 "합계로만" 대신 — 같은 함수라 노드 할당기가 고를 것에 가깝다). 맞춘 뒤 새로 켜는 VM은 Raft와 같은 합계(`exceeded`)도 본다(맞춰지지 않은 앞 VM, `isolate`·`any`). 까닭은 `fit`의 것이 먼저다(“has 1 free dedicated CPUs …”가 “would use 5 of 4 …”보다 알기 쉽다).
- **도는 VM은 기록만 본다**: `hardware`가 있으면 `hardware.numa`가 묶는 모양일 때만 `fit_fixed`(옮기기)이고, 기록이 없거나 묶지 않으면(계획 64 전 기록, 스펙만 전용으로 바꾼 보류) 아무것도 세지 않는다. 스테이지 4 전에는 작업자가 `numa`를 기록하지 않으므로 도는 전용 VM은 합계로만 센다.
- **함수 이름**: `node_fits(spec)`는 저장소의 어느 VM도 이 VM으로 보지 않는다. 저장된 VM에는 `vm_fits(vm, inv, node, assign)`(후보·`blocked`·HA `pick`), `replica_fits_with`·`replica_migration_fits_with`·`migration_fits_with`(`assign`을 받는 판), `numa_receives`(옮기기·고정 노드 켜기, 기록 모양)·`numa_starts`(껐다 켜기, 스펙)를 더했다. `cpu_blocks_every_move`는 NUMA 까닭도 보므로 `blocks_every_move`로 이름을 바꿨다.
- **경로마다**: 켜기(`vm_action start`)는 고정 노드가 CPU·NUMA로 안 되면(`starts_on`) 다른 노드를 고르고, `HostResourcesExceeded`면 그 노드를 `Inventory`에서 빼고 한 번 더 `modify_spec`을 돈다(`Box::pin`). 만들기(`power: running`)는 다시 고르지 않고 409. 껐다 켜기는 `cpu_starts`에 `numa_starts`를 더했다. 마이그레이션 API는 `numa_receives`가 먼저라 409 `migration_unsupported`(§11), 그 밖은 지금처럼 `migration_fits`의 409 `no_capacity`. HA 복구는 회차의 `assign`을 세는 `replica_fits_with`·`vm_fits`로 고르고, Raft가 거절하면 지금처럼 그 VM만 다음 회차다. HA 검사의 옮기기(`pick_live`)·되돌아가기·균형 조정기의 대상 조건은 `replica_migration_fits(_with)`로 NUMA를 본다. 비우기 묶음은 앞 구성원을 대상 노드에 둔 `assign`으로 다음 구성원을 맞춘다. 균형 조정기의 `movable`은 전용 VM(기록, 없으면 스펙)을 `dedicated CPUs`로 뺀다.
- **보통 메모리**: `scheduler::small_memory_mib(node)`(노드 메모리 − 토폴로지의 hugepages 풀)와 `vm_small_mib(vm)`(hugepages VM은 0 — 도는 VM은 기록의 `page`, 아니면 스펙)을 두고 `Inventory::free_memory_mib`·`placement::free_mib`(후보 순위·비우기·되돌아가기·마이그레이션의 메모리 확인)에 썼다. `allocated_mib`·균형 조정기의 사용률(노드 메모리 전체 대비)은 그대로다 — 사용률 그림이 바뀌지 않게, 그리고 hugepages VM이 드물어서다.
- **시험**: opl-types(`exceeded`·`HostLimits`·`Demand`), opl-cluster 단위(노드마다 셈·늘지 않는 쓰기·마이그레이션 확정·hugepages와 `any`·토폴로지 없음)와 `three_nodes`의 두 팔로워 동시 만들기·켜기(전용 4개에 24번, 4번만 성공), opl-api 단위(스케줄러·`Assign`·`fit_fixed`의 옮기기와 오는 마이그레이션·`blocks_every_move`·hugepages와 보통 메모리·균형 조정기)와 `tests/numa.rs`(켜기 배치와 `no_node` 까닭, 꽉 찬 고정 노드에서 옮겨 켜기, 껐다 켜기 409, Raft 거절의 API 오류). 두 API 노드의 실제 경쟁에서 다시 고르기가 도는 것은 결정적으로 만들 수 없어 장부 시험(Raft)과 `host_full`(단위)·`starts_on`(꽉 찬 고정 노드에서 옮겨 켜기) 시험으로 본다.

### 스테이지 4 (명령줄·고정·할당기)

- **`VmRuntimeSpec`에 셋**: `numa`·`host_placement`(기록에서, `apply_hardware`가 옮김)와 **`pinning: RuntimePinning`**(§5에 없던 것) — `{ emulator: CpuSet, vcpus: Vec<CpuSet> /* 자리마다 */, slots_per_socket, scope: Option<CpuSet> }`. 작업자·받는 쪽이 순수 함수 `numa::pinning(numa, placement, hw.cpu, topology, sets)`로 셈해 넘기고, 드라이버는 이것만 건다(세 집합을 모른다). 배치가 없고 노드에 예약·전용 집합이 없으면 None — 친화·`AllowedCPUs`·`systemd-run` 인자가 지금 그대로다(시험). 예약 집합만 있어도 공유 VM은 공유 집합에 둔다(opld 친화를 물려받지 않게, 스테이지 2 보고). 묶는 공유 VM의 vCPU는 게스트 노드의 (공유 ∩ 호스트 노드 CPU), 비면 공유 집합.
- **드라이버**: 스코프 `--property=AllowedCPUs=<scope>`, `pre_exec`의 `sched_setaffinity(0, emulator)`(QEMU와 swtpm 모두), QMP가 붙은 뒤(`-S`, 받는 쪽은 받기 전) **모든 스레드**를 고정한다 — vCPU는 `query-cpus-fast`의 `props`(소켓·코어 → 자리)로 찾아 자리의 CPU, 나머지는 에뮬레이터 집합(스코프 cpuset에 들어갈 때 6.2 전 커널은 친화를 cpuset으로 넓히므로 다시 건다). vCPU는 `/proc/<pid>/task/<tid>/status`로 확인하고 틀리면 QEMU를 끄고 `cannot pin vCPU 3 to CPU 7: …`. 고정은 실행 디렉터리 `pinning.json`에 남아 실행 중 붙인 vCPU(`device_add` 뒤 고정, 실패하면 `device_del`)와 검사 루프가 읽는다. 시간 제한은 30초 + 미리 잡는 GiB마다 1초. 스테이지 2의 `pin_tasks`는 `opl_driver::affinity`로 옮겨 opld가 다시 쓴다.
- **명령줄**: §10 그대로이고 `numa`가 없으면 바이트까지 같다(시험: `host`·`max`, 고정이 있어도). 백엔드 id `ram-node<i>`, `prealloc-threads` = 켤 때 vCPU 수(1~16). virtio-mem은 게스트 노드마다 `vmem<i>`(+`node=<i>`, 2M hugepages면 장치에 `prealloc=on`); 상한 몫은 `numa::split_blocks`(2 MiB 블록, 남는 것은 노드 0), 꽂은 크기는 `split_plugged`(장치 자리를 넘지 않게)로 나누고 드라이버 `set_memory`도 같은 함수로 장치마다 건다(`/objects/vmem<i>`의 크기를 자리로). 장치 목록·마이그레이션 견주기는 `vmem<i>-dev`. 1G VM의 virtio-mem 백엔드는 hugetlb 없이 둔다(검증이 1G + 메모리 핫플러그를 막아 닿지 않는다) — 그리고 **할당기는 메모리 핫플러그 VM의 `any`를 2M으로 고른다**(블록 2M).
- **노드 할당기**(`opl-reconciler/src/host_alloc.rs`, `HostAllocator`, opld마다 하나 — `ReconcilerConfig.host_alloc`과 `IncomingMigrations::with_host_alloc`이 나눔): `claim(cluster, vm, Shape::Spec(spec, hw.cpu) | Shape::Fixed(numa, cpu))` → `Claim { numa, placement, pinning }`. 묶는 VM만 예약한다. 사용량 = Raft 기록(이 노드의 `host_placement`, 예약이 있는 VM은 빼고) + 예약. 빈 hugepages는 장부와 `/sys`의 `free_hugepages`(NUMA 커널은 노드마다) − 켜는 중 예약 가운데 작은 것. 거절은 예약을 빼면 들어갈 때 `Waiting`(작업자가 실패로 세지 않고 다시), 아니면 `Cannot`(노드 이름을 담고 VM 상태에는 `without_node_name`). 예약은 VM이 없거나 이 노드를 떠났거나(배정도, 받는 마이그레이션도 아님) Raft 기록이 같은 배치면 지워지고(다음 `claim` 때), 작업자는 켜기 실패·QEMU가 없음을 볼 때, 받는 쪽은 실패·취소 때 푼다. 받는 중 예약은 `INCOMING_MARK` 옆 `placement.json` 대신 이미 있는 `hardware.json`(`save_hardware`, 배치를 이 노드의 것으로 바꾼 기록)에 남기고 opld가 뜰 때 `list_incoming`으로 되살린다.
- **스펙 모양은 `hw.cpu`로 맞춘다**: 노드가 CPU 핫플러그를 못 하면 상한이 빠진 `hw.cpu`로 `layout`하므로 할당기가 잡는 pCPU가 Raft·스케줄러의 셈(스펙 상한)보다 적을 수 있다(안전한 쪽). 명령줄의 자리 수와 같아야 고정이 맞는다.
- **작업자**: `cpu_at_start` 다음 `numa_at_start`(새로 켜면 스펙, 메모리 스냅샷은 담을 때 모양 그대로 `Fixed`), 거절은 CPU와 같은 길. 작업자가 스스로 다시 켜는데 새 모양이 안 맞으면 꺼지기 전 모양(`restart_numa`, 소켓·메모리 합이 같을 때)으로 켜고 보류 `numa`를 기록에 남긴다. 켤 때 기록의 보류 `numa`는 늘 `numa::pending_change(spec, hw)`로 다시 적는다. 받은 마이그레이션을 넘겨받을 때 기록의 배치가 원본 노드의 것이면 실행 디렉터리의 기록(받는 쪽이 고른 것)으로 고친다.
- **마이그레이션**: `MigrateIncomingResponse.host_placement`(옛 대상은 없음 → 쓰지 않음), 원본 Task의 확정 뒤 status 쓰기(`finish`)가 `hardware.host_placement`를 대상의 것으로 쓴다. 대상 작업자의 고치기와 둘 다 있어 어느 쪽이 먼저여도 맞다.
- **보류 `numa`**: `hotplug::changes`(→ `plan`·`plan_resources`·작업자의 `changed_while_starting`)가 `numa::pending_change`를 더하고, `hardware_changed`가 `numa`·`hugepages`를 본다(정책은 `cpu` 비교에 이미 든다).
- **검사 루프의 다시 고정**: `checks.rs` `PinWatch`가 여섯 회차(30초)마다, `host_resources`가 바뀐 회차에는 곧바로 `host_alloc::repin`을 부른다 — 도는 VM마다 기록과 지금 세 집합으로 셈한 고정을 드라이버 `Hypervisor::repin`(스코프 `AllowedCPUs`를 `systemctl show/set-property --runtime`으로 먼저, 그다음 스레드마다 다를 때만)으로 맞추고 바꿨으면 경고 로그. 고정 없이 켰고 지금도 걸 것이 없는 VM은 건드리지 않고(`saved_pinning`이 없음), 고정했던 VM이 이제 걸 것이 없으면 온라인 CPU 전체로 되돌린다. swtpm은 다시 걸지 않는다(켤 때의 에뮬레이터 집합).
- **시험**: opl-types(`pinning`·`split_*`, 보류 `numa`와 vmem 장치), opl-qemu(바이트 그대로·공유 vNUMA·전용 1G 묶기와 거리·노드마다 virtio-mem·aarch64), opl-driver(`AllowedCPUs` 인자·시간 제한·`Cpus_allowed_list`·자리 찾기), opl-reconciler `tests/numa.rs`(전용 배치·기록·고정, 설정 없는 노드의 그대로, `/sys` 빈 hugepages, 켜는 중 예약의 기다림, 다시 고정과 공유 집합 바뀜), opl-api `tests/incoming.rs`(받는 쪽 다시 고르기·응답·`hardware.json`·취소). 실제 KVM은 `opld/numa-it` `tests/numa.rs`(§14의 (1)(2)(3)(5)(6)(7): 시험 고리 `/v1/test/vm-spec`·`host-resources`로 스펙·노드 설정을 바로 쓴다). (4) 공유 vNUMA의 `node-id`와 노드 둘 이상의 virtio-mem은 GitHub에서 보지 않는다(명령줄 시험만).

### 스테이지 5 (API·CLI)

- **임시 400을 풀었다**: `vms.rs` `prepare`는 계획 64 칸이 있으면 `require_feature(NUMA)`(검증보다 먼저)만 하고, `VmSpec::validate`의 `ValidationError::Numa`는 400 `invalid_numa`다(`cpu::validation_error`). 복제·배포(`apply_overrides`의 `TemplateError::Invalid`)도 같은 코드로 바꿨다(전에는 `invalid_request`).
- **`PUT /nodes/{key}`의 `host_resources`는 PATCH를 두지 않고 "칸이 없으면 그대로"로 정했다**: `NodeSpec`은 `deny_unknown_fields`라 이 칸을 모르는 클라이언트(업그레이드 중의 옛 `oplctl`·웹)가 읽어 다시 PUT하면 칸이 빠진다 — 지우면 노드 설정이 조용히 사라진다. 지우기는 `{}`(모두 빈 설정 = 설정 없음과 같음, `HostResources::is_empty`)이고 CLI `node resources --clear`가 보낸다. 같은 값이면 검증하지 않는다(CPU가 오프라인이 된 노드의 주소·유지보수 PUT이 막히지 않게). 바뀌면 `require_feature(NUMA)`, 위트니스·토폴로지가 없는 노드는 400 `invalid_host_resources`, `HostResources::validate`(문구 앞에 `node "n1": `), 409 `host_cpus_in_use`(`details.vms`)·`hugepages_in_use`(`details.short`, `2M@0: 100 reserved, 128 used`), `?force=true`면 쓰고 `orbivisor-warning`. 감사 `node set n1 host_resources dedicated=4-31 reserved=0-1 hugepages=2M:1024@0`.
- **`force`의 보류 `numa`는 더하지 않았다**(§7의 "그 VM들은 경고와 보류 numa"): 보류는 작업자가 스펙과 기록을 견줘 쓰는 것이라 노드 설정에서 만들 길이 없다. 대신 응답 경고에 VM과 밖으로 나간 CPU를 적고, 검사 루프의 다시 고정(스테이지 4 `repin`)은 기록된 pCPU를 그대로 건다 — 껐다 켜면 새 전용 집합에서 다시 고른다.
- **`hugepages_in_use`의 셈**: 예약 `(크기, 호스트 노드)`마다 스케줄러 장부(`Inventory::host_usage` — pub로 바꿈)의 그 노드·크기 페이지(호스트 노드를 정하지 않은 예약은 모든 노드 합)와 견준다. 설정에서 뺀 크기는 커널 수를 그대로 두므로 보지 않는다.
- **권한 `vm.dedicated`**(`numa::check_privilege`·`require_privilege`): 만들기는 폴더에서(`require_in_folder`), 바꾸기는 그 VM에서, 정책·스레드·에뮬레이터·hugepages가 바뀔 때만(`numa::host_resources_changed` — 관리자가 켠 전용 VM의 vCPU 수는 운영자가 바꾼다). 복제·배포·가져오기·복원·카탈로그 템플릿은 새 VM 스펙이 전용·hugepages면 새 폴더에서 본다(원본에서 물려받아도). vNUMA·`numa.memory`만 쓰는 공유 VM은 권한이 들지 않는다. 403 문구는 `dedicated CPUs and hugepages need the vm.dedicated privilege`.
- **덮어쓰기 칸**: `CloneOverrides`·`CloneRequest`에 `cpu_policy`·`cpu_thread_policy`·`cpu_emulator`·`numa`·`hugepages`와 지우기 `no_numa`·`no_hugepages`(§13의 `--numa none`·`--hugepages none` — `Option`으로는 "지움"을 적을 수 없다), `ImportRun`·`TemplateDraft`·`CatalogTemplate`은 앞의 다섯(새 VM이라 지우기가 필요 없다). 적용은 `numa::NumaOverrides::apply` 하나(`shared`면 스레드·에뮬레이터도 지움). 가져오기 Task도 `import_spec`에서 같은 적용을 한다.
- **`GET /vms/{key}/numa`**: `{spec, wanted, running, placement, pending, nodes: [{node, fits, reason, plan: {host_nodes, pcpus, page}}]}`. 노드마다의 판정은 스케줄러와 같은 `numa_plan`(`numa_fits`가 맞춘 배치를 돌려주게 나눔) — 하드웨어 기록이 있으면 기록 모양으로 옮기기, 없으면 스펙으로 켜기. 루트 `cluster.read`가 없으면 `placement`·`nodes`를 비운다. "지금 모양"은 따로 두지 않았다(`wanted` 문구 + 노드마다 `plan`).
- **`GET /nodes/{key}/topology`**: `{topology, host_resources, sets, host_nodes: [{id, cpus, memory_mib, dedicated, dedicated_used, hugepages: [{size, total, used}]}], vms: [{vm, host_nodes, vcpus, siblings, emulator, page, pages}], live, warnings}`. `live`는 노드 사이 RPC `ReadTopology`(`opl_peer::TopologySource`, 이 노드면 `AppState.topology`로 직접, 5초) — opld `topology::live`가 노드마다 `MemFree`·`free/surplus_hugepages`를 읽고 `affinity_ok`는 `/proc/self/status`의 친화가 예약 CPU(없으면 온라인 전체)와 같은지다. Linux opld만 소스를 넣는다(macOS·위트니스·옛 노드는 `unimplemented` → `live` 없음).
- **`GET /topology/usage`**: `{nodes: [{node, pinning, dedicated_total, dedicated_used, hugepages: [{size, total_mib, used_mib}]}]}`(위트니스 빼고 이름순). 사용은 스케줄러 장부의 배치(Raft 합계가 아니다 — 같은 전용 CPU를 두 번 세지 않는다). 경로표는 둘 다 루트 `cluster.read`, `/vms/{key}/numa`는 `Read(Vm)`.
- **할당량 `dedicated_vcpus`**: 스테이지 1이 상태 머신에서 이미 센다. CLI `folder quota set --dedicated-vcpus N|none`과 사용량 줄(쓰거나 할당량이 있을 때만 — 지금 출력 그대로)을 더했다.
- **CLI**: `vm create|set|clone`·`template deploy`·`import run`·`image get --template`에 `--cpu-policy`·`--cpu-threads`·`--emulator-threads`·`--numa`·`--numa-memory`·`--hugepages`(`oplctl/src/numa.rs` `NumaArgs`). `vm set --numa 2`는 지금 `numa.memory`를 두고, `--numa-memory`만 주면 vNUMA가 없을 때 `auto`로 만든다. `vm get`은 `numa:`·`numa running:`·`placement:`·`numa pending:` 줄(안 쓰면 없음). `vm numa`, `vm migrate`는 대상의 NUMA 까닭을 경고로 먼저 찍는다, `node topology [--live]`, `node resources [--reserved-cpus] [--dedicated-cpus] [--hugepages 2M:1024@0] [--clear] [--force]`(준 칸만 바꾸고 `If-Match`로 쓴다, 칸이 없으면 지금 설정을 보인다). 새 디스패치는 `run_cli` 안에 `Box::pin`으로 둔다(계획 62의 스택 넘침).
- **`vm set`이 전용 VM에서 된다**(스테이지 4의 400은 임시 게이트였다): 상한 안의 vCPU·메모리는 핫플러그 Task, 배치 칸은 보류 `numa`.
- **실제 VM 시험(`opld/tests/numa.rs`)은 CLI로 바꾸고 시험 고리(`numa_hook.rs`, `/v1/test/vm-spec`·`host-resources`)를 지웠다.** `install_media.rs`에 게스트 vNUMA 단계(`--sockets 2 --numa 2`, 게스트의 `/sys/devices/system/node/node*/cpulist`·`MemTotal`)를 더했다(GitHub `install-media`).
- **스테이지 4 GitHub 탐침의 실패 고침**: x86 러너의 QEMU `task` 목록에 커널 일꾼 `kvm-nx-lpage-recovery`(vhost_task, `PF_USER_WORKER`)가 보이고 그 친화는 커널이 정한다(vCPU 친화를 따라감). 드라이버 `pin_threads`(켤 때·받을 때·검사 루프의 `repin`)는 `/proc/<pid>/task/<tid>/stat`의 플래그(`PF_USER_WORKER 0x4000`·`PF_KTHREAD 0x200000`·`PF_NO_SETAFFINITY 0x4000000`)가 있는 vCPU 아닌 스레드를 건너뛴다(`affinity::is_kernel_worker`) — 상한은 스코프 `AllowedCPUs`가 쥐고, 검사 루프가 30초마다 커널과 다투지 않는다. `vhost-*` 일꾼도 같다. 시험도 같은 플래그로 그 스레드를 뺀다.
- `schema.d.ts`를 다시 만들었다.

### 스테이지 6 (웹)

- **VM 폼 › 일반의 접힌 "CPU 배치와 NUMA"에 소켓 수를 더했다**: 웹 폼에는 소켓 입력이 없어(`sockets`는 스펙 값을 그대로 돌려보냄) vNUMA 노드를 둘 이상 고를 길이 없었다. 고르기(1·2·3·4·6·8·12·16, 지금 값이 밖이면 그것도)로 두고 노드 수는 소켓 수의 약수(8까지)만 보인다. 소켓이 하나면 "소켓을 늘리세요", 노드 없음 + 전용·hugepages면 "늘 자동 NUMA"라고 말한다. 직접 나누기(노드마다 vCPU 자리·MiB)는 VM 폼에만 두었다.
- **권한**: 전용 정책·hugepages는 `vm.dedicated`가 없으면 새로 고를 수 없고(지금 값이면 그대로 둠) 까닭을 보인다. 만들 때는 고른 폴더, 고칠 때는 그 VM에서 본다. 복제·배포·가져오기는 어딘가에 권한이 있으면 고르게 하고 폴더는 서버가 본다(403). `permissions.ts`의 목록에 `vm.dedicated`(operator 밖)를 더해 설정 › 역할에도 보인다.
- **"들어갈 노드"는 어림**: `/topology/usage`로 전용 CPU 남은 수(vCPU 자리 + 에뮬레이터 `isolate` 1, `isolate` 형제는 노드의 SMT를 몰라 빼고)와 그 크기 풀의 남은 MiB만 견준다. 호스트 노드 나눔은 서버가 켤 때 본다.
- **VM 상세 › 요약의 "CPU 배치"는 VM 기록(`status.hardware.numa`·`host_placement`)을 바로 쓴다**(`/vms/{key}/numa`를 묻지 않음 — VM을 읽을 수 있으면 기록도 보인다). 보류 `numa`는 계획 63의 전원 껐다 켜기 띠와 VM 목록의 "CPU 변경 보류" 배지·거름을 함께 쓴다(`cpuPending`이 `numa`도 본다).
- **마이그레이션 대화상자**: 계획 64 칸을 쓰거나 기록에 `numa`가 있는 VM만 `/vms/{key}/numa`를 묻고, 맞지 않는 노드는 "(VM의 CPU·메모리를 둘 수 없음)"으로 막고 고른 노드면 까닭을 보인다.
- **노드 격자에 소켓 층은 없다**: 토폴로지에 CPU마다 소켓 번호가 없어 호스트 노드 › L3 › 코어(형제) › 스레드로 그린다. 호스트 자원 설정의 처음 값은 노드 스펙의 `host_resources`이고, 409 `host_cpus_in_use`·`hugepages_in_use`면 `details`(VM·모자란 것)를 보이고 단추가 "그래도 저장"(`?force=true`)이 되며 `orbivisor-warning`은 알림으로 보인다. 모두 비우면 `{}`로 지운다.
- **노드 목록 열 둘**은 우선순위 3(1280px 미만에서 숨김), 폴더 할당량 "전용 vCPU"는 쓰거나 상한이 있을 때만 막대를 보인다.
- 복제·배포·가져오기의 접힌 칸(`NumaChoice`)은 바꾼 칸만 보낸다. 스레드·에뮬레이터를 기본으로 되돌리면 `prefer`·`shared`를 보내고(덮어쓰기에 지움이 없다), 가져오기는 `no_numa`·`no_hugepages`를 쓰지 않는다.

### 스테이지 7 (문서)

- **"호스트 준비"는 `docs/guide/cluster.md`에 두었다**(§13 그대로, 따로 된 `numa.md`를 만들지 않음): 노드 설정·커널 명령줄·
  systemd라 노드 운영 문서에 맞고, 바로 앞 절 "클러스터 CPU 기준"과 이웃한다. 절은 노드가 알리는 토폴로지, 세 CPU 집합(검증·409·
  `force`·`PUT`에서 칸이 없으면 그대로), hugepages 부팅 때·실행 중 예약과 메모리 셈, `isolcpus`·`CPUAffinity=`·`system.slice`를 묶지
  않는 까닭과 Orbivisor가 거는 `AllowedCPUs`·친화, opld 경고 표, 2소켓 64 CPU 예(커널 명령줄·`system.conf`·`node resources`·`node
  topology` 출력), OpenStack·Proxmox VE 견주기다. opld 경고의 "see the host preparation guide"가 이 절을 가리킨다.
- **`docs/guide/vms.md` "NUMA·전용 CPU·hugepages"**: 칸 표, `auto`와 소켓(웹 폼의 소켓 칸 — 스테이지 6 인계), 묶는 VM, 핫플러그 상한까지
  잡기, 권한·할당량, 전원 껐다 켜기 규칙, 켤 때 기록과 어긋남 고치기, 노드 고르기의 까닭, 마이그레이션·HA·균형 조정, CLI·웹·API, 한계.
  CLI 예의 출력 줄은 `oplctl/src/numa.rs`의 `numa_lines`·`spec_text`·`placement_text`와 opl-types `pending_change`·`describe`의 글꼴을
  따라 손으로 적었다(cargo를 돌리지 않음 — 리뷰가 같은 때 코드를 읽는다).
- **`docs/architecture.md`에 §9.7을 더했다**(탐침, 세 층의 자원 세기, 기록, 명령줄과 고정, 어긋남 고치기, 업그레이드)와 §3.2 표의
  `HostResourcesExceeded` 줄, §6.1 `ReadTopology`(RPC 28개), §8.4 보류 `numa`, §9.1 vNUMA, §14 기능 `numa`. 줄 번호는 이 브랜치
  `656c72d`의 것이다.
- `web-ui.md` "NUMA 화면", `folders.md`의 `vm.dedicated`(operator에서 뺀 까닭)·`dedicated_vcpus`, `development.md`의 시험 명령(root로 도는
  `numa-it`와 `install-media`의 게스트 vNUMA), README·`docs/index.md`, 계획 문서 `2026-11-19-plan64-numa.md`.

### 리뷰 1에서 고친 것 (서버)

- **C1 호스트 노드마다 묶인 보통 메모리.** `HostUsage.memory`·`HostFree.memory`(호스트 노드 id → MiB)를 더했다. `usage`는 hugepages 없이 묶는 기록(`numa.page`가 없고 `numa.memory`가 있음)의 게스트 노드 메모리를 그 호스트 노드에 세고, `HostFree`는 `small_memory_mib() − 묶인 것`이다. 맞춤의 `cell.memory`는 이 빈 몫과 견주고 거절 문구는 `has N and M MiB of unbound memory on host NUMA nodes …`다. `preferred`도 센다(넘쳐도 OOM은 아니지만 몰리면 `strict` VM이 들어갈 자리를 먹는다 — 보수적으로). **순서 키를 채우기에서 펴기로 바꿨다**: (최대 거리, 남는 전용 CPU 많은 쪽, 남는 페이지 많은 쪽, 남는 보통 메모리 많은 쪽, 번호) — 스테이지 1의 "남는 것 적은 쪽"(best-fit)은 전용 VM을 한 호스트 노드에 몰아 묶인 메모리가 그 노드를 넘쳤다(OpenStack도 Zed부터 펴기가 기본). 한 호스트 노드 안의 pCPU 고르기(형제·L3)는 그대로다. **Raft 장부도 같이**: `Demand.bound_mib`(전용·`numa.memory` VM의 `memory_mib`, hugepages VM은 0)와 `HostLimits.small_mib`(호스트 노드의 보통 메모리 합), 자원 `bound_memory`(`MiB of memory bound to host NUMA nodes`). 장부는 노드 합만 보고 호스트 노드마다의 판정은 스케줄러·할당기가 한다(스케줄러가 더 엄하다). 공유 VM이라도 `numa.memory`만 있으면 장부 검사를 탄다(전에는 요구 0).
- **I1** `HostAllocator::claim`은 잠금을 먼저 잡고 VM을 읽는다(읽기는 동기 로컬). 순서만 바뀐 고침이라 결정적으로 재현하는 시험은 두지 않았다(끼어들기 고리가 없다).
- **I2** "메모리 핫플러그 VM의 `any`는 2M"을 할당기에서 `numa::layout`·`numa::demand`(같은 `page_size`)로 옮겼다 — 스케줄러·Raft 장부·`GET /vms/{key}/numa`·할당기가 같은 크기를 본다(장부도 2M 몫에 센다).
- **I3** virtio-mem hugetlb 백엔드(`vmem<i>`)에 `reserve=off`를 붙였다(켤 때 메모리 `ram-node<i>`는 그대로 예약하고 미리 잡는다). 확인: svc01 QEMU 8.2.2의 `-object memory-backend-memfd,help`에 `reserve=<bool>`, `-device virtio-mem-pci,help`에 `prealloc=<bool>`이 있다(svc01은 hugepages 0이고 호스트 설정을 바꾸지 않아 `HugePages_Rsvd` 확인은 못 했다). 이 Mac의 Homebrew QEMU 11.1은 `memfd`·`virtio-mem`이 없고(`reserve`는 Linux 빌드에만) 확인할 수 없었다. 최소 판: 백엔드 `reserve`는 QEMU 6.1, 장치 `prealloc`은 7.1 — Debian 12(7.2)·Ubuntu 24.04(8.2)는 되고, Ubuntu 22.04(6.2)에서는 2M hugepages + 메모리 핫플러그 VM이 장치 속성 오류로 켜지지 않는다(한계로 둠). 꽂은 메모리는 기록(`nodes[].memory_mib`)에 없어 스케줄러 사용에 안 들지만 Raft 장부는 스펙 전체(`memory_mib`)를 세고, 할당기는 `/sys`의 실제 빈 페이지(미리 잡은 꽂은 페이지가 빠짐)로 줄이므로 따로 세지 않았다.
- **I4** `systemctl show --property=AllowedCPUs`는 범위를 빈칸으로 가른다(`0-1 4-15`): `affinity::systemd_cpus`가 빈칸을 쉼표로 바꿔 읽는다(드라이버의 스코프 비교). `CpuSet::from_str`(API 검증)은 그대로다.
- **I5** 고정이 없는 VM도 opld 자신의 친화(`/proc/self/status`)가 온라인 CPU(`/sys/devices/system/cpu/online`)와 다르면 QEMU·swtpm 자식을 온라인 전체에 둔다(`affinity::unpinned_child`, `pre_exec`) — 설정을 지운 뒤 opld가 풀리기 전(다음 검사 회차) 켠 QEMU가 예약 CPU를 물려받지 않는다. `repin`은 걸 것이 없는 VM도 공유 집합(설정이 없으면 온라인 전체)으로 되돌린다(스코프는 걸었던 VM만 넓힘). 그래서 모든 도는 VM이 30초마다 `/proc`로 확인되고 `pinning.json`이 생긴다.
- **M1** `{}`(지우기)도 고정된 VM이 있으면 409 `host_cpus_in_use`, `force`면 경고와 함께 지운다(전용 VM은 껐다 켤 때까지 기록된 pCPU에 남고 공유 VM이 그리로 퍼진다 — 주석도 고침). 지우기는 기능 게이트·토폴로지 검사를 하지 않는다(옛 노드가 섞여도 설정을 지울 수 있게). hugepages 예약을 빼도 커널 수는 그대로라 보지 않는다.
- **M2** `repin`은 실행 디렉터리의 기록(`saved_hardware` — 지금 도는 QEMU의 것)으로 고정을 셈한다. 없으면(켠 직후 기록 전) 그 회차는 건너뛴다.
- **M3** opld가 뜰 때 `HostAllocator::hold_saved`가 도는 QEMU(`hv.list`)와 받는 QEMU의 `hardware.json` 배치를 예약으로 다시 잡는다(Raft 기록이 같으면 다음 `claim`이 지운다).
- **M4** `TopologyWatch`가 `systemctl --version`을 한 번 읽어 244 전(또는 모름)이면 `cpuset`을 거짓으로 둔다 — 스코프에 `AllowedCPUs=`를 걸지 않고 스레드 친화만 건다. 노드 설정이 있으면 경고 `systemd N cannot limit VM scopes to CPUs (AllowedCPUs needs systemd 244); only the thread affinity is set`.
- **M5** 같은 크기를 노드마다와 노드 전체로 섞은 hugepages 예약은 400(`hugepages: 2M is reserved both per host NUMA node and for the whole node; use one form`). 웹도 같은 검사를 한다.
- **M6** 도는 `any` VM은 켤 때 고른 크기(`hardware.numa.page`)로 센다(`Demand::resolve_any`): Raft(`HostCheck`가 다른 VM의 상태 JSON에서 `hardware.numa.page`를 읽음, 그 VM 자신의 스펙 쓰기도 같은 크기)와 스케줄러 합계가 같다.
- **M7** 설정 뒤 CPU가 오프라인이 되어 공유 집합이 비면 `HostSets::new`가 온라인 CPU 전체를 공유로 둔다(빈 친화로는 QEMU를 띄울 수 없다). opld 경고 `no shared CPU is online (…); shared VMs and emulator threads run on all online CPUs`.
- **GitHub 탐침(656c72d)**: `oplctl node resources`가 레이블 없는 노드(GET이 빈 `labels`를 뺌)를 `"labels": null`로 PUT해 400이었다 — `labels`가 없으면 `{}`를 보낸다. 같은 꼴의 `group set`도 고쳤다. 서버의 `UpdateRequest.labels`는 `null`을 "레이블 없음"(칸이 없는 것과 같음)으로 받는다 — "그대로 둠"으로 받지 않은 것은 칸이 없을 때의 뜻(바꾼 뒤 전체 = 없음)과 맞추기 위해서다. 시험: oplctl 단위 시험(가짜 API로 PUT 본문), macOS에서도 도는 opld `cluster.rs`(`node resources node1 --clear`)·`permissions.rs`(`group set`). 러너의 형제 짝이 이어지지 않는 모양(`0,2`·`1,3`)은 opl-types 시험으로 고정·스코프를 확인했다(코드는 이미 형제 묶음을 집합으로 다룬다).

### 리뷰 1에서 고친 것 (웹)

- **C1** 서버는 `NodeTopology`의 빈 목록(`nodes`·`distances`·`siblings`·`l3`·`page_sizes_kib`·`warnings`, `HostNode.hugepages`)을 보내지 않는다 — 그 칸에 `schema(default)`를 달아 `schema.d.ts`에서 선택 칸이 되었고, 웹은 `fullTopology`로 채워 쓴다(`cpuGrid`도 안에서 채움). 시험 고정물 `SVC01_TOPOLOGY`(svc01이 알린 서버 모양: SMT·L3·거리·풀 없음)로 노드 화면과 대화상자를 그린다. 앞 시험의 토폴로지에서 서버가 보내지 않는 `warnings: []`를 뺐다.
- **I1** VM 폼의 소켓 고르기는 vCPU 수(소켓 × 코어)를 그대로 둔다: 바꾸면 코어 = vCPU ÷ 소켓(`coresFor`), 나눌 수 없는 소켓 수는 목록에 없다(지금 값은 늘 보임). 소켓이 둘 이상이면 코어 칸 이름이 "소켓당 코어"이고 `소켓 2 × 코어 = vCPU 4개`를 보인다. 소켓 도움말도 vCPU 수가 그대로라고 말한다.
- **I2** 호스트 자원 대화상자는 연 때의 노드(판·스펙·레이블)로 쓴다 — 그사이 바뀌었으면 412(`revision_mismatch`, "다른 사람이 먼저 바꿨습니다")를 보인다. 모두 비우면 설정이 있었을 때만 한 번 묻고("설정 지우기") `{}`를 보내며, 없었으면 보내지 않고 닫는다.
- **M1** 보류 이름 `vms.pendingField.numa`("CPU 배치와 NUMA")를 더했고 전원 껐다 켜기 띠 문구가 "CPU나 그 배치"를 말한다. 배지("CPU 변경 대기")는 그대로 두었다.
- **M2** "들어갈 노드" 어림은 고치는 VM의 기록(`host_placement`·`numa.page`)이 잡은 pCPU·페이지를 그 노드에서 빈 것으로 되돌려 센다(`ownUse`).
- **M3** 복제·배포·가져오기에서 원본이 전용·hugepages인데 어디에도 `vm.dedicated`가 없으면 접힌 칸을 펼치고 "이 VM은 전용 CPU나 hugepages를 씁니다 …"를 보인다.
- **M4** 격자 누르기는 고르는 칸의 목록이 틀리면 덮지 않고 "먼저 고치세요"를 보인다(다른 칸이 틀리면 그 칸은 그대로 둔다).
- **M5** 쓰지 않는 `numaPending`·`pageName`을 지웠다. `usesHostResources`는 M3에 쓴다.

### 리뷰 2에서 고친 것 (서버)

- **N1 HA로 옮긴 `any` VM.** 기록의 페이지 크기(`hardware.numa.page`)는 그 기록이 쓰는 노드의 것일 때만 쓴다: Raft 장부(`recorded_page(status, node)` — `status.node`와 `hardware.host_placement.node`가 둘 다 그 노드)와 스케줄러(`host_usage`의 `resolve_any`). 다른 노드로 옮기는 쓰기는 옛 기록을 보지 않는다. 스케줄러는 다른 노드의 기록을 가진 VM을 마이그레이션 대상일 때만(`status.migration.node`) 기록 모양으로 맞추고, HA가 옮기는 VM(자리·`spec.node`만 이 노드)은 스펙으로 맞춘다.
- **N2 늘린 메모리.** 꽂은 virtio-mem 메모리도 같은 호스트 노드에 묶이므로(`vmem<i>`의 `host-nodes`) 노드마다 센다: `numa::now(hw)`(켤 때 기록 + `plugged_per_node` — 드라이버와 같은 `split_blocks`·`split_plugged` 나눔)를 스케줄러 `host_usage`·할당기(Raft 기록과 `hold_saved`)가 쓴다. 마이그레이션 받는 쪽은 `Shape::Fixed(numa, cpu, plugged)`로 지금 메모리만큼 맞추고 예약하되 기록할 모양은 켤 때 나눔 그대로다(`ram-node<i>` 크기). 실행 중 늘리기는 API가 호스트 노드마다 본다(`numa::grown_fits` — 이 VM을 뺀 빈 몫과 늘린 뒤 모양, hugepages·`strict` 메모리; 안 들면 **409** `no_capacity` "VM … cannot grow its memory to N MiB: node … has …"). 상한까지 미리 잡는 길(vCPU처럼)은 고르지 않았다 — 큰 상한을 둔 VM이 쓰지 않는 메모리로 노드를 막는다. §18 리뷰 1 I3의 "꽂은 메모리는 따로 세지 않는다"는 이것으로 바뀐다.
- **N3 친화 되돌리기를 좁힘.** opld가 뜰 때(예약 CPU로 좁히기 전) 자기 친화를 적어 둔다(`affinity::remember_start` — 관리자의 `CPUAffinity=`·`taskset`·컨테이너 cpuset 그대로). 고정 없는 QEMU·swtpm은 opld가 **스스로** 예약 CPU로 좁혀 있을 때만(`set_narrowed`) 그 친화로 다시 걸고, 아니면 물려받는다. 예약을 빼면 opld도 온라인 전체가 아니라 그 친화로 되돌린다. `repin`은 고정할 것이 없는 VM을 opld가 고정했던 것(`pinning.json`이 있음 — 설정을 지움)만 한 번 풀고(뜰 때의 친화, 모르면 공유 집합) 고정 기록을 지운다(`Hypervisor::forget_pinning`) — 고정한 적 없는 VM은 보지 않는다(설정 없는 노드에서는 아무것도 하지 않음, §12의 "계획 64 필드가 없는 VM은 그대로"). `pin_threads`는 프로세스의 cgroup cpuset(`cpuset.cpus.effective`)으로 자른 친화를 맞은 것으로 본다(`affinity::effective` — 컨테이너·슬라이스에서 회차마다 다시 걸고 경고하지 않음). `pinning.json`은 바뀔 때만 쓴다. `topology::live`의 `affinity_ok`도 예약이 없으면 뜰 때의 친화와 견준다.
- **N4 QEMU 7.1 확인.** `-device virtio-mem-pci,help`에 `prealloc=`이 있으면 `NodeHotplug.memory_prealloc`(거짓이면 쓰지 않음)을 알린다. 없는 노드는 hugepages VM을 메모리 핫플러그 없이 켠다(`hotplug::memory_hotplug`·`start_spec`: 상한 = 메모리, `at_start`가 virtio-mem을 두지 않음) — 메모리 바꾸기는 보류 `unsupported`("this node's QEMU cannot preallocate hugepages for virtio-mem (needs QEMU 7.1)"), 상한 바꾸기 보류는 만들지 않는다. `prealloc`을 조용히 빼지 않는다. 그런 노드는 hugepages + virtio-mem으로 도는 VM을 받지 않는다(`can_receive` → 마이그레이션 409·균형 조정·비우기가 고르지 않음). `start_spec`이 상한을 없애지 않고 메모리로 두는 것은 상한 있는 VM의 직접 목록 검증(아래)이 그대로 맞게 하려는 것이다.
- **N5 채우기로 되돌림.** 순서 키는 (최대 거리, 남는 전용 CPU, 남는 페이지 MiB, 남는 묶인 보통 메모리, 번호)의 **오름차순**(best-fit)이고 VM이 쓰지 않는 자원은 0이다(공유 hugepages VM은 전용 CPU로, hugepages VM은 보통 메모리로 줄 서지 않음). 묶인 메모리가 맞춤의 엄한 조건(리뷰 1 C1)이 된 뒤로는 펴기가 안전에 필요하지 않고, 펴기는 큰 VM이 들 호스트 노드를 조각냈다(호스트 노드 둘 × 1G 8개에서 2 GiB VM 넷 뒤 8 GiB 한 노드 VM이 거절됨). 펴기(메모리 대역)는 나중 할 일로 둔다.
- **N6** 전용·hugepages VM을 다른 폴더로 옮기기(`POST /folders/{id}/members`)는 옮길 폴더에 `vm.dedicated`가 있어야 한다(복제·배포와 같다, 항목별 403 문구도 같음).
- **N7 `preferred`는 부드럽게.** 맞춤은 `preferred` 게스트 노드를 묶이지 않은 몫이 모자라도 거절하지 않고(순서에는 씀), 장부의 `bound_mib`는 `strict`만 센다(`preferred`는 넘치면 커널이 다른 노드에서 준다 — OOM이 아니다). 기록의 셈(`usage`)은 그대로 `preferred`도 세어 `strict` VM이 그 노드의 남은 몫을 정확히 본다.
- **N8** 고치지 않음(기록만): 장부의 받아들이기 함수가 계획 64 빌드 사이에 바뀌었지만(리뷰 1 C1·I2·M6, 리뷰 2 N1·N7·N9) 계획 64는 아직 내보낸 적이 없어 고치기 전 빌드로 도는 클러스터가 없다(svc01·svc02에도 올리지 않았다). 앞으로 `demand`·`exceeded`를 바꾸면 기능 판(`numa2` 등)을 올린다.
- **N9** `any` + 메모리 상한의 2M은 그 노드가 메모리 핫플러그로 켤 때만이다: 스케줄러(`host_usage`·`numa_plan`)와 Raft 장부(노드 상태의 `hotplug`)가 `hotplug::start_spec(spec, caps)`로, 작업자는 켤 하드웨어(`hardware.max_memory_mib() > memory_mib`)로 같은 모양을 본다. virtio-mem이 없거나 미리 잡지 못하는 노드에서는 1G 풀에도 든다.
- **웹 중요 2(서버) 직접 목록은 켤 때 나눔.** 메모리 상한이 있는 VM은 목록 합 ≤ 메모리이고 남는 것이 2 MiB의 배수면 된다(`validate`). 다음 켜기의 게스트 노드 메모리는 목록에 남는 것을 virtio-mem처럼 `split_blocks`로 더한 것이다(`boot_split` — 늘린 만큼이 꽂혀 있던 노드에 그대로 켤 때 메모리가 된다). 보류 `numa`는 `boot_split(목록, hw.memory_mib)`과 기록을 견주므로 실행 중 메모리만 늘리면 생기지 않는다. 상한이 없으면 지금처럼 합 = 메모리다.
- **GitHub 탐침(29d1023, 67ae027f) 브라우저 e2e.** `browser_uploads_image_runs_vm_and_cleans_up`이 메모리 스냅샷에서 QMP가 2분 답하지 않아 실패했다(설정 없는 노드의 보통 VM). 같은 잡의 `snapshots.rs`(실제 QEMU 메모리 스냅샷)는 지나갔고 opld 로그에 `repin`의 경고(다시 고정·실패)도 없어 원인을 로그로 못 박지 못했다. 리뷰 1 I5가 설정 없는 노드의 모든 VM에 손대게 한 두 길(30초마다 `repin`이 보통 VM의 모든 스레드를 읽고 `pinning.json`을 씀, opld 친화가 온라인과 다르면 `pre_exec`로 친화를 바꿈 — fork 경로도 바뀜)을 N3이 없앴다: 설정 없는 노드의 보통 VM은 스테이지 3(통과한 탐침)과 같이 드라이버가 친화·고정 기록을 건드리지 않는다. 시험: `repin_leaves_vms_it_never_pinned_and_releases_cleared_ones_once`(설정 없는 노드에서 `repin` 호출 0), `unpinned_children_get_the_start_affinity_only_when_opld_narrowed_itself`. 이 Mac의 브라우저 e2e(실제 QEMU)와 이 고침 뒤 GitHub 탐침 결과는 실행 기록에 적는다.
