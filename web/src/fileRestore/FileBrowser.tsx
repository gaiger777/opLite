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
import { type KeyboardEvent, useEffect, useRef, useState } from "react";
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
  displayPath,
  downloadFormat,
  entryKey,
  entryPath,
  type EntryView,
  fileRestoreError,
  modeString,
  PAGE_SIZE,
  type Part,
  partOf,
  pathQuery,
  type Session,
  sessionKey,
  volumeLabel,
  type VolumeView,
} from "@/fileRestore/fileRestore";
import { RestoreToVmDialog } from "@/fileRestore/RestoreToVmDialog";
import { formatBytes, formatTime } from "@/lib/format";

const KIND_ICON = { dir: Folder, file: File, symlink: Link2, other: FileQuestion } as const;

/**
 * 오른쪽 파일 표(설계 §14): 이동 경로, 이 디렉터리 안 이름 거르기, 이름·크기·수정 시각·모드·소유자, 쪽 넘김(아래에 닿으면 다음
 * 쪽), 심볼릭 링크 대상, 체크 상자 여러 고르기, 동작 띠(내려받기·VM으로 복원). 키보드: 화살표로 행 옮기기, Enter로 들어가기,
 * Backspace로 위로, Space로 고르기.
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
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const [parts, setParts] = useState<Part[]>([]);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [focus, setFocus] = useState(0);
  const [restoring, setRestoring] = useState(false);
  const [busy, setBusy] = useState(false);
  const rows = useRef<(HTMLTableRowElement | null)[]>([]);
  const sentinel = useRef<HTMLDivElement | null>(null);
  const restoreButton = useRef<HTMLButtonElement | null>(null);
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
  const all = entries.data?.pages.flatMap((p) => p.entries) ?? [];
  const needle = filter.trim().toLowerCase();
  const visible = needle ? all.filter((e) => e.name.toLowerCase().includes(needle)) : all;
  const chosen = all.filter((e) => selected.has(entryKey(e)));
  const error = entries.error;

  // 첫 나열이 볼륨을 붙였거나 붙이지 못했다 — 나무의 상태를 새로 읽는다. 끝난 세션이면 머리가 바뀐다.
  const firstPage = entries.isSuccess && volume.state === "unmounted";
  const ended = error instanceof ApiError && error.code === "session_closed";
  const mountFailed = error instanceof ApiError && error.code === "volume_not_mountable";
  useEffect(() => {
    if (firstPage || ended || mountFailed) {
      void queryClient.invalidateQueries({ queryKey: sessionKey(session.id) });
    }
  }, [firstPage, ended, mountFailed, queryClient, session.id]);

  // 아래 끝이 보이면 다음 쪽을 읽는다(브라우저가 지원할 때 — 없으면 "더 보기" 단추).
  const { hasNextPage, isFetchingNextPage, fetchNextPage } = entries;
  useEffect(() => {
    const target = sentinel.current;
    if (!target || !hasNextPage || typeof IntersectionObserver === "undefined") {
      return;
    }
    const observer = new IntersectionObserver((seen) => {
      if (seen.some((s) => s.isIntersecting) && !isFetchingNextPage) {
        void fetchNextPage();
      }
    });
    observer.observe(target);
    return () => observer.disconnect();
  }, [hasNextPage, isFetchingNextPage, fetchNextPage]);

  function go(next: Part[]) {
    setParts(next);
    setFilter("");
    setSelected(new Set());
    setFocus(0);
  }

  function toggle(entry: EntryView) {
    const key = entryKey(entry);
    setSelected((prev) => {
      const next = new Set(prev);
      if (!next.delete(key)) {
        next.add(key);
      }
      return next;
    });
  }

  function onRowKey(e: KeyboardEvent<HTMLTableRowElement>, entry: EntryView, i: number) {
    // 행 안의 단추·체크 상자에서 누른 Enter·Space는 그 요소에 맡긴다.
    if (e.target !== e.currentTarget && (e.key === "Enter" || e.key === " ")) {
      return;
    }
    const move = (to: number) => {
      const next = Math.max(0, Math.min(visible.length - 1, to));
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
        move(visible.length - 1);
        break;
      case "Enter":
        if (entry.kind === "dir") {
          go([...parts, partOf(entry)]);
        }
        break;
      case " ":
        toggle(entry);
        break;
      case "Backspace":
        if (parts.length > 0) {
          go(parts.slice(0, -1));
        }
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  async function download(wanted?: "tar.gz") {
    setBusy(true);
    try {
      // 고른 것이 없으면 지금 디렉터리 전체다(볼륨 루트도 된다).
      const paths = chosen.length
        ? chosen.map((e) => entryPath([...parts, partOf(e)]))
        : [entryPath(parts)];
      const format = chosen.length ? downloadFormat(chosen, wanted) : (wanted ?? "zip");
      const link = await call(
        api.POST("/file-restore/sessions/{id}/download-link", {
          params: { path: { id: session.id } },
          body: { volume: volume.id, paths, format },
        }),
      );
      startDownload(`/api/v1${link.url}`);
      toast.success(t("fileRestore.downloadStarted"));
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
  const allChosen = visible.length > 0 && visible.every((e) => selected.has(entryKey(e)));
  const someChosen = visible.some((e) => selected.has(entryKey(e)));
  const canRestore = perms.anywhere("vm.config");
  const downloadLabel = chosen.length
    ? chosen.length === 1 && chosen[0]!.kind === "file"
      ? t("fileRestore.downloadFile")
      : t("fileRestore.downloadZip", { count: chosen.length })
    : t("fileRestore.downloadFolder");

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
                onClick={() => go([])}
              >
                {t(root.key, root.values)}
              </button>
            )}
          </li>
          {parts.map((p, i) => (
            <li key={i} className="flex min-w-0 items-center gap-1">
              <ChevronRight aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
              {i === parts.length - 1 ? (
                <span aria-current="page" className="truncate font-medium">
                  {p.name}
                </span>
              ) : (
                <button
                  type="button"
                  className="truncate text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
                  onClick={() => go(parts.slice(0, i + 1))}
                >
                  {p.name}
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
          <Button variant="ghost" size="sm" onClick={() => go(parts.slice(0, -1))}>
            <FolderUp />
            {t("fileRestore.up")}
          </Button>
        )}
        <div className="ml-auto flex flex-wrap items-center gap-2">
          {chosen.length > 0 && (
            <span className="text-sm text-muted-foreground" aria-live="polite">
              {t("fileRestore.selected", { count: chosen.length })}
            </span>
          )}
          <div className="flex">
            <Button
              size="sm"
              className="rounded-r-none"
              disabled={busy || !entries.isSuccess}
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
                  disabled={busy || !entries.isSuccess}
                  aria-label={t("fileRestore.downloadAs")}
                >
                  <ChevronDown />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onSelect={() => void download()}>
                  {chosen.length === 1 && chosen[0]!.kind === "file"
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
              disabled={chosen.length === 0}
              title={chosen.length === 0 ? t("fileRestore.selectFirst") : undefined}
              onClick={() => setRestoring(true)}
            >
              <Undo2 />
              {t("fileRestore.toVm")}
            </Button>
          )}
        </div>
      </div>

      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {fileRestoreError(t, error)}
        </p>
      ) : (
        <div className="min-w-0 overflow-x-auto rounded-lg border">
          <Table aria-label={t("fileRestore.filesIn", { path: displayPath(parts) })}>
            <TableHeader>
              <TableRow>
                <TableHead className="w-8">
                  <Checkbox
                    aria-label={t("fileRestore.selectAll")}
                    checked={allChosen ? true : someChosen ? "indeterminate" : false}
                    disabled={visible.length === 0}
                    onCheckedChange={() =>
                      setSelected(allChosen ? new Set() : new Set(visible.map(entryKey)))
                    }
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
              {entries.isSuccess && visible.length === 0 && (
                <TableRow>
                  <TableCell colSpan={6} className="text-muted-foreground">
                    {needle ? t("fileRestore.noMatch") : t("fileRestore.emptyDir")}
                  </TableCell>
                </TableRow>
              )}
              {visible.map((entry, i) => {
                const key = entryKey(entry);
                const Icon = KIND_ICON[entry.kind];
                const isSelected = selected.has(key);
                return (
                  <TableRow
                    key={key}
                    ref={(el) => {
                      rows.current[i] = el;
                    }}
                    tabIndex={i === Math.min(focus, visible.length - 1) ? 0 : -1}
                    aria-selected={isSelected}
                    data-state={isSelected ? "selected" : undefined}
                    onFocus={(e) => e.target === e.currentTarget && setFocus(i)}
                    onKeyDown={(e) => onRowKey(e, entry, i)}
                    className="outline-none focus-visible:bg-muted"
                  >
                    <TableCell>
                      <Checkbox
                        tabIndex={-1}
                        aria-label={t("fileRestore.select", { name: entry.name })}
                        checked={isSelected}
                        onCheckedChange={() => toggle(entry)}
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
                            className="truncate text-left font-medium underline-offset-4 hover:underline"
                            onClick={() => go([...parts, partOf(entry)])}
                          >
                            {entry.name}
                          </button>
                        ) : (
                          <span className="truncate">{entry.name}</span>
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
                          <span className="truncate text-xs text-muted-foreground">
                            → {entry.link_target}
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
              })}
            </TableBody>
          </Table>
        </div>
      )}
      {hasNextPage && (
        <div ref={sentinel} className="flex flex-col items-center gap-1">
          <Button
            variant="outline"
            size="sm"
            disabled={isFetchingNextPage}
            onClick={() => void fetchNextPage()}
          >
            {isFetchingNextPage ? t("common.loading") : t("fileRestore.more")}
          </Button>
          {needle && (
            <span className="text-xs text-muted-foreground">{t("fileRestore.filterLoaded")}</span>
          )}
        </div>
      )}
      {entries.isSuccess && (
        <p className="text-xs text-muted-foreground">{t("fileRestore.keys")}</p>
      )}
      {restoring && (
        <RestoreToVmDialog
          session={session}
          volume={volume}
          parent={parts}
          entries={chosen}
          originalVm={originalVm}
          returnFocus={restoreButton}
          onClose={() => setRestoring(false)}
        />
      )}
    </section>
  );
}
