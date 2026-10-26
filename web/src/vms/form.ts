import type { components } from "@/api/schema";
import type { Node, Vm, VmSpec } from "@/api/types";

type Schemas = components["schemas"];

/** 서버의 `MIN_MEMORY_MIB` */
export const MIN_MEMORY_MIB = 64;
/** 서버의 `MAX_NICS` */
export const MAX_NICS = 16;
/** 서버의 `cores`(u16)와 `boot_order`(u8)의 최댓값 */
const MAX_CORES = 65535;
const MAX_BOOT_ORDER = 255;
/** 서버의 리소스 이름 규칙: 소문자·숫자·`-`, 63자까지, `-`로 시작하거나 끝나지 않는다. */
const NAME = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const MAC = /^[0-9a-f]{2}(:[0-9a-f]{2}){5}$/i;

export interface DiskRow {
  /** 있는 볼륨을 붙이거나, 만들기 전에 새 빈 볼륨을 만든다. */
  kind: "existing" | "new";
  volume: string;
  pool: string;
  sizeGib: string;
  bus: Schemas["DiskBus"];
  bootOrder: string;
}

export interface NicRow {
  network: string;
  model: Schemas["NicModel"];
  mac: string;
}

export interface VmFormValues {
  name: string;
  sockets: number;
  cores: string;
  model: Schemas["CpuModel"];
  memory: string;
  firmware: Schemas["Firmware"];
  machine: Schemas["MachineType"];
  disks: DiskRow[];
  nics: NicRow[];
  /** 빈 문자열이면 스케줄러가 고른다. */
  node: string;
  restart: Schemas["RestartPolicy"];
  /** 만든 뒤 곧바로 시작한다(만들 때만). */
  start: boolean;
}

/**
 * 노드의 아키텍처를 API가 알려 주지 않으므로, 모든 노드가 hvf(Apple Silicon)면 `virt`, 아니면 `q35`를
 * 기본값으로 한다. 다른 경우는 사용자가 고른다.
 */
export function defaultMachine(nodes: Node[]): Schemas["MachineType"] {
  return nodes.length > 0 && nodes.every((n) => n.status.hypervisor === "qemu-hvf")
    ? "virt"
    : "q35";
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

export function emptyForm(machine: Schemas["MachineType"]): VmFormValues {
  return {
    name: "",
    sockets: 1,
    cores: "2",
    model: "host",
    memory: "2048",
    firmware: "uefi",
    machine,
    disks: [],
    nics: [{ network: "user", model: "virtio", mac: "" }],
    node: "",
    restart: "never",
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
    memory: String(s.memory_mib),
    firmware: s.firmware ?? "uefi",
    machine: s.machine,
    disks: (s.disks ?? []).map((d) => ({
      kind: "existing",
      volume: d.volume,
      pool: "",
      sizeGib: "",
      bus: d.bus,
      bootOrder: d.boot_order == null ? "" : String(d.boot_order),
    })),
    nics: (s.nics ?? []).map((n) => ({ network: n.network, model: n.model, mac: n.mac ?? "" })),
    node: s.node ?? "",
    restart: s.restart ?? "never",
    start: false,
  };
}

/** 서버의 리소스 이름 규칙에 맞는지. id(UUID)처럼 보이는 이름은 id와 헷갈리므로 받지 않는다. */
export function validName(name: string): boolean {
  return NAME.test(name) && !UUID.test(name);
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
  const boots = new Set<number>();
  const names = new Set<string>();
  v.disks.forEach((d, i) => {
    if (d.kind === "existing" && d.volume === "") {
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeChoose";
    } else if (!validName(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volume";
    } else if (names.has(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeDuplicate";
    } else if (d.kind === "new" && volumes.includes(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeExists";
    }
    names.add(d.volume);
    if (d.kind === "new" && !d.pool) {
      errors[`disks.${i}.pool`] = "vmForm.errors.pool";
    }
    if (d.kind === "new" && !(Number(d.sizeGib) > 0)) {
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
  const macs = new Set<string>();
  v.nics.forEach((n, i) => {
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
  return errors;
}

/** 폼 값을 VM spec으로. `power`는 만들 때만 정하고, 고칠 때는 지금 값을 그대로 둔다. */
export function toSpec(v: VmFormValues, power: Schemas["PowerState"]): VmSpec {
  return {
    cpu: { sockets: v.sockets, cores: Number(v.cores), model: v.model },
    memory_mib: Number(v.memory),
    firmware: v.firmware,
    machine: v.machine,
    disks: v.disks.map((d) => ({
      volume: d.volume,
      bus: d.bus,
      boot_order: d.bootOrder === "" ? null : Number(d.bootOrder),
    })),
    nics: v.nics.map((n) => ({
      network: n.network,
      model: n.model,
      mac: n.mac === "" ? null : n.mac.toLowerCase(),
    })),
    node: v.node === "" ? null : v.node,
    power,
    restart: v.restart,
  };
}
