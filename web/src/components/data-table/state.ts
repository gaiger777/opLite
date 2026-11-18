// 공통 표의 상태(설계 §5.3): 검색·정렬·거름은 URL 쿼리에(링크로 나눌 수 있게), 저장 보기는 `views.{table}`,
// 숨긴 열과 마지막 정렬은 `cols.{table}`에 둔다(브라우저 저장, 실패는 무시).
import { useCallback, useMemo, useState } from "react";
import { useSearchParams } from "react-router";

import { readPref, writePref } from "@/lib/prefs";

/** 한 열 정렬. URL에서는 `sort=name`(오름)·`sort=-memory`(내림)이다. */
export interface Sort {
  id: string;
  desc: boolean;
}

export function parseSort(value: string | null | undefined): Sort | null {
  if (!value) {
    return null;
  }
  const desc = value.startsWith("-");
  const id = desc ? value.slice(1) : value;
  return id ? { id, desc } : null;
}

export function formatSort(sort: Sort | null): string {
  return sort ? `${sort.desc ? "-" : ""}${sort.id}` : "";
}

/** 이름 붙인 쿼리(`phase=running&node=a`) */
export interface View {
  name: string;
  query: string;
}

function parseJson(raw: string | null): unknown {
  try {
    return raw === null ? null : JSON.parse(raw);
  } catch {
    return null;
  }
}

/** 저장해 둔 보기. 모양이 다르면 버린다. */
export function readViews(table: string): View[] {
  const value = parseJson(readPref(`views.${table}`));
  return Array.isArray(value)
    ? value.filter(
        (v: unknown): v is View =>
          typeof v === "object" &&
          v !== null &&
          typeof (v as View).name === "string" &&
          typeof (v as View).query === "string",
      )
    : [];
}

export function writeViews(table: string, views: View[]) {
  writePref(`views.${table}`, JSON.stringify(views));
}

/** 열 설정: 숨긴 열과 마지막으로 고른 정렬 */
export interface ColPrefs {
  hidden: string[];
  sort?: string;
}

/**
 * 열 설정을 읽는다. `cols.vms`가 없으면 옛 `vms.sort`(react-table의 `[{id, desc}]`)를 옮겨 읽는다.
 */
export function readCols(table: string): ColPrefs {
  const value = parseJson(readPref(`cols.${table}`)) as Partial<ColPrefs> | null;
  if (value && typeof value === "object") {
    return {
      hidden: Array.isArray(value.hidden)
        ? value.hidden.filter((h): h is string => typeof h === "string")
        : [],
      ...(typeof value.sort === "string" && { sort: value.sort }),
    };
  }
  if (table === "vms") {
    const old = parseJson(readPref("vms.sort"));
    const first: unknown = Array.isArray(old) ? old[0] : undefined;
    if (
      typeof first === "object" &&
      first !== null &&
      typeof (first as Sort).id === "string" &&
      typeof (first as Sort).desc === "boolean"
    ) {
      return { hidden: [], sort: formatSort(first as Sort) };
    }
  }
  return { hidden: [] };
}

export function writeCols(table: string, cols: ColPrefs) {
  writePref(`cols.${table}`, JSON.stringify(cols));
}

/** 표가 쓰는 쿼리 키만 남긴 쿼리 글(저장 보기와 비교에 쓴다) */
export function pickQuery(params: URLSearchParams, keys: readonly string[]): string {
  const out = new URLSearchParams();
  for (const key of keys) {
    const value = params.get(key);
    if (value) {
      out.set(key, value);
    }
  }
  return out.toString();
}

export interface TableState {
  /** 검색 글(`q`) */
  q: string;
  sort: Sort | null;
  /** 거름 값(없으면 "") */
  get(key: string): string;
  /** 값을 바꾼다(""면 지운다). */
  set(key: string, value: string): void;
  setSort(sort: Sort | null): void;
  /** 표 키만의 지금 쿼리 */
  query: string;
  /** 표 키를 이 쿼리로 바꾼다(다른 쿼리는 둔다). */
  apply(query: string): void;
}

/**
 * 표 상태. `url`이면 주소 쿼리에(기록을 늘리지 않게 replace), 아니면 화면 안에만 둔다(상세 화면 안의 표). `keys`는
 * 거름 키다. 정렬이 주소에 없으면 마지막으로 고른 정렬(`cols.{table}`)을 쓴다.
 */
export function useTableState(table: string, keys: readonly string[] = [], url = true): TableState {
  const [params, setParams] = useSearchParams();
  const [local, setLocal] = useState(() => new URLSearchParams());
  const source = url ? params : local;
  const all = useMemo(() => ["q", "sort", ...keys], [keys]);
  const [saved, setSaved] = useState(() => readCols(table).sort);
  const update = useCallback(
    (change: (next: URLSearchParams) => void) => {
      if (url) {
        setParams(
          (prev) => {
            const next = new URLSearchParams(prev);
            change(next);
            return next;
          },
          { replace: true },
        );
      } else {
        setLocal((prev) => {
          const next = new URLSearchParams(prev);
          change(next);
          return next;
        });
      }
    },
    [url, setParams],
  );
  const sortText = source.get("sort") ?? saved ?? null;
  return {
    q: source.get("q") ?? "",
    sort: parseSort(sortText),
    get: (key) => source.get(key) ?? "",
    set: (key, value) => update((next) => (value ? next.set(key, value) : next.delete(key))),
    setSort: (sort) => {
      writeCols(table, { ...readCols(table), sort: formatSort(sort) });
      setSaved(formatSort(sort));
      update((next) => (sort ? next.set("sort", formatSort(sort)) : next.delete("sort")));
    },
    query: pickQuery(source, all),
    apply: (query) =>
      update((next) => {
        const chosen = new URLSearchParams(query);
        for (const key of all) {
          const value = chosen.get(key);
          if (value) {
            next.set(key, value);
          } else {
            next.delete(key);
          }
        }
      }),
  };
}
