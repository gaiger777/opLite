import type { Node, Snapshot, StoragePool, Task, Vm, Volume } from "@/api/types";
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
