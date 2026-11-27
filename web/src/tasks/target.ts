// Task 대상과 경보 대상의 깊은 링크(설계 §5.4). 화면 경로는 `nav.ts`의 탭·상세 경로를 따른다.
import type { AlertSubject } from "@/alerts/alerts";
import type { Backup, Node, Task, Vm, VmImport, Volume } from "@/api/types";

/** Task 대상의 이름과 그 화면(모르면 없음) */
export interface TaskTarget {
  name: string;
  href?: string;
}

/** 대상 id로 찾을 리소스(이미 받은 `useList` 목록) */
export interface TargetLookup {
  vm: Map<string, Vm>;
  volume: Map<string, Volume>;
  backup: Map<string, Backup>;
  import: Map<string, VmImport>;
  node: Map<string, Node>;
}

const enc = encodeURIComponent;

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
    return { name: node.name, href: `/nodes/${enc(node.name)}` };
  }
  const name = task.spec.target.slice(-8);
  // 저장소 Task(계획 68)의 대상은 저장소 리소스다.
  if (op.kind.startsWith("backup-store-")) {
    return { name, href: `/backup-stores?open=${enc(task.spec.target)}` };
  }
  // VMware 이전 Task(계획 72)의 대상은 이전 항목, 내보내기 지우기는 내보내기다.
  if (op.kind.startsWith("vmware-")) {
    return { name, href: "/vmware-migrations" };
  }
  if (op.kind === "vm-export-delete") {
    return { name, href: "/vm-exports" };
  }
  // 선언 적용(계획 73)의 대상은 적용 묶음(없으면 계획), ACME 주문은 인증서다.
  if (op.kind === "apply") {
    return { name, href: op.set ? "/apply-sets" : "/history" };
  }
  if (op.kind === "acme-order") {
    return { name, href: "/settings/certificates" };
  }
  return op.kind.startsWith("ceph-")
    ? { name, href: "/ceph" }
    : op.kind.startsWith("sdn-")
      ? { name, href: "/sdn" }
      : { name };
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
    // 인증서·적용 묶음(계획 73): 설정 › 보안 › 인증서, 활동 › 감사 › 선언 묶음의 그 묶음
    case "certificate":
      return "/settings/certificates";
    case "apply_set":
      return `/apply-sets/${enc(subject.name)}`;
    // 복구 계획(계획 76): 데이터 보호 › DR의 그 계획(개요 탭이 보호 상태·보고)
    case "recovery_plan":
      return `/dr-plans/${enc(subject.name)}`;
  }
}
