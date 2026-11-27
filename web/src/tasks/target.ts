// Task 대상과 경보 대상의 깊은 링크(설계 §5.4). 화면 경로는 `nav.ts`의 탭·상세 경로를 따른다.
import type { AlertSubject } from "@/alerts/alerts";
import type { Backup, Node, Task, Vm, VmImport, Volume } from "@/api/types";

/** Task 대상의 이름과 그 화면(모르면 없음) */
export interface TaskTarget {
  name: string;
  href?: string;
}

/** 이름과 종류만 아는 리소스(그 밖의 종류 — 이미 받은 목록이나 Task 레이블에서) */
export interface Named {
  name: string;
  /** 서버 종류 이름(`backup-store`, `apply-set` …) */
  kind: string;
}

/** 대상 id로 찾을 리소스(이미 받은 `useList` 목록) */
export interface TargetLookup {
  vm: Map<string, Vm>;
  volume: Map<string, Volume>;
  backup: Map<string, Backup>;
  import: Map<string, VmImport>;
  node: Map<string, Node>;
  /** 그 밖의 종류(네트워크·풀·저장소·적용 묶음·내보내기·인증서 …) — id → 이름·종류 */
  named?: Map<string, Named>;
}

/** 서버가 Task를 만들 때 단 레이블(계획 77): 요청한 사용자의 id·이름, 대상의 이름·종류(만들 때의 스냅숏) */
export const TASK_LABELS = {
  requestedBy: "requested-by",
  requestedByName: "requested-by-name",
  targetName: "target-name",
  targetKind: "target-kind",
} as const;

const enc = encodeURIComponent;

/** 이름과 종류로 갈 화면(모르는 종류는 없음). id는 서랍을 여는 목록(`?open=`)이 쓴다. */
export function kindHref(kind: string, id: string, name: string): string | undefined {
  switch (kind) {
    case "vm":
      return `/vms/${enc(name)}`;
    case "node":
      return `/nodes/${enc(name)}`;
    case "volume":
      return `/volumes?q=${enc(name)}`;
    case "backup":
      return `/backups?q=${enc(name)}`;
    case "vm-import":
      return `/imports/${enc(name)}`;
    case "backup-store":
      return `/backup-stores?open=${enc(id)}`;
    case "backup-job":
      return "/backup-jobs";
    case "apply-set":
      return `/apply-sets/${enc(name)}`;
    case "vm-export":
      return "/vm-exports";
    case "certificate":
      return "/settings/certificates";
    case "vmware-migration":
      return `/vmware-migrations/${enc(name)}`;
    case "vmware-vm":
      return "/vmware-migrations";
    case "network":
      return "/networks";
    case "storage-pool":
      return `/storage-pools?q=${enc(name)}`;
    case "router":
      return `/routers?open=${enc(id)}`;
    case "load-balancer":
      return `/load-balancers?open=${enc(id)}`;
    case "recovery-plan":
      return `/dr-plans/${enc(name)}`;
    case "dr-run":
      return `/dr-runs/${enc(id)}`;
    case "remote":
      return `/remotes/${enc(name)}`;
    case "ha-group":
      return "/ha-groups";
    case "ceph":
      return "/ceph";
    case "sdn":
      return "/sdn";
    default:
      return undefined;
  }
}

/** VM 상세에서 그 Task를 볼 탭 */
function vmTab(task: Task): string | undefined {
  const kind = task.spec.op.kind;
  if (kind.startsWith("snapshot-")) {
    return "snapshots";
  }
  if (kind === "vm-backup" || kind === "backup-replicate" || kind === "backup-live-restore") {
    return "backups";
  }
  if (kind === "vm-firmware-reset" || kind === "vm-hotplug") {
    return "hardware";
  }
  return undefined;
}

/** 노드 상세에서 그 Task를 볼 탭(계획 77-6 노드 탭): CPU 다시 읽기는 하드웨어, SDN 작업은 네트워크 */
function nodeTab(task: Task): string | undefined {
  const kind = task.spec.op.kind;
  if (kind === "node-cpu-probe") {
    return "hardware";
  }
  return kind.startsWith("sdn-") ? "network" : undefined;
}

/**
 * Task 대상의 이름과 화면 경로(`?tab=`까지). VM은 상세의 알맞은 탭, 노드는 노드 상세, 볼륨·백업은 목록 검색,
 * 가져오기는 가져오기 상세, Ceph Task는 Ceph 화면이다. 지워진 대상은 id의 끝(UUIDv7의 무작위 부분)을 이름으로 한다.
 */
export function taskTarget(task: Task, lookup: TargetLookup): TaskTarget {
  const op = task.spec.op;
  const vm = lookup.vm.get(task.spec.target);
  if (vm) {
    const tab = vmTab(task);
    return { name: vm.name, href: `/vms/${enc(vm.name)}${tab ? `?tab=${tab}` : ""}` };
  }
  // 복원·가져오기의 대상은 Task가 끝날 때 만들어지는 새 VM이다. 그 전에는 요청한 이름을 보인다.
  if (op.kind === "backup-restore" || op.kind === "backup-live-restore") {
    return { name: op.name };
  }
  // 파일 복원 세션(계획 69)의 대상은 세션 id다. 백업과 그 찾아보기 화면을 보인다.
  if (op.kind === "file-restore-session") {
    const backup = lookup.backup.get(op.backup);
    return backup
      ? { name: backup.name, href: `/backups/${enc(backup.id)}/files` }
      : { name: op.backup.slice(-8) };
  }
  if (op.kind === "vm-import") {
    return { name: op.run.name };
  }
  const imp = lookup.import.get(task.spec.target);
  if (imp) {
    return { name: imp.name, href: `/imports/${enc(imp.name)}` };
  }
  const volume = lookup.volume.get(task.spec.target);
  if (volume) {
    return { name: volume.name, href: `/volumes?q=${enc(volume.name)}` };
  }
  const backup = lookup.backup.get(task.spec.target);
  if (backup) {
    return { name: backup.name, href: `/backups?q=${enc(backup.name)}` };
  }
  // 노드 업데이트 확인·업그레이드·비우기·끄기, HA 검사의 대상은 노드다.
  const node = lookup.node.get(task.spec.target);
  if (node) {
    const tab = nodeTab(task);
    return { name: node.name, href: `/nodes/${enc(node.name)}${tab ? `?tab=${tab}` : ""}` };
  }
  // 그 밖의 종류(이미 받은 목록)
  const named = lookup.named?.get(task.spec.target);
  if (named) {
    return {
      name: named.name,
      href: kindHref(named.kind, task.spec.target, named.name) ?? fallbackHref(task),
    };
  }
  // 찾지 못하면(지운 대상, 받지 않은 목록) 서버가 만들 때 적어 둔 이름(계획 77). 지운 VM·볼륨·백업은 화면이 없다.
  const labelled = task.labels?.[TASK_LABELS.targetName];
  if (labelled) {
    const kind = task.labels?.[TASK_LABELS.targetKind] ?? "";
    const gone = ["vm", "volume", "backup", "vm-import", "node"].includes(kind);
    return {
      name: labelled,
      href: gone ? undefined : (kindHref(kind, task.spec.target, labelled) ?? fallbackHref(task)),
    };
  }
  return { name: task.spec.target.slice(-8), href: fallbackHref(task) };
}

/** 대상을 모를 때 Task 종류로 고른 화면 */
function fallbackHref(task: Task): string | undefined {
  const op = task.spec.op;
  // 저장소 Task(계획 68)의 대상은 저장소 리소스다.
  if (op.kind.startsWith("backup-store-")) {
    return `/backup-stores?open=${enc(task.spec.target)}`;
  }
  // VMware 이전 Task(계획 72)의 대상은 이전 항목, 내보내기 지우기는 내보내기다.
  if (op.kind.startsWith("vmware-")) {
    return "/vmware-migrations";
  }
  if (op.kind === "vm-export-delete") {
    return "/vm-exports";
  }
  // 선언 적용(계획 73)의 대상은 적용 묶음(없으면 계획), ACME 주문은 인증서다.
  if (op.kind === "apply") {
    return op.set ? "/apply-sets" : "/history";
  }
  if (op.kind === "acme-order") {
    return "/settings/certificates";
  }
  return op.kind.startsWith("ceph-") ? "/ceph" : op.kind.startsWith("sdn-") ? "/sdn" : undefined;
}

/** Task와 이어진 대상(대상 말고): 만든 백업, 복원한 백업, 적용한 묶음, 주문한 인증서, 원격 클러스터 */
export interface RelatedLink {
  /** `tasks.drawer.related.<kind>` 번역 키의 끝 */
  kind: "backup" | "sourceBackup" | "applySet" | "plan" | "certificate" | "remote" | "store";
  name: string;
  href?: string;
}

export function relatedLinks(task: Task, lookup: TargetLookup): RelatedLink[] {
  const op = task.spec.op;
  const out: RelatedLink[] = [];
  const backupLink = (kind: "backup" | "sourceBackup", id: string) => {
    const b = lookup.backup.get(id);
    out.push(
      b ? { kind, name: b.name, href: `/backups?q=${enc(b.name)}` } : { kind, name: id.slice(-8) },
    );
  };
  switch (op.kind) {
    case "vm-backup":
      backupLink("backup", op.backup);
      break;
    case "backup-restore":
    case "backup-live-restore":
    case "file-restore-session":
    case "file-restore-to-guest":
      backupLink("sourceBackup", op.backup);
      break;
    case "backup-copy":
    case "backup-convert":
      out.push({ kind: "store", name: op.store, href: "/backup-stores" });
      break;
    case "backup-replicate":
      out.push({ kind: "remote", name: op.remote, href: `/remotes/${enc(op.remote)}` });
      break;
    case "apply": {
      // 묶음이 대상이면(늘 그렇다 — 계획 73) 대상 줄이 이미 그 묶음이다. 아니면 대상 이름 레이블은 묶음이 아니라
      // 대상의 이름이므로 대신 쓰지 않는다(계획 77-1 리뷰 S3).
      if (op.set && op.set !== task.spec.target) {
        const set = lookup.named?.get(op.set)?.name;
        out.push(
          set
            ? { kind: "applySet", name: set, href: `/apply-sets/${enc(set)}` }
            : { kind: "applySet", name: op.set.slice(-8), href: "/apply-sets" },
        );
      }
      out.push({ kind: "plan", name: op.plan.slice(-8), href: "/history" });
      break;
    }
    case "acme-order":
      out.push({
        kind: "certificate",
        name: lookup.named?.get(op.certificate)?.name ?? op.certificate.slice(-8),
        href: "/settings/certificates",
      });
      break;
  }
  return out;
}

/**
 * 경보 대상의 화면: 노드·VM은 상세(포트가 있는 노드 경보는 지도의 그 포트), 풀은 목록 검색, 백업 작업·Task는 그 목록, 라우터·부하 분산기는 그 서랍을 연 목록,
 * SDN은 SDN 보기(계획 67), 백업은 목록 검색, 백업 저장소는 그 서랍을 연 저장소 목록(계획 68)
 */
export function alertHref(subject: AlertSubject): string {
  switch (subject.kind) {
    case "node":
      // 링크 경보(계획 75 리뷰 1)는 지도의 그 포트로
      return subject.port
        ? `/cluster/map?focus=${enc(`port:${subject.name}/${subject.port}`)}`
        : `/nodes/${enc(subject.name)}`;
    case "vm":
      return `/vms/${enc(subject.name)}`;
    case "pool":
      return `/storage-pools?q=${enc(subject.name)}`;
    case "backup_job":
      return "/backup-jobs";
    case "task":
      return "/tasks";
    case "sdn":
      return "/sdn";
    case "router":
      return `/routers?open=${enc(subject.id)}`;
    case "load_balancer":
      return `/load-balancers?open=${enc(subject.id)}`;
    case "backup":
      return `/backups?q=${enc(subject.name)}`;
    case "backup_store":
      return `/backup-stores?open=${enc(subject.id)}`;
    // 인증서·적용 묶음(계획 73): 관리 › 보안·인증서 › 인증서, 기록 › 코드로 관리(YAML)의 그 묶음
    case "certificate":
      return "/settings/certificates";
    case "apply_set":
      return `/apply-sets/${enc(subject.name)}`;
    // 복구 계획(계획 76): 백업·복구 › DR(재해 복구)의 그 계획(개요 탭이 보호 상태·보고)
    case "recovery_plan":
      return `/dr-plans/${enc(subject.name)}`;
  }
}
