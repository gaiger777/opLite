// VM 빌더 모델 시험의 세상(계획 71): 노드 셋(x86_64 둘 — 영역 a·b, aarch64 하나), 공유 풀 ceph와 node-a 로컬 풀,
// 단순·SDN 네트워크, 보안 그룹, ISO, 남는 볼륨, 템플릿 VM, 폴더, 부하.
import type { Network, Node, NodeLoadView, Vm } from "@/api/types";
import { Permissions, type Privilege } from "@/auth/permissions";
import type { CpuModelEntry } from "@/cpu/cpu";
import {
  folder,
  haGroup,
  isoVolume,
  NODE,
  pool,
  poolState,
  securityGroup,
  vm,
  volume,
} from "@/test/fixtures";
import type { BuilderCtx } from "@/vmBuilder/model/state";

const HA = { fencing: "watchdog", recover_after_secs: 60 } as NonNullable<Node["status"]["ha"]>;

function node(id: string, name: string, extra: Partial<Node["status"]>, labels = {}): Node {
  return {
    ...NODE,
    id,
    name,
    labels,
    status: {
      ...NODE.status,
      arch: "x86_64",
      firmware_caps: { tpm: true, secure_boot: true },
      ha: HA,
      qos: true,
      ...extra,
    },
  };
}

export const NODE_A = node(
  "n1",
  "node-a",
  { pools: { ceph: poolState("ceph"), local: poolState("local") } },
  { zone: "a" },
);
export const NODE_B = node(
  "n2",
  "node-b",
  { pools: { ceph: poolState("ceph") }, ha: undefined, firmware_caps: {} },
  { zone: "b" },
);
export const NODE_ARM = node("n3", "node-arm", {
  arch: "aarch64",
  pools: { ceph: poolState("ceph") },
});

export const CEPH = pool("ceph", { backend: { kind: "nfs", server: "nas", export: "/srv/vm" } });
export const LOCAL = pool("local", { nodes: ["n1"] });

export function network(name: string, sdn = false): Network {
  return {
    id: `019a0f3c-0000-7000-8003-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: sdn ? { sdn: {}, subnets: [{ cidr: "10.10.0.0/24" }] } : { bridge: "br0" },
    status: {},
  };
}

export const TEMPLATE: Vm = vm("tmpl", "stopped", {
  spec: {
    ...vm("tmpl", "stopped").spec,
    template: true,
    machine: "q35",
    node: null,
    cpu: { sockets: 1, cores: 2, model: "host" },
    memory_mib: 2048,
    disks: [{ volume: "tmpl-disk0", bus: "virtio-blk", boot_order: 1 }],
    nics: [{ network: "lan", model: "virtio", mac: "52:54:00:00:00:09" }],
  },
});

export const MODELS: CpuModelEntry[] = [
  { name: "host", kind: "basic", usable: 2 },
  { name: "max", kind: "basic", usable: 2 },
  {
    name: "x86-64-v3",
    kind: "level",
    usable: 1,
    nodes: [
      { node: "n1", can_run: true },
      { node: "n2", can_run: false, missing: ["avx2"] },
    ],
  },
  { name: "Icelake-Server", kind: "qemu", usable: 0 },
  { name: "Old", kind: "qemu", usable: 2, deprecated: true },
];

export const LOADS: NodeLoadView[] = [
  {
    node: "n1",
    name: "node-a",
    cpu_use_pct: 10,
    mem_use_pct: 10,
    mem_total_mib: 16384,
    mem_allocated_mib: 4096,
    vms: 1,
  },
  {
    node: "n2",
    name: "node-b",
    cpu_use_pct: 10,
    mem_use_pct: 10,
    mem_total_mib: 8192,
    mem_allocated_mib: 6144,
    vms: 1,
  },
];

/** 권한: 기본은 admin. `only`를 주면 그 권한만 루트에 둔다. */
export function perms(only?: Privilege[]): Permissions {
  return only
    ? new Permissions({
        all: false,
        root: only,
        root_contents: only,
        folders: {},
        contents: {},
        objects: [],
      })
    : Permissions.fromRole("admin");
}

/** 빌더 시험의 세상. 칸을 바꿔 쓴다. */
export function builderCtx(extra: Partial<BuilderCtx> = {}): BuilderCtx {
  return {
    nodes: [NODE_A, NODE_B, NODE_ARM],
    pools: [CEPH, LOCAL],
    volumes: [
      volume("spare", "ceph"),
      volume("used", "ceph", { attached_to: "x" }),
      volume("tmpl-disk0", "ceph", { attached_to: TEMPLATE.id }),
      isoVolume("alpine", "ceph"),
      isoVolume("local-iso", "local"),
    ],
    networks: [network("lan"), network("tenant", true)],
    securityGroups: [securityGroup("web"), securityGroup("ssh")],
    haGroups: [haGroup("pair", 1)],
    folders: [folder("team"), folder("prod", "vm", "team")],
    vms: [vm("taken", "stopped"), TEMPLATE],
    rules: [],
    cpuModels: MODELS,
    loads: LOADS,
    perms: perms(),
    ...extra,
  };
}
