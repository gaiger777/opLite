import type { components } from "@/api/schema";
import type { ImportRun, VmImport } from "@/api/types";
import { MIN_MEMORY_MIB, validName } from "@/vms/form";

type Schemas = components["schemas"];

/** `/vm-imports/ova` 경로와 겹쳐 서버가 가져오기 이름으로 받지 않는다. */
export const RESERVED_IMPORT = "ova";
/** 서버의 `cores`·`sockets`(u16)와 `memory_mib`(u32)의 최댓값 */
const MAX_COUNT = 65535;
const MAX_MEMORY_MIB = 2 ** 32 - 1;
/** 서버의 리소스 이름 길이 */
const MAX_NAME = 63;

/** 새 가져오기 이름의 입력 오류(번역 키). `taken`은 이미 있는 가져오기의 이름이다. */
export function importNameError(name: string, taken: readonly string[]): string | undefined {
  if (!validName(name)) {
    return "forms.name";
  }
  if (name === RESERVED_IMPORT) {
    return "imports.errors.reserved";
  }
  if (taken.includes(name)) {
    return "imports.errors.exists";
  }
  return undefined;
}

/** 원본 NIC들의 네트워크. 처음 나온 차례로, 겹치지 않게. */
export function sourceNetworks(imp: VmImport): string[] {
  return [...new Set((imp.status.source_vm?.nics ?? []).map((n) => n.network))];
}

/**
 * 서버의 `volume_name`처럼 디스크마다 만들 볼륨 이름: `{VM 이름}-{디스크 이름}`, 63자로 자르고 끝의 `-`를
 * 뗀다.
 */
export function volumeNames(imp: VmImport, vmName: string): string[] {
  return (imp.status.source_vm?.disks ?? []).map((d) =>
    `${vmName}-${d.name}`.slice(0, MAX_NAME).replace(/-+$/, ""),
  );
}

/** 실행 폼. 숫자 칸은 입력 그대로의 문자열이다. */
export interface RunForm {
  name: string;
  pool: string;
  sockets: string;
  cores: string;
  memory: string;
  firmware: Schemas["Firmware"];
  diskBus: Schemas["DiskBus"];
  nicModel: Schemas["NicModel"];
  /** 원본 네트워크 → opLite 네트워크(고르지 않았으면 빈 문자열) */
  networks: Record<string, string>;
  keepMacs: boolean;
}

/** 제안으로 채운 실행 폼. VM 이름은 가져오기 이름, 풀은 고르게 비워 둔다. */
export function runForm(imp: VmImport): RunForm {
  const p = imp.status.proposal;
  return {
    name: imp.name,
    pool: "",
    sockets: String(p?.sockets ?? 1),
    cores: String(p?.cores ?? 1),
    memory: String(p?.memory_mib ?? MIN_MEMORY_MIB),
    firmware: p?.firmware ?? "bios",
    diskBus: p?.disk_bus ?? "virtio-blk",
    nicModel: p?.nic_model ?? "virtio",
    networks: Object.fromEntries(sourceNetworks(imp).map((n) => [n, p?.networks[n] ?? ""])),
    keepMacs: true,
  };
}

/** `min`부터 `max`까지의 정수인지 */
function intIn(value: string, min: number, max: number): boolean {
  const n = Number(value);
  return value.trim() !== "" && Number.isInteger(n) && n >= min && n <= max;
}

/**
 * 입력 오류: 필드 → 번역 키. 서버도 검사하지만 흔한 실수는 보내기 전에 알린다. `taken`은 이미 있는 VM과
 * 볼륨 이름, `networks`는 고를 수 있는 네트워크 이름이다. 네트워크 오류의 필드는 `networks.<원본 네트워크>`다.
 */
export function validateRun(
  form: RunForm,
  imp: VmImport,
  taken: { vms: readonly string[]; volumes: readonly string[] },
  networks: readonly string[],
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!validName(form.name)) {
    errors.name = "forms.name";
  } else if (taken.vms.includes(form.name)) {
    errors.name = "imports.errors.vmExists";
  } else if (volumeNames(imp, form.name).some((v) => taken.volumes.includes(v))) {
    errors.name = "imports.errors.volumeExists";
  }
  if (form.pool === "") {
    errors.pool = "imports.errors.pool";
  }
  if (!intIn(form.sockets, 1, MAX_COUNT)) {
    errors.sockets = "imports.errors.sockets";
  }
  if (!intIn(form.cores, 1, MAX_COUNT)) {
    errors.cores = "vmForm.errors.cores";
  }
  if (!intIn(form.memory, MIN_MEMORY_MIB, MAX_MEMORY_MIB)) {
    errors.memory = "vmForm.errors.memory";
  }
  for (const source of sourceNetworks(imp)) {
    // 제안한 네트워크가 그새 지워졌을 수도 있다.
    if (!networks.includes(form.networks[source] ?? "")) {
      errors[`networks.${source}`] = "imports.errors.network";
    }
  }
  return errors;
}

/** 실행 요청 본문. 폼의 값을 모두 보낸다(제안과 같아도). */
export function toRun(form: RunForm): ImportRun {
  return {
    name: form.name,
    pool: form.pool,
    networks: form.networks,
    disk_bus: form.diskBus,
    nic_model: form.nicModel,
    firmware: form.firmware,
    sockets: Number(form.sockets),
    cores: Number(form.cores),
    memory_mib: Number(form.memory),
    keep_macs: form.keepMacs,
  };
}
