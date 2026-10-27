import type { Node, StoragePool, Task, Vm, Volume } from "@/api/types";

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
  status: { online: true, cpus: 8, memory_mib: 16384, hypervisor: "qemu-kvm", bridges: [] },
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
    status: { available_bytes: 64 * 1024 ** 3, total_bytes: 128 * 1024 ** 3 },
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
