import { useQuery } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useSearchParams } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Task } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { CopyButton } from "@/components/CopyButton";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import { serverSkew } from "@/fileRestore/fileRestore";
import { formatTime, uuidTime } from "@/lib/format";
import { CENTER_BUTTON, setTaskOpener, TASK_PARAM } from "@/tasks/center";
import { failureText } from "@/tasks/failures/failure";
import { FailureExplain } from "@/tasks/failures/FailureExplain";
import { useFailureStrings } from "@/tasks/failures/strings";
import { requesterName } from "@/tasks/taskLink";
import { relatedLinks, taskTarget, TASK_LABELS } from "@/tasks/target";
import { bytePercent, describeDetail, describeProgress, describeTask } from "@/tasks/tasks";
import { TaskStateBadge, useTargetLookup } from "@/tasks/TasksPage";

/** 밀리초를 "12초"·"3분 5초"·"2시간 3분"으로 */
export function formatElapsed(ms: number, t: TFunction): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) {
    return t("tasks.drawer.secs", { s });
  }
  if (s < 3600) {
    return t("tasks.drawer.mins", { m: Math.floor(s / 60), s: s % 60 });
  }
  return t("tasks.drawer.hours", { h: Math.floor(s / 3600), m: Math.floor((s % 3600) / 60) });
}

/**
 * 서버가 이름 레이블을 달기 전(옛 Task)의 대상·요청자 이름을 찾으려고, 서랍을 열 때만 그 목록을 읽는다(읽을 수 있을 때).
 * 화면들과 같은 쿼리 키라 이미 받았으면 다시 읽지 않고, `useTargetLookup`이 그 캐시에서 찾는다.
 */
function useNamesFor(task: Task) {
  const perms = usePermissions();
  const kind = task.spec.op.kind;
  const op = task.spec.op;
  const labels = task.labels ?? {};
  const unnamed = !labels[TASK_LABELS.targetName];
  // 묶음 이름이 필요할 때만: 이름 레이블이 없는 옛 Task, 묶음이 대상이 아닌 적용(이어진 대상 줄 — 리뷰 S9)
  const needsSet = op.kind === "apply" && (unnamed || (!!op.set && op.set !== task.spec.target));
  useQuery({
    queryKey: ["apply-sets"],
    queryFn: () => call(api.GET("/apply-sets")),
    enabled: needsSet && perms.reads("apply-set"),
    staleTime: 60_000,
  });
  useQuery({
    queryKey: ["backup-stores"],
    queryFn: () => call(api.GET("/backup-stores")),
    enabled: unnamed && kind.startsWith("backup-store-") && perms.reads("backup-store"),
    staleTime: 60_000,
  });
  useQuery({
    queryKey: ["users"],
    queryFn: () => call(api.GET("/users")),
    enabled:
      !labels[TASK_LABELS.requestedByName] &&
      !!labels[TASK_LABELS.requestedBy] &&
      perms.reads("user"),
    staleTime: 60_000,
  });
}

/**
 * 서버 시계와 이 브라우저 시계의 차이(ms, 서버 − 브라우저). 도는 Task의 걸린 시간은 서버 시각(`started_at`)에서 재므로,
 * 그 Task를 한 번 읽은 응답의 `Date` 머리로 잰다(리뷰 S4). 끝난 Task(서버 시각끼리)에는 읽지 않는다.
 */
function useServerSkew(id: string, enabled: boolean): number {
  const query = useQuery({
    queryKey: ["task-clock", id],
    queryFn: async () => {
      const { response } = await api.GET("/tasks/{key}", { params: { path: { key: id } } });
      return serverSkew(response.headers.get("date")) * 1000;
    },
    enabled,
    staleTime: Infinity,
    retry: false,
  });
  return query.data ?? 0;
}

/** 지금 시각(ms). `ticking`이면 1초마다 다시 그린다(도는 Task의 걸린 시간). */
function useNow(ticking: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!ticking) {
      return;
    }
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [ticking]);
  return now;
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[8rem_1fr] gap-x-3 text-sm max-sm:grid-cols-1">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

/**
 * Task 상세(계획 77, 진단 §3.4): 무엇을·누가·언제·어디서, 상태·진행, 오류 원문 전체(복사), 이어진 대상. 옛 Task(서버가
 * 시각·이름을 남기지 않음)는 아는 것만 보인다.
 */
export function TaskDetail({ task }: { task: Task }) {
  const { t, i18n } = useTranslation();
  useNamesFor(task);
  const lookup = useTargetLookup();
  const target = taskTarget(task, lookup);
  const node = lookup.node.get(task.spec.node);
  const nodeName = (id: string) => lookup.node.get(id)?.name;
  const label = describeTask(task, t, target.name, nodeName);
  const detail = describeDetail(task, t);
  const progress = describeProgress(task, t);
  const related = relatedLinks(task, lookup);
  const { state, error, started_at: started, finished_at: finished } = task.status;
  const created = uuidTime(task.id);
  const time = (ms: number) => formatTime(new Date(ms), i18n.language);
  const ticking = started !== undefined && finished === undefined;
  const now = useNow(ticking) + useServerSkew(task.id, ticking);
  // 옛 Task(시작 시각 없음)는 걸린 시간을 모른다(만든 시각부터 재면 기다린 시간이 섞인다). 시작과 끝을 다른 노드가
  // 적어 시계가 어긋나 음수면 보이지 않는다(리뷰 S4).
  const took = started !== undefined ? (finished ?? now) - started : -1;
  const elapsed = took >= 0 ? formatElapsed(took, t) : null;
  const p = task.status.progress;
  const pct = p && p.total_bytes > 0 ? bytePercent(p.done_bytes, p.total_bytes) : null;
  const kindName = task.labels?.[TASK_LABELS.targetKind];
  // 원인 코드가 있으면 사전을 받아 "무엇이 / 왜 / 이렇게 하세요"를 보이고 원문은 접는다(계획 77-2). 없으면 원문 그대로.
  const stringsReady = useFailureStrings(!!task.status.failure);
  const explain = stringsReady ? failureText(task, t) : null;
  const raw = error && (
    <>
      <pre
        className={
          "max-h-80 overflow-y-auto rounded-md border bg-muted p-2 font-mono text-xs break-words whitespace-pre-wrap " +
          (state === "succeeded" ? "text-warning-fg" : "text-danger-fg")
        }
      >
        {error}
      </pre>
      {state === "failed" && (
        <p className="text-xs text-muted-foreground">{t("tasks.drawer.rawHint")}</p>
      )}
    </>
  );
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
        <TaskStateBadge task={task} />
        {elapsed && <span>{elapsed}</span>}
        <span>
          · {t("tasks.drawer.requestedBy", { name: requesterName(task, t, lookup.named) })}
        </span>
      </div>
      {explain && <FailureExplain task={task} text={explain} />}
      {error && explain && (
        <details className="flex flex-col gap-1.5 text-sm">
          <summary className="cursor-pointer text-sm font-medium">{t("failureUi.raw")}</summary>
          <div className="mt-1.5 flex flex-col gap-1.5">
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs text-muted-foreground">
                {t("failureUi.code")} <code>{explain.code}</code>
              </span>
              <CopyButton text={error} label={t("tasks.drawer.copyError")} />
            </div>
            {raw}
          </div>
        </details>
      )}
      {error && !explain && (
        <section aria-labelledby="task-error" className="flex flex-col gap-1.5">
          <div className="flex items-center justify-between gap-2">
            <h3 id="task-error" className="text-sm font-medium">
              {state === "succeeded" ? t("tasks.warning") : t("tasks.error")}
            </h3>
            <CopyButton text={error} label={t("tasks.drawer.copyError")} />
          </div>
          {raw}
        </section>
      )}
      <dl className="flex flex-col gap-1.5">
        <Row label={t("tasks.drawer.what")}>
          {label} <code className="text-xs text-muted-foreground">({task.spec.op.kind})</code>
        </Row>
        <Row label={t("tasks.drawer.target")}>
          {target.href ? (
            <Link to={target.href} className="text-brand underline-offset-4 hover:underline">
              {target.name}
            </Link>
          ) : (
            target.name
          )}
          {kindName && (
            <span className="text-xs text-muted-foreground">
              {" "}
              ({t(`tasks.drawer.kinds.${kindName}`, { defaultValue: kindName })})
            </span>
          )}
        </Row>
        <Row label={t("tasks.drawer.node")}>
          {node ? (
            <Link
              to={`/nodes/${encodeURIComponent(node.name)}`}
              className="text-brand underline-offset-4 hover:underline"
            >
              {node.name}
            </Link>
          ) : (
            task.spec.node.slice(-8)
          )}
        </Row>
        <Row label={t("tasks.drawer.requester")}>{requesterName(task, t, lookup.named)}</Row>
        <Row label={t("tasks.drawer.created")}>{formatTime(created, i18n.language)}</Row>
        <Row label={t("tasks.drawer.started")}>
          {started !== undefined
            ? time(started)
            : state === "pending"
              ? t("tasks.drawer.waiting")
              : // 끝 시각만 있으면 시작하지 않고 끝났다(기다리다 정리 작업·SDN·Ceph가 끝냄 — 리뷰 I1). 둘 다 없으면 옛 노드.
                finished !== undefined
                ? t("tasks.drawer.notStarted")
                : t("tasks.drawer.notRecorded")}
        </Row>
        <Row label={t("tasks.drawer.finished")}>
          {finished !== undefined
            ? time(finished)
            : state === "pending" || state === "running"
              ? t("tasks.drawer.notYet")
              : t("tasks.drawer.notRecorded")}
        </Row>
        {elapsed && <Row label={t("tasks.drawer.elapsed")}>{elapsed}</Row>}
        {(progress || pct !== null) && (
          <Row label={t("tasks.drawer.progress")}>
            {progress}
            {pct !== null && (
              <div
                role="progressbar"
                aria-label={label}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={pct}
                className="mt-1 h-1.5 overflow-hidden rounded-full bg-muted"
              >
                <div className="h-full bg-brand" style={{ width: `${pct}%` }} />
              </div>
            )}
          </Row>
        )}
        {detail && <Row label={t("tasks.drawer.detail")}>{detail}</Row>}
        {related.map((r) => (
          <Row key={`${r.kind}-${r.name}`} label={t(`tasks.drawer.related.${r.kind}`)}>
            {r.href ? (
              <Link to={r.href} className="text-brand underline-offset-4 hover:underline">
                {r.name}
              </Link>
            ) : (
              r.name
            )}
          </Row>
        ))}
        <Row label={t("tasks.drawer.id")}>
          <span className="inline-flex items-center gap-1">
            <code className="text-xs break-all">{task.id}</code>
            <CopyButton text={task.id} label={t("tasks.drawer.copyId")} />
          </span>
        </Row>
      </dl>
    </div>
  );
}

/**
 * Task 상세 서랍(계획 77). 주소의 `?task=<id>`로 열리므로 작업 목록·센터·홈·VM 작업 탭·실패 알림 어디서 열어도 같고,
 * 주소를 나눌 수 있다. 셸(`AppLayout`)에 하나만 둔다. 열렸을 때만 목록을 읽는다(화면마다 늘 읽지 않게).
 */
export function TaskDrawer() {
  const [params, setParams] = useSearchParams();
  const id = params.get(TASK_PARAM);
  // 서랍을 열기 전 마지막으로 포커스를 받은 것: 닫을 때 그리로 돌려준다(서랍에는 여는 단추가 따로 없다 — 리뷰 S2).
  const lastFocus = useRef<Element | null>(null);
  useEffect(() => {
    if (id !== null) {
      return;
    }
    const track = (e: FocusEvent) => {
      lastFocus.current = e.target instanceof Element ? e.target : null;
    };
    document.addEventListener("focusin", track);
    return () => document.removeEventListener("focusin", track);
  }, [id]);
  useEffect(
    () =>
      setTaskOpener((next) =>
        setParams((prev) => {
          const p = new URLSearchParams(prev);
          p.set(TASK_PARAM, next);
          return p;
        }),
      ),
    [setParams],
  );
  if (id === null) {
    return null;
  }
  // 안의 링크로 다른 화면에 가면 주소에서 `task`가 빠져 저절로 닫힌다. 닫기는 기록을 바꿔 쓴다 — "뒤로"가 서랍을
  // 다시 열지 않게(리뷰 S1).
  const close = () =>
    setParams(
      (prev) => {
        const p = new URLSearchParams(prev);
        p.delete(TASK_PARAM);
        return p;
      },
      { replace: true },
    );
  return <OpenTaskDrawer id={id} onClose={close} opener={lastFocus} />;
}

function OpenTaskDrawer({
  id,
  onClose,
  opener,
}: {
  id: string;
  onClose(): void;
  opener: React.RefObject<Element | null>;
}) {
  const { t } = useTranslation();
  const tasks = useList("task");
  const task = useMemo(() => tasks.data?.find((x) => x.id === id), [tasks.data, id]);
  const lookup = useTargetLookup();
  const title = task
    ? describeTask(task, t, taskTarget(task, lookup).name, (n) => lookup.node.get(n)?.name)
    : t("tasks.drawer.title");
  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent
        className="w-full overflow-y-auto sm:max-w-xl"
        onCloseAutoFocus={(e) => {
          // 연 곳(링크)이 남아 있으면 그리로, 센터 안의 링크·알림처럼 사라졌으면 알림 종으로
          e.preventDefault();
          const back = opener.current;
          const target =
            back instanceof HTMLElement && back.isConnected && back !== document.body
              ? back
              : document.querySelector<HTMLElement>(CENTER_BUTTON);
          target?.focus();
        }}
      >
        <SheetTitle className="px-4 pt-4 pr-10 text-lg break-words">{title}</SheetTitle>
        <SheetDescription className="px-4">{t("tasks.drawer.description")}</SheetDescription>
        <div className="px-4 pb-4">
          {task ? (
            <TaskDetail task={task} />
          ) : (
            <p className="text-sm text-muted-foreground">
              {tasks.isPending ? t("common.loading") : t("tasks.drawer.gone")}
            </p>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}
