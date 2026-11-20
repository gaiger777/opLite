import type { components } from "@/api/schema";
import type { ImportRun, VmImport } from "@/api/types";
import { emptyNuma, numaErrors, numaOverride, type NumaValues } from "@/numa/numa";
import { ipsOverride } from "@/sdn/ips";
import { MIN_MEMORY_MIB, validName } from "@/vms/form";

type Schemas = components["schemas"];

/** `/vm-imports/ova`·`/vm-imports/esxi` 경로와 겹쳐 서버가 가져오기 이름으로 받지 않는다. */
export const RESERVED_IMPORTS = ["ova", "esxi"];
/** 서버의 `CREDENTIALS_LOST`. 원본 노드 메모리에 ESXi 자격 증명이 없는(opld가 다시 시작했거나 요청이 끊긴) 가져오기의 오류다. */
export const CREDENTIALS_LOST =
  "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again";
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
  if (RESERVED_IMPORTS.includes(name)) {
    return "imports.errors.reserved";
  }
  if (taken.includes(name)) {
    return "imports.errors.exists";
  }
  return undefined;
}

/** VM 이름에서 이름 규칙에 맞춘 가져오기 이름. 맞출 수 없으면 빈 문자열이다. */
export function nameFromVm(vm: string): string {
  return vm
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+/, "")
    .slice(0, MAX_NAME)
    .replace(/-+$/, "");
}

/**
 * ESXi VM으로 만들 가져오기 이름: VM 이름에서, 맞출 수 없으면 `.vmx`가 있는 디렉터리 이름에서, 그래도
 * 안 되면 `esxi-import`.
 */
export function esxiImportName(vm: { name: string; vmx: string }): string {
  const dir = vm.vmx.split("/").slice(0, -1).pop() ?? "";
  return nameFromVm(vm.name) || nameFromVm(dir) || "esxi-import";
}

/** 원본이 어디 있는지: OVA면 파일 이름, ESXi면 `호스트 / 데이터스토어 / .vmx 경로` */
export function sourceText(imp: VmImport): string {
  const source = imp.spec.source;
  if ("ova" in source) {
    return source.ova.file_name;
  }
  return `${source.esxi.host} / ${source.esxi.datastore} / ${source.esxi.vmx}`;
}

/** ESXi 연결 폼. 포트는 입력 그대로의 문자열이다. 비밀번호는 이 상태에만 있고 저장하지 않는다. */
export interface ConnectionForm {
  host: string;
  port: string;
  user: string;
  password: string;
  datacenter: string;
}

/** ESXi 연결 폼의 입력 오류: 필드 → 번역 키. 비밀번호는 비어 있어도 된다. */
export function validateConnection(form: ConnectionForm): Record<string, string> {
  const errors: Record<string, string> = {};
  if (form.host.trim() === "") {
    errors.host = "imports.errors.host";
  }
  if (!intIn(form.port, 1, 65535)) {
    errors.port = "imports.errors.port";
  }
  if (form.user === "") {
    errors.user = "imports.errors.user";
  }
  if (form.datacenter.trim() === "") {
    errors.datacenter = "imports.errors.datacenter";
  }
  return errors;
}

/** 연결 요청 본문. 포트와 데이터센터도 늘 보낸다. 지문은 믿기로 한 뒤에만 붙인다. */
export function esxiConnection(
  form: ConnectionForm,
  fingerprint?: string,
): Schemas["EsxiConnection"] {
  return {
    host: form.host.trim(),
    port: Number(form.port),
    user: form.user,
    password: form.password,
    datacenter: form.datacenter.trim(),
    ...(fingerprint ? { fingerprint } : {}),
  };
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
  /** 원본 네트워크 → Orbivisor 네트워크(고르지 않았으면 빈 문자열) */
  networks: Record<string, string>;
  keepMacs: boolean;
  /** CPU 모델(`host`면 보내지 않는다 — 서버 기본)과 플래그 */
  cpuModel: string;
  cpuFlags: string[];
  /** CPU 배치와 NUMA(계획 64). 기본(공유·vNUMA 없음)이면 보내지 않는다. */
  numa: NumaValues;
  /** 원본 NIC 순번별 SDN 주소 입력(계획 65, 비면 서브넷에서 받음). 없으면 모두 자동이다. */
  ips?: string[];
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
    cpuModel: "host",
    cpuFlags: [],
    numa: emptyNuma(),
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
  Object.assign(errors, runNumaErrors(form));
  ipsOverride(form.ips ?? [], errors, "ips");
  for (const source of sourceNetworks(imp)) {
    // 제안한 네트워크가 그새 지워졌을 수도 있다.
    if (!networks.includes(form.networks[source] ?? "")) {
      errors[`networks.${source}`] = "imports.errors.network";
    }
  }
  return errors;
}

/**
 * NUMA 칸을 폼의 소켓·코어·메모리로 다시 본 오류(리뷰 2 웹): 노드 수를 고른 뒤 소켓을 바꾸면 서버가 400
 * `invalid_numa`로 거절했다. 가져오기에는 핫플러그 상한이 없다.
 */
export function runNumaErrors(form: RunForm): Record<string, string> {
  return numaErrors(form.numa, {
    sockets: Number(form.sockets) || 1,
    cores: form.cores,
    maxVcpus: "",
    memory: form.memory,
    maxMemory: "",
  });
}

/**
 * 실행 요청 본문. 폼의 값을 모두 보낸다(제안과 같아도). `sdn[i]`는 원본 NIC `i`가 고른 네트워크가 SDN인지다(그 NIC의
 * 주소만 보낸다).
 */
export function toRun(form: RunForm, sdn: readonly boolean[] = []): ImportRun {
  const ips = ipsOverride(
    (form.ips ?? []).map((text, i) => (sdn[i] ? text : "")),
    {},
    "ips",
  );
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
    ...(form.cpuModel !== "host" && { cpu_model: form.cpuModel }),
    ...(form.cpuFlags.length > 0 && { cpu_flags: form.cpuFlags }),
    ...numaOverride(form.numa, emptyNuma(), false),
    ...(ips && { ips }),
  };
}
