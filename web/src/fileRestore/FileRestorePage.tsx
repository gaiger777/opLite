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
  serverSkew,
  type Session,
  sessionKey,
  volumeBlocked,
  type VolumeView,
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

/**
 * 다시 물어도 같을 답: 4xx(권한을 뺐으면 403, 기능이 꺼졌으면 409 …). 408·429는 잠깐의 것이다. 네트워크 오류·5xx·
 * `node_unreachable`(503)는 노드에 잠깐 닿지 않은 것으로 보고 다시 묻는다.
 */
function isFinal(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    error.status >= 400 &&
    error.status < 500 &&
    error.status !== 408 &&
    error.status !== 429
  );
}

/** 세션 보기와 서버 시계의 차이(초, 서버 − 브라우저 — 응답의 `Date`로 잰 것) */
type SessionData = Session & { skew?: number };

/**
 * 세션 노드에 닿지 못해 서버가 Raft로 만든 보기: 준비됨인데 볼륨이 없고 닫힐 시각을 모른다(서버 `raft_view`). 볼륨이 정말
 * 없는 세션이 아니다 — 지금 화면을 두고 다시 묻는다.
 */
function isFallback(s: Session | undefined): boolean {
  return s?.phase === "ready" && s.expires_at === 0 && !s.volumes?.length;
}

const NO_VOLUMES: readonly VolumeView[] = [];

function SessionView({ initial, onReopen }: { initial: Session; onReopen(): void }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  // 닫기를 보냈다. 서버가 닫힘(또는 실패)을 알릴 때까지 "닫는 중"으로 보이고 자주 묻는다.
  const [closeRequested, setCloseRequested] = useState(false);
  const query = useQuery<SessionData>({
    queryKey: sessionKey(initial.id),
    queryFn: async () => {
      const request = api.GET("/file-restore/sessions/{id}", {
        params: { path: { id: initial.id } },
      });
      const view = await call(request);
      const { response } = await request;
      return { ...view, skew: serverSkew(response.headers.get("date")) };
    },
    initialData: initial,
    initialDataUpdatedAt: 0,
    retry: false,
    refetchInterval: (q) => {
      const phase = q.state.data?.phase;
      if (isFinal(q.state.error) || (phase !== "booting" && phase !== "ready")) {
        return false;
      }
      // 부팅 중·닫는 중·노드에 닿지 않을 때는 자주 묻는다.
      return phase === "booting" || closeRequested || q.state.error || isFallback(q.state.data)
        ? BOOT_POLL_MS
        : READY_POLL_MS;
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
  // 서버 시각(초): 닫힐 시각·부팅 경과는 서버 시계로 센다(브라우저 시계가 어긋나도).
  const now = useNow(session.phase === "booting" ? 1000 : 15_000) + (session.skew ?? 0);
  const [volumeId, setVolumeId] = useState<string | null>(null);
  // 세션이 없어졌거나(노드 재시작으로 세션을 잊음) 더는 쓸 수 없으면(권한을 뺐음 …) 끝난 것으로 본다. 잠깐 닿지 않은 것은
  // 지금 화면을 두고 다시 묻는다.
  const lost = isFinal(query.error);
  const gone = isGone(query.error);
  const phase = lost ? "failed" : session.phase;
  const fallback = isFallback(session);
  const unreachable = (phase === "booting" || phase === "ready") && (fallback || !!query.error);
  const closingNow = closeRequested && (phase === "booting" || phase === "ready");
  // 노드가 알려 준 마지막 볼륨 목록. 노드에 잠깐 닿지 않는 동안에도 나무와 파일 표(고른 것·열린 대화상자)를 그대로 둔다.
  const [kept, setKept] = useState<readonly VolumeView[]>(NO_VOLUMES);
  const live = session.phase === "ready" && !fallback ? session.volumes : undefined;
  if (live && live !== kept) {
    setKept(live);
  }
  const volumes = live ?? (fallback ? kept : NO_VOLUMES);
  const chosen =
    volumes.find((v) => v.id === volumeId && !volumeBlocked(v)) ?? firstBrowsable(volumes);

  async function close() {
    setCloseRequested(true);
    try {
      await call(
        api.DELETE("/file-restore/sessions/{id}", { params: { path: { id: session.id } } }),
      );
    } catch (err) {
      setCloseRequested(false);
      toast.error(fileRestoreError(t, err));
      return;
    }
    void queryClient.invalidateQueries({ queryKey: sessionKey(session.id) });
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
        status={
          <PhaseChip
            phase={phase}
            closing={closingNow}
            elapsed={now - session.opened_at}
            slow={slow}
          />
        }
        actions={
          (phase === "booting" || phase === "ready") && (
            <>
              {phase === "ready" && left !== null && !closingNow && (
                <span className="text-sm text-muted-foreground" title={t("fileRestore.idleHint")}>
                  {t("fileRestore.closesIn", { count: Math.ceil(left / 60) })}
                </span>
              )}
              <Button
                variant="outline"
                size="sm"
                disabled={closeRequested}
                onClick={() => void close()}
              >
                {closeRequested ? t("fileRestore.closing") : t("fileRestore.close")}
              </Button>
            </>
          )
        }
      />
      {unreachable && (
        <p role="status" className="rounded-md bg-warning-soft px-3 py-2 text-sm text-warning-fg">
          {t("fileRestore.unreachable")}
        </p>
      )}
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
          title={t(
            lost && !gone
              ? "fileRestore.unusable"
              : phase === "failed"
                ? "fileRestore.failed"
                : "fileRestore.closed",
          )}
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
          ) : fallback ? (
            <EmptyState icon={LoaderCircle} title={t("fileRestore.waitingForNode")} />
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

/** 세션 상태 칩: 부팅 중(경과, 느린 노드 안내), 준비됨, 닫는 중, 실패, 닫힘 */
function PhaseChip({
  phase,
  closing,
  elapsed,
  slow,
}: {
  phase: Session["phase"];
  /** 닫기를 보냈고 서버가 아직 닫힘을 알리지 않았다 */
  closing: boolean;
  elapsed: number;
  slow: boolean;
}) {
  const { t } = useTranslation();
  if (closing) {
    return (
      <StatusChip tone="neutral" busy>
        {t("fileRestore.phase.closing")}
      </StatusChip>
    );
  }
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
