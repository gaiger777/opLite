import type { components } from "@/api/schema";
import type { Node, NicSpec, Vm, VmSpec } from "@/api/types";
import { emptyNuma, numaErrors, numaFrom, numaSpec, type NumaValues } from "@/numa/numa";
import { MAX_NIC_SECURITY_GROUPS } from "@/security-groups/rules";
import { formatIps, parseIps } from "@/sdn/ips";
import { effectiveBoot, MAX_BOOT_ORDER } from "@/vms/boot";
import {
  cloudInitErrors,
  cloudInitFrom,
  type CloudInitValues,
  emptyCloudInit,
  toCloudInit,
} from "@/vms/cloudInit";
import {
  DEFAULT_GUEST_WATCH_SECS,
  DEFAULT_HA,
  haPolicy,
  haSetting,
  MAX_GUEST_WATCH_SECS,
  MAX_HA_COUNT,
  MAX_HA_PRIORITY,
  MIN_GUEST_WATCH_SECS,
} from "@/vms/ha";
import { MAX_REPLICATION_MINUTES, MAX_REPLICATION_TARGETS } from "@/vms/replication";

type Schemas = components["schemas"];

/** 서버의 `MIN_MEMORY_MIB` */
export const MIN_MEMORY_MIB = 64;
/** 서버의 `MAX_NICS` */
export const MAX_NICS = 16;
/** 서버의 `MAX_VCPUS`(핫플러그 vCPU 상한의 최댓값) */
export const MAX_VCPUS = 256;
/** 서버의 `MAX_HOTPLUG_MEMORY_MIB`(512 GiB) */
export const MAX_HOTPLUG_MEMORY_MIB = 512 * 1024;
/** 서버의 `MEMORY_HOTPLUG_ALIGN_MIB`: 메모리 상한이 있으면 메모리와 상한이 이 배수여야 한다. */
export const MEMORY_HOTPLUG_ALIGN_MIB = 128;
/** 서버의 `MAX_CDROMS` */
export const MAX_CDROMS = 4;
/** 서버의 `Q35_SATA_PORTS`: q35 내장 AHCI 포트를 SATA 디스크·CD-ROM·cloud-init 드라이브가 나눠 쓴다. */
export const Q35_SATA_PORTS = 6;
/** 서버의 `cores`(u16)의 최댓값 */
const MAX_CORES = 65535;
/** 서버의 `MAX_SCHEDULE_KEEP` */
const MAX_SCHEDULE_KEEP = 100;
/** 서버의 리소스 이름 규칙: 소문자·숫자·`-`, 63자까지, `-`로 시작하거나 끝나지 않는다. */
const NAME = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const MAC = /^[0-9a-f]{2}(:[0-9a-f]{2}){5}$/i;
/** `/volumes/import`·`/volumes/download` 경로와 겹쳐 서버가 볼륨 이름으로 받지 않는다. */
export const RESERVED_VOLUMES: readonly string[] = ["import", "download"];
const GIB = 1024 ** 3;

export interface DiskRow {
  /** 있는 볼륨을 붙이거나, 만들기 전에 새 빈 볼륨을 만든다. */
  kind: "existing" | "new";
  volume: string;
  pool: string;
  sizeGib: string;
  bus: Schemas["DiskBus"];
  bootOrder: string;
}

/** CD-ROM 드라이브 하나(`spec.cdroms`). */
export interface CdromRow {
  /** 넣은 ISO 볼륨 이름. 비면 빈 드라이브 */
  iso: string;
  /** 비면 디스크 뒤(`effectiveBoot`) */
  bootOrder: string;
}

export interface NicRow {
  network: string;
  model: Schemas["NicModel"];
  mac: string;
  /** 거는 보안 그룹 이름. `user` 네트워크와 SDN 네트워크(계획 65)는 쓸 수 없다. */
  securityGroups: string[];
  /**
   * SDN 네트워크의 주소(계획 65, 쉼표로 나눈 IPv4·IPv6 하나씩). 비면 자동이다: 새 NIC는 서브넷에서 받고, 있던 NIC는
   * 서버가 지금 주소를 그대로 둔다.
   */
  ips: string;
  /** 속도 제한·DSCP(계획 67, 글 입력 그대로). 없거나 모두 비면 보내지 않는다. */
  qos?: QosRow;
}

/** NIC QoS 입력: 보내기·받기 Mbit/s, 버스트 kbit, DSCP(SDN NIC만) */
export interface QosRow {
  out: string;
  in: string;
  burst: string;
  dscp: string;
}

/** 서버 `NicQos::validate`의 범위 */
export const MAX_QOS_MBPS = 100_000;
export const MAX_QOS_BURST_KBIT = 10_000_000;
export const MAX_DSCP = 63;

export function qosRow(q: NicSpec["qos"]): QosRow | undefined {
  return q
    ? {
        out: q.out_mbps?.toString() ?? "",
        in: q.in_mbps?.toString() ?? "",
        burst: q.burst_kbit?.toString() ?? "",
        dscp: q.dscp?.toString() ?? "",
      }
    : undefined;
}

/** 입력이 하나라도 있는지(모두 비면 QoS 없음) */
export function hasQos(q: QosRow | undefined): q is QosRow {
  return !!q && [q.out, q.in, q.burst, q.dscp].some((v) => v.trim() !== "");
}

/**
 * QoS 입력의 오류(칸 → 번역 키): 범위, 속도 없이 버스트만(`BurstWithoutRate`), `user` 네트워크(tap이 없다 — 400
 * `invalid_qos`), 단순 NIC의 DSCP(400 `qos_dscp_needs_sdn`).
 */
export function qosErrors(
  q: QosRow | undefined,
  network: string,
  sdn: boolean,
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!hasQos(q)) {
    return errors;
  }
  const check = (key: keyof QosRow, min: number, max: number) => {
    const v = q[key].trim();
    if (v !== "" && !(/^\d+$/.test(v) && intIn(v, min, max))) {
      errors[key] = `vmForm.errors.qos.${key}`;
    }
  };
  check("out", 1, MAX_QOS_MBPS);
  check("in", 1, MAX_QOS_MBPS);
  check("burst", 1, MAX_QOS_BURST_KBIT);
  check("dscp", 0, MAX_DSCP);
  if (q.burst.trim() !== "" && q.out.trim() === "" && q.in.trim() === "") {
    errors.burst = "vmForm.errors.qos.burstWithoutRate";
  }
  if (q.dscp.trim() !== "" && !sdn) {
    errors.dscp = "vmForm.errors.qos.dscpNeedsSdn";
  }
  if (network === "user") {
    errors.out = "vmForm.errors.qos.user";
  }
  return errors;
}

/** 보낼 `qos`(모두 비면 없음) */
export function toQos(q: QosRow | undefined): NicSpec["qos"] {
  if (!hasQos(q)) {
    return undefined;
  }
  const n = (v: string) => (v.trim() === "" ? undefined : Number(v.trim()));
  return {
    ...(n(q.out) !== undefined && { out_mbps: n(q.out) }),
    ...(n(q.in) !== undefined && { in_mbps: n(q.in) }),
    ...(n(q.burst) !== undefined && { burst_kbit: n(q.burst) }),
    ...(n(q.dscp) !== undefined && { dscp: n(q.dscp) }),
  };
}

/** 예약 스냅샷 입력. 끄더라도 보관 개수와 메모리는 남겨 두어 다시 켤 때 쓴다. */
export interface ScheduleRow {
  every: Schemas["ScheduleEvery"] | "off";
  keep: string;
  memory: boolean;
}

/** 복제 대상 하나(`spec.replication`). 주기는 `*\/N`의 N(분)이다. */
export interface ReplicationRow {
  /** 대상 노드 id. 비면 아직 고르지 않음 */
  node: string;
  minutes: string;
  /** 대상 풀. 비면 디스크마다 같은 이름의 풀 */
  pool: string;
}

export interface VmFormValues {
  name: string;
  sockets: number;
  cores: string;
  model: Schemas["CpuModel"];
  /** CPU 플래그(`-avx512vbmi` 숨기기, `+aes` 켜기, 계획 63) */
  cpuFlags: string[];
  memory: string;
  /** 실행 중 늘릴 수 있는 vCPU 상한(`cpu.max_vcpus`). 비면 없음 */
  maxVcpus: string;
  /** 실행 중 늘릴 수 있는 메모리 상한(MiB, `max_memory_mib`). 비면 없음 */
  maxMemory: string;
  /** CPU 배치와 NUMA(계획 64): 정책·스레드·에뮬레이터·vNUMA·hugepages */
  numa: NumaValues;
  firmware: Schemas["Firmware"];
  /** 빈 문자열이면 서버가 클러스터 아키텍처의 기본값을 고른다(노드 목록을 못 읽는 폴더 사용자, 계획 62). */
  machine: Schemas["MachineType"] | "";
  /** VM 폴더 id. 빈 문자열이면 루트다(계획 62). 고칠 때는 바꾸지 않는다(옮기기는 폴더 보기). */
  folder: string;
  disks: DiskRow[];
  cdroms: CdromRow[];
  nics: NicRow[];
  cloudInit: CloudInitValues;
  /** 빈 문자열이면 스케줄러가 고른다. */
  node: string;
  restart: Schemas["RestartPolicy"];
  snapshotSchedule: ScheduleRow;
  /** 로컬 디스크 복제 대상(최대 3개) */
  replication: ReplicationRow[];
  /** HA 정책이 있다(`ignored`가 아니면 펜싱이 켜진 노드에서만 돈다). */
  ha: boolean;
  /** HA 정책의 값. HA를 꺼도 남겨 두어 다시 켤 때 쓴다. */
  haState: Schemas["HaState"];
  /** HA 그룹 이름. 비면 그룹 없음 */
  haGroup: string;
  maxRestart: string;
  maxRelocate: string;
  /** 복구 우선순위(0~100, 클수록 먼저) */
  haPriority: string;
  /** 게스트 에이전트 감시를 켠다. 끄더라도 초는 남겨 다시 켤 때 쓴다. */
  guestWatch: boolean;
  guestWatchSecs: string;
  /** TPM 2.0(swtpm). UEFI가 있어야 한다. */
  tpm: boolean;
  /** Secure Boot 펌웨어. UEFI가 있어야 한다. */
  secureBoot: boolean;
  /** QEMU 게스트 에이전트 채널(`guest_agent`). 끄더라도 아래 두 값은 남겨 다시 켤 때 쓴다. */
  guestAgent: boolean;
  /** 스냅샷·백업·복제 때 게스트의 파일 시스템을 얼린다(`freeze_fs`). */
  agentFreeze: boolean;
  /** 끌 때 에이전트(`guest-shutdown`)를 먼저 쓰거나 ACPI만 쓴다. */
  agentShutdown: Schemas["AgentShutdown"];
  /** 균형 조정기가 옮기지 않는다(`placement.pinned`). 끄면 `placement`를 보내지 않는다. */
  pinned: boolean;
  /** 만든 뒤 곧바로 시작한다(만들 때만). */
  start: boolean;
}

const MACHINE_OF: Record<Schemas["Arch"], Schemas["MachineType"]> = {
  x86_64: "q35",
  aarch64: "virt",
};

/**
 * 머신 기본값: 지정 노드(`selectedNode`, 빈 문자열이면 자동)의 아키텍처, 아니면 아키텍처를 아는 노드(위트니스
 * 제외)가 모두 같을 때 그 아키텍처의 머신, 그 밖에는 `q35`다.
 */
export function defaultMachine(nodes: Node[], selectedNode = ""): Schemas["MachineType"] {
  const chosen = nodes.find((n) => n.id === selectedNode)?.status.arch;
  if (chosen) {
    return MACHINE_OF[chosen];
  }
  const [only, ...others] = new Set(
    nodes.flatMap((n) => (n.status.arch && !n.spec.witness ? [n.status.arch] : [])),
  );
  return only && others.length === 0 ? MACHINE_OF[only] : "q35";
}

export function emptyDisk(vmName: string, index: number): DiskRow {
  return {
    kind: "new",
    volume: vmName ? `${vmName}-disk${index}` : "",
    pool: "",
    sizeGib: "16",
    bus: "virtio-blk",
    bootOrder: index === 0 ? "1" : "",
  };
}

export function emptyForm(machine: VmFormValues["machine"], folder = ""): VmFormValues {
  return {
    name: "",
    sockets: 1,
    cores: "2",
    model: "host",
    cpuFlags: [],
    memory: "2048",
    maxVcpus: "",
    maxMemory: "",
    numa: emptyNuma(),
    firmware: "uefi",
    machine,
    folder,
    disks: [],
    cdroms: [],
    nics: [{ network: "user", model: "virtio", mac: "", securityGroups: [], ips: "" }],
    cloudInit: emptyCloudInit(),
    node: "",
    restart: "never",
    snapshotSchedule: { every: "off", keep: "7", memory: false },
    replication: [],
    ...haFields(null),
    ...agentFields(undefined),
    tpm: false,
    secureBoot: false,
    pinned: false,
    start: true,
  };
}

export function fromVm(vm: Vm): VmFormValues {
  const s = vm.spec;
  return {
    name: vm.name,
    sockets: s.cpu.sockets ?? 1,
    cores: String(s.cpu.cores),
    model: s.cpu.model ?? "host",
    cpuFlags: s.cpu.flags ?? [],
    memory: String(s.memory_mib),
    maxVcpus: s.cpu.max_vcpus == null ? "" : String(s.cpu.max_vcpus),
    maxMemory: s.max_memory_mib == null ? "" : String(s.max_memory_mib),
    numa: numaFrom(s),
    firmware: s.firmware ?? "uefi",
    machine: s.machine,
    folder: s.folder ?? "",
    disks: (s.disks ?? []).map((d) => ({
      kind: "existing",
      volume: d.volume,
      pool: "",
      sizeGib: "",
      bus: d.bus,
      bootOrder: d.boot_order == null ? "" : String(d.boot_order),
    })),
    cdroms: (s.cdroms ?? []).map((c) => ({
      iso: c.iso ?? "",
      bootOrder: c.boot_order == null ? "" : String(c.boot_order),
    })),
    nics: (s.nics ?? []).map((n) => ({
      network: n.network,
      model: n.model,
      mac: n.mac ?? "",
      securityGroups: n.security_groups ?? [],
      ips: formatIps(n.ips),
      ...(n.qos && { qos: qosRow(n.qos) }),
    })),
    cloudInit: cloudInitFrom(s.cloud_init),
    node: s.node ?? "",
    restart: s.restart ?? "never",
    snapshotSchedule: s.snapshot_schedule
      ? {
          every: s.snapshot_schedule.every,
          keep: String(s.snapshot_schedule.keep),
          memory: s.snapshot_schedule.memory ?? false,
        }
      : { every: "off", keep: "7", memory: false },
    // 틀린 주기(서버가 받지 않는 모양)는 그대로 두어 검증이 알린다.
    replication: (s.replication ?? []).map((r) => ({
      node: r.node,
      minutes: r.schedule.replace(/^\*\//, ""),
      pool: r.pool ?? "",
    })),
    ...haFields(haPolicy(s)),
    tpm: s.tpm ?? false,
    secureBoot: s.secure_boot ?? false,
    ...agentFields(s.guest_agent),
    pinned: s.placement?.pinned ?? false,
    start: false,
  };
}

/** 게스트 에이전트 설정(없으면 undefined)의 폼 값. 없으면 서버의 기본값(얼리기, 에이전트로 끄기)을 채워 둔다. */
function agentFields(
  agent: Schemas["GuestAgentSpec"] | undefined,
): Pick<VmFormValues, "guestAgent" | "agentFreeze" | "agentShutdown"> {
  return {
    guestAgent: agent != null,
    agentFreeze: agent?.freeze_fs ?? true,
    agentShutdown: agent?.shutdown ?? "agent",
  };
}

/**
 * "Windows 11" 미리 채우기(CLI의 `vm create --preset windows11`과 같은 값): q35·UEFI·Secure Boot·TPM·게스트 에이전트,
 * 2코어·4096 MiB, 디스크는 SATA, NIC는 e1000e(Windows 설치 프로그램에 VirtIO 드라이버가 없다). 이름·노드 등 다른 값은 둔다.
 */
export function windows11(v: VmFormValues): VmFormValues {
  return {
    ...v,
    machine: "q35",
    firmware: "uefi",
    secureBoot: true,
    tpm: true,
    guestAgent: true,
    agentFreeze: true,
    agentShutdown: "agent",
    sockets: 1,
    cores: "2",
    memory: "4096",
    disks: v.disks.map((d) => ({ ...d, bus: "sata" })),
    nics: v.nics.map((n) => ({ ...n, model: "e1000e" })),
  };
}

/** 정책(없으면 null)의 폼 값. 정책이 없으면 기본 정책의 값을 채워 둔다. */
function haFields(
  policy: Schemas["VmHa"] | null,
): Pick<
  VmFormValues,
  | "ha"
  | "haState"
  | "haGroup"
  | "maxRestart"
  | "maxRelocate"
  | "haPriority"
  | "guestWatch"
  | "guestWatchSecs"
> {
  const p = policy ?? DEFAULT_HA;
  return {
    ha: policy !== null,
    haState: p.state,
    haGroup: p.group ?? "",
    maxRestart: String(p.max_restart),
    maxRelocate: String(p.max_relocate),
    haPriority: String(p.priority),
    guestWatch: p.guest_watch != null,
    guestWatchSecs: String(p.guest_watch?.timeout_secs ?? DEFAULT_GUEST_WATCH_SECS),
  };
}

/** 폼이 HA를 활성으로 두는지(서버의 `ha_active`: 정책이 있고 `ignored`가 아님) */
export function haActive(v: Pick<VmFormValues, "ha" | "haState">): boolean {
  return v.ha && v.haState !== "ignored";
}

/** 서버의 리소스 이름 규칙에 맞는지. id(UUID)처럼 보이는 이름은 id와 헷갈리므로 받지 않는다. */
export function validName(name: string): boolean {
  return NAME.test(name) && !UUID.test(name);
}

/** GiB 입력을 바이트로. 숫자가 아니거나 양의 안전한 정수가 되지 않으면 undefined */
export function gibToBytes(text: string): number | undefined {
  if (!/^\d+(\.\d+)?$/.test(text)) {
    return undefined;
  }
  const bytes = Math.round(Number(text) * GIB);
  return Number.isSafeInteger(bytes) && bytes > 0 ? bytes : undefined;
}

/** 새 빈 볼륨 만들기 본문(`POST /volumes`). 폼의 보내기와 VM 빌더의 요청 미리 보기가 함께 쓴다. */
export function volumeRequest(
  disk: Pick<DiskRow, "volume" | "pool" | "sizeGib">,
  format: Schemas["ImageFormat"],
): { name: string; spec: { pool: string; size_bytes: number; format: Schemas["ImageFormat"] } } {
  return {
    name: disk.volume,
    spec: { pool: disk.pool, size_bytes: gibToBytes(disk.sizeGib)!, format },
  };
}

/** `min`부터 `max`까지의 정수인지 */
function intIn(value: string, min: number, max: number): boolean {
  const n = Number(value);
  return Number.isInteger(n) && n >= min && n <= max;
}

/**
 * 입력 오류: 필드 → 번역 키. 서버도 검증하지만 흔한 실수는 보내기 전에 알린다. `volumes`는 이미 있는
 * 볼륨의 이름이다(새 볼륨이 겹치지 않게).
 */
export function validate(
  v: VmFormValues,
  creating: boolean,
  volumes: readonly string[] = [],
  sdn: readonly boolean[] = [],
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (creating && !validName(v.name)) {
    errors.name = "vmForm.errors.name";
  }
  if (!intIn(v.cores, 1, MAX_CORES)) {
    errors.cores = "vmForm.errors.cores";
  }
  if (!(Number.isInteger(Number(v.memory)) && Number(v.memory) >= MIN_MEMORY_MIB)) {
    errors.memory = "vmForm.errors.memory";
  }
  Object.assign(errors, maxErrors(v));
  Object.assign(errors, numaErrors(v.numa, v));
  // 서버의 `TpmNeedsUefi`·`SecureBootNeedsUefi`
  if (v.firmware !== "uefi" && v.tpm) {
    errors.tpm = "vmForm.errors.needsUefi";
  }
  if (v.firmware !== "uefi" && v.secureBoot) {
    errors.secureBoot = "vmForm.errors.needsUefi";
  }
  const boots = new Set<number>();
  const names = new Set<string>();
  v.disks.forEach((d, i) => {
    if (d.kind === "existing" && d.volume === "") {
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeChoose";
    } else if (!validName(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volume";
    } else if (names.has(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeDuplicate";
    } else if (d.kind === "new" && RESERVED_VOLUMES.includes(d.volume)) {
      errors[`disks.${i}.volume`] = "volumes.errors.reserved";
    } else if (d.kind === "new" && volumes.includes(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeExists";
    }
    names.add(d.volume);
    if (d.kind === "new" && !d.pool) {
      errors[`disks.${i}.pool`] = "vmForm.errors.pool";
    }
    if (d.kind === "new" && gibToBytes(d.sizeGib) === undefined) {
      errors[`disks.${i}.size`] = "vmForm.errors.size";
    }
    if (d.bootOrder !== "") {
      // "01"과 "1"은 같은 순서다.
      const order = Number(d.bootOrder);
      if (!intIn(d.bootOrder, 1, MAX_BOOT_ORDER) || boots.has(order)) {
        errors[`disks.${i}.bootOrder`] = "vmForm.errors.bootOrder";
      }
      boots.add(order);
    }
  });
  v.cdroms.forEach((c, i) => {
    if (c.bootOrder !== "") {
      const order = Number(c.bootOrder);
      if (!intIn(c.bootOrder, 1, MAX_BOOT_ORDER) || boots.has(order)) {
        errors[`cdroms.${i}.bootOrder`] = "vmForm.errors.bootOrder";
      }
      boots.add(order);
    }
  });
  Object.assign(errors, cdromErrors(v));
  const macs = new Set<string>();
  v.nics.forEach((n, i) => {
    if (n.securityGroups.length > MAX_NIC_SECURITY_GROUPS) {
      errors[`nics.${i}.securityGroups`] = "vmForm.errors.securityGroups";
    }
    const ips = parseIps(n.ips);
    if ("error" in ips) {
      errors[`nics.${i}.ips`] = ips.error;
    }
    for (const [key, error] of Object.entries(qosErrors(n.qos, n.network, sdn[i] !== false))) {
      errors[`nics.${i}.qos.${key}`] = error;
    }
    if (n.mac === "") {
      return;
    }
    const mac = n.mac.toLowerCase();
    if (!MAC.test(mac)) {
      errors[`nics.${i}.mac`] = "vmForm.errors.mac";
    } else if (parseInt(mac.slice(0, 2), 16) & 1) {
      // 첫 바이트의 가장 낮은 비트가 멀티캐스트 표시다.
      errors[`nics.${i}.mac`] = "vmForm.errors.macMulticast";
    } else if (macs.has(mac)) {
      errors[`nics.${i}.mac`] = "vmForm.errors.macDuplicate";
    }
    macs.add(mac);
  });
  const schedule = v.snapshotSchedule;
  if (schedule.every !== "off" && !intIn(schedule.keep, 1, MAX_SCHEDULE_KEEP)) {
    errors["schedule.keep"] = "vmForm.errors.keep";
  }
  Object.assign(errors, replicationErrors(v));
  Object.assign(errors, cloudInitErrors(v.cloudInit, v.nics.length, sdn));
  if (v.ha) {
    for (const [field, value] of [
      ["maxRestart", v.maxRestart],
      ["maxRelocate", v.maxRelocate],
    ] as const) {
      if (!/^\d+$/.test(value) || !intIn(value, 0, MAX_HA_COUNT)) {
        errors[field] = "vmForm.errors.haCount";
      }
    }
    if (!/^\d+$/.test(v.haPriority) || !intIn(v.haPriority, 0, MAX_HA_PRIORITY)) {
      errors.haPriority = "vmForm.errors.haPriority";
    }
    if (
      v.guestWatch &&
      (!/^\d+$/.test(v.guestWatchSecs) ||
        !intIn(v.guestWatchSecs, MIN_GUEST_WATCH_SECS, MAX_GUEST_WATCH_SECS))
    ) {
      errors.guestWatchSecs = "vmForm.errors.guestWatch";
    }
  }
  return errors;
}

/** 폼의 부팅 순서 값(빈 칸은 null) */
export function bootOrders(v: Pick<VmFormValues, "disks" | "cdroms">): {
  disks: (number | null)[];
  cdroms: (number | null)[];
} {
  const order = (text: string) => (text === "" ? null : Number(text));
  return {
    disks: v.disks.map((d) => order(d.bootOrder)),
    cdroms: v.cdroms.map((c) => order(c.bootOrder)),
  };
}

/**
 * CD-ROM 드라이브의 오류(서버의 `VmSpec::validate`): 드라이브 수, q35 SATA 포트 나눠 쓰기(CD-ROM이나 cloud-init이 있을
 * 때만), 디스크 뒤에 둘 부팅 순서 자리.
 */
function cdromErrors(v: VmFormValues): Record<string, string> {
  const errors: Record<string, string> = {};
  if (v.cdroms.length > MAX_CDROMS) {
    errors.cdroms = "vmForm.errors.cdromCount";
    return errors;
  }
  const seed = v.cloudInit.enabled ? 1 : 0;
  const sata = v.disks.filter((d) => d.bus === "sata").length;
  if (
    v.machine === "q35" &&
    (v.cdroms.length > 0 || seed > 0) &&
    sata + v.cdroms.length + seed > Q35_SATA_PORTS
  ) {
    errors.cdroms = "vmForm.errors.sataPorts";
    return errors;
  }
  const orders = bootOrders(v);
  // 틀린 값은 그 행의 오류가 알린다.
  const valid = [...orders.disks, ...orders.cdroms].every(
    (o) => o === null || intIn(String(o), 1, MAX_BOOT_ORDER),
  );
  const boot = effectiveBoot(orders.disks, orders.cdroms);
  // 값 없는 CD-ROM과 (값이 CD-ROM에만 있을 때) 차례로 매긴 디스크가 255를 넘으면 서버가 거절한다.
  const overflow =
    boot.cdroms.includes(null) ||
    (v.cdroms.length > 0 && orders.disks.every((o) => o === null) && boot.disks.includes(null));
  if (valid && overflow) {
    errors.cdroms = "vmForm.errors.bootOverflow";
  }
  return errors;
}

/**
 * 복제 대상의 오류: 서버의 `VmSpec::validate`와 같은 규칙(대상 수, 노드 겹침, 주기, VM 자신의 노드)과 스냅샷 예약과
 * 함께 쓰지 않는 규칙(서버의 409 `replication_unsupported`). 노드를 고르지 않은 줄도 알린다.
 */
function replicationErrors(v: VmFormValues): Record<string, string> {
  const errors: Record<string, string> = {};
  if (v.replication.length === 0) {
    return errors;
  }
  if (v.replication.length > MAX_REPLICATION_TARGETS) {
    errors.replication = "vmForm.errors.replicationTargets";
  } else if (v.snapshotSchedule.every !== "off") {
    errors.replication = "vmForm.errors.replicationSnapshots";
  }
  const seen = new Set<string>();
  v.replication.forEach((r, i) => {
    if (r.node === "") {
      errors[`replication.${i}.node`] = "vmForm.errors.replicationNode";
    } else if (seen.has(r.node)) {
      errors[`replication.${i}.node`] = "vmForm.errors.replicationDuplicate";
    } else if (r.node === v.node) {
      errors[`replication.${i}.node`] = "vmForm.errors.replicationOwnNode";
    }
    seen.add(r.node);
    if (!/^\d+$/.test(r.minutes) || !intIn(r.minutes, 1, MAX_REPLICATION_MINUTES)) {
      errors[`replication.${i}.minutes`] = "vmForm.errors.replicationMinutes";
    }
  });
  return errors;
}

/** 핫플러그 상한의 오류: 서버의 `VmSpec::validate`와 같은 규칙. 비운 상한은 없음이다. */
function maxErrors(v: VmFormValues): Record<string, string> {
  const errors: Record<string, string> = {};
  const vcpus = v.sockets * Number(v.cores);
  if (v.maxVcpus !== "") {
    const max = Number(v.maxVcpus);
    if (!/^\d+$/.test(v.maxVcpus) || max < vcpus) {
      errors.maxVcpus = "vmForm.errors.maxVcpusLow";
    } else if (max % v.sockets !== 0) {
      errors.maxVcpus = "vmForm.errors.maxVcpusSockets";
    } else if (max > MAX_VCPUS) {
      errors.maxVcpus = "vmForm.errors.maxVcpusHigh";
    }
  }
  if (v.maxMemory !== "") {
    const max = Number(v.maxMemory);
    const memory = Number(v.memory);
    if (!/^\d+$/.test(v.maxMemory) || max < memory) {
      errors.maxMemory = "vmForm.errors.maxMemoryLow";
    } else if (max > MAX_HOTPLUG_MEMORY_MIB) {
      errors.maxMemory = "vmForm.errors.maxMemoryHigh";
    } else if (max % MEMORY_HOTPLUG_ALIGN_MIB !== 0 || memory % MEMORY_HOTPLUG_ALIGN_MIB !== 0) {
      errors.maxMemory = "vmForm.errors.memoryAlign";
    }
  }
  return errors;
}

/** NIC 행의 주소(틀린 입력은 검사가 막으므로 빈 목록) */
function nicIps(n: NicRow): string[] {
  const parsed = parseIps(n.ips);
  return "ips" in parsed ? parsed.ips : [];
}

/**
 * 폼 값을 VM spec으로. `power`는 만들 때만 정하고, 고칠 때는 지금 값을 그대로 둔다. `sdn[i]`는 NIC `i`가 SDN 네트워크에
 * 있는지다(주소를 보내고 cloud-init 주소 칸을 비운다).
 */
export function toSpec(
  v: VmFormValues,
  power: Schemas["PowerState"],
  sdn: readonly boolean[] = [],
): VmSpec {
  // 기본(공유·vNUMA 없음·보통 페이지)이면 빈 칸이다(옛 클러스터는 이 필드를 모른다).
  const numa = numaSpec(v.numa);
  // `machine`을 빼면 서버가 채운다(스키마는 늘 있는 값으로 적는다).
  return {
    cpu: {
      sockets: v.sockets,
      cores: Number(v.cores),
      model: v.model,
      // 플래그가 없으면 보내지 않는다(옛 클러스터는 이 필드를 모른다).
      ...(v.cpuFlags.length > 0 && { flags: v.cpuFlags }),
      // 상한이 없으면 보내지 않는다(옛 클러스터는 이 필드를 모른다).
      ...(v.maxVcpus !== "" && { max_vcpus: Number(v.maxVcpus) }),
      ...numa.cpu,
    },
    memory_mib: Number(v.memory),
    ...(v.maxMemory !== "" && { max_memory_mib: Number(v.maxMemory) }),
    ...(numa.numa && { numa: numa.numa }),
    ...(numa.hugepages && { hugepages: numa.hugepages }),
    firmware: v.firmware,
    // 비면 보내지 않는다(서버가 고른다).
    ...(v.machine !== "" && { machine: v.machine }),
    ...(v.folder !== "" && { folder: v.folder }),
    disks: v.disks.map((d) => ({
      volume: d.volume,
      bus: d.bus,
      boot_order: d.bootOrder === "" ? null : Number(d.bootOrder),
    })),
    // CD-ROM과 cloud-init이 없으면 보내지 않는다(옛 클러스터는 이 필드를 모른다).
    ...(v.cdroms.length > 0 && {
      cdroms: v.cdroms.map((c) => ({
        ...(c.iso !== "" && { iso: c.iso }),
        ...(c.bootOrder !== "" && { boot_order: Number(c.bootOrder) }),
      })),
    }),
    nics: v.nics.map((n, i) => ({
      network: n.network,
      model: n.model,
      mac: n.mac === "" ? null : n.mac.toLowerCase(),
      // SDN NIC도 보안 그룹을 받는다(계획 66 — OVN ACL). 모든 노드가 `sdn-routing`을 알리기 전이면 서버가 409
      // `cluster_upgrading`이다.
      ...(n.securityGroups.length > 0 && { security_groups: n.securityGroups }),
      ...(sdn[i] !== false && nicIps(n).length > 0 && { ips: nicIps(n) }),
      ...(hasQos(n.qos) && { qos: toQos(n.qos) }),
    })),
    ...(v.cloudInit.enabled && { cloud_init: toCloudInit(v.cloudInit, v.nics.length, sdn) }),
    node: v.node === "" ? null : v.node,
    power,
    restart: v.restart,
    snapshot_schedule:
      v.snapshotSchedule.every === "off"
        ? null
        : {
            every: v.snapshotSchedule.every,
            keep: Number(v.snapshotSchedule.keep),
            memory: v.snapshotSchedule.memory,
          },
    // 복제가 없으면 보내지 않는다(옛 클러스터는 이 필드를 모른다).
    ...(v.replication.length > 0 && {
      replication: v.replication.map((r) => ({
        node: r.node,
        schedule: `*/${Number(r.minutes)}`,
        ...(r.pool !== "" && { pool: r.pool }),
      })),
    }),
    ha: haSetting(
      v.ha
        ? {
            state: v.haState,
            ...(v.haGroup !== "" && { group: v.haGroup }),
            max_restart: Number(v.maxRestart),
            max_relocate: Number(v.maxRelocate),
            priority: Number(v.haPriority),
            ...(v.guestWatch && { guest_watch: { timeout_secs: Number(v.guestWatchSecs) } }),
          }
        : null,
    ),
    // 끈 TPM·Secure Boot와 없는 에이전트는 보내지 않는다(옛 클러스터는 이 필드를 모른다).
    ...(v.tpm && { tpm: true }),
    ...(v.secureBoot && { secure_boot: true }),
    ...(v.guestAgent && {
      guest_agent: {
        freeze_fs: v.agentFreeze,
        ...(v.agentShutdown !== "agent" && { shutdown: v.agentShutdown }),
      },
    }),
    // 고정하지 않으면 보내지 않는다(서버도 `pinned: false`를 저장하지 않고, 옛 클러스터는 이 필드를 모른다).
    ...(v.pinned && { placement: { pinned: true } }),
  } as VmSpec;
}

/** 서버의 `ha_capable`: 위트니스가 아니고 펜싱이 켜진 노드. HA VM은 이런 노드에서만 돈다. */
export function haCapable(node: Node): boolean {
  return !node.spec.witness && node.status.ha != null;
}

/**
 * HA를 켤 수 없는 이유(번역 키와 값). 서버처럼 지정 노드(`node`, 빈 문자열이면 자동)만 보고, 자동이면 HA 가능
 * 노드가 하나도 없을 때다. 노드 목록을 아직 모르면 막지 않는다.
 */
export function haUnavailable(
  nodes: Node[] | undefined,
  node: string,
): { key: string; values?: Record<string, string> } | null {
  if (!nodes) {
    return null;
  }
  if (node !== "") {
    const chosen = nodes.find((n) => n.id === node);
    return chosen && !haCapable(chosen)
      ? { key: "vmForm.haNodeOff", values: { node: chosen.name } }
      : null;
  }
  return nodes.some(haCapable) ? null : { key: "vmForm.haNoNode" };
}

/** 노드의 펌웨어 능력. 옛 노드·위트니스는 `firmware_caps`가 없다(모름). */
export type FirmwareCap = "tpm" | "secure_boot";

/**
 * TPM·Secure Boot를 켤 수 없는 이유(번역 키와 값). {@link haUnavailable}처럼 지정 노드(`node`, 빈 문자열이면 자동)만
 * 보고, 자동이면 그 능력을 알린 노드(위트니스 제외)가 하나도 없을 때다. 능력을 모르는 노드는 없는 것으로 본다(서버의
 * 스케줄러). 노드 목록을 아직 모르면 막지 않는다.
 */
export function firmwareUnavailable(
  nodes: Node[] | undefined,
  node: string,
  cap: FirmwareCap,
): { key: string; values?: Record<string, string> } | null {
  if (!nodes) {
    return null;
  }
  const has = (n: Node) => n.status.firmware_caps?.[cap] === true;
  const what = cap === "tpm" ? "tpm" : "secureBoot";
  if (node !== "") {
    const chosen = nodes.find((n) => n.id === node);
    return chosen && !has(chosen)
      ? { key: `vmForm.${what}NodeOff`, values: { node: chosen.name } }
      : null;
  }
  return nodes.some((n) => !n.spec.witness && has(n)) ? null : { key: `vmForm.${what}NoNode` };
}

/**
 * 단순 NIC 속도 제한(tc)을 걸 수 없는 곳(서버 `check_qos_node` — `NodeStatus.qos`가 참이 아니면 409 `qos_unsupported`):
 * 고른 노드가 못 하면 그 이름, 노드를 고르지 않았는데 할 수 있는 노드가 하나도 없으면 빈 글, 아니면 null. 노드 목록을
 * 읽지 못했으면 null(모름).
 */
export function tcMissing(nodes: readonly Node[] | undefined, node: string): string | null {
  if (!nodes) {
    return null;
  }
  if (node !== "") {
    const chosen = nodes.find((n) => n.id === node);
    return chosen && chosen.status.qos !== true ? chosen.name : null;
  }
  return nodes.some((n) => !n.spec.witness && n.status.qos === true) ? null : "";
}
