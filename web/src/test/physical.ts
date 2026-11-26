// 시험용 물리 관계·용량(계획 75). 노드 node-a(id n1): eno1·eno2 → bond0(eno1 끊김, 본딩 저하) → vmbr0(lan) 과 bond0.100 →
// vmbr100(tenant), eno3에 underlay 주소(overlay), eno4는 쓰지 않음. node-b(id n2)는 장애 없음·NUMA 없음.
import type {
  Capacity,
  NetRail,
  NodeCapacity,
  NodePhysical,
  PhysicalPort,
  PhysicalTopology,
} from "@/physical/api";

export const LAN = "019a0f3c-0000-7000-8003-000000000001";
export const TENANT = "019a0f3c-0000-7000-8003-000000000002";
export const OVERLAY = "019a0f3c-0000-7000-8003-000000000003";

const port = (p: Partial<PhysicalPort> & Pick<PhysicalPort, "name">): PhysicalPort => ({
  kind: "ethernet",
  state: "up",
  carrier: true,
  speed_mbps: 10000,
  role: "uplink",
  health: "up",
  ...p,
});

export const NODE_A: NodePhysical = {
  id: "n1",
  node: "node-a",
  online: true,
  ports: [
    port({
      name: "eno1",
      state: "down",
      carrier: false,
      health: "down",
      master: "bond0",
      lldp: { chassis_name: "tor-1", port_id: "Gi1/0/1" },
    }),
    port({ name: "eno2", master: "bond0", lldp: { chassis_name: "tor-2", port_id: "Gi1/0/1" } }),
    port({
      name: "bond0",
      kind: "bond",
      health: "degraded",
      master: "vmbr0",
      bond: {
        mode: "802.3ad",
        state: "degraded",
        members: [
          { name: "eno1", state: "down" },
          { name: "eno2", state: "up", speed_mbps: 10000 },
        ],
      },
    }),
    port({
      name: "bond0.100",
      kind: "vlan",
      health: "degraded",
      parent: "bond0",
      vlan_id: 100,
      master: "vmbr100",
    }),
    port({ name: "eno3", role: "underlay", speed_mbps: 1000 }),
    port({ name: "eno4", role: "unused", state: "down", carrier: false, health: "down" }),
  ],
  bridges: [
    {
      name: "vmbr0",
      uplinks: ["bond0"],
      vlan_filtering: false,
      networks: [LAN],
      vm_nics: 2,
      health: "degraded",
    },
    {
      name: "vmbr100",
      uplinks: ["bond0.100"],
      vlan_filtering: false,
      networks: [TENANT],
      vm_nics: 1,
      health: "degraded",
    },
  ],
  underlay: { address: "10.0.0.1", port: "eno3" },
  impact: [
    {
      port: "eno1",
      bond: "bond0",
      severity: "degraded",
      bridges: ["vmbr0", "vmbr100"],
      networks: [LAN, TENANT],
      vms: 2,
      vm_ids: [],
    },
  ],
};

export const NODE_B: NodePhysical = {
  id: "n2",
  node: "node-b",
  online: true,
  ports: [port({ name: "eth0", master: "vmbr0" })],
  bridges: [
    {
      name: "vmbr0",
      uplinks: ["eth0"],
      vlan_filtering: false,
      networks: [LAN],
      vm_nics: 0,
      health: "up",
    },
  ],
};

export const RAILS: NetRail[] = [
  {
    id: LAN,
    name: "lan",
    kind: "bridge",
    carrier: "vmbr0",
    nodes: [
      { node: "node-a", state: "degraded" },
      { node: "node-b", state: "up" },
    ],
  },
  {
    id: OVERLAY,
    name: "overlay",
    kind: "vxlan",
    carrier: "underlay",
    nodes: [
      { node: "node-a", state: "up" },
      { node: "node-b", state: "absent" },
    ],
  },
  {
    id: TENANT,
    name: "tenant",
    kind: "vlan",
    carrier: "vmbr100",
    vlan: 100,
    nodes: [
      { node: "node-a", state: "degraded" },
      { node: "node-b", state: "absent" },
    ],
  },
];

export const PHYSICAL: PhysicalTopology = { nodes: [NODE_A, NODE_B], networks: RAILS };

const GIB = 1024 ** 3;

/** node-a: 16 스레드(예약 0, 전용 8-15 중 8-9 사용), NUMA 둘, 공유 vCPU 14 → ×2, 로컬 풀 fast(90 % 사용) */
export const CAP_A: NodeCapacity = {
  id: "n1",
  node: "node-a",
  online: true,
  cpu: {
    threads: 16,
    reserved: 1,
    dedicated_total: 8,
    dedicated_used: 2,
    vcpus_allocated: 16,
    vcpus_running: 16,
    vcpus_shared: 14,
    overcommit: 2,
    use_pct: 40,
  },
  memory: {
    total_mib: 65536,
    reserved_mib: 2048,
    allocated_mib: 32768,
    used_mib: 20480,
    overcommit: 0.5,
    hugepages: [{ size_kib: 2048, total: 1024, used: 128 }],
  },
  numa: [
    {
      id: 0,
      cpus: "0-7",
      reserved: "0",
      dedicated: "",
      dedicated_used: "",
      memory_mib: 32768,
      bound_mib: 4096,
      pinned_vcpus: 0,
    },
    {
      id: 1,
      cpus: "8-15",
      reserved: "",
      dedicated: "8-15",
      dedicated_used: "8-9",
      memory_mib: 32768,
      bound_mib: 0,
      pinned_vcpus: 2,
      hugepages: [{ size_kib: 2048, total: 512, used: 128 }],
    },
  ],
  storage: [
    {
      pool: "fast",
      ready: true,
      total_bytes: 100 * GIB,
      used_bytes: 95 * GIB,
      provisioned_bytes: 150 * GIB,
      volumes: 3,
    },
  ],
};

/** node-b: 8 스레드, NUMA 모름, 메모리 ×1.5, 풀 오류 */
export const CAP_B: NodeCapacity = {
  id: "n2",
  node: "node-b",
  online: true,
  cpu: {
    threads: 8,
    reserved: 0,
    dedicated_total: 0,
    dedicated_used: 0,
    vcpus_allocated: 4,
    vcpus_running: 2,
    vcpus_shared: 4,
    overcommit: 0.5,
    use_pct: 10,
  },
  memory: {
    total_mib: 16384,
    reserved_mib: 0,
    allocated_mib: 24576,
    used_mib: 8192,
    overcommit: 1.5,
  },
  storage: [
    {
      pool: "slow",
      ready: false,
      total_bytes: 0,
      used_bytes: 0,
      provisioned_bytes: 0,
      volumes: 0,
    },
  ],
};

/** node-c: 문제 없음 */
export const CAP_C: NodeCapacity = {
  ...CAP_B,
  id: "n3",
  node: "node-c",
  memory: { ...CAP_B.memory, allocated_mib: 4096, overcommit: 0.25 },
  storage: [],
};

export const CAPACITY: Capacity = {
  nodes: [CAP_A, CAP_B, CAP_C],
  shared_pools: [
    {
      pool: "ceph",
      total_bytes: 1000 * GIB,
      used_bytes: 100 * GIB,
      provisioned_bytes: 300 * GIB,
      volumes: 10,
      nodes_ready: 2,
      nodes: 3,
    },
  ],
  totals: {
    nodes: 3,
    cpu: {
      threads: 32,
      reserved: 1,
      dedicated_total: 8,
      dedicated_used: 2,
      vcpus_allocated: 24,
      vcpus_running: 20,
      vcpus_shared: 22,
      overcommit: 22 / 23,
      use_pct: 25,
    },
    memory: {
      total_mib: 98304,
      reserved_mib: 2048,
      allocated_mib: 61440,
      used_mib: 36864,
      overcommit: 0.625,
    },
    storage: {
      total_bytes: 1100 * GIB,
      used_bytes: 195 * GIB,
      provisioned_bytes: 450 * GIB,
      volumes: 13,
    },
  },
};
