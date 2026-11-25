// 원본 인벤토리를 화면에 맞게 바꾸는 순수 함수(계획 72 §8.2·§13): 나무(VM 폴더 또는 클러스터·호스트), 찾기·거르기, 고른
// VM이 쓰는 네트워크·데이터스토어, 마법사 폼 → 만들기 본문.
import type { components } from "@/api/schema";
import { validName } from "@/vms/form";
import type {
  CreateMigrationRequest,
  InvVm,
  ItemState,
  MigrationItem,
  MigrationPhase,
  PreflightLevel,
  Rules,
} from "@/vmware/api";

type DiskBus = components["schemas"]["DiskBus"];
type NicModel = components["schemas"]["NicModel"];

/** 나무 보기: VM 폴더(데이터센터 › 폴더) 또는 컴퓨트(데이터센터 › 클러스터 › 호스트) */
export type TreeView = "folders" | "hosts";

export type TreeKind = "datacenter" | "folder" | "cluster" | "host";

export interface TreeNode {
  /** 보기 안에서 고유한 경로(`folders:DC1/Prod/Web`) */
  id: string;
  label: string;
  kind: TreeKind;
  children: TreeNode[];
  /** 이 마디 아래(자식 포함) VM 수 */
  count: number;
}

interface Segment {
  label: string;
  kind: TreeKind;
}

/** VM의 데이터센터 경로(`DC1`, `Europe/DC2`). 인벤토리가 주지 않으면 경로의 첫 조각이다. */
function datacenterOf(vm: InvVm): string {
  return vm.datacenter ?? vm.path.split("/")[0] ?? "";
}

/** 나무에서 VM이 놓이는 마디들(뿌리부터) */
function segmentsOf(vm: InvVm, view: TreeView): Segment[] {
  const dc = datacenterOf(vm);
  const root: Segment = { label: dc, kind: "datacenter" };
  if (view === "hosts") {
    return [
      root,
      ...(vm.cluster ? [{ label: vm.cluster, kind: "cluster" as const }] : []),
      ...(vm.host_name ? [{ label: vm.host_name, kind: "host" as const }] : []),
    ];
  }
  // `DC1/vm/Prod/Web` → Prod › Web (데이터센터의 VM 폴더 `vm`은 뿌리와 같다)
  const folder = vm.folder ?? "";
  const rest = folder.startsWith(`${dc}/`) ? folder.slice(dc.length + 1) : "";
  const parts = rest.split("/").filter(Boolean);
  if (parts[0] === "vm") {
    parts.shift();
  }
  return [root, ...parts.map((label) => ({ label, kind: "folder" as const }))];
}

function idOf(view: TreeView, segments: readonly Segment[]): string {
  return `${view}:${segments.map((s) => s.label).join("/")}`;
}

/** VM들의 나무. 마디는 이름 차례이고 VM 수를 센다. */
export function buildTree(vms: readonly InvVm[], view: TreeView): TreeNode[] {
  const roots: TreeNode[] = [];
  for (const vm of vms) {
    const segments = segmentsOf(vm, view);
    let level = roots;
    for (let i = 0; i < segments.length; i++) {
      const segment = segments[i]!;
      const id = idOf(view, segments.slice(0, i + 1));
      let node = level.find((n) => n.id === id);
      if (!node) {
        node = { id, label: segment.label, kind: segment.kind, children: [], count: 0 };
        level.push(node);
      }
      node.count += 1;
      level = node.children;
    }
  }
  const sort = (nodes: TreeNode[]) => {
    nodes.sort((a, b) => a.label.localeCompare(b.label));
    nodes.forEach((n) => sort(n.children));
  };
  sort(roots);
  return roots;
}

/** 마디(`null`이면 모두) 아래의 VM */
export function vmsUnder(vms: readonly InvVm[], view: TreeView, node: string | null): InvVm[] {
  if (!node) {
    return [...vms];
  }
  return vms.filter((vm) => {
    const segments = segmentsOf(vm, view);
    return segments.some((_, i) => idOf(view, segments.slice(0, i + 1)) === node);
  });
}

export type PowerFilter = "all" | "powered-on" | "powered-off";

/** 찾기(이름·경로·OS·호스트·IP)와 전원 거르기. 템플릿은 꺼진 VM으로 본다. */
export function filterVms(vms: readonly InvVm[], query: string, power: PowerFilter): InvVm[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  return vms
    .filter((vm) => power === "all" || (vm.power ?? "powered-off") === power)
    .filter((vm) => {
      if (words.length === 0) {
        return true;
      }
      const text = [
        vm.name,
        vm.path,
        vm.guest_id,
        vm.guest_full_name ?? "",
        vm.host_name ?? "",
        ...Object.values(vm.guest_ips).flat(),
      ]
        .join(" ")
        .toLowerCase();
      return words.every((w) => text.includes(w));
    })
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** 디스크의 데이터스토어 이름. 인벤토리가 주지 않으면 파일 경로 `[ds] dir/x.vmdk`에서 읽는다. */
export function datastoreOf(disk: InvVm["disks"][number]): string {
  return disk.datastore ?? /^\[([^\]]+)\]/.exec(disk.file)?.[1] ?? "";
}

/** 고른 VM들이 쓰는 원본 네트워크(이름 차례) */
export function usedNetworks(vms: readonly InvVm[]): string[] {
  return [...new Set(vms.flatMap((vm) => vm.nics.map((n) => n.network)))].sort();
}

/** 고른 VM들이 쓰는 데이터스토어(이름 차례) */
export function usedDatastores(vms: readonly InvVm[]): string[] {
  return [...new Set(vms.flatMap((vm) => vm.disks.map(datastoreOf)).filter(Boolean))].sort();
}

/** VM 디스크 용량 합(바이트) */
export function vmBytes(vm: InvVm): number {
  return vm.disks.reduce((sum, d) => sum + d.capacity, 0);
}

/** 우리 스냅샷 이름의 머리(서버 `SNAPSHOT_PREFIX`) */
export const SNAPSHOT_PREFIX = "orbivisor-migration-";

/** 사용자 스냅샷(우리 이름이 아닌 것) 수 */
export function snapshotCount(vm: InvVm): number {
  return vm.snapshots.filter((s) => !s.name.startsWith(SNAPSHOT_PREFIX)).length;
}

/** 새 계획 마법사의 입력(문자열은 입력 칸 그대로) */
export interface WizardForm {
  name: string;
  source: string;
  mode: "warm" | "cold";
  /** 고른 VM의 moref */
  selected: string[];
  /** 원본 네트워크 → 우리 네트워크(비면 같은 이름 제안) */
  networks: Record<string, string>;
  /** 데이터스토어 → 풀(비면 기본 풀) */
  datastores: Record<string, string>;
  defaultPool: string;
  folderKind: "flat" | "mirror";
  /** 폴더 id(비면 루트) */
  folder: string;
  /** 노드 id(비면 자동) */
  node: string;
  devices: "auto" | "fixed";
  diskBus: DiskBus;
  nicModel: NicModel;
  /** 드라이버 ISO 볼륨 이름 */
  driverIso: string;
  keepMacs: boolean;
  reserveIps: boolean;
  startAfter: boolean;
  maxParallel: string;
  maxPerHost: string;
  maxPerNode: string;
  /** MiB/s, 비면 한도 없음 */
  bandwidth: string;
  intervalMin: string;
  stopAfterDays: string;
  shutdown: "guest" | "power-off";
  shutdownTimeout: string;
  protectSource: boolean;
  /** `datetime-local` 값(비면 손으로 전환) */
  cutoverAt: string;
  /** GiB, 비면 한도 없음 */
  maxFinalDelta: string;
}

export function emptyWizard(source = ""): WizardForm {
  return {
    name: "",
    source,
    mode: "warm",
    selected: [],
    networks: {},
    datastores: {},
    defaultPool: "",
    folderKind: "flat",
    folder: "",
    node: "",
    devices: "auto",
    diskBus: "virtio-blk",
    nicModel: "virtio",
    driverIso: "",
    keepMacs: true,
    reserveIps: true,
    startAfter: true,
    maxParallel: "4",
    maxPerHost: "2",
    maxPerNode: "2",
    bandwidth: "",
    intervalMin: "60",
    stopAfterDays: "14",
    shutdown: "guest",
    shutdownTimeout: "600",
    protectSource: true,
    cutoverAt: "",
    maxFinalDelta: "",
  };
}

export type WizardStep = "source" | "vms" | "mapping" | "policy" | "check";
export const WIZARD_STEPS: readonly WizardStep[] = ["source", "vms", "mapping", "policy", "check"];

function intIn(text: string, min: number, max: number): boolean {
  const n = Number(text);
  return /^\d+$/.test(text.trim()) && n >= min && n <= max;
}

/**
 * 단계의 입력 오류(칸 → 번역 키). 매핑 단계는 디스크를 둘 풀만 요구한다(네트워크를 비우면 같은 이름을 제안하고, 그래도 없으면
 * 미리 확인이 알린다).
 */
export function checkWizardStep(
  step: WizardStep,
  form: WizardForm,
  vms: readonly InvVm[],
  taken: readonly string[],
): Record<string, string> {
  const errors: Record<string, string> = {};
  switch (step) {
    case "source":
      if (!form.source) {
        errors.source = "vmware.wizard.errors.source";
      }
      if (!validName(form.name)) {
        errors.name = "vmware.wizard.errors.name";
      } else if (taken.includes(form.name)) {
        errors.name = "vmware.wizard.errors.nameTaken";
      }
      break;
    case "vms":
      if (form.selected.length === 0) {
        errors.vms = "vmware.wizard.errors.vms";
      } else if (form.selected.length > 200) {
        errors.vms = "vmware.wizard.errors.tooMany";
      }
      break;
    case "mapping": {
      const chosen = vms.filter((vm) => form.selected.includes(vm.moref));
      for (const ds of usedDatastores(chosen)) {
        if (!form.datastores[ds] && !form.defaultPool) {
          errors[`ds:${ds}`] = "vmware.wizard.errors.pool";
        }
      }
      break;
    }
    case "policy":
      for (const [field, value, min, max] of [
        ["maxParallel", form.maxParallel, 1, 64],
        ["maxPerHost", form.maxPerHost, 1, 64],
        ["maxPerNode", form.maxPerNode, 1, 64],
        ["intervalMin", form.intervalMin, 5, 1440],
        ["stopAfterDays", form.stopAfterDays, 0, 365],
        ["shutdownTimeout", form.shutdownTimeout, 30, 3600],
      ] as const) {
        if (!intIn(value, min, max)) {
          errors[field] = "vmware.wizard.errors.range";
        }
      }
      if (form.bandwidth.trim() && !intIn(form.bandwidth, 1, 100_000)) {
        errors.bandwidth = "vmware.wizard.errors.range";
      }
      if (form.maxFinalDelta.trim() && !intIn(form.maxFinalDelta, 1, 100_000)) {
        errors.maxFinalDelta = "vmware.wizard.errors.range";
      }
      if (form.cutoverAt && Number.isNaN(new Date(form.cutoverAt).getTime())) {
        errors.cutoverAt = "vmware.wizard.errors.time";
      }
      break;
    case "check":
      break;
  }
  return errors;
}

/** 폼의 규칙(`rules`). 고른 VM이 쓰는 네트워크·데이터스토어의 매핑만 넣는다. */
export function rulesOf(form: WizardForm, vms: readonly InvVm[]): Rules {
  const chosen = vms.filter((vm) => form.selected.includes(vm.moref));
  const networks = usedNetworks(chosen)
    .filter((from) => form.networks[from])
    .map((from) => ({ from, to: form.networks[from]! }));
  const datastores = usedDatastores(chosen)
    .filter((from) => form.datastores[from])
    .map((from) => ({ from, to: form.datastores[from]! }));
  const folder = form.folder || undefined;
  // 비운 칸은 서버 기본값이다(`@default null` 칸은 보내지 않는다).
  return {
    networks,
    datastores,
    ...(form.defaultPool ? { default_pool: form.defaultPool } : {}),
    folders:
      form.folderKind === "flat"
        ? { kind: "flat", ...(folder ? { into: folder } : {}) }
        : { kind: "mirror", ...(folder ? { under: folder } : {}) },
    ...(form.node ? { node: form.node } : {}),
    ...(form.devices === "fixed"
      ? { devices: { disk_bus: form.diskBus, nic_model: form.nicModel } }
      : {}),
    ...(form.driverIso ? { driver_iso: form.driverIso } : {}),
    keep_macs: form.keepMacs,
    reserve_ips: form.reserveIps,
    start_after: form.startAfter,
  } as Rules;
}

/** 만들기 본문(`POST /vmware-migrations`) */
export function createRequestOf(form: WizardForm, vms: readonly InvVm[]): CreateMigrationRequest {
  const at = form.cutoverAt ? Math.floor(new Date(form.cutoverAt).getTime() / 1000) : undefined;
  return {
    name: form.name,
    source: form.source,
    mode: form.mode,
    rules: rulesOf(form, vms),
    limits: {
      max_parallel: Number(form.maxParallel),
      max_per_host: Number(form.maxPerHost),
      max_per_node: Number(form.maxPerNode),
      ...(form.bandwidth.trim() ? { bandwidth_mib_s: Number(form.bandwidth) } : {}),
    } as CreateMigrationRequest["limits"],
    sync: {
      interval_min: Number(form.intervalMin),
      stop_after_days: Number(form.stopAfterDays),
    },
    cutover: {
      shutdown: form.shutdown,
      shutdown_timeout_s: Number(form.shutdownTimeout),
      protect_source: form.protectSource,
      ...(at !== undefined ? { at } : {}),
      ...(form.maxFinalDelta.trim() ? { max_final_delta_gib: Number(form.maxFinalDelta) } : {}),
    } as CreateMigrationRequest["cutover"],
    vms: form.selected.map((ref) => ({ ref })),
  };
}

/** 매핑의 제안: 우리 쪽에 같은 이름 네트워크가 있으면 그것 */
export function proposeNetworks(
  used: readonly string[],
  ours: readonly string[],
  current: Record<string, string>,
): Record<string, string> {
  const next = { ...current };
  for (const from of used) {
    if (next[from] === undefined && ours.includes(from)) {
      next[from] = from;
    }
  }
  return next;
}

/** 상태 칩의 뜻(색) */
export function stateTone(state: ItemState): "success" | "warning" | "danger" | "info" | "neutral" {
  switch (state) {
    case "done":
    case "finalized":
      return "success";
    case "ready":
    case "syncing":
      return "info";
    case "pending":
    case "copying":
    case "cutting-over":
      return "warning";
    case "failed":
      return "danger";
    default:
      return "neutral";
  }
}

/** 바뀌는 중인 상태(점이 깜박인다) */
export function stateBusy(state: ItemState): boolean {
  return state === "copying" || state === "cutting-over";
}

export function levelTone(level: PreflightLevel): "success" | "warning" | "danger" {
  return level === "ok" ? "success" : level === "warn" ? "warning" : "danger";
}

/** 지금 회차의 복사 진행(0~1). 디스크 표가 없으면(아직 시작 전) 없다. */
export function copyProgress(item: MigrationItem): number | undefined {
  // 빈 칸은 서버가 보내지 않는다(디스크 표·회차).
  const disks = item.status.disks ?? [];
  if (disks.length === 0) {
    return undefined;
  }
  const total = disks.reduce((sum, d) => sum + d.capacity_bytes, 0);
  if (item.status.state !== "copying") {
    return (item.status.rounds ?? 0) > 0 ? 1 : undefined;
  }
  const copied = disks.reduce((sum, d) => sum + Math.min(d.copied_bytes ?? 0, d.capacity_bytes), 0);
  return total > 0 ? copied / total : undefined;
}

/** 마지막 델타 크기 합(바이트) */
export function lastDelta(item: MigrationItem): number | undefined {
  const deltas = (item.status.disks ?? []).map((d) => d.last_delta_bytes);
  return deltas.some((d) => d !== undefined)
    ? deltas.reduce<number>((sum, d) => sum + (d ?? 0), 0)
    : undefined;
}

/** 전환할 수 있는 항목(`ready`·`syncing`, 설계 §10.1 — 자동 되돌림한 `rolled-back`도 다시 전환할 수 있다) */
export function canCutover(item: MigrationItem): boolean {
  return ["ready", "syncing", "rolled-back"].includes(item.status.state);
}

/**
 * 전환의 자동 되돌림이 실패한 항목(`failed`인데 걸음이 남음). 오류에 손으로 할 일이 적혀 있고, 다시 하기는 되돌림을 다시 한다.
 */
export function rollbackFailed(item: MigrationItem): boolean {
  return item.status.state === "failed" && item.status.cutover_step !== undefined;
}

/** 오래 전환하지 않아 동기화가 멈춘 따뜻한 항목(`syncing`인데 다음 동기화가 없음 — `stop_after_days`). 다시 하기로 잇는다. */
export function syncStopped(item: MigrationItem): boolean {
  return item.status.state === "syncing" && item.status.next_sync_at === undefined;
}

/**
 * 다시 하기를 서버가 받는 항목(`retry_item`과 같다): 계획이 `running`·`paused`이고 항목이 `failed`이거나 동기화가 멈췄다.
 * 도는 Task가 있으면 서버가 409 `migration_busy`로 답한다(화면은 모른다).
 */
export function canRetry(phase: MigrationPhase, item: MigrationItem): boolean {
  return (
    (phase === "running" || phase === "paused") &&
    (item.status.state === "failed" || syncStopped(item))
  );
}

/** 취소할 수 있는 항목(설계 §10.3 — `pending`~`syncing`·`failed`·`rolled-back`) */
export function canCancel(item: MigrationItem): boolean {
  return ["draft", "pending", "copying", "syncing", "ready", "failed", "rolled-back"].includes(
    item.status.state,
  );
}

/** 초를 `4m 12s`·`1h 3m` 꼴로 */
export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) {
    return `${s}s`;
  }
  if (s < 3600) {
    return `${Math.floor(s / 60)}m ${s % 60}s`;
  }
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}
