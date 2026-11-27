import { hashKey, type QueryClient } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { toast } from "sonner";

import { type List, listKey } from "@/api/live";
import type { Task } from "@/api/types";
import { formatBytes } from "@/lib/format";
import { UNDO_MS } from "@/lib/undo";
import { isCenterOpen, openTask, setCenterOpen } from "@/tasks/center";

/** 결과를 기다리는 Task: id → 알림에 쓸 설명과 만든 revision */
const pending = new Map<string, { label: string; revision: number }>();

let tracked = () => {};

/** 지켜볼 Task가 생기면 부를 함수를 정한다(`TaskToasts`). 해제 함수를 돌려준다. */
export function onTrack(listener: () => void) {
  tracked = listener;
  return () => {
    // 그 사이 다른 리스너로 바뀌었으면 그대로 둔다.
    if (tracked === listener) {
      tracked = () => {};
    }
  };
}

/** 지켜보던 Task를 모두 잊고 그 알림을 닫는다. 로그아웃하거나 세션이 끝나면 부른다. */
export function resetTasks() {
  for (const id of pending.keys()) {
    toast.dismiss(id);
  }
  pending.clear();
  setCenterOpen(false);
}

/**
 * API가 202로 돌려준 Task의 진행을 알림으로 보여 준다. 결과는 watch로 들어온 Task 목록에서
 * `settleTasks`가 찾는다.
 */
export function trackTask(task: Task, label: string) {
  pending.set(task.id, { label, revision: task.revision });
  // 열린 작업·알림 센터가 진행을 보여 주므로 도는 알림은 띄우지 않는다.
  if (!isCenterOpen()) {
    toast.loading(label, { id: task.id });
  }
  tracked();
}

/** 결과 알림에 달 단추(정지 뒤 "다시 켜기") */
export interface TaskToastAction {
  label: string;
  onClick(): void;
}

/**
 * 끝난 Task의 알림을 결과로 바꾼다. Task 목록이 바뀔 때마다 부른다. 만든 뒤에 읽은 목록에 없는 Task는
 * watch가 끊긴 사이 정리 작업이 지운 것이므로 결과를 알 수 없다고 알린다. 작업·알림 센터가 열려 있으면 단추 없는
 * 성공 알림은 띄우지 않는다(센터에 보인다, 설계 §5.4). 실패 알림에는 그 Task의 상세 서랍을 여는 "보기"를 단다(계획 77). `actionFor`는
 * 성공 알림에 달 단추를 정한다(정지 뒤 "다시 켜기", §5.8).
 */
export function settleTasks(
  list: List<Task>,
  t: TFunction,
  actionFor: (task: Task) => TaskToastAction | undefined = () => undefined,
) {
  const byId = new Map(list.items.map((task) => [task.id, task]));
  for (const [id, { label, revision }] of pending) {
    const task = byId.get(id);
    if (task === undefined) {
      // 목록의 revision은 다시 읽을 때만 바뀌므로, 만들기 전에 읽은 목록이면 아직 들어오지 않은 것이다.
      if (list.revision >= revision) {
        pending.delete(id);
        toast.info(t("tasks.gone", { label }), { id });
      }
    } else if (task.status.state === "succeeded" && task.status.error) {
      // 성공했지만 남긴 경고가 있다(백업 지우기의 "원격 사본이 남았을 수 있음").
      pending.delete(id);
      toast.warning(t("tasks.doneWarning", { label, warning: task.status.error }), {
        id,
        duration: Infinity,
        closeButton: true,
      });
    } else if (task.status.state === "succeeded") {
      pending.delete(id);
      const action = actionFor(task);
      if (action) {
        toast.success(t("tasks.done", { label }), { id, duration: UNDO_MS, action });
      } else if (isCenterOpen()) {
        toast.dismiss(id);
      } else {
        toast.success(t("tasks.done", { label }), { id });
      }
    } else if (task.status.state === "failed") {
      pending.delete(id);
      toast.error(t("tasks.failed", { label, error: task.status.error ?? "" }), {
        id,
        duration: Infinity,
        closeButton: true,
        // 그 Task의 상세 서랍(원문 오류 전체·대상·요청자, 계획 77)
        action: { label: t("center.view"), onClick: () => openTask(id) },
      });
    }
  }
}

/**
 * Task를 사람이 읽을 문장으로: "web-01 시작". `nodeName`은 노드 id의 이름(HA 복구의 죽은 노드)이고,
 * 모르면 id를 보인다.
 */
export function describeTask(
  task: Task,
  t: TFunction,
  target: string,
  nodeName: (id: string) => string | undefined = () => undefined,
): string {
  const op = task.spec.op;
  const from = op.kind === "vm-ha-recover" ? (nodeName(op.from) ?? op.from) : undefined;
  const remote = op.kind === "backup-replicate" ? op.remote : undefined;
  // 옮기기·복사(계획 68)의 대상 저장소
  const store = op.kind === "backup-convert" || op.kind === "backup-copy" ? op.store : undefined;
  const check = op.kind === "ha-check" ? op.check : undefined;
  const action = op.kind === "ha-check" ? t(`haChecks.actions.${op.action}`) : undefined;
  const bond = op.kind === "bond-degraded" ? op.bond : undefined;
  // 복제의 대상 노드, Ceph Task는 실행 노드(대상은 Ceph 리소스 하나뿐이다)
  const node =
    op.kind === "vm-replicate"
      ? (nodeName(op.node) ?? op.node)
      : op.kind.startsWith("ceph-") || op.kind.startsWith("sdn-")
        ? (nodeName(task.spec.node) ?? task.spec.node)
        : undefined;
  // 꺼졌거나 지운 OVN 중앙을 내쫓는 Task(리뷰 2 N-1·N-2): 실행 노드는 살아 있는 중앙, 내쫓는 노드는 이름(지웠으면 주소)
  const kicked = op.kind === "sdn-central-kick" ? (nodeName(op.node) ?? op.address) : undefined;
  // 단순 → SDN 옮기기(계획 67)의 걸음(start·rollback·finish — 번역한 낱말)
  const step = op.kind === "network-migrate" ? t(`migration.taskStep.${op.step}`) : undefined;
  const device = op.kind === "ceph-osd-add" ? op.device : undefined;
  const pool = op.kind === "ceph-pool-create" ? op.pool : undefined;
  const name = op.kind === "ceph-pool-create" ? op.name : undefined;
  const size = op.kind === "ceph-pool-create" ? op.size : undefined;
  // 클러스터 CPU 기준 기록(계획 63): 아키텍처와 앞·새 모델(없으면 "없음")
  const arch = op.kind === "cpu-baseline" ? op.arch : undefined;
  const previous = op.kind === "cpu-baseline" ? (op.previous ?? t("cpu.none")) : undefined;
  const current = op.kind === "cpu-baseline" ? (op.current ?? t("cpu.none")) : undefined;
  // 묶음 적용(계획 73)은 "YAML 적용 — 묶음 web-prod"(계획 77-7 — "web-prod 적용"이 무엇인지 몰랐다). 묶음 없는 적용
  // (되돌리기 계획)은 그대로.
  const key = op.kind === "apply" && op.set ? "apply-set" : op.kind;
  // 모르는 작업(새 서버)은 번역 키 대신 종류를 그대로 보여 준다.
  return t(`tasks.op.${key}`, {
    target,
    from,
    remote,
    store,
    check,
    action,
    bond,
    node,
    kicked,
    step,
    device,
    pool,
    name,
    size,
    arch,
    previous,
    current,
    defaultValue: `${op.kind} ${target}`,
  });
}

/**
 * 기록 Task가 남긴 설명(실행하지 않는 Task라 오류 칸에는 실패만 있다): HA 검사 대응의 설명과 옮긴 VM, CPU 기준 기록의 제한한 노드·영향 VM 수, 본딩
 * 포트의 상태. 없거나 오류와 같으면 null이다.
 */
export function describeDetail(task: Task, t: TFunction): string | null {
  const op = task.spec.op;
  if (op.kind === "ha-check") {
    const vms = op.vms ?? [];
    const parts = [op.detail, vms.length > 0 ? t("tasks.detail.vms", { vms: vms.join(", ") }) : ""];
    const text = parts.filter((p) => p !== "" && p !== task.status.error).join(" ");
    return text || null;
  }
  if (op.kind === "bond-degraded") {
    return op.ports.join(", ") || null;
  }
  if (op.kind === "cpu-baseline") {
    const limited = op.limited_by ?? [];
    const parts = [
      limited.length > 0 ? t("tasks.detail.limitedBy", { nodes: limited.join(", ") }) : "",
      op.running_vms ? t("tasks.detail.runningVms", { count: op.running_vms }) : "",
    ];
    return parts.filter(Boolean).join(" ") || null;
  }
  return null;
}

/** 바이트를 소수 한 자리 GiB 숫자로(`14.8`) */
function gib(bytes: number): string {
  return (bytes / 1024 ** 3).toFixed(1);
}

/** 바이트 진행률(%, 내림, 넘치면 100). 전체를 모르면 0. */
export function bytePercent(done: number, total: number): number {
  return total > 0 ? Math.floor((Math.min(done, total) * 100) / total) : 0;
}

/**
 * 마이그레이션 Task의 진행률(클러스터 지도도 씀): 바이트 수가 뜻 있는 것은 디스크 복사 단계뿐이다 — 메모리 단계는 서버가
 * 끝난 디스크 복사의 바이트(다 됨)를 그대로 두므로 퍼센트로 보이면 100%가 된다. 그 밖의 단계는 `null`.
 */
export function migrationPercent(progress: Task["status"]["progress"]): number | null {
  return progress?.stage === "copying-disks"
    ? bytePercent(progress.done_bytes, progress.total_bytes)
    : null;
}

/** 마이그레이션 단계의 글(퍼센트가 없는 단계): "메모리 옮기는 중". 모르는 단계는 `null`(날 이름을 보이지 않음). */
export function migrationStageText(stage: string, t: (key: string) => string): string | null {
  return stage === "migrating-memory" ? t("tasks.progress.migratingMemory") : null;
}

/**
 * 실행 중인 Task의 진행률 문구(로컬 디스크를 복사하는 마이그레이션과 복제): "디스크 복사 37% (14.8/40.0 GiB)",
 * "메모리 옮기는 중", "복제 12% (0.1/1.0 GiB)", 받기(체크섬 파일, "받는 중 40% (0.4/1.0 GiB)", 확인, 가져오기, 템플릿 만들기), Ceph Task의 단계("패키지 설치 중", 모르는 단계는 그 이름). 진행률이
 * 없거나 모르는 단계면 null이다.
 */
export function describeProgress(task: Task, t: TFunction): string | null {
  const progress = task.status.progress;
  if (progress?.stage === "copying-disks" || progress?.stage === "replicating") {
    const { done_bytes: done, total_bytes: total } = progress;
    const percent = bytePercent(done, total);
    return t(
      progress.stage === "replicating"
        ? "tasks.progress.replicating"
        : "tasks.progress.copyingDisks",
      { percent, done: gib(done), total: gib(total) },
    );
  }
  // URL에서 받기: 받은 양(전체 길이를 모르면 0), 그 뒤 확인·가져오기 단계
  if (progress?.stage === "downloading") {
    const { done_bytes: done, total_bytes: total } = progress;
    return total > 0
      ? t("tasks.progress.downloading", {
          percent: Math.floor((Math.min(done, total) * 100) / total),
          done: gib(done),
          total: gib(total),
        })
      : t("tasks.progress.downloadingUnknown", { done: gib(done) });
  }
  // 올린·받은 이미지 가져오기(계획 77-8): 원본 확인 → 변환(qemu-img 진행) → 마무리
  // 바이트는 원본 파일(압축된 qcow2·vmdk) 크기라 쓰는 양과 달라 퍼센트만 보인다(리뷰 77-8 S8).
  if (progress?.stage === "converting") {
    return t("tasks.progress.converting", {
      percent: bytePercent(progress.done_bytes, progress.total_bytes),
    });
  }
  if (progress?.stage === "checking" || progress?.stage === "finishing") {
    return t(`tasks.progress.${progress.stage}`);
  }
  // 체크섬 파일 받기(받기 전), 확인·가져오기, 받은 디스크로 템플릿 만들기(계획 58)
  if (
    progress?.stage === "checksum" ||
    progress?.stage === "verifying" ||
    progress?.stage === "importing" ||
    progress?.stage === "template"
  ) {
    return t(`tasks.progress.${progress.stage}`);
  }
  if (progress?.stage === "migrating-memory") {
    return migrationStageText(progress.stage, t);
  }
  // Ceph Task는 단계 이름만 알린다(바이트 0/0).
  if (progress && task.spec.op.kind.startsWith("ceph-")) {
    return t(`tasks.cephStage.${progress.stage}`, { defaultValue: progress.stage });
  }
  // 파일 복원(계획 69): 세션은 단계(부팅·준비·닫는 중), VM으로 되돌려 쓰기는 쓴 양
  if (progress && task.spec.op.kind === "file-restore-to-guest") {
    return t("tasks.progress.guestWriting", { done: formatBytes(progress.done_bytes) });
  }
  if (progress && task.spec.op.kind === "file-restore-session") {
    return t(`tasks.fileRestoreStage.${progress.stage}`, { defaultValue: progress.stage });
  }
  // SDN(OVN) Task도 단계 이름만 알린다.
  if (progress && task.spec.op.kind.startsWith("sdn-")) {
    return t(`tasks.sdnStage.${progress.stage}`, { defaultValue: progress.stage });
  }
  return null;
}

/** `waitForTask`가 기다리는 가장 긴 시간. 볼륨 만들기는 보통 몇 초 안에 끝난다. */
export const TASK_TIMEOUT_MS = 10 * 60 * 1000;

/**
 * watch로 들어온 Task 목록에서 그 Task가 끝나기를 기다린다. 실패하면 Task의 오류로, `signal`이
 * 중단되면 그 이유로, `TASK_TIMEOUT_MS`가 지나면 시간 초과로 거부한다.
 */
export function waitForTask(
  queryClient: QueryClient,
  id: string,
  signal?: AbortSignal,
): Promise<Task> {
  const key = hashKey(listKey("task"));
  return new Promise((resolve, reject) => {
    let unsubscribe = () => {};
    const onAbort = () => finish(() => reject(signal?.reason));
    const finish = (settle: () => void) => {
      unsubscribe();
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      settle();
    };
    const check = () => {
      const task = queryClient
        .getQueryData<List<Task>>(listKey("task"))
        ?.items.find((t) => t.id === id);
      if (task?.status.state === "succeeded") {
        finish(() => resolve(task));
      } else if (task?.status.state === "failed") {
        finish(() => reject(new Error(task.status.error ?? "task failed")));
      }
    };
    if (signal?.aborted) {
      reject(signal.reason);
      return;
    }
    signal?.addEventListener("abort", onAbort);
    const timer = setTimeout(
      () => finish(() => reject(new Error(`task ${id} did not finish in time`))),
      TASK_TIMEOUT_MS,
    );
    unsubscribe = queryClient.getQueryCache().subscribe((event) => {
      if (event.query.queryHash === key) {
        check();
      }
    });
    check();
  });
}
