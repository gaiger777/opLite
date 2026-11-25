// VMware 이전(계획 72) 화면 시험의 견본: 서버 보기 모양 그대로의 원본·인벤토리·계획·항목.
import type { InvVm, Migration, MigrationItem, SourceInventory, VmwareSource } from "@/vmware/api";

export const VC_FP = Array.from({ length: 32 }, (_, i) =>
  ((i * 7) % 256).toString(16).padStart(2, "0").toUpperCase(),
).join(":");

/** 원본 연결(비밀번호 없는 보기) */
export function vmwareSource(name: string, n = 1, extra: Partial<VmwareSource> = {}): VmwareSource {
  return {
    id: `019a0f3c-0000-7000-8072-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: {
      host: `${name}.example.com`,
      port: 443,
      user: "migr@vsphere.local",
      fingerprint: VC_FP,
    },
    status: { kind: "vcenter", version: "8.0.2", checked_at: 1_793_761_200 },
    ...extra,
  };
}

/** 인벤토리 VM. `folder`는 `DC1/vm/…`, 디스크·NIC는 데이터스토어·네트워크 이름으로 */
export function invVm(
  name: string,
  n: number,
  {
    folder = "DC1/vm/Prod",
    power = "powered-on",
    datastores = ["ds1"],
    networks = ["VM Network"],
    host = "esx1.example.com",
    cluster = "Cluster1",
    ...extra
  }: Partial<InvVm> & {
    datastores?: string[];
    networks?: string[];
  } = {},
): InvVm {
  return {
    moref: `vm-${n}`,
    name,
    path: `${folder}/${name}`,
    folder,
    datacenter: "DC1",
    host: `host-${n}`,
    host_name: host,
    cluster,
    power,
    guest_id: "ubuntu64Guest",
    guest_full_name: "Ubuntu Linux (64-bit)",
    firmware: "bios",
    hw_version: 14,
    cpus: 2,
    cores_per_socket: 1,
    memory_mib: 4096,
    disks: datastores.map((ds, i) => ({
      key: 2000 + i,
      file: `[${ds}] ${name}/${name}${i ? `_${i}` : ""}.vmdk`,
      current_file: `[${ds}] ${name}/${name}${i ? `_${i}` : ""}.vmdk`,
      capacity: 4 * 1024 ** 3,
      mode: "persistent",
      thin: true,
      backing: "flat",
      multi_writer: false,
      datastore: ds,
      controller: "scsi",
      bus: 0,
      unit: i,
    })),
    nics: networks.map((net, i) => ({
      key: 4000 + i,
      network: net,
      adapter: "vmxnet3",
      mac: `00:50:56:00:00:${String(n * 10 + i).padStart(2, "0")}`,
      start_connected: true,
    })),
    snapshots: [],
    cbt: false,
    tools: "guestToolsRunning",
    ft: false,
    template: false,
    guest_ips: { [`00:50:56:00:00:${String(n * 10).padStart(2, "0")}`]: [`10.0.0.${n}`] },
    committed_bytes: 1024 ** 3,
    instance_uuid: `5003-${n}`,
    ...extra,
  };
}

/** 원본 인벤토리 답(`GET /vmware-sources/{key}/inventory`) */
export function inventoryOf(
  vms: InvVm[],
  migrating: SourceInventory["migrating"] = [],
): SourceInventory {
  return {
    fetched_at: 1_793_761_200,
    inventory: {
      datacenters: [{ moref: "datacenter-1", name: "DC1", path: "DC1" }],
      folders: [],
      computes: [],
      hosts: [],
      datastores: [],
      networks: [],
      vms,
    },
    migrating,
  };
}

/** 이전 계획 */
export function migration(
  name: string,
  n = 1,
  extra: { spec?: Partial<Migration["spec"]>; status?: Partial<Migration["status"]> } = {},
): Migration {
  return {
    id: `019a0f3c-0000-7000-8073-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: { source: vmwareSource("vc1").id, mode: "warm", ...extra.spec },
    status: { phase: "running", counts: { syncing: 1 }, ...extra.status },
  };
}

/** 이전 항목 */
export function migrationItem(
  target: string,
  n: number,
  status: Partial<MigrationItem["status"]> = {},
  spec: Partial<MigrationItem["spec"]> = {},
): MigrationItem {
  return {
    id: `019a0f3c-0000-7000-8074-${String(n).padStart(12, "0")}`,
    name: `wave1-${target}`,
    revision: 1,
    labels: {},
    spec: {
      migration: migration("wave1").id,
      source_ref: `vm-${n}`,
      instance_uuid: `5003-${n}`,
      target_name: target,
      ...spec,
    },
    status: {
      state: "syncing",
      cbt_enabled_by_us: true,
      disks: [],
      history: [],
      rounds: 0,
      source_changes: { annotation_added: false, nics_disconnected: [], powered_off_by_us: false },
      sync_failures: 0,
      warnings: [],
      ...status,
    } as MigrationItem["status"],
  };
}
