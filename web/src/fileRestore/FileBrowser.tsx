import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { cn } from "cn";
import {
  ChevronDown,
  ChevronRight,
  Download,
  File,
  FileQuestion,
  Folder,
  FolderUp,
  Link2,
  Undo2,
} from "lucide-react";
import {
  type KeyboardEvent,
  memo,
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import { usePermissions } from "@/auth/usePermissions";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { startDownload } from "@/fileRestore/download";
import {
  downloadFormat,
  entryKey,
  entryPath,
  type EntryView,
  fileRestoreError,
  MAX_PATHS,
  modeString,
  PAGE_SIZE,
  type Part,
  partOf,
  pathQuery,
  RENDER_STEP,
  REPORT_FILE,
  type Session,
  sessionKey,
  volumeLabel,
  visibleName,
  visiblePath,
  type VolumeView,
} from "@/fileRestore/fileRestore";
import { type RestoreItem, RestoreToVmDialog } from "@/fileRestore/RestoreToVmDialog";
import { SafeName } from "@/fileRestore/SafeName";
import { formatBytes, formatTime } from "@/lib/format";

const KIND_ICON = { dir: Folder, file: File, symlink: Link2, other: FileQuestion } as const;

/**
 * 오른쪽 파일 표(설계 §14): 이동 경로, 이 디렉터리 안 이름 거르기, 이름·크기·수정 시각·모드·소유자, 쪽 넘김(아래에 닿으면 다음
 * 쪽 — 거르는 동안은 단추로만), 심볼릭 링크 대상, 체크 상자 여러 고르기, 동작 띠(내려받기·VM으로 복원). 키보드: 화살표로 행
 * 옮기기, Enter로 들어가기, Backspace로 위로, Space로 고르기. 큰 디렉터리는 `RENDER_STEP`씩 그린다.
 */
export function FileBrowser({
  session,
  volume,
  originalVm,
}: {
  session: Session;
  volume: VolumeView;
  /** 백업의 VM id(VM으로 복원의 기본) */
  originalVm?: string;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const [parts, setParts] = useState<Part[]>([]);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [focus, setFocus] = useState(0);
  const [limit, setLimit] = useState(RENDER_STEP);
  const [restoring, setRestoring] = useState(false);
  const [busy, setBusy] = useState(false);
  const rows = useRef<(HTMLTableRowElement | null)[]>([]);
  const box = useRef<HTMLDivElement | null>(null);
  const alertRef = useRef<HTMLParagraphElement | null>(null);
  const sentinel = useRef<HTMLDivElement | null>(null);
  const restoreButton = useRef<HTMLButtonElement | null>(null);
  const tooManyId = useId();
  // 옮긴 뒤 새 목록이 오면 키보드 초점을 둘 행: null이면 첫 행, 글이면 그 열쇠의 행(위로 가면 온 폴더), undefined면 그대로
  const focusAfterLoad = useRef<string | null | undefined>(undefined);
  const where = pathQuery(parts);
  const entries = useInfiniteQuery({
    queryKey: ["file-restore", "entries", session.id, volume.id, where],
    queryFn: ({ pageParam }) =>
      call(
        api.GET("/file-restore/sessions/{id}/volumes/{volume}/entries", {
          params: {
            path: { id: session.id, volume: volume.id },
            query: { ...where, ...(pageParam && { cursor: pageParam }), limit: PAGE_SIZE },
          },
        }),
      ),
    initialPageParam: "",
    getNextPageParam: (last) => last.next ?? undefined,
    retry: false,
    staleTime: Infinity,
  });
  // 세션을 다시 읽거나 시계가 바뀌어도 행을 다시 그리지 않게 목록은 데이터가 바뀔 때만 새로 만든다.
  const all = useMemo(() => entries.data?.pages.flatMap((p) => p.entries) ?? [], [entries.data]);
  const needle = filter.trim().toLowerCase();
  const visible = useMemo(
    () => (needle ? all.filter((e) => e.name.toLowerCase().includes(needle)) : all),
    [all, needle],
  );
  const shown = useMemo(() => visible.slice(0, limit), [visible, limit]);
  // 동작(내려받기·VM으로 복원)은 보이는 고른 행에만 한다. 거르기로 숨은 고른 행은 수만 알린다.
  const chosen = useMemo(
    () => visible.filter((e) => selected.has(entryKey(e))),
    [visible, selected],
  );
  const hiddenChosen = selected.size - chosen.length;
  const loaded = entries.data !== undefined;
  const error = entries.error;
  // 첫 쪽이 실패하면 표 대신 오류다. 다음 쪽이 실패하면 읽은 행은 두고 아래에 다시 시도 줄을 둔다.
  const firstError = !loaded ? error : null;
  const pageError = entries.isFetchNextPageError ? error : null;

  // 첫 나열이 볼륨을 붙였거나 붙이지 못했다 — 나무의 상태를 새로 읽는다. 끝난 세션이면 머리가 바뀐다.
  const firstPage = entries.isSuccess && volume.state === "unmounted";
  const ended = error instanceof ApiError && error.code === "session_closed";
  const mountFailed = error instanceof ApiError && error.code === "volume_not_mountable";
  useEffect(() => {
    if (firstPage || ended || mountFailed) {
      void queryClient.invalidateQueries({ queryKey: sessionKey(session.id) });
    }
  }, [firstPage, ended, mountFailed, queryClient, session.id]);

  // 아래 끝이 보이면 읽은 행을 더 그리고, 다 그렸으면 다음 쪽을 읽는다(브라우저가 지원할 때 — 없으면 단추). 거르는 동안과
  // 다음 쪽이 실패한 뒤에는 저절로 읽지 않는다(거르기가 디렉터리 전체를 읽거나 실패를 되풀이하지 않게 — 단추로만).
  const { hasNextPage, isFetchingNextPage, fetchNextPage } = entries;
  const canGrow = shown.length < visible.length;
  const autoFetch = hasNextPage && !needle && !pageError;
  const loadMore = useCallback(() => {
    if (canGrow) {
      setLimit((l) => l + RENDER_STEP);
    } else {
      void fetchNextPage();
    }
  }, [canGrow, fetchNextPage]);
  useEffect(() => {
    const target = sentinel.current;
    if (
      !target ||
      !(canGrow || autoFetch) ||
      isFetchingNextPage ||
      typeof IntersectionObserver === "undefined"
    ) {
      return;
    }
    const observer = new IntersectionObserver((seen) => {
      if (seen.some((s) => s.isIntersecting)) {
        loadMore();
      }
    });
    observer.observe(target);
    return () => observer.disconnect();
  }, [canGrow, autoFetch, isFetchingNextPage, loadMore, shown.length]);

  // 옮긴 뒤 새 목록이 오면 초점을 표에 둔다(지운 행에 있던 초점이 문서로 빠지지 않게).
  // 새 폴더의 첫 쪽이 실패했으면 오류 글에 둔다(Backspace로 위로 갈 수 있게).
  useEffect(() => {
    const want = focusAfterLoad.current;
    if (want === undefined) {
      return;
    }
    if (firstError) {
      focusAfterLoad.current = undefined;
      alertRef.current?.focus();
      return;
    }
    if (!entries.data) {
      return;
    }
    focusAfterLoad.current = undefined;
    const at =
      want === null
        ? 0
        : Math.max(
            0,
            shown.findIndex((e) => entryKey(e) === want),
          );
    const row = shown.length > 0 ? rows.current[at] : null;
    (row ?? box.current)?.focus();
  }, [entries.data, shown, firstError]);

  const go = useCallback((next: Part[], from?: Part) => {
    setParts(next);
    setFilter("");
    setSelected(new Set());
    setFocus(0);
    setLimit(RENDER_STEP);
    focusAfterLoad.current = from ? entryKey(from) : null;
  }, []);

  const toggle = useCallback((entry: EntryView) => {
    const key = entryKey(entry);
    setSelected((prev) => {
      const next = new Set(prev);
      if (!next.delete(key)) {
        next.add(key);
      }
      return next;
    });
  }, []);

  const open = useCallback((entry: EntryView) => go([...parts, partOf(entry)]), [go, parts]);
  const up = useCallback(() => go(parts.slice(0, -1), parts.at(-1)), [go, parts]);

  const count = shown.length;
  const onRowKey = useCallback(
    (e: KeyboardEvent<HTMLTableRowElement>, entry: EntryView, i: number) => {
      // 행 안의 단추·체크 상자에서 누른 Enter·Space는 그 요소에 맡긴다.
      if (e.target !== e.currentTarget && (e.key === "Enter" || e.key === " ")) {
        return;
      }
      const move = (to: number) => {
        const next = Math.max(0, Math.min(count - 1, to));
        setFocus(next);
        rows.current[next]?.focus();
      };
      switch (e.key) {
        case "ArrowDown":
          move(i + 1);
          break;
        case "ArrowUp":
          move(i - 1);
          break;
        case "Home":
          move(0);
          break;
        case "End":
          move(count - 1);
          break;
        case "Enter":
          if (entry.kind === "dir") {
            open(entry);
          }
          break;
        case " ":
          toggle(entry);
          break;
        case "Backspace":
          if (parts.length > 0) {
            up();
          }
          break;
        default:
          return;
      }
      e.preventDefault();
    },
    [count, open, toggle, up, parts.length],
  );

  // 빈 폴더의 표 상자·첫 쪽 오류 글에 초점이 있을 때: Backspace로 위로, ↓·Home으로 첫 행에
  function onBoxKey(e: KeyboardEvent<HTMLElement>) {
    if (e.target !== e.currentTarget) {
      return;
    }
    if (e.key === "Backspace" && parts.length > 0) {
      up();
    } else if ((e.key === "ArrowDown" || e.key === "Home") && count > 0) {
      setFocus(0);
      rows.current[0]?.focus();
    } else {
      return;
    }
    e.preventDefault();
  }

  const setRow = useCallback((i: number, el: HTMLTableRowElement | null) => {
    rows.current[i] = el;
  }, []);

  async function download(wanted?: "tar.gz") {
    setBusy(true);
    try {
      // 고른 것이 없거나 (한도를 넘게) 다 골랐으면 지금 디렉터리 전체다(볼륨 루트도 된다).
      const each = chosen.length > 0 && !wholeFolder;
      const paths = each ? chosen.map((e) => entryPath([...parts, partOf(e)])) : [entryPath(parts)];
      const format = each ? downloadFormat(chosen, wanted) : (wanted ?? "zip");
      const link = await call(
        api.POST("/file-restore/sessions/{id}/download-link", {
          params: { path: { id: session.id } },
          body: { volume: volume.id, paths, format },
        }),
      );
      startDownload(`/api/v1${link.url}`);
      // 묶음(zip·tar.gz)은 건너뛴 항목이나 일찍 멈춘 까닭을 맨 위 보고서 파일에 적는다.
      toast.success(
        format === "auto"
          ? t("fileRestore.downloadStarted")
          : t("fileRestore.downloadStartedArchive", { report: REPORT_FILE }),
      );
    } catch (err) {
      toast.error(fileRestoreError(t, err));
      if (err instanceof ApiError && err.code === "session_closed") {
        void queryClient.invalidateQueries({ queryKey: sessionKey(session.id) });
      }
    } finally {
      setBusy(false);
    }
  }

  const root = volumeLabel(volume);
  const allChosen = visible.length > 0 && chosen.length === visible.length;
  // 서버는 한 번에 `MAX_PATHS`개까지 받는다. 다 읽은 폴더의 모든 항목을 (거르지 않고) 골랐으면 폴더 하나로 보내고, 아니면 막는다.
  const overLimit = chosen.length > MAX_PATHS;
  const wholeFolder = overLimit && !hasNextPage && !needle && chosen.length === all.length;
  const tooMany = overLimit && !wholeFolder;
  const canRestore = perms.anywhere("vm.config");
  const downloadLabel =
    chosen.length && !wholeFolder
      ? chosen.length === 1 && chosen[0]!.kind === "file"
        ? t("fileRestore.downloadFile")
        : t("fileRestore.downloadZip", { count: chosen.length })
      : t("fileRestore.downloadFolder");
  // VM으로 복원할 것: 고른 항목, 또는 폴더를 통째로 보낼 때는 그 폴더(부모 아래에 그 이름으로 — 볼륨 루트면 그 내용)
  const restoreParent = wholeFolder ? parts.slice(0, -1) : parts;
  const restoreItems: readonly RestoreItem[] = wholeFolder
    ? [{ part: parts.at(-1), kind: "dir" }]
    : chosen.map((e) => ({ part: partOf(e), kind: e.kind }));

  // 머리의 "모두 고르기"는 보이는 행만 넣고 뺀다(숨은 고른 행은 그대로 둔다).
  function selectAll() {
    setSelected((prev) => {
      const next = new Set(prev);
      for (const e of visible) {
        if (allChosen) {
          next.delete(entryKey(e));
        } else {
          next.add(entryKey(e));
        }
      }
      return next;
    });
  }

  return (
    <section aria-labelledby="fr-files" className="flex min-w-0 flex-col gap-3">
      <h2 id="fr-files" className="sr-only">
        {t("fileRestore.files")}
      </h2>
      <nav aria-label={t("fileRestore.folderPath")} className="min-w-0">
        <ol className="flex min-w-0 flex-wrap items-center gap-1 text-sm">
          <li className="flex items-center">
            {parts.length === 0 ? (
              <span aria-current="page" className="font-medium">
                {t(root.key, root.values)}
              </span>
            ) : (
              <button
                type="button"
                className="text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
                onClick={() => go([], parts[0])}
              >
                {t(root.key, root.values)}
              </button>
            )}
          </li>
          {parts.map((p, i) => (
            <li key={i} className="flex min-w-0 items-center gap-1">
              <ChevronRight aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
              {i === parts.length - 1 ? (
                <span
                  aria-current="page"
                  className="truncate font-medium"
                  title={visibleName(p.name)}
                >
                  <SafeName name={p.name} />
                </span>
              ) : (
                <button
                  type="button"
                  className="truncate text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
                  title={visibleName(p.name)}
                  onClick={() => go(parts.slice(0, i + 1), parts[i + 1])}
                >
                  <SafeName name={p.name} />
                </button>
              )}
            </li>
          ))}
        </ol>
      </nav>

      <div className="flex flex-wrap items-center gap-2">
        <Input
          type="search"
          aria-label={t("fileRestore.filter")}
          placeholder={t("fileRestore.filter")}
          className="h-8 w-full sm:w-56"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        {parts.length > 0 && (
          <Button variant="ghost" size="sm" onClick={up}>
            <FolderUp />
            {t("fileRestore.up")}
          </Button>
        )}
        <div className="ml-auto flex flex-wrap items-center gap-2">
          {selected.size > 0 && (
            <span className="text-sm text-muted-foreground" aria-live="polite">
              {t("fileRestore.selected", { count: chosen.length })}
              {hiddenChosen > 0 && ` ${t("fileRestore.selectedHidden", { count: hiddenChosen })}`}
            </span>
          )}
          {tooMany && (
            <span id={tooManyId} role="status" className="text-sm text-destructive">
              {t("fileRestore.tooMany", { max: MAX_PATHS })}
            </span>
          )}
          <div className="flex">
            <Button
              size="sm"
              className="rounded-r-none"
              disabled={busy || !loaded || tooMany}
              aria-describedby={tooMany ? tooManyId : undefined}
              onClick={() => void download()}
            >
              <Download />
              {downloadLabel}
            </Button>
            <DropdownMenu modal={false}>
              <DropdownMenuTrigger asChild>
                <Button
                  size="sm"
                  className="rounded-l-none border-l border-primary-foreground/20 px-2"
                  disabled={busy || !loaded || tooMany}
                  aria-label={t("fileRestore.downloadAs")}
                >
                  <ChevronDown />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onSelect={() => void download()}>
                  {chosen.length === 1 && chosen[0]!.kind === "file" && !wholeFolder
                    ? t("fileRestore.asFile")
                    : t("fileRestore.asZip")}
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => void download("tar.gz")}>
                  <span className="flex flex-col">
                    <span>{t("fileRestore.asTarGz")}</span>
                    <span className="text-xs text-muted-foreground">
                      {t("fileRestore.asTarGzHint")}
                    </span>
                  </span>
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
          {canRestore && (
            <Button
              ref={restoreButton}
              variant="outline"
              size="sm"
              disabled={chosen.length === 0 || tooMany}
              aria-describedby={tooMany ? tooManyId : undefined}
              title={chosen.length === 0 ? t("fileRestore.selectFirst") : undefined}
              onClick={() => setRestoring(true)}
            >
              <Undo2 />
              {t("fileRestore.toVm")}
            </Button>
          )}
        </div>
      </div>

      {firstError ? (
        <p
          ref={alertRef}
          role="alert"
          tabIndex={-1}
          className="text-sm text-destructive outline-none"
          onKeyDown={onBoxKey}
        >
          {fileRestoreError(t, firstError)}
        </p>
      ) : (
        <div
          ref={box}
          tabIndex={-1}
          onKeyDown={onBoxKey}
          className="min-w-0 overflow-x-auto rounded-lg border outline-none"
        >
          <Table aria-label={t("fileRestore.filesIn", { path: visiblePath(parts) })}>
            <TableHeader>
              <TableRow>
                <TableHead className="w-8">
                  <Checkbox
                    aria-label={t("fileRestore.selectAll")}
                    checked={allChosen ? true : chosen.length > 0 ? "indeterminate" : false}
                    disabled={visible.length === 0}
                    onCheckedChange={selectAll}
                  />
                </TableHead>
                <TableHead>{t("common.name")}</TableHead>
                <TableHead className="text-right">{t("fileRestore.size")}</TableHead>
                <TableHead className="hidden sm:table-cell">{t("fileRestore.modified")}</TableHead>
                <TableHead className="hidden md:table-cell">{t("fileRestore.mode")}</TableHead>
                <TableHead className="hidden lg:table-cell">{t("fileRestore.owner")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {entries.isPending && (
                <TableRow>
                  <TableCell colSpan={6} role="status" className="text-muted-foreground">
                    {t("common.loading")}
                  </TableCell>
                </TableRow>
              )}
              {loaded && visible.length === 0 && !pageError && (
                <TableRow>
                  <TableCell colSpan={6} className="text-muted-foreground">
                    {needle ? t("fileRestore.noMatch") : t("fileRestore.emptyDir")}
                  </TableCell>
                </TableRow>
              )}
              {shown.map((entry, i) => {
                const key = entryKey(entry);
                return (
                  <FileRow
                    key={key}
                    entry={entry}
                    index={i}
                    selected={selected.has(key)}
                    tabbable={i === Math.min(focus, shown.length - 1)}
                    onKey={onRowKey}
                    onToggle={toggle}
                    onOpen={open}
                    onFocusRow={setFocus}
                    setRow={setRow}
                  />
                );
              })}
              {pageError && (
                <TableRow>
                  <TableCell colSpan={6}>
                    <span className="flex flex-wrap items-center gap-2">
                      <span role="alert" className="text-sm text-destructive">
                        {t("fileRestore.pageFailed", { error: fileRestoreError(t, pageError) })}
                      </span>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={isFetchingNextPage}
                        onClick={() => void fetchNextPage()}
                      >
                        {isFetchingNextPage ? t("common.loading") : t("fileRestore.retry")}
                      </Button>
                    </span>
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </div>
      )}
      {/* 다음 쪽이 실패해도 읽은 행은 더 그릴 수 있다 — 다음 쪽 읽기만 다시 시도 줄에 맡긴다. */}
      {!firstError && (canGrow || (hasNextPage && !pageError)) && (
        <div ref={sentinel} className="flex flex-col items-center gap-1">
          <Button variant="outline" size="sm" disabled={isFetchingNextPage} onClick={loadMore}>
            {isFetchingNextPage
              ? t("common.loading")
              : canGrow
                ? t("fileRestore.showMore")
                : t("fileRestore.more")}
          </Button>
          {canGrow && (
            <span className="text-xs text-muted-foreground">
              {t("fileRestore.showing", { shown: shown.length, loaded: visible.length })}
            </span>
          )}
          {needle && hasNextPage && !pageError && (
            <span className="text-xs text-muted-foreground">{t("fileRestore.filterLoaded")}</span>
          )}
        </div>
      )}
      {loaded && <p className="text-xs text-muted-foreground">{t("fileRestore.keys")}</p>}
      {restoring && (
        <RestoreToVmDialog
          session={session}
          volume={volume}
          parent={restoreParent}
          items={restoreItems}
          originalVm={originalVm}
          returnFocus={restoreButton}
          onClose={() => setRestoring(false)}
        />
      )}
    </section>
  );
}

/**
 * 파일 표의 한 행. 세션을 다시 읽거나 시계가 바뀌어 위가 다시 그려도 이 행의 값이 같으면 다시 그리지 않는다(큰 디렉터리).
 */
const FileRow = memo(function FileRow({
  entry,
  index,
  selected,
  tabbable,
  onKey,
  onToggle,
  onOpen,
  onFocusRow,
  setRow,
}: {
  entry: EntryView;
  index: number;
  selected: boolean;
  tabbable: boolean;
  onKey(e: KeyboardEvent<HTMLTableRowElement>, entry: EntryView, i: number): void;
  onToggle(entry: EntryView): void;
  onOpen(entry: EntryView): void;
  onFocusRow(i: number): void;
  setRow(i: number, el: HTMLTableRowElement | null): void;
}) {
  const { t, i18n } = useTranslation();
  const Icon = KIND_ICON[entry.kind];
  const shownName = visibleName(entry.name);
  return (
    <TableRow
      ref={(el) => setRow(index, el)}
      tabIndex={tabbable ? 0 : -1}
      aria-selected={selected}
      data-state={selected ? "selected" : undefined}
      onFocus={(e) => e.target === e.currentTarget && onFocusRow(index)}
      onKeyDown={(e) => onKey(e, entry, index)}
      className="outline-none focus-visible:bg-muted"
    >
      <TableCell>
        <Checkbox
          tabIndex={-1}
          aria-label={t("fileRestore.select", { name: shownName })}
          checked={selected}
          onCheckedChange={() => onToggle(entry)}
        />
      </TableCell>
      <TableCell className="max-w-0 min-w-48">
        <span className="flex min-w-0 items-center gap-2">
          <Icon
            aria-hidden
            className={cn(
              "size-4 shrink-0",
              entry.kind === "dir" ? "text-brand" : "text-muted-foreground",
            )}
          />
          {entry.kind === "dir" ? (
            <button
              type="button"
              tabIndex={-1}
              title={shownName}
              className="truncate text-left font-medium underline-offset-4 hover:underline"
              onClick={() => onOpen(entry)}
            >
              <SafeName name={entry.name} />
            </button>
          ) : (
            <span className="truncate" title={shownName}>
              <SafeName name={entry.name} />
            </span>
          )}
          {entry.raw && (
            <span
              className="shrink-0 text-xs text-muted-foreground"
              title={t("fileRestore.rawNameHint")}
            >
              {t("fileRestore.rawName")}
            </span>
          )}
          {entry.kind === "symlink" && entry.link_target && (
            <span
              className="truncate text-xs text-muted-foreground"
              title={visibleName(entry.link_target)}
            >
              → <SafeName name={entry.link_target} />
            </span>
          )}
        </span>
      </TableCell>
      <TableCell className="text-right whitespace-nowrap tabular-nums">
        {entry.kind === "file" ? formatBytes(entry.size) : "—"}
      </TableCell>
      <TableCell className="hidden whitespace-nowrap sm:table-cell">
        {formatTime(new Date(entry.mtime * 1000), i18n.language)}
      </TableCell>
      <TableCell className="hidden font-mono text-xs md:table-cell">
        {modeString(entry.kind, entry.mode)}
      </TableCell>
      <TableCell className="hidden text-xs tabular-nums lg:table-cell">
        {entry.uid}:{entry.gid}
      </TableCell>
    </TableRow>
  );
});
