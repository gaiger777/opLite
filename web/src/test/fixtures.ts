import type {
  Backup,
  BackupJob,
  Node,
  Rule,
  SecurityGroup,
  Snapshot,
  StoragePool,
  Task,
  Vm,
  Volume,
} from "@/api/types";
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
