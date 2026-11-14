import type { components } from "@/api/schema";

type Schemas = components["schemas"];

export type Vm = Schemas["Resource_VmSpec_VmStatus"];
export type VmSpec = Schemas["VmSpec"];
export type Volume = Schemas["Resource_VolumeSpec_VolumeStatus"];
export type StoragePool = Schemas["Resource_StoragePoolSpec_StoragePoolStatus"];
export type Network = Schemas["Resource_NetworkSpec_NetworkStatus"];
export type Node = Schemas["Resource_NodeSpec_NodeStatus"];
export type Task = Schemas["Resource_TaskSpec_TaskStatus"];
export type Snapshot = Schemas["Resource_SnapshotSpec_SnapshotStatus"];
export type Backup = Schemas["Resource_BackupSpec_BackupStatus"];
export type BackupJob = Schemas["Resource_BackupJobSpec_BackupJobStatus"];
export type VmImport = Schemas["Resource_VmImportSpec_VmImportStatus"];
export type ImportRun = Schemas["ImportRun"];
export type Weekday = Schemas["Weekday"];
export type SecurityGroup = Schemas["Resource_SecurityGroupSpec_SecurityGroupStatus"];
export type Rule = Schemas["Rule"];
export type Principal = Schemas["Principal"];
export type Role = Schemas["Role"];
export type Settings = Schemas["Resource_SettingsSpec_SettingsStatus"];
export type Channel = Schemas["Channel"];
export type SettingsPatch = Schemas["SettingsPatch"];
export type SubscriptionInfo = Schemas["SubscriptionInfo"];
export type SubscriptionState = Schemas["SubscriptionState"];
export type TelemetryReport = Schemas["TelemetryReport"];
export type AuditEntry = Schemas["AuditEntry"];
export type HaGroup = Schemas["Resource_HaGroupSpec_HaGroupStatus"];
export type PlacementRule = Schemas["Resource_PlacementRuleSpec_PlacementRuleStatus"];
export type RuleKind = Schemas["RuleKind"];
export type VmHa = Schemas["VmHa"];
export type HaState = Schemas["HaState"];
export type ShutdownPolicy = Schemas["ShutdownPolicy"];
export type HaCheck = Schemas["Resource_HaCheckSpec_HaCheckStatus"];
export type HaCheckRole = Schemas["HaCheckRole"];
export type HaCheckAction = Schemas["HaCheckAction"];
export type HaCheckTarget = Schemas["HaCheckTarget"];
export type CheckReport = Schemas["CheckReport"];
export type NodeInterface = Schemas["NodeInterface"];
export type PlacementStatus = Schemas["PlacementStatus"];
export type PlacementSettings = Schemas["PlacementSettings"];
export type PlacementPatch = Schemas["PlacementPatch"];
export type PlacementMode = Schemas["PlacementMode"];
export type PlacementPolicy = Schemas["PlacementPolicy"];
export type PlacementMove = Schemas["PlacementMove"];
export type NodeLoadView = Schemas["NodeLoadView"];

/** watch로 받는 리소스 종류. 이름은 API의 `kinds`와 같다. */
export type WatchKind =
  | "vm"
  | "volume"
  | "storage-pool"
  | "node"
  | "task"
  | "network"
  | "snapshot"
  | "security-group"
  | "backup"
  | "backup-job"
  | "vm-import"
  | "ha-group"
  | "placement-rule";

/** 종류별 리소스 */
export interface Resources {
  vm: Vm;
  volume: Volume;
  "storage-pool": StoragePool;
  node: Node;
  task: Task;
  network: Network;
  snapshot: Snapshot;
  "security-group": SecurityGroup;
  backup: Backup;
  "backup-job": BackupJob;
  "vm-import": VmImport;
  "ha-group": HaGroup;
  "placement-rule": PlacementRule;
}
