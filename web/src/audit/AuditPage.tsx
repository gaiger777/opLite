import { useQuery } from "@tanstack/react-query";
import { FileSearch, RefreshCw } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { AuditEntry, Task } from "@/api/types";
import { auditOutcome, auditTarget, describeAudit } from "@/audit/describe";
import { usePrincipal } from "@/auth/AuthProvider";
import { CopyButton } from "@/components/CopyButton";
import { focusBack } from "@/components/focusBack";
import { type Column, DataTable } from "@/components/data-table/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { errorMessage } from "@/i18n/errors";
import { formatTime } from "@/lib/format";
import { useTaskHref } from "@/tasks/taskLink";

/** 한 쪽에 그리는 감사 줄 수(계획 77 실험실 — 로그인 줄이 쌓이면 화면이 수천 px로 길어졌다). */
const AUDIT_PAGE_SIZE = 50;

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[7rem_1fr] gap-x-3 text-sm max-sm:grid-cols-1">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

/**
 * 결과 칸: 성공·실패·미리 보기, 202는 "시작함"과 그 작업(서랍 링크 — 지금 상태: 진행 중·성공·실패). 작업 목록에서 이미 지워진
 * 작업은 "작업 보기"(서랍이 없다고 알린다).
 */
function OutcomeText({ entry, tasks }: { entry: AuditEntry; tasks: ReadonlyMap<string, Task> }) {
  const { t } = useTranslation();
  const taskHref = useTaskHref();
  const outcome = auditOutcome(entry, t);
  if (!outcome) {
    return <>—</>;
  }
  const task = outcome.task ? tasks.get(outcome.task) : undefined;
  return (
    <span className={outcome.failed ? "font-medium text-destructive" : undefined}>
      {outcome.text}
      {outcome.task && (
        <>
          {" · "}
          <Link
            to={taskHref(outcome.task)}
            className="underline underline-offset-4"
            // 줄을 누르면 항목 창이 열린다 — 링크는 작업 서랍만
            onClick={(e) => e.stopPropagation()}
          >
            {task
              ? t("auditOps.ui.taskState", { state: t(`tasks.state.${task.status.state}`) })
              : t("auditOps.ui.taskOpen")}
          </Link>
        </>
      )}
    </span>
  );
}

/** 항목 하나를 사람 말로: 결과·대상·누가·언제·어디서·걸린 시간·자세히, 요청 원문(복사) */
function EntrySummary({
  entry,
  tasks,
  names,
}: {
  entry: AuditEntry;
  tasks: ReadonlyMap<string, Task>;
  names: ReadonlyMap<string, string>;
}) {
  const { t, i18n } = useTranslation();
  const outcome = auditOutcome(entry, t);
  const raw = entry.method ? `${entry.method} /api/v1${entry.path}` : entry.action;
  const target =
    entry.target && `${entry.target.kind}/${names.get(entry.target.key) ?? entry.target.key}`;
  return (
    <dl className="flex flex-col gap-1">
      {outcome && (
        <Row label={t("auditOps.ui.result")}>
          <OutcomeText entry={entry} tasks={tasks} />
          {outcome.started && (
            <span className="block text-xs text-muted-foreground">
              {t("auditOps.ui.startedHint")}
            </span>
          )}
        </Row>
      )}
      {target && (
        <Row label={t("auditOps.ui.target")}>
          <code className="font-mono text-xs break-all">{target}</code>
        </Row>
      )}
      <Row label={t("auditOps.ui.who")}>
        {entry.user
          ? `${entry.user} · ${t(`auditOps.ui.auth.${entry.auth}`)}`
          : entry.method
            ? t("auditOps.ui.auth.none")
            : t("auditOps.ui.byNode")}
      </Row>
      <Row label={t("auditOps.ui.when")}>{formatTime(new Date(entry.time), i18n.language)}</Row>
      <Row label={t("auditOps.ui.where")}>
        {entry.ip
          ? t("auditOps.ui.whereValue", { ip: entry.ip, node: entry.node })
          : t("auditOps.ui.whereNode", { node: entry.node })}
      </Row>
      {entry.method && (
        <Row label={t("auditOps.ui.took")}>
          {t("auditOps.ui.tookValue", { ms: entry.duration_ms })}
        </Row>
      )}
      {entry.detail && <Row label={t("auditOps.ui.detail")}>{entry.detail}</Row>}
      <Row label={t("auditOps.ui.raw")}>
        <span className="inline-flex items-start gap-1">
          <code className="font-mono text-xs break-all">{raw}</code>
          <CopyButton text={raw} label={t("auditOps.ui.copyRaw")} />
        </span>
      </Row>
    </dl>
  );
}

/** 고를 수 있는 시간 범위(지금부터 거꾸로, 밀리초) */
const RANGES = { "1h": 3_600_000, "24h": 86_400_000, "7d": 604_800_000 } as const;
type Range = keyof typeof RANGES;

/**
 * 모든 노드의 감사 로그. admin만 읽는다. 자동으로 다시 읽지 않고, 범위·거르기를 바꾸거나 새로고침할 때
 * 읽는다(그때의 지금부터 범위를 잰다).
 */
export function AuditPage() {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const admin = me.role === "admin";
  const [range, setRange] = useState<Range>("24h");
  // 입력한 값과 적용한 값. 적용 버튼을 눌러야 쿼리에 들어간다.
  const [user, setUser] = useState("");
  const [action, setAction] = useState("");
  const [filter, setFilter] = useState({ user: "", action: "" });
  const [opened, setOpened] = useState<AuditEntry | null>(null);
  const returnFocus = useRef<HTMLElement | null>(null);
  // id로 부른 VM(콘솔 열기 등)·폴더(저장 이름이 id — 화면 이름은 spec.name)를 이름으로
  const vms = useList("vm");
  const folders = useList("folder");
  const names = useMemo(
    () =>
      new Map([
        ...(vms.data ?? []).map((vm) => [vm.id, vm.name] as const),
        ...(folders.data ?? []).map((f) => [f.id, f.spec.name] as const),
      ]),
    [vms.data, folders.data],
  );
  // 202가 시작한 작업의 지금 상태(이미 받는 작업 목록)
  const taskList = useList("task");
  const tasks = useMemo(
    () => new Map((taskList.data ?? []).map((task) => [task.id, task])),
    [taskList.data],
  );
  const audit = useQuery({
    queryKey: ["audit", range, filter],
    queryFn: () =>
      call(
        api.GET("/audit", {
          params: {
            query: {
              since: String(Date.now() - RANGES[range]),
              ...(filter.user && { user: filter.user }),
              ...(filter.action && { action: filter.action }),
            },
          },
        }),
      ),
    enabled: admin,
  });

  // 메뉴에 없지만 주소로 직접 들어올 수 있다. admin이 아니면 읽지 않고 안내한다.
  if (!admin) {
    return (
      <div className="flex flex-col gap-4">
        <PageHeader title={t("nav.audit")} help="audit" intro={t("help.audit.intro")} />
        <p className="text-sm text-muted-foreground">{t("audit.adminOnly")}</p>
      </div>
    );
  }

  const columns: Column<AuditEntry>[] = [
    {
      id: "time",
      header: t("audit.time"),
      className: "whitespace-nowrap",
      cell: (entry) => {
        const time = new Date(entry.time);
        return <time dateTime={time.toISOString()}>{formatTime(time, i18n.language)}</time>;
      },
    },
    {
      id: "user",
      header: t("audit.user"),
      cell: (entry) => entry.user ?? (entry.method ? "—" : t("auditOps.ui.byNode")),
    },
    {
      id: "action",
      header: t("audit.action"),
      className: "whitespace-normal",
      cell: (entry) => {
        // 사람 말(라우트 → 문구 표, 계획 77-7). 표에 없는 동작은 원문 그대로. 원문·전체는 줄을 열어 본다.
        const named = describeAudit(entry, t, names);
        return (
          <>
            {/* 키보드로도 열 수 있게 단추로 둔다. 누르면 행의 onClick이 연다. */}
            <button
              type="button"
              className={
                named
                  ? "text-left underline-offset-4 hover:underline"
                  : "text-left font-mono text-xs underline-offset-4 hover:underline"
              }
            >
              {named ?? entry.action}
            </button>
            {/* 대상이 없는 줄의 짧은 설명(로그인의 username=…, 버린 수) */}
            {entry.detail && (
              <span className="block text-xs break-words text-muted-foreground">
                {entry.detail}
              </span>
            )}
          </>
        );
      },
    },
    {
      // 무엇에 한 일인가: 동작 이름에 대상이 없을 때만(폴더 지우기, 권한 거두기, 표에 없는 동작)
      id: "target",
      header: t("auditOps.ui.target"),
      className: "font-mono text-xs break-all",
      priority: 2,
      cell: (entry) => auditTarget(entry, describeAudit(entry, t, names), names) ?? "",
    },
    {
      id: "status",
      header: t("auditOps.ui.result"),
      cell: (entry) => <OutcomeText entry={entry} tasks={tasks} />,
    },
    { id: "node", header: t("common.node"), priority: 2, cell: (entry) => entry.node },
    {
      id: "ip",
      header: t("audit.ip"),
      className: "font-mono text-xs",
      priority: 3,
      cell: (entry) => entry.ip || "—",
    },
  ];

  const entries = audit.data?.entries ?? [];
  const unreachable = audit.data?.unreachable ?? [];
  const disabled = audit.data?.disabled ?? [];
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="audit-title"
        title={t("nav.audit")}
        help="audit"
        intro={t("help.audit.intro")}
        actions={
          <Button
            variant="outline"
            disabled={audit.isFetching}
            onClick={() => void audit.refetch()}
          >
            <RefreshCw />
            {t("audit.refresh")}
          </Button>
        }
      />
      <div className="flex flex-wrap items-end gap-4">
        <div className="flex flex-col gap-2">
          <Label htmlFor="audit-range">{t("audit.range")}</Label>
          <NativeSelect
            id="audit-range"
            value={range}
            onChange={(e) => setRange(e.target.value as Range)}
          >
            {(Object.keys(RANGES) as Range[]).map((r) => (
              <NativeSelectOption key={r} value={r}>
                {t(`audit.ranges.${r}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </div>
        <form
          noValidate
          className="flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            setFilter({ user: user.trim(), action: action.trim() });
          }}
        >
          <div className="flex flex-col gap-2">
            <Label htmlFor="audit-user">{t("audit.user")}</Label>
            <Input id="audit-user" value={user} onChange={(e) => setUser(e.target.value)} />
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="audit-action">{t("audit.action")}</Label>
            <Input
              id="audit-action"
              placeholder={t("auditOps.ui.actionPlaceholder")}
              title={t("auditOps.ui.actionHint")}
              value={action}
              onChange={(e) => setAction(e.target.value)}
            />
          </div>
          <Button type="submit" variant="outline">
            {t("audit.apply")}
          </Button>
        </form>
      </div>
      {/* 다시 읽지 못하면 마지막으로 읽은 내용을 그대로 보이고 오류를 알린다. */}
      {audit.isError && entries.length > 0 && (
        <p role="alert" className="text-sm text-destructive">
          {t("audit.refreshFailed", { error: errorMessage(t, audit.error) })}
        </p>
      )}
      {unreachable.length > 0 && (
        <p
          role="alert"
          className="rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm"
        >
          {t("audit.unreachable", { nodes: unreachable.join(", ") })}
        </p>
      )}
      {disabled.length > 0 && (
        <p role="status" className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm">
          {t("audit.disabled", { nodes: disabled.join(", ") })}
        </p>
      )}
      {audit.data?.truncated && (
        <p role="status" className="text-sm text-muted-foreground">
          {t("audit.truncated", { count: entries.length })}
        </p>
      )}
      <DataTable
        id="audit"
        rows={entries}
        columns={columns}
        rowId={(entry) => entry.id}
        query={audit}
        empty={t("audit.empty")}
        emptyState={
          <EmptyState
            icon={FileSearch}
            title={t("audit.empty")}
            description={t("help.audit.empty")}
            help="audit.page"
          />
        }
        labelledBy="audit-title"
        keys
        pageSize={AUDIT_PAGE_SIZE}
        rowProps={(entry) => ({
          className: "cursor-pointer",
          onClick: (e) => {
            returnFocus.current = e.currentTarget.querySelector("button");
            setOpened(entry);
          },
        })}
      />
      {opened && (
        <Dialog open onOpenChange={(open) => !open && setOpened(null)}>
          <DialogContent
            aria-describedby={undefined}
            className="sm:max-w-2xl"
            onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
          >
            <DialogHeader>
              <DialogTitle className="break-words">
                {describeAudit(opened, t, names) ?? t("audit.entryTitle")}
              </DialogTitle>
            </DialogHeader>
            <EntrySummary entry={opened} tasks={tasks} names={names} />
            <details>
              <summary className="cursor-pointer text-sm">{t("auditOps.ui.rawJson")}</summary>
              <pre className="mt-2 max-h-80 overflow-auto rounded-md bg-muted p-2 font-mono text-xs">
                {JSON.stringify(opened, null, 2)}
              </pre>
            </details>
            <DialogFooter>
              <Button variant="outline" onClick={() => setOpened(null)}>
                {t("common.done")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
    </div>
  );
}
