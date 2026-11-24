import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CircleAlert, CircleCheck, CircleX, FolderSearch, LoaderCircle } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "react-router";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import { EmptyState } from "@/components/EmptyState";
import { ResourceHeader } from "@/components/ResourceHeader";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { FileBrowser } from "@/fileRestore/FileBrowser";
import {
  clock,
  fileRestoreError,
  firstBrowsable,
  type Session,
  sessionKey,
  volumeBlocked,
} from "@/fileRestore/fileRestore";
import { VolumeTree } from "@/fileRestore/VolumeTree";
import { formatTime } from "@/lib/format";

/** 부팅 중 세션을 묻는 간격(설계 §14: 2초) */
const BOOT_POLL_MS = 2000;
/** 준비된 세션(닫힐 시각·볼륨 상태)을 다시 읽는 간격 */
const READY_POLL_MS = 20_000;

/** 지금 시각(초). `every` ms마다 바뀐다. */
function useNow(every: number): number {
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now() / 1000), every);
    return () => clearInterval(timer);
  }, [every]);
  return now;
}

/**
 * 백업 안 파일 찾아보기(계획 69 §14 웹, `/backups/:id/files`). 열면 세션을 연다(같은 사용자의 열린 세션이 있으면 서버가 그것을
 * 돌려준다). 세션은 화면을 떠나도 남는다 — 돌아오면 다시 쓰고, 쓰지 않으면 서버가 유휴 시간 뒤 닫는다(되돌려 쓰기·내려받기가 도는
 * 동안 닫지 않게). 머리의 "닫기"가 바로 닫는다.
 */
export function FileRestorePage() {
  const { t } = useTranslation();
  const { id = "" } = useParams();
  // "다시 열기"마다 새로 묻는다.
  const [attempt, setAttempt] = useState(0);
  const open = useQuery({
    queryKey: ["file-restore", "open", id, attempt],
    queryFn: () => call(api.POST("/backups/{key}/file-restore", { params: { path: { key: id } } })),
    retry: false,
    staleTime: Infinity,
    // 다시 오면 새로 연다(닫힌 세션을 붙잡지 않게).
    gcTime: 0,
  });
  const backups = useList("backup");
  const backup = backups.data?.find((b) => b.id === id);

  if (open.isError) {
    return (
      <div className="flex flex-col gap-(--gap)">
        <ResourceHeader
          name={backup?.name ?? id}
          title={t("fileRestore.title", { name: backup?.name ?? id })}
        />
        <EmptyState
          icon={CircleAlert}
          title={t("fileRestore.openFailed")}
          description={fileRestoreError(t, open.error)}
          action={
            <Button variant="outline" onClick={() => setAttempt((a) => a + 1)}>
              {t("fileRestore.reopen")}
            </Button>
          }
        />
      </div>
    );
  }
  if (!open.data) {
    return (
      <p role="status" className="text-muted-foreground">
        {t("fileRestore.opening")}
      </p>
    );
  }
  return (
    <SessionView
      key={open.data.session.id}
      initial={open.data.session}
      onReopen={() => setAttempt((a) => a + 1)}
    />
  );
}

/** 세션이 더는 없다는 답(404) */
function isGone(error: unknown): boolean {
  return error instanceof ApiError && error.status === 404;
}

function SessionView({ initial, onReopen }: { initial: Session; onReopen(): void }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey: sessionKey(initial.id),
    queryFn: () =>
      call(api.GET("/file-restore/sessions/{id}", { params: { path: { id: initial.id } } })),
    initialData: initial,
    initialDataUpdatedAt: 0,
    retry: false,
    refetchInterval: (q) => {
      const phase = q.state.data?.phase;
      return isGone(q.state.error)
        ? false
        : phase === "booting"
          ? BOOT_POLL_MS
          : phase === "ready"
            ? READY_POLL_MS
            : false;
    },
  });
  const session = query.data;
  const backups = useList("backup");
  const vms = useList("vm");
  const nodes = useList("node");
  const backup = backups.data?.find((b) => b.id === session.backup);
  const vm = vms.data?.find((v) => v.id === backup?.spec.vm);
  const node = nodes.data?.find((n) => n.id === session.node);
  const slow = node?.status.file_restore?.accel === "tcg";
  const now = useNow(session.phase === "booting" ? 1000 : 15_000);
  const [closing, setClosing] = useState(false);
  const [volumeId, setVolumeId] = useState<string | null>(null);
  // 세션이 없어졌으면(노드 재시작으로 세션을 잊음) 끝난 것으로 본다. 잠깐 닿지 않은 것은 다음 물음을 기다린다.
  const lost = isGone(query.error);
  const phase = lost ? "failed" : session.phase;
  const volumes = session.volumes ?? [];
  const chosen =
    volumes.find((v) => v.id === volumeId && !volumeBlocked(v)) ?? firstBrowsable(volumes);

  async function close() {
    setClosing(true);
    try {
      await call(
        api.DELETE("/file-restore/sessions/{id}", { params: { path: { id: session.id } } }),
      );
      await queryClient.invalidateQueries({ queryKey: sessionKey(session.id) });
    } catch (err) {
      toast.error(fileRestoreError(t, err));
    } finally {
      setClosing(false);
    }
  }

  const name = backup?.name ?? session.backup_name;
  const created = backup?.status.created_at;
  const meta = [
    vm?.name ?? backup?.spec.vm_name,
    created ? formatTime(new Date(created * 1000), i18n.language) : undefined,
  ]
    .filter(Boolean)
    .join(" · ");
  const left = session.expires_at > 0 ? Math.max(0, session.expires_at - now) : null;

  return (
    <div className="flex min-h-0 flex-col gap-(--gap)">
      <ResourceHeader
        name={name}
        title={t("fileRestore.title", { name })}
        meta={meta || undefined}
        status={<PhaseChip phase={phase} elapsed={now - session.opened_at} slow={slow} />}
        actions={
          (phase === "booting" || phase === "ready") && (
            <>
              {phase === "ready" && left !== null && (
                <span className="text-sm text-muted-foreground" title={t("fileRestore.idleHint")}>
                  {t("fileRestore.closesIn", { count: Math.ceil(left / 60) })}
                </span>
              )}
              <Button variant="outline" size="sm" disabled={closing} onClick={() => void close()}>
                {t("fileRestore.close")}
              </Button>
            </>
          )
        }
      />
      {phase === "booting" && (
        <EmptyState
          icon={LoaderCircle}
          title={t("fileRestore.booting", { time: clock(now - session.opened_at) })}
          description={t(slow ? "fileRestore.bootingSlowHint" : "fileRestore.bootingHint")}
        />
      )}
      {(phase === "failed" || phase === "closed") && (
        <EmptyState
          icon={phase === "failed" ? CircleX : FolderSearch}
          title={t(phase === "failed" ? "fileRestore.failed" : "fileRestore.closed")}
          description={lost ? fileRestoreError(t, query.error) : (session.error ?? undefined)}
          action={<Button onClick={onReopen}>{t("fileRestore.reopen")}</Button>}
        />
      )}
      {phase === "ready" && (
        <div className="grid min-h-0 gap-(--gap) md:grid-cols-[minmax(14rem,18rem)_1fr]">
          <VolumeTree volumes={volumes} chosen={chosen?.id} onChoose={setVolumeId} />
          {chosen ? (
            <FileBrowser
              key={chosen.id}
              session={session}
              volume={chosen}
              originalVm={backup?.spec.vm}
            />
          ) : (
            <EmptyState
              icon={CircleAlert}
              title={t("fileRestore.nothingToBrowse")}
              description={t("fileRestore.nothingToBrowseHint")}
            />
          )}
        </div>
      )}
    </div>
  );
}

/** 세션 상태 칩: 부팅 중(경과, 느린 노드 안내), 준비됨, 실패, 닫힘 */
function PhaseChip({
  phase,
  elapsed,
  slow,
}: {
  phase: Session["phase"];
  elapsed: number;
  slow: boolean;
}) {
  const { t } = useTranslation();
  switch (phase) {
    case "booting":
      return (
        <span className="flex items-center gap-2">
          <StatusChip tone="info" busy>
            {t("fileRestore.phase.booting", { time: clock(elapsed) })}
          </StatusChip>
          {slow && (
            <span className="text-xs text-muted-foreground">{t("fileRestore.slowNode")}</span>
          )}
        </span>
      );
    case "ready":
      return (
        <StatusChip tone="success" icon={CircleCheck}>
          {t("fileRestore.phase.ready")}
        </StatusChip>
      );
    case "failed":
      return (
        <StatusChip tone="danger" icon={CircleX}>
          {t("fileRestore.phase.failed")}
        </StatusChip>
      );
    case "closed":
      return <StatusChip tone="neutral">{t("fileRestore.phase.closed")}</StatusChip>;
  }
}
