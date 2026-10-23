import type { components } from "@/api/schema";

type Schemas = components["schemas"];

export type Vm = Schemas["Resource_VmSpec_VmStatus"];
export type VmSpec = Schemas["VmSpec"];
export type Volume = Schemas["Resource_VolumeSpec_VolumeStatus"];
export type StoragePool = Schemas["Resource_StoragePoolSpec_StoragePoolStatus"];
export type Network = Schemas["Resource_NetworkSpec_NetworkStatus"];
export type Node = Schemas["Resource_NodeSpec_NodeStatus"];
export type Task = Schemas["Resource_TaskSpec_TaskStatus"];
export type Principal = Schemas["Principal"];
export type Role = Schemas["Role"];

/** watch로 받는 리소스 종류. 이름은 API의 `kinds`와 같다. */
export type WatchKind = "vm" | "volume" | "storage-pool" | "node" | "task" | "network";

/** 종류별 리소스 */
export interface Resources {
  vm: Vm;
  volume: Volume;
  "storage-pool": StoragePool;
  node: Node;
  task: Task;
  network: Network;
}
