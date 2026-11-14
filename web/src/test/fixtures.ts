import type {
  Backup,
  BackupJob,
  HaCheck,
  HaGroup,
  Node,
  PlacementRule,
  PlacementStatus,
  Rule,
  SecurityGroup,
  Snapshot,
  StoragePool,
  Task,
  Vm,
  VmImport,
  Volume,
} from "@/api/types";
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
