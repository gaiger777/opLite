import type {
  Backup,
  BackupJob,
  CephStatusView,
  CephView,
  Folder,
  HaCheck,
  HaGroup,
  HotplugLayout,
  Node,
  NodeDisk,
  PlacementRule,
  PlacementStatus,
  ReplicaStatus,
  Rule,
  RunningHardware,
  SecurityGroup,
  Snapshot,
  StoragePool,
  Task,
  Vm,
  VmImport,
  Volume,
} from "@/api/types";
import type { BackupStore } from "@/backupStores/stores";
import type { RemoteView } from "@/remotes/remotes";
import type { PoolState } from "@/storage/pools";

/** 테스트용 VM. 필요한 필드만 바꿔 쓴다. */
export function vm(name: string, phase: Vm["status"]["phase"], extra: Partial<Vm> = {}): Vm {
  return {
    id: `019a0f3c-0000-7000-8000-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: {
      cpu: { sockets: 1, cores: 2, model: "host" },
      memory_mib: 2048,
      firmware: "uefi",
      machine: "virt",
      disks: [],
      nics: [],
      power: phase === "running" ? "running" : "stopped",
      restart: "never",
      node: "n1",
      ha: false,
    },
    status: {
      phase,
      node: phase === "running" ? "n1" : null,
      pid: null,
      last_error: null,
      snapshot: null,
      schedule_error: null,
    },
    ...extra,
  } satisfies Vm;
}

/**
 * 테스트용 실행 중 하드웨어: 1 × 2코어(최대 8), 2048 MiB(최대 16384), 디스크 하나·NIC 하나로 켠 핫플러그 배치.
 * `layout: null`이면 배치 없이 켠 VM이다.
 */
export function hardware(
  extra: Partial<Omit<RunningHardware, "layout">> & { layout?: Partial<HotplugLayout> | null } = {},
): RunningHardware {
  const { layout, ...rest } = extra;
  return {
    cpu: { sockets: 1, cores: 2, model: "host", max_vcpus: 8 },
    memory_mib: 2048,
    firmware: "uefi",
    machine: "virt",
    ...rest,
    ...(layout !== null && {
      layout: {
        vcpus: 2,
        max_memory_mib: 16384,
        plugged_mib: 0,
        ports: 8,
        boot_disks: 1,
        boot_nics: 1,
        ...layout,
      },
    }),
  };
}

export const NODE: Node = {
  id: "n1",
  name: "node-a",
  revision: 1,
  labels: {},
  spec: { address: "10.0.0.1", maintenance: false },
  status: {
    online: true,
    cpus: 8,
    memory_mib: 16384,
    hypervisor: "qemu-kvm",
    bridges: [],
    arch: null,
    pools: {},
  },
};

/**
 * svc01(aarch64, 계획 64)이 알린 토폴로지의 서버 모양 그대로: CPU 4개·NUMA 노드 하나, SMT·L3 없음, hugepages 예약 없음.
 * 서버는 빈 목록(`siblings`·`l3`·`distances`·`warnings`·노드의 `hugepages`)을 보내지 않는다.
 */
export const SVC01_TOPOLOGY = {
  sockets: 1,
  threads_per_core: 1,
  cpus: "0-3",
  nodes: [{ id: 0, cpus: "0-3", memory_mib: 7940 }],
  page_sizes_kib: [64, 2048, 32768, 1048576],
  pinning: true,
  cpuset: true,
  probed_at: 1795107585,
};

export function task(id: string, kind: Task["spec"]["op"]["kind"], target: string): Task {
  return {
    id,
    name: id,
    revision: 1,
    spec: { op: { kind }, target, node: "n1" },
    status: { state: "pending", error: null },
  } as Task;
}

export function pool(name: string, extra: Partial<StoragePool["spec"]> = {}): StoragePool {
  return {
    id: `019a0f3c-0000-7000-8001-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: { backend: { kind: "dir", path: `/srv/${name}` }, nodes: [], ...extra },
    // 서버는 풀의 용량을 비워 두고, 노드가 풀 상태에 용량을 알린다.
    status: { available_bytes: null, total_bytes: null },
  };
}

/** 노드가 알린 풀 `name`(`pool(name)`)의 상태. 기본은 준비된 128 GiB 풀이다. */
export function poolState(name: string, extra: Partial<PoolState> = {}): PoolState {
  return {
    pool_id: pool(name).id,
    ready: true,
    error: null,
    total_bytes: 128 * 1024 ** 3,
    available_bytes: 64 * 1024 ** 3,
    ...extra,
  };
}

export function volume(name: string, pool: string, extra: Partial<Volume["status"]> = {}): Volume {
  return {
    id: `019a0f3c-0000-7000-8002-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: { pool, size_bytes: 16 * 1024 ** 3, format: "qcow2" },
    status: { phase: "ready", node: "n1", allocated_bytes: 1024 ** 3, attached_to: null, ...extra },
  };
}

/** 테스트용 ISO 이미지 볼륨(`content: iso`, raw, 600 MiB) */
export function isoVolume(
  name: string,
  pool: string,
  extra: Partial<Volume["status"]> = {},
): Volume {
  const v = volume(name, pool, { allocated_bytes: 600 * 1024 ** 2, ...extra });
  return { ...v, spec: { pool, size_bytes: 600 * 1024 ** 2, format: "raw", content: "iso" } };
}

/**
 * 테스트용 설치 VM: 디스크 하나, ISO `alpine`을 넣은 CD-ROM 하나와 빈 드라이브 하나, cloud-init(사용자 `orbi`, 가려진
 * 비밀번호, 키 하나, NIC 0 정적 주소).
 */
export function installVm(name: string, phase: Vm["status"]["phase"], extra: Partial<Vm> = {}): Vm {
  const v = vm(name, phase, extra);
  return {
    ...v,
    spec: {
      ...v.spec,
      disks: [{ volume: `${name}-root`, bus: "virtio-blk", boot_order: null }],
      cdroms: [{ iso: "alpine" }, {}],
      nics: [{ network: "user", model: "virtio", mac: "52:54:00:00:00:01" }],
      cloud_init: {
        user: "orbi",
        password: "********",
        ssh_keys: ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK0 me@laptop"],
        networks: [{ ipv4: { mode: "static", address: "10.0.0.5/24", gateway: "10.0.0.1" } }],
      },
      ...extra.spec,
    },
  };
}

/** 대상 노드 `node`의 복제 상태. 기본은 볼륨 `web-root`(`volume("web-root", …)`)를 담은 준비된 증분 복제본이다. */
export function replica(node: string, extra: Partial<ReplicaStatus> = {}): ReplicaStatus {
  return {
    node,
    state: "ready",
    generation: "019a0f3c-0000-7000-8003-000000000001",
    synced_at: 1_800_000_000,
    volumes: [{ id: volume("web-root", "local").id, size_bytes: 16 * 1024 ** 3 }],
    last_bytes: 12 * 1024 ** 2,
    last_secs: 4,
    last_full: false,
    last_try: 1_800_000_000,
    ...extra,
  };
}

/**
 * 테스트용 복제 VM `web`: 노드 n1의 로컬 dir 풀 `local`에 있는 디스크 `web-root`(`volume("web-root", "local")`)를
 * 노드 n2(node-b)로 15분마다 복제하고, 그 복제본이 준비돼 있다.
 */
export function replicatedVm(
  phase: Vm["status"]["phase"] = "running",
  extra: Partial<Vm> = {},
): Vm {
  const base = vm("web", phase);
  return {
    ...base,
    spec: {
      ...base.spec,
      disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }],
      replication: [{ node: "n2", schedule: "*/15" }],
    },
    status: { ...base.status, replication: [replica("n2")] },
    ...extra,
  };
}

/** 테스트용 스냅샷. `n`은 id 끝자리(UUIDv7처럼 만든 차례)다. 리소스 이름은 서버처럼 id다. */
export function snapshot(
  name: string,
  of: Vm,
  n: number,
  extra: { spec?: Partial<Snapshot["spec"]>; status?: Partial<Snapshot["status"]> } = {},
): Snapshot {
  const id = `019a0f3c-0000-7000-8003-${String(n).padStart(12, "0")}`;
  return {
    id,
    name: id,
    revision: 1,
    labels: {},
    spec: {
      vm: of.id,
      name,
      parent: null,
      memory: false,
      description: "",
      scheduled: false,
      vm_spec: of.spec,
      ...extra.spec,
    },
    status: { phase: "ready", created_at: 1_792_000_000, vmstate_bytes: null, ...extra.status },
  };
}

/** 테스트용 보안 그룹 */
export function securityGroup(
  name: string,
  rules: Rule[] = [],
  description?: string,
): SecurityGroup {
  return {
    id: `019a0f3c-0000-7000-8005-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: { rules, ...(description && { description }) },
    status: {},
  };
}

/** 테스트용 백업. `n`은 id 끝자리(UUIDv7처럼 만든 차례)다. */
export function backup(
  name: string,
  of: Vm,
  n: number,
  extra: { spec?: Partial<Backup["spec"]>; status?: Partial<Backup["status"]> } = {},
): Backup {
  return {
    id: `019a0f3c-0000-7000-8006-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: { vm: of.id, vm_name: of.name, pool: "bk", vm_spec: of.spec, ...extra.spec },
    status: {
      phase: "ready",
      created_at: 1_793_761_200,
      disks: [
        {
          name: "disk0",
          file: "disk0.qcow2",
          virtual_bytes: 16 * 1024 ** 3,
          stored_bytes: 1536 * 1024 ** 2,
        },
      ],
      uefi_vars: true,
      error: null,
      ...extra.status,
    },
  };
}

/** 테스트용 백업 작업: 매일 03:00(UTC)에 모든 VM을 `bk`에 백업하고 최신 3개를 남긴다. */
export function backupJob(
  name: string,
  n: number,
  extra: { spec?: Partial<BackupJob["spec"]>; status?: Partial<BackupJob["status"]> } = {},
): BackupJob {
  return {
    id: `019a0f3c-0000-7000-8007-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: {
      vms: { all: true },
      pool: "bk",
      schedule: { at: "03:00", days: [] },
      keep: { last: 3, daily: 0, weekly: 0, monthly: 0 },
      enabled: true,
      ...extra.spec,
    },
    status: { next_run: 1_793_761_200, run: null, last_run: null, errors: [], ...extra.status },
  };
}

/** 테스트용 HA 그룹: node-a(우선순위 2)와 node-b(1) */
export function haGroup(name: string, n: number, spec: Partial<HaGroup["spec"]> = {}): HaGroup {
  return {
    id: `019a0f3c-0000-7000-8009-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: {
      nodes: [
        { node: "node-a", priority: 2 },
        { node: "node-b", priority: 1 },
      ],
      restricted: false,
      nofailback: false,
      ...spec,
    },
    status: {},
  };
}

/** 테스트용 HA 검사: `storage` 역할의 링크 검사 `bond1`(서버처럼 기본값은 쓰지 않는다) */
export function haCheck(name: string, n: number, spec: Partial<HaCheck["spec"]> = {}): HaCheck {
  return {
    id: `019a0f3c-0000-7000-800b-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: { role: "storage", target: { kind: "link", iface: "bond1" }, ...spec } as HaCheck["spec"],
    status: {},
  };
}

/**
 * 인터페이스와 HA 검사 결과를 보고한 노드: 본딩 bond0(eno1 정상, eno2 끊김)과 그 위의 브리지 vmbr0, 실패한
 * 검사 `stor`, degraded 검사 `lan`, 통과한 자동 검사 `cluster-link-1`
 */
export const NET_NODE: Node = {
  ...NODE,
  status: {
    ...NODE.status,
    interfaces: [
      {
        name: "bond0",
        kind: "bond",
        state: "up",
        carrier: true,
        speed_mbps: 10000,
        mac: "52:54:00:00:00:01",
        master: "vmbr0",
        bond: {
          mode: "802.3ad",
          state: "degraded",
          ports: [
            { name: "eno1", state: "up", speed_mbps: 10000, aggregator: 1 },
            { name: "eno2", state: "down", aggregator: 2 },
          ],
        },
      },
      { name: "eno1", kind: "ethernet", state: "up", carrier: true, master: "bond0" },
      { name: "eno2", kind: "ethernet", state: "down", carrier: false, master: "bond0" },
      {
        name: "vmbr0",
        kind: "bridge",
        state: "up",
        carrier: true,
        addresses: ["10.0.0.1/24"],
      },
    ],
    checks: [
      { name: "cluster-link-1", state: "ok" },
      { name: "lan", state: "degraded", error: "bond0: eno2 down", since: 1_793_761_200 },
      { name: "stor", state: "failing", error: "nfs pool is not mounted", since: 1_793_761_200 },
    ],
  },
};

/** 테스트용 배치 규칙: 켜진 `anti-affinity`(VM web1·web2) */
export function placementRule(
  name: string,
  n: number,
  spec: Partial<PlacementRule["spec"]> = {},
): PlacementRule {
  return {
    id: `019a0f3c-0000-7000-800a-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: { kind: "anti-affinity", vms: ["web1", "web2"], enabled: true, ...spec },
    status: {},
  };
}

/**
 * 테스트용 VM 가져오기: 노드 n1에 올린 `web01.ova`. 원본은 vSphere의 Linux VM(2 vCPU·소켓당 코어 2,
 * 1 GiB, UEFI, SCSI 디스크 하나, "VM Network"의 vmxnet3 NIC 하나)이고, 제안은 `propose` 결과다.
 */
export function vmImport(
  name: string,
  n: number,
  status: Partial<VmImport["status"]> = {},
): VmImport {
  return {
    id: `019a0f3c-0000-7000-8008-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: { node: "n1", source: { ova: { file_name: `${name}.ova`, size_bytes: 3 * 1024 ** 3 } } },
    status: {
      phase: "ready",
      source_vm: {
        name: "Web 01",
        guest_os: "ubuntu64Guest",
        firmware: "uefi",
        vcpus: 2,
        cores_per_socket: 2,
        memory_mib: 1024,
        disks: [
          {
            name: "disk0",
            file: "web01-disk1.vmdk",
            capacity_bytes: 16 * 1024 ** 3,
            controller: "scsi",
          },
        ],
        nics: [{ network: "VM Network", adapter: "vmxnet3", mac: "00:50:56:01:02:03" }],
      },
      proposal: {
        sockets: 1,
        cores: 2,
        memory_mib: 1024,
        firmware: "uefi",
        disk_bus: "virtio-blk",
        nic_model: "virtio",
        networks: { "VM Network": null },
      },
      warnings: [
        "UEFI boot entries (NVRAM) are not imported; the guest boots from its default boot path",
      ],
      ...status,
    },
  };
}

/** ESXi에서 만든 가져오기. 원본 VM과 제안은 `vmImport`와 같다. */
export function esxiImport(
  name: string,
  n: number,
  status: Partial<VmImport["status"]> = {},
): VmImport {
  const imp = vmImport(name, n, status);
  return {
    ...imp,
    spec: {
      node: "n1",
      source: {
        esxi: {
          host: "esxi1.lab",
          port: 443,
          datacenter: "ha-datacenter",
          datastore: "datastore1",
          vmx: `${name}/${name}.vmx`,
          user: "root",
          fingerprint: FINGERPRINT,
        },
      },
    },
  };
}

/** 가짜 ESXi 인증서 지문 */
export const FINGERPRINT = Array.from({ length: 32 }, (_, i) =>
  (i * 7 + 16).toString(16).toUpperCase().padStart(2, "0"),
).join(":");

/** 테스트용 원격 클러스터. 기본은 닿는 한 노드 클러스터다. */
export function remote(name: string, status: Partial<RemoteView["status"]> = {}): RemoteView {
  return {
    id: `019a0f3c-0000-7000-8002-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    url: `https://${name}.example:8006`,
    ca_fingerprint: `sha256:${"ab".repeat(32)}`,
    token_id: "0192f0c1-5d2e-7000-8000-000000000000",
    status: {
      reachable: true,
      checked_at: 1700000000,
      error: null,
      cluster_name: name,
      version: "0.37.0",
      nodes: { total: 1, online: 1 },
      vms: { total: 2, running: 1 },
      cpus: 8,
      memory_gib: 32,
      ...status,
    },
  };
}

/**
 * 최적 배치 상태: recommend 모드(서버처럼 기본 설정은 쓰지 않음), 바쁜 node-a(요약 3초 전)와 요약 없는 node-b,
 * node-a의 web1을 node-b로 옮기는 균형 추천과 규칙 `only-b`를 바로잡는 db1 추천
 */
export const PLACEMENT: PlacementStatus = {
  leader: "n1",
  settings: {} as PlacementStatus["settings"],
  last_run: 1_793_761_200,
  next_run: 1_793_761_500,
  imbalance: { cpu_pct: 182, mem_pct: 238, total_pct: 210 },
  nodes: [
    {
      node: "n1",
      name: "node-a",
      cpu1_pct: 905,
      cpu5_pct: 880,
      mem_used_mib: 12288,
      mem_allocated_mib: 10240,
      mem_total_mib: 16384,
      cpu_use_pct: 880,
      mem_use_pct: 750,
      vms: 5,
      age_secs: 3,
    },
    {
      node: "n2",
      name: "node-b",
      mem_allocated_mib: 2048,
      mem_total_mib: 16384,
      cpu_use_pct: 880,
      mem_use_pct: 125,
      vms: 1,
    },
  ],
  recommendations: [
    {
      id: "019a0f3c-0000-7000-800c-000000000001",
      vm: "019a0f3c-0000-7000-8000-00000000web1",
      vm_name: "web1",
      from: "n1",
      from_name: "node-a",
      to: "n2",
      to_name: "node-b",
      reason: "balance",
      gain_pct: 85,
      cost_mib: 4096,
    },
    {
      id: "019a0f3c-0000-7000-800c-000000000002",
      vm: "019a0f3c-0000-7000-8000-000000000db1",
      vm_name: "db1",
      from: "n1",
      from_name: "node-a",
      to: "n2",
      to_name: "node-b",
      reason: "rule",
      gain_pct: 0,
      cost_mib: 2048,
      rules: ["only-b"],
    },
  ],
};

/** 관리 Ceph: node-a(n1)·node-b(n2)·node-c(n3)에 mon, n1·n2에 mgr, n1·n2에 OSD */
export const CEPH: CephView = {
  fsid: "5f0c1c2e-4d7a-4b55-9a51-1f0e6c9b7a01",
  public_network: "10.0.0.0/24",
  phase: "ready",
  mons: [
    { node: "n1", name: "node-a", addr: "10.0.0.1" },
    { node: "n2", name: "node-b", addr: "10.0.0.2" },
    { node: "n3", name: "node-c", addr: "10.0.0.3" },
  ],
  mgrs: ["n1", "n2"],
  osd_nodes: ["n1", "n2"],
  release: "19.2.3 squid",
  init_node: "n1",
  managed_conf: "/etc/opl/ceph/ceph.conf",
  tasks: [],
};

/** Ceph 상태: HEALTH_WARN(검사 둘, 하나는 muted), OSD 넷(하나 down, 하나 out), 풀 둘 */
export const CEPH_STATUS: CephStatusView = {
  health: {
    status: "HEALTH_WARN",
    checks: [
      {
        code: "OSD_DOWN",
        severity: "HEALTH_WARN",
        summary: "1 osds down",
        count: 1,
        muted: false,
      },
      {
        code: "MON_DOWN",
        severity: "HEALTH_WARN",
        summary: "1/3 mons down",
        count: 1,
        muted: true,
      },
    ],
  },
  mons: [
    { name: "node-a", addr: "10.0.0.1:3300", in_quorum: true },
    { name: "node-b", addr: "10.0.0.2:3300", in_quorum: true },
    { name: "node-c", addr: "10.0.0.3:3300", in_quorum: false },
  ],
  quorum: ["node-a", "node-b"],
  mgr: { active: "node-a", standbys: ["node-b"], available: true },
  osds: [
    {
      id: 2,
      host: "node-b",
      up: true,
      in: true,
      class: "hdd",
      weight: 0.0195,
      device: "/dev/sdb",
      total_bytes: 21474836480,
      used_bytes: 1073741824,
    },
    {
      id: 0,
      host: "node-a",
      up: true,
      in: true,
      class: "ssd",
      weight: 0.0195,
      device: "/dev/sdb",
      total_bytes: 21474836480,
      used_bytes: 2147483648,
    },
    {
      id: 1,
      host: "node-a",
      up: false,
      in: true,
      class: "ssd",
      weight: 0.0195,
      device: "/dev/sdc",
      total_bytes: 21474836480,
      used_bytes: 0,
    },
    { id: 3, host: "node-b", up: true, in: false, weight: 0.0195, total_bytes: 0, used_bytes: 0 },
  ],
  osd_hosts_up: 2,
  pgs: {
    total: 129,
    states: [
      ["active+undersized", 9],
      ["active+clean", 120],
    ],
  },
  usage: { total_bytes: 85899345920, used_bytes: 3221225472, available_bytes: 82678120448 },
  pools: [
    {
      name: ".mgr",
      id: 1,
      size: 2,
      stored_bytes: 1048576,
      max_available_bytes: 26843545600,
      percent_used: 0,
    },
    {
      name: "vmpool",
      id: 2,
      size: 2,
      stored_bytes: 1073741824,
      max_available_bytes: 26843545600,
      percent_used: 0.0384,
      orbivisor_pool: "vms",
    },
  ],
  read_at: 1_793_761_200,
  via: "n1",
};

/** 노드의 디스크: 빈 디스크, 지워야 하는 디스크, 시스템 디스크 */
export const DISKS: NodeDisk[] = [
  {
    path: "/dev/sda",
    size_bytes: 128849018880,
    rotational: false,
    model: "Samsung SSD",
    state: "in-use",
    reason: "system disk (/ is on it)",
  },
  {
    path: "/dev/sdb",
    by_id: "/dev/disk/by-id/wwn-0x5000c500a1b2c3d4",
    size_bytes: 21474836480,
    rotational: true,
    model: "ST2000",
    serial: "Z1X2",
    state: "available",
  },
  {
    path: "/dev/sdc",
    size_bytes: 21474836480,
    rotational: true,
    state: "wipeable",
    reasons: ["gpt partition table", "ext4 filesystem"],
  },
];

/** 테스트용 지표 기록 응답: 칸마다 같은 세 점(AVG·MAX·MIN)과 요약. `approx`면 근사 백분위다. */
export function history(fields: string[], { approx = false } = {}) {
  const column = { avg: [10, null, 30], max: [15, null, 40], min: [5, null, 20] };
  return {
    tier: approx ? "min1" : "raw5s",
    step_ms: 5_000,
    start: 1_800_000_000_000,
    columns: Object.fromEntries(fields.map((f) => [f, column])),
    stats: Object.fromEntries(fields.map((f) => [f, { avg: 20, max: 40, min: 5 }])),
    pct: Object.fromEntries(fields.map((f) => [f, { p50: 20, p95: 38, approx }])),
  };
}

export const NODE_FIELDS = [
  "cpu",
  "mem_used",
  "mem_total",
  "disk_read",
  "disk_write",
  "net_rx",
  "net_tx",
  "fs_used",
  "fs_total",
];

/** 테스트용 폴더(계획 62). id는 이름에서 만든다. */
export function folder(
  name: string,
  kind: Folder["spec"]["kind"] = "vm",
  parent?: string,
  extra: Partial<Folder["spec"]> = {},
): Folder {
  return {
    id: folderId(name),
    name: folderId(name),
    revision: 1,
    labels: {},
    spec: { name, kind, ...(parent && { parent: folderId(parent) }), ...extra },
    status: {},
  };
}

export function folderId(name: string): string {
  return `019a0f3c-0000-7000-8009-${name.padStart(12, "0").slice(-12)}`;
}

/**
 * SDN(계획 65) 시험의 노드: `NODE`와 같은 Linux 노드에 id·이름·underlay 주소(`10.0.0.<n>`)를 주고 `sdn`이 있으면
 * `NodeStatus.sdn`(서버 `NodeSdn` 모양 — 빈 칸은 빠진다)을 둔다.
 */
export function sdnNode(
  n: number,
  sdn?: Node["status"]["sdn"],
  extra: Partial<Node["status"]> = {},
): Node {
  return {
    ...NODE,
    id: `n${n}`,
    name: `node-${String.fromCharCode(96 + n)}`,
    spec: { ...NODE.spec, address: `10.0.0.${n}:8006` },
    status: { ...NODE.status, underlay: `10.0.0.${n}`, ...(sdn && { sdn }), ...extra },
  };
}

/** 준비된 chassis의 `NodeSdn`(서버가 보내는 칸 차례). `central`이면 NB·SB 역할과 northd */
export function readyChassis(
  n: number,
  central?: "leader" | "follower",
): NonNullable<Node["status"]["sdn"]> {
  return {
    chassis: "ready",
    ovn_version: "24.03.8",
    ovs_version: "3.3.9",
    encap_ip: `10.0.0.${n}`,
    ...(central && { central: { nb: central, sb: central, northd_active: central === "leader" } }),
  };
}

/**
 * 서버 `SdnView::new(Some(sdn), nodes, tasks)`의 모양: 준비된 SDN, `centrals`의 노드가 중앙, `nodes`마다 chassis 행(이름
 * 순, `sdn`은 노드의 것 그대로). 빈 칸(`last_error` 등)은 서버처럼 빠진다.
 */
export function sdnView(centrals: Node[], nodes: Node[], extra: Record<string, unknown> = {}) {
  return {
    phase: "ready",
    centrals: centrals.map((n) => ({
      node: n.id,
      name: n.name,
      address: n.status.underlay!,
      ...(n.status.sdn?.central && { health: n.status.sdn.central }),
    })),
    release: "24.03.8",
    init_node: centrals[0]?.id,
    sync: { term: 3, index: 120, at: 1795107585, nb_cfg: 7, hv_cfg: 7 },
    chassis: [...nodes]
      .filter((n) => !n.spec.witness)
      .sort((a, b) => a.name.localeCompare(b.name))
      .map((n) => ({ node: n.id, name: n.name, ...(n.status.sdn && { sdn: n.status.sdn }) })),
    tasks: [],
    ...extra,
  };
}

/** SDN 네트워크(서버 모양: `sdn: {}`, 빈 `mtu`·`subnets`·`labels`는 빠진다) */
export function sdnNetwork(name: string, subnets: Record<string, unknown>[] = [], mtu?: number) {
  return {
    id: `019a0f3c-0000-7000-8003-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 4,
    spec: {
      sdn: {},
      ...(mtu !== undefined && { mtu }),
      ...(subnets.length > 0 && { subnets }),
    },
    status: {},
  } as unknown as import("@/api/types").Network;
}

/** SDN Task(서버 `create_exclusive`·`create_task`의 응답 모양 — 요청한 사용자 레이블 포함) */
export function sdnTask(
  id: string,
  kind: "sdn-init" | "sdn-central-add" | "sdn-central-remove" | "sdn-chassis-install",
  node: string,
  status: Partial<Task["status"]> = {},
): Task {
  return {
    id,
    name: id,
    revision: 2,
    labels: { "requested-by": "admin" },
    spec: { op: { kind }, target: kind === "sdn-chassis-install" ? node : "sdn-0001", node },
    status: { state: "pending", error: null, ...status },
  } as Task;
}

/**
 * 공급자(외부) 네트워크(계획 66, 서버 모양: `sdn.provider`의 `vlan`은 flat이면 빠지고, 빈 `mtu`·`subnets`·`labels`도 빠진다)
 */
export function providerNetwork(
  name: string,
  physnet: string,
  subnets: Record<string, unknown>[] = [],
  vlan?: number,
) {
  return {
    id: `019a0f3c-0000-7000-8003-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 5,
    spec: {
      sdn: { provider: { physnet, ...(vlan !== undefined && { vlan }) } },
      ...(subnets.length > 0 && { subnets }),
    },
    status: {},
  } as unknown as import("@/api/types").Network;
}

/**
 * `GET /routers`의 항목(서버 `RouterView`): 리소스를 평평하게 펼치고(빈 `labels`·`interfaces`·`routes`·`external.ips`는
 * 빠진다) `gateways`·`gateway_error`·`hosting`·`live_error`·`upstream_routes`는 있을 때만이다. `status`는 늘 `{}`.
 */
export function routerView(
  name: string,
  spec: Record<string, unknown> = {},
  view: Record<string, unknown> = {},
) {
  return {
    id: `019a0f3c-0000-7000-800a-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 7,
    spec,
    status: {},
    ...view,
  } as unknown as import("@/routing/routing").Router;
}

/** `GET /floating-ips`의 항목(서버 `FloatingIpView`): 리소스 + `state`(늘) + `vm_name`(읽을 수 있을 때만) */
export function floatingIpView(
  address: string,
  network: string,
  state: Record<string, unknown> = { kind: "unattached" },
  extra: { target?: { vm: string; mac: string }; vm_name?: string; folder?: string } = {},
) {
  const name = address.replaceAll(".", "-");
  return {
    id: `019a0f3c-0000-7000-800b-${name.replaceAll("-", "").padStart(12, "0").slice(-12)}`,
    name,
    revision: 8,
    spec: {
      network,
      address,
      ...(extra.target && { target: extra.target }),
      ...(extra.folder && { folder: extra.folder }),
    },
    status: {},
    state,
    ...(extra.vm_name && { vm_name: extra.vm_name }),
  } as unknown as import("@/routing/routing").FloatingIp;
}

/**
 * `GET /load-balancers`의 항목(서버 `LoadBalancerView`, 계획 67): 리소스를 평평하게 펼치고(빈 `labels`·`monitors`·리스너
 * `members`는 빠진다, `status`는 늘 `{}`) `vip_state`·`members`는 늘, `external_state`(바깥 VIP가 있을 때)·`live_error`는
 * 있을 때만이다. 멤버 줄의 `vm`·`vm_name`·`mac`·`ip`·`reason`·`warning`도 있을 때만이다.
 */
export function lbView(
  name: string,
  spec: Record<string, unknown>,
  view: Record<string, unknown> = {},
) {
  return {
    id: `019a0f3c-0000-7000-800c-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 9,
    spec,
    status: {},
    vip_state: { state: "ready" },
    members: [],
    ...view,
  } as unknown as import("@/lb/lb").LoadBalancer;
}

/**
 * 네트워크 옮기기 리소스(서버 `Resource<NetworkMigrationSpec, NetworkMigrationStatus>`): 스펙의 빈 칸(`vms`·`mode` auto·
 * `addresses`·`bridge`·`bridge_node`·`allow_mtu_shrink` 거짓·`parallel`)은 빠지고, 상태의 `phase`는 늘, `vms`·`bridge`·
 * `task`·`error`는 있을 때만이다. `VmMove`의 `state`는 늘(`ips`·`error`·`at`는 있을 때만, `from.security_groups`·`cloud_init`도).
 */
export function migrationRes(
  name: string,
  spec: Record<string, unknown>,
  status: Record<string, unknown> = { phase: "planned" },
) {
  return {
    id: `019a0f3c-0000-7000-800d-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 11,
    spec,
    status,
  } as unknown as import("@/migration/migration").Migration;
}

/**
 * 테스트용 백업 저장소(계획 68, 서버 `BackupStoreView` — 비밀이 빠진 보기). 기본은 백업 풀 `bk` 안의 준비된 평문 저장소이고
 * 매일 05:00 GC, 일요일 06:00 검증이다.
 */
export function backupStore(
  name: string,
  n: number,
  extra: { spec?: Partial<BackupStore["spec"]>; status?: Partial<BackupStore["status"]> } = {},
): BackupStore {
  return {
    id: `019a0f3c-0000-7000-8068-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: {
      backend: { pool: "bk" },
      gc: { at: "05:00", days: [] },
      verify: { schedule: { at: "06:00", days: ["sun"] }, reverify_days: 30, mode: "full" },
      max_streams: 8,
      ...extra.spec,
    },
    status: { phase: "ready", ...extra.status },
  };
}

/** 저장소 `store`에 든 테스트용 백업(풀이 없고 청크 통계가 있다) */
export function storeBackup(
  name: string,
  of: Vm,
  n: number,
  store: string,
  extra: { spec?: Partial<Backup["spec"]>; status?: Partial<Backup["status"]> } = {},
): Backup {
  const b = backup(name, of, n, extra);
  const spec = { ...b.spec, store, ...extra.spec };
  delete spec.pool;
  return {
    ...b,
    spec,
    status: {
      ...b.status,
      disks: [
        { name: "disk0", file: "disk0.fidx", virtual_bytes: 16 * 1024 ** 3, stored_bytes: 0 },
      ],
      store_stats: {
        logical_bytes: 16 * 1024 ** 3,
        read_bytes: 16 * 1024 ** 3,
        new_bytes: 300 * 1024 ** 2,
        new_chunks: 75,
        reused_chunks: 10,
        zero_chunks: 4011,
      },
      ...extra.status,
    },
  };
}
