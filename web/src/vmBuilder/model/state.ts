// VM 빌더의 상태(계획 71 설계 §7.1): 폼과 같은 `VmFormValues`에 빌더만의 바탕(빈 VM | 템플릿)·영역 규칙·자동 볼륨
// 이름을 더한 것. 검사·요청·프리셋은 폼의 것을 그대로 쓴다. 화면 없이 시험하는 순수 모듈이다.
import type { components } from "@/api/schema";
import type {
  Folder,
  HaGroup,
  Network,
  Node,
  NodeLoadView,
  PlacementRule,
  SecurityGroup,
  StoragePool,
  Vm,
  Volume,
} from "@/api/types";
import type { Permissions } from "@/auth/permissions";
import type { CpuModelEntry } from "@/cpu/cpu";
import type { Reason } from "@/dnd/types";
import { emptyDisk, emptyForm, type VmFormValues } from "@/vms/form";
import type { Preset } from "@/vms/form/steps";

/** 바탕: 빈 VM(`POST /vms`)이나 템플릿 배포(`POST /vms/{id}/clone`). `linked` null은 서버 기본(되는 디스크는 연결). */
export type Base =
  { kind: "blank" } | { kind: "template"; id: string; linked: boolean | null; pool: string };

/** 장애 영역에 놓으면 VM보다 먼저 만드는 배치 규칙(`node_tag` `key=value`, hard면 node-affinity) */
export interface Zone {
  key: string;
  value: string;
  hard: boolean;
}

export interface BuilderState {
  values: VmFormValues;
  base: Base;
  zone: Zone | null;
  /** 새 디스크마다 볼륨 이름이 VM 이름을 따라가는지(살피기에서 이름을 고치면 거짓) */
  autoNames: boolean[];
  /** 마지막으로 얹은 OS 프리셋(`applyPreset`이 Windows를 떠날 때 장치를 돌리는 데 쓴다) */
  preset: Preset;
  /** 템플릿을 놓을 때 비운, 폼에서 고친 칸(배포가 싣지 못함 — 틀 위 칩이 알림) */
  cleared?: CarriedKey[];
}

/** 부품·놓기 규칙·문제가 보는 세상(목록은 watch로 늘 최신). 노드를 읽지 못하면 `nodes`가 null이다. */
export interface BuilderCtx {
  nodes: Node[] | null;
  pools: StoragePool[];
  volumes: Volume[];
  networks: Network[];
  securityGroups: SecurityGroup[];
  haGroups: HaGroup[];
  folders: Folder[];
  vms: Vm[];
  rules: PlacementRule[];
  /** `GET /cpu/models`(머신의 아키텍처). 아직 모르면 없음 */
  cpuModels?: CpuModelEntry[];
  /** `GET /placement`의 노드 부하(메모리 여유). 읽지 못하면 없음 */
  loads?: NodeLoadView[];
  /** 고른 폴더의 사용량과 할당량(`GET /folders/{id}/usage`). 읽지 못하면 없음 */
  usage?: components["schemas"]["FolderUsageView"];
  perms: Pick<Permissions, "has" | "can">;
}

export type { Reason };

/**
 * 처음 상태: 폼과 같은 `emptyForm`이되 디스크·NIC가 없는 빈 틀(첫 놓기가 "무엇을 붙이는지"를 보이게).
 */
export function initialState(machine: VmFormValues["machine"], folder = ""): BuilderState {
  return {
    values: { ...emptyForm(machine, folder), disks: [], nics: [] },
    base: { kind: "blank" },
    zone: null,
    autoNames: [],
    preset: "blank",
  };
}

/** 폼에서 넘겨받은 값(디스크·NIC 그대로). 새 디스크의 이름이 VM 이름을 따른 꼴이면 따라가기로 본다. */
export function fromValues(values: VmFormValues): BuilderState {
  return {
    values,
    base: { kind: "blank" },
    zone: null,
    autoNames: values.disks.map(
      (d, i) => d.kind === "new" && d.volume === emptyDisk(values.name, i).volume,
    ),
    preset: "blank",
  };
}

/** 따라가는 새 디스크의 볼륨 이름을 지금 VM 이름과 자리로 다시 짓는다. */
export function renumber(s: BuilderState): BuilderState {
  const disks = s.values.disks.map((d, i) =>
    s.autoNames[i] && d.kind === "new" ? { ...d, volume: emptyDisk(s.values.name, i).volume } : d,
  );
  return disks.every((d, i) => d === s.values.disks[i])
    ? s
    : { ...s, values: { ...s.values, disks } };
}

/** VM 이름을 바꾼다(따라가는 새 디스크 이름도). */
export function setName(s: BuilderState, name: string): BuilderState {
  return renumber({ ...s, values: { ...s.values, name } });
}

/** 되돌리기 쌓기(설계 §8.3 — 놓기·빼기·순서 바꾸기마다, 20개까지) */
export const UNDO_LIMIT = 20;

export interface History {
  past: BuilderState[];
  present: BuilderState;
}

export function record(h: History, next: BuilderState): History {
  return next === h.present
    ? h
    : { past: [...h.past, h.present].slice(-UNDO_LIMIT), present: next };
}

export function undo(h: History): History {
  const prev = h.past.at(-1);
  return prev ? { past: h.past.slice(0, -1), present: prev } : h;
}

/** 빌더가 그리고 고치는 폼 칸(설계 §13 — 폼에 새 칸이 생기면 시험이 어느 쪽인지 묻는다) */
export const DRAWN_KEYS = [
  "name",
  "sockets",
  "cores",
  "model",
  "memory",
  "machine",
  "folder",
  "disks",
  "cdroms",
  "nics",
  "cloudInit",
  "node",
  "ha",
  "haState",
  "haGroup",
  "maxRestart",
  "maxRelocate",
  "haPriority",
  "guestWatch",
  "guestWatchSecs",
  "tpm",
  "secureBoot",
  "guestAgent",
  "pinned",
  "start",
] as const satisfies readonly (keyof VmFormValues)[];

/** 빌더가 그리지 않고 들고 다니는 칸(폼에서 고침 — JSON·CLI·만들기에 그대로 들어감) */
export const CARRIED_KEYS = [
  "cpuFlags",
  "maxVcpus",
  "maxMemory",
  "numa",
  "firmware",
  "restart",
  "snapshotSchedule",
  "replication",
  "agentFreeze",
  "agentShutdown",
] as const satisfies readonly (keyof VmFormValues)[];

export type CarriedKey = (typeof CARRIED_KEYS)[number];

/** 템플릿 배포 요청(`CloneRequest`)에 싣지 않는 들고 다니는 칸 — 템플릿을 놓으면 비운다(CPU 플래그만 싣는다). */
export const NOT_DEPLOYED: readonly CarriedKey[] = CARRIED_KEYS.filter((k) => k !== "cpuFlags");

/** 폼에서 고친(처음 값과 다른) 들고 다니는 칸 — 틀 위 "폼에서 고친 칸 n개" */
export function formOnly(v: VmFormValues): CarriedKey[] {
  const empty = emptyForm(v.machine, v.folder);
  return CARRIED_KEYS.filter((k) => JSON.stringify(v[k]) !== JSON.stringify(empty[k]));
}
