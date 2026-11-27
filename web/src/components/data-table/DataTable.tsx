import {
  type ColumnDef,
  getCoreRowModel,
  getSortedRowModel,
  type SortingState,
  type Updater,
  useReactTable,
} from "@tanstack/react-table";
import { cn } from "cn";
import { ArrowDown, ArrowUp, ArrowUpDown, ChevronRight } from "lucide-react";
import {
  Fragment,
  type HTMLAttributes,
  type ReactNode,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { usePrincipal } from "@/auth/AuthProvider";
import { ColumnMenu, ViewMenu } from "@/components/data-table/Toolbar";
import { readCols, type TableState, type View, writeCols } from "@/components/data-table/state";
import { ACTION_ICONS } from "@/components/icons";
import { Pager, usePage } from "@/components/Pager";
import { tableMessage, TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Sheet, SheetContent } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { isTyping, letterKey } from "@/lib/shortcuts";

/** 표의 열 하나 */
export interface Column<T> {
  id: string;
  /** 머리 글. 열 고르기·휴대폰 카드의 이름도 된다. */
  header: string;
  cell(row: T): ReactNode;
  /** 주면 이 값으로 정렬할 수 있다. */
  value?(row: T): string | number;
  /** 칸의 클래스(행에 따라 다르면 함수) */
  className?: string | ((row: T) => string | undefined);
  /**
   * 1(늘), 2(1024px 미만에서 숨김), 3(1280px 미만에서 숨김), 4(열로는 안 보이고 펼침 행에만). 숨은 열은 행의 펼침 단추(▸)로
   * 행 아래에 보인다(계획 77-6 — 넓은 화면에서도 가로 스크롤 없이). 휴대폰 카드는 1만 보이고 나머지는 "자세히"로 편다.
   */
  priority?: 1 | 2 | 3 | 4;
  /** 행 동작(머리 글은 화면 읽기에만, 카드에서는 오른쪽 위). 숨기거나 정렬하지 않는다. */
  actions?: boolean;
}

/** 목록 거름(주소 쿼리 키 = `id`). `options`가 비면 고르는 칸 없이 화면이 값을 정한다. */
export interface Filter<T> {
  id: string;
  label: string;
  options: { value: string; label: string }[];
  test(row: T, value: string): boolean;
}

const PRIORITY_CLASS = { 1: "", 2: "max-lg:hidden", 3: "max-xl:hidden", 4: "hidden" } as const;
/** 펼침 행 안에서 그 열이 보일 폭(표에 열로 보이는 폭에서는 감춘다) */
const DETAIL_CLASS = { 1: "hidden", 2: "lg:hidden", 3: "xl:hidden", 4: "" } as const;
const PHONE = "(max-width: 767px)";
/** 목록 쿼리를 주지 않은 표 */
const NO_QUERY = { isPending: false, isError: false, error: null };
/** 한 글자 키가 이 안에서 눌렸으면 그 요소의 기본 동작에 맡긴다. */
const INTERACTIVE = "a, button, select, [role=checkbox], [role=switch], [role=menuitem]";

/** 휴대폰 폭이면 카드 목록으로 그린다(설계 §5.9). */
export function useIsPhone(): boolean {
  const [phone, setPhone] = useState(() => window.matchMedia(PHONE).matches);
  useEffect(() => {
    const media = window.matchMedia(PHONE);
    const change = () => setPhone(media.matches);
    media.addEventListener("change", change);
    return () => media.removeEventListener("change", change);
  }, []);
  return phone;
}

interface Props<T> {
  /** 저장 보기·열 설정의 이름(`views.{id}`·`cols.{id}`) */
  id: string;
  rows: readonly T[];
  columns: Column<T>[];
  rowId(row: T): string;
  /** 체크박스·미리 보기 단추의 이름에 넣을 행 이름(기본은 id) */
  rowName?(row: T): string;
  /** 처음 읽는 중이면 스켈레톤 행, 읽지 못했으면 그 이유 */
  query?: { isPending: boolean; isError: boolean; error: unknown };
  empty: string;
  /** 목록이 정말 비었을 때(읽었고 행이 0) 표 대신 보일 것 — 보통 `EmptyState`(그림·만들기·자세히, 계획 77-4) */
  emptyState?: ReactNode;
  labelledBy?: string;
  /** 주면 검색·정렬·거름·저장 보기를 쓴다(`useTableState`). */
  state?: TableState;
  /** 검색이 볼 글(소문자로 비교). 주면 검색 상자가 있다. */
  search?(row: T): string;
  searchLabel?: string;
  filters?: Filter<T>[];
  /** 코드에 둔 기본 보기 */
  views?: View[];
  /** 고르기 체크박스(Shift 범위)와 일괄 막대 */
  bulk?(rows: T[], clear: () => void): ReactNode;
  /** `Enter`·행 열기로 갈 상세 경로 */
  rowHref?(row: T): string;
  /** 미리 보기 서랍의 내용(`Space`·눈 단추). 서랍이 열린 채 `j`/`k`로 행을 옮긴다. */
  peek?(row: T): ReactNode;
  /** `j`/`k`/`x`/`Space`/`Enter`를 받는다(화면의 주 목록 하나에만). */
  keys?: boolean;
  /** 행의 속성. `selected`는 지금 고른 행이다(고른 행을 함께 끌기). */
  rowProps?(row: T, selected: readonly T[]): HTMLAttributes<HTMLTableRowElement> & { id?: string };
  /** 도구 줄 끝에 더할 것 */
  toolbar?: ReactNode;
  /** 주면 이만큼씩 페이지를 나눈다(계획 77-6 — 긴 목록을 한 쪽에 다 그리지 않게). 단축키·고르기는 지금 페이지 안에서다. */
  pageSize?: number;
}

/**
 * 공통 데이터 표(설계 §5.3): 주소에 둔 검색·정렬·거름, 저장 보기, 열 고르기, 스켈레톤, 좁은 화면의 열 숨기기와
 * 휴대폰 카드, 체크박스·Shift 범위 고르기와 일괄 막대, 미리 보기 서랍, 목록 단축키.
 */
export function DataTable<T>(props: Props<T>) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const phone = useIsPhone();
  const { state, filters = [], rowId, peek, rowHref, keys } = props;
  const rowName = props.rowName ?? rowId;
  const selectable = props.bulk !== undefined;
  const { user } = usePrincipal();
  const [hidden, setHidden] = useState(() => readCols(user, props.id).hidden);
  const [selected, setSelected] = useState<ReadonlySet<string>>(() => new Set());
  // 고른 행의 id(행이 바뀌거나 다시 정렬돼도 같은 행을 가리킨다)
  const [active, setActive] = useState<string | null>(null);
  const [peekOpen, setPeekOpen] = useState(false);
  // 펼친 행의 id(숨은 열을 행 아래에 보인다)
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const detailId = useId();
  // Shift 범위의 기준 행 id(색인을 두면 정렬·거름·상태가 바뀐 뒤 다른 행을 가리킨다)
  const anchor = useRef<string | null>(null);
  const rowEls = useRef(new Map<number, HTMLElement>());

  const q = state?.q.trim().toLowerCase() ?? "";
  const filterValues = filters.map((f) => state?.get(f.id) ?? "");
  const filtered = useMemo(
    () =>
      props.rows.filter(
        (row) =>
          (!q || !props.search || props.search(row).toLowerCase().includes(q)) &&
          filters.every((f, i) => !filterValues[i] || f.test(row, filterValues[i])),
      ),
    // 거름 정의는 화면이 그릴 때마다 새로 만든다. 값이 같으면 다시 거르지 않는다.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [props.rows, q, filterValues.join("\n")],
  );

  const peekColumn: Column<T> | null = props.peek
    ? {
        id: "peek",
        header: t("table.peek"),
        actions: true,
        cell: (row) => {
          const Icon = ACTION_ICONS.peek;
          return (
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t("table.peekNamed", { name: rowName(row) })}
              onClick={() => openPeek(row)}
            >
              <Icon />
            </Button>
          );
        },
      }
    : null;
  const allColumns = [
    ...props.columns.filter((c) => !c.actions),
    ...(peekColumn ? [peekColumn] : []),
    ...props.columns.filter((c) => c.actions),
  ];
  const defs: ColumnDef<T>[] = allColumns.map((c) => ({
    id: c.id,
    header: c.header,
    accessorFn: c.value,
    enableSorting: state !== undefined && c.value !== undefined,
  }));
  const sorting: SortingState =
    state?.sort && allColumns.some((c) => c.id === state.sort?.id && c.value) ? [state.sort] : [];
  const table = useReactTable({
    data: filtered as T[],
    columns: defs,
    state: {
      sorting,
      columnVisibility: Object.fromEntries(hidden.map((id) => [id, false])),
    },
    getRowId: rowId,
    onSortingChange: (updater: Updater<SortingState>) => {
      const next = typeof updater === "function" ? updater(sorting) : updater;
      state?.setSort(next[0] ?? null);
    },
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    // 페이지 나누기를 쓰지 않으므로 행 모델이 바뀔 때의 페이지 되돌리기를 끈다. 켜 두면 react-table이 그릴 때마다
    // 상태 바꾸기를 예약해, 경로 이동(startTransition)이 새 표를 그리다 처음부터 다시 시작하기를 끝없이
    // 되풀이하고 화면이 바뀌지 않는다(브라우저에서 사이드바를 눌러도 주소만 바뀜).
    autoResetAll: false,
  });
  const sortedRows = table.getRowModel().rows.map((r) => r.original);
  const paged = usePage(
    sortedRows,
    props.pageSize ?? Math.max(sortedRows.length, 1),
    JSON.stringify([q, filterValues, sorting]),
  );
  const rows = paged.pageItems;
  const visible = allColumns.filter((c) => !hidden.includes(c.id));
  const byId = new Map(allColumns.map((c) => [c.id, c]));

  // 거름·검색으로 보이지 않게 된 행은 고르기에서 뺀다(보이지 않는 VM에 일괄 동작이 가지 않게).
  const visibleKey = sortedRows.map(rowId).join("\n");
  useEffect(() => {
    const shown = new Set(visibleKey.split("\n"));
    setSelected((prev) => {
      const next = new Set([...prev].filter((id) => shown.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [visibleKey]);
  // 다른 페이지에서 고른 행도 일괄 동작에 든다.
  const chosen = sortedRows.filter((row) => selected.has(rowId(row)));
  const found = active === null ? -1 : rows.findIndex((row) => rowId(row) === active);
  const current = found >= 0 ? found : null;
  const activeRow = current !== null ? rows[current] : undefined;

  function toggle(index: number, range: boolean) {
    const target = rows[index]!;
    const on = !selected.has(rowId(target));
    // 기준 행이 지금 보이지 않으면 범위 없이 이 행만 바꾼다.
    const start =
      range && anchor.current !== null ? rows.findIndex((r) => rowId(r) === anchor.current) : -1;
    const from = start >= 0 ? Math.min(start, index) : index;
    const to = start >= 0 ? Math.max(start, index) : index;
    const next = new Set(selected);
    for (const row of rows.slice(from, to + 1)) {
      if (on) {
        next.add(rowId(row));
      } else {
        next.delete(rowId(row));
      }
    }
    anchor.current = rowId(target);
    setSelected(next);
  }

  function openPeek(row: T) {
    setActive(rowId(row));
    setPeekOpen(true);
  }

  // 목록 단축키(설계 §5.7). 입력 칸·다른 대화상자·메뉴에서는 받지 않고, 미리 보기 서랍 안에서는 받는다.
  const handlers = { rows, current, peekOpen, toggle, openPeek, peek, rowHref, navigate, rowId };
  const latest = useRef(handlers);
  useEffect(() => {
    latest.current = handlers;
  });
  useEffect(() => {
    if (!keys) {
      return;
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing || e.metaKey || e.ctrlKey || e.altKey) {
        return;
      }
      if (
        isTyping(e.target) ||
        document.querySelector("[role=dialog]:not([data-peek]), [role=alertdialog], [role=menu]")
      ) {
        return;
      }
      const { rows, current, peekOpen, toggle, openPeek, peek, rowHref, navigate, rowId } =
        latest.current;
      if (rows.length === 0) {
        return;
      }
      const target = e.target instanceof Element ? e.target : null;
      const onControl = target?.closest(INTERACTIVE) != null && !target.matches("tr");
      const move = (index: number) => {
        e.preventDefault();
        setActive(rowId(rows[index]!));
        if (!peekOpen) {
          rowEls.current.get(index)?.focus();
        }
      };
      const at = current ?? -1;
      const key = letterKey(e);
      if (key === "j") {
        move(Math.min(at + 1, rows.length - 1));
      } else if (key === "k") {
        move(Math.max(at - 1, 0));
      } else if (onControl || current === null) {
        return;
      } else if (key === "x" && selectable) {
        e.preventDefault();
        toggle(current, false);
      } else if (e.key === " " && peek) {
        e.preventDefault();
        if (peekOpen) {
          setPeekOpen(false);
        } else {
          openPeek(rows[current]!);
        }
      } else if (e.key === "Enter" && rowHref) {
        e.preventDefault();
        void navigate(rowHref(rows[current]!));
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [keys, selectable]);

  const selectBox = (row: T, index: number) => (
    <Checkbox
      aria-label={t("table.select", { name: rowName(row) })}
      checked={selected.has(rowId(row))}
      onClick={(e) => {
        // 체크박스 스스로 바꾸지 않게 하고 여기서 바꾼다(Shift 범위).
        e.preventDefault();
        toggle(index, e.shiftKey);
      }}
    />
  );
  const onPage = rows.filter((row) => selected.has(rowId(row))).length;
  const all = rows.length > 0 && onPage === rows.length;
  const selectAll = (
    <Checkbox
      aria-label={t("table.selectAll")}
      checked={all || (onPage > 0 && "indeterminate")}
      onCheckedChange={(v) => {
        const next = new Set(selected);
        for (const row of rows) {
          if (v === true) {
            next.add(rowId(row));
          } else {
            next.delete(rowId(row));
          }
        }
        setSelected(next);
      }}
    />
  );
  // 이 폭에서 열로 안 보이는 것(펼침 행에 보일 것). 가장 늦게 숨는 열이 펼침 단추를 보일 폭을 정한다.
  const detailCols = visible.filter((c) => !c.actions && (c.priority ?? 1) > 1);
  const deepest = Math.max(1, ...detailCols.map((c) => c.priority ?? 1)) as 1 | 2 | 3 | 4;
  const expandable = detailCols.length > 0;
  const expandClass = DETAIL_CLASS[deepest];
  const toggleExpanded = (row: T) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (!next.delete(rowId(row))) {
        next.add(rowId(row));
      }
      return next;
    });
  const expandButton = (row: T, open: boolean, controls: string) => (
    <Button
      variant="ghost"
      size="icon-sm"
      aria-expanded={open}
      aria-controls={controls}
      aria-label={t("table.details", { name: rowName(row) })}
      onClick={() => toggleExpanded(row)}
    >
      <ChevronRight aria-hidden className={cn("transition-transform", open && "rotate-90")} />
    </Button>
  );
  /** 숨은 열을 이름·값으로(넓은 화면에서 열로 보이는 것은 그 폭에서 감춘다) */
  const detailList = (row: T, phoneCard: boolean) => (
    <dl className="grid grid-cols-[repeat(auto-fill,minmax(12rem,1fr))] gap-x-6 gap-y-2 text-sm">
      {detailCols.map((c) => (
        <div
          key={c.id}
          className={cn("flex min-w-0 flex-col", !phoneCard && DETAIL_CLASS[c.priority ?? 1])}
        >
          <dt className="text-xs text-muted-foreground">{c.header}</dt>
          <dd className="min-w-0 break-words whitespace-normal">{c.cell(row)}</dd>
        </div>
      ))}
    </dl>
  );
  const colSpan = visible.length + (selectable ? 1 : 0) + (expandable ? 1 : 0);
  const loading = props.query?.isPending === true && props.rows.length === 0;
  const hideable = props.columns.filter((c, i) => i > 0 && !c.actions);
  const rowState = (row: T, index: number) => ({
    "data-state": selected.has(rowId(row)) ? "selected" : undefined,
    "data-active": index === current ? "" : undefined,
    tabIndex: -1,
    ref: (el: HTMLElement | null) => {
      if (el) {
        rowEls.current.set(index, el);
      } else {
        rowEls.current.delete(index);
      }
    },
  });

  const toolbar = (props.search ||
    filters.length > 0 ||
    props.toolbar ||
    (state && props.views) ||
    hideable.length > 0) && (
    <div className="flex flex-wrap items-center gap-2">
      {props.search && state && (
        <Input
          data-list-search=""
          className="w-48"
          placeholder={props.searchLabel ?? t("table.search")}
          aria-label={props.searchLabel ?? t("table.search")}
          value={state.q}
          onChange={(e) => state.set("q", e.target.value)}
        />
      )}
      {state &&
        filters
          .filter((f) => f.options.length > 0)
          .map((f) => (
            <NativeSelect
              key={f.id}
              aria-label={f.label}
              value={state.get(f.id)}
              onChange={(e) => state.set(f.id, e.target.value)}
            >
              <NativeSelectOption value="">{f.label}</NativeSelectOption>
              {f.options.map((o) => (
                <NativeSelectOption key={o.value} value={o.value}>
                  {o.label}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          ))}
      {props.toolbar}
      <div className="ml-auto flex items-center gap-2">
        {state && props.views && <ViewMenu table={props.id} state={state} defaults={props.views} />}
        {hideable.length > 0 && (
          <ColumnMenu
            columns={hideable}
            hidden={hidden}
            onChange={(next) => {
              setHidden(next);
              writeCols(user, props.id, { ...readCols(user, props.id), hidden: next });
            }}
          />
        )}
      </div>
    </div>
  );

  // 읽었는데 하나도 없으면 빈 상태 판(검색·거름으로 0이면 표 안에서 "맞는 것 없음")
  if (props.emptyState && props.rows.length === 0 && !loading && !props.query?.isError) {
    return props.emptyState;
  }
  // 행은 있는데 검색·거름으로 모두 빠졌을 때의 문장(빈 목록 문장과 구분 — Cloudscape 빈 상태)
  const empty = props.rows.length > 0 ? t("table.noMatch") : props.empty;

  return (
    <div className="flex min-w-0 flex-col gap-2">
      {toolbar}
      {selectable && (
        // 고른 수를 읽어 주는 곳은 늘 있어야 한다(고르기 전에는 화면에서 숨긴다).
        <div
          role={chosen.length > 0 ? "region" : undefined}
          aria-label={chosen.length > 0 ? t("table.bulk") : undefined}
          className={
            chosen.length > 0
              ? "flex flex-wrap items-center gap-2 rounded-lg border bg-surface-1 px-3 py-2 text-sm"
              : "sr-only"
          }
        >
          <span aria-live="polite" className="font-medium">
            {chosen.length > 0 ? t("table.selected", { count: chosen.length }) : ""}
          </span>
          {chosen.length > 0 && props.bulk?.(chosen, () => setSelected(new Set()))}
        </div>
      )}
      {phone ? (
        <ul aria-labelledby={props.labelledBy} className="flex flex-col gap-2">
          {(loading || rows.length === 0) && (
            <li className="text-center text-sm text-muted-foreground">
              {tableMessage(t, props.query ?? NO_QUERY, empty)}
            </li>
          )}
          {rows.map((row, index) => {
            const [first, ...rest] = visible.filter((c) => !c.actions);
            const open = expanded.has(rowId(row));
            const controls = `${detailId}-${index}`;
            return (
              <li
                key={rowId(row)}
                {...rowState(row, index)}
                className="flex flex-col gap-2 rounded-lg border p-3 data-[state=selected]:bg-muted"
              >
                <div className="flex min-h-10 items-center gap-2">
                  {selectable && selectBox(row, index)}
                  <div className="min-w-0 flex-1 font-medium">{first?.cell(row)}</div>
                  {visible
                    .filter((c) => c.actions)
                    .map((c) => (
                      <div key={c.id}>{c.cell(row)}</div>
                    ))}
                </div>
                <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
                  {rest
                    .filter((c) => (c.priority ?? 1) === 1)
                    .map((c) => (
                      <div key={c.id} className="contents">
                        <dt className="text-muted-foreground">{c.header}</dt>
                        <dd className="min-w-0">{c.cell(row)}</dd>
                      </div>
                    ))}
                </dl>
                {expandable && (
                  <>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="self-start"
                      aria-expanded={open}
                      aria-controls={controls}
                      aria-label={t("table.details", { name: rowName(row) })}
                      onClick={() => toggleExpanded(row)}
                    >
                      <ChevronRight
                        aria-hidden
                        className={cn("transition-transform", open && "rotate-90")}
                      />
                      {t("table.more")}
                    </Button>
                    {open && <div id={controls}>{detailList(row, true)}</div>}
                  </>
                )}
              </li>
            );
          })}
        </ul>
      ) : (
        <Table aria-labelledby={props.labelledBy}>
          <TableHeader>
            <TableRow>
              {selectable && <TableHead className="w-8">{selectAll}</TableHead>}
              {table.getHeaderGroups()[0]!.headers.map((header) => {
                const col = byId.get(header.column.id)!;
                const sorted = header.column.getIsSorted();
                const SortIcon =
                  sorted === "asc" ? ArrowUp : sorted === "desc" ? ArrowDown : ArrowUpDown;
                return (
                  <TableHead
                    key={header.id}
                    className={PRIORITY_CLASS[col.priority ?? 1]}
                    aria-sort={
                      !header.column.getCanSort()
                        ? undefined
                        : sorted === "asc"
                          ? "ascending"
                          : sorted === "desc"
                            ? "descending"
                            : "none"
                    }
                  >
                    {col.actions ? (
                      <span className="sr-only">{col.header}</span>
                    ) : header.column.getCanSort() ? (
                      <button
                        type="button"
                        className="inline-flex items-center gap-1"
                        onClick={header.column.getToggleSortingHandler()}
                      >
                        {col.header}
                        <SortIcon aria-hidden className={sorted ? "size-3" : "size-3 opacity-50"} />
                      </button>
                    ) : (
                      col.header
                    )}
                  </TableHead>
                );
              })}
              {expandable && (
                <TableHead className={cn("w-8", expandClass)}>
                  <span className="sr-only">{t("table.more")}</span>
                </TableHead>
              )}
            </TableRow>
          </TableHeader>
          <TableBody>
            {loading ? (
              <>
                <TableRow>
                  <TableCell colSpan={colSpan} className="p-0">
                    <span className="sr-only">{t("common.loading")}</span>
                  </TableCell>
                </TableRow>
                {[0, 1, 2, 3, 4].map((i) => (
                  <TableRow key={i} aria-hidden>
                    {Array.from({ length: colSpan }, (_, j) => (
                      <TableCell key={j}>
                        <Skeleton className="h-4 w-full max-w-32" />
                      </TableCell>
                    ))}
                  </TableRow>
                ))}
              </>
            ) : (
              rows.length === 0 && (
                <TableMessage colSpan={colSpan} query={props.query ?? NO_QUERY} empty={empty} />
              )
            )}
            {rows.map((row, index) => {
              const extra = props.rowProps?.(row, chosen);
              const open = expandable && expanded.has(rowId(row));
              const controls = `${detailId}-${index}`;
              return (
                <Fragment key={rowId(row)}>
                  <TableRow
                    {...extra}
                    {...rowState(row, index)}
                    className={cn(
                      "outline-none focus-visible:bg-muted/50 data-active:bg-muted/50",
                      extra?.className,
                    )}
                  >
                    {selectable && <TableCell>{selectBox(row, index)}</TableCell>}
                    {visible.map((c) => (
                      <TableCell
                        key={c.id}
                        className={cn(
                          PRIORITY_CLASS[c.priority ?? 1],
                          c.actions && "text-right whitespace-nowrap",
                          typeof c.className === "function" ? c.className(row) : c.className,
                        )}
                      >
                        {c.cell(row)}
                      </TableCell>
                    ))}
                    {expandable && (
                      <TableCell className={expandClass}>
                        {expandButton(row, open, controls)}
                      </TableCell>
                    )}
                  </TableRow>
                  {open && (
                    <TableRow
                      id={controls}
                      className={cn("h-auto bg-surface-1 hover:bg-surface-1", expandClass)}
                    >
                      <TableCell colSpan={colSpan} className="px-4 py-3 whitespace-normal">
                        {detailList(row, false)}
                      </TableCell>
                    </TableRow>
                  )}
                </Fragment>
              );
            })}
          </TableBody>
        </Table>
      )}
      {props.pageSize !== undefined && paged.pageCount > 1 && (
        <Pager
          page={paged.page}
          pageCount={paged.pageCount}
          setPage={paged.setPage}
          pageSize={props.pageSize}
          total={sortedRows.length}
        />
      )}
      {props.peek && (
        <Sheet open={peekOpen && activeRow !== undefined} onOpenChange={setPeekOpen}>
          <SheetContent data-peek="" className="overflow-y-auto">
            {activeRow !== undefined && props.peek(activeRow)}
          </SheetContent>
        </Sheet>
      )}
    </div>
  );
}
