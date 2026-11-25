// 설정 기록(계획 73 §12)의 질의. 이력은 watch 종류가 아니다 — 화면을 열 때·거르기를 바꿀 때·되돌리기를 시작한 뒤 다시 읽는다.
import { type QueryClient, useInfiniteQuery, useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import { planOf } from "@/declarative/api";
import type { components } from "@/api/schema";
import {
  type Kind,
  kindPrivilege,
  type MemberKind,
  type Permissions,
  ROOT,
  type Target,
} from "@/auth/permissions";

type Schemas = components["schemas"];
export type HistoryEntry = Schemas["HistoryEntry"];
export type HistoryDetail = Schemas["HistoryDetail"];
export type HistoryRecord = Schemas["HistoryRecord"];
export type HistoryRef = Schemas["HistoryRef"];
export type Change = Schemas["Change"];
export type Actor = Schemas["Actor"];
export type ChangeSource = Schemas["ChangeSource"];
export type HistoryRestoreRequest = Schemas["HistoryRestoreRequest"];
export type PointRestoreRequest = Schemas["PointRestoreRequest"];

export const HISTORY_KEY = ["history"] as const;

/** 출처 칩의 차례(서버 `ChangeSource`) */
export const SOURCES: readonly ChangeSource[] = [
  "web",
  "cli",
  "api",
  "apply",
  "terraform",
  "task",
  "system",
];

/** 거르기 줄의 기간(지금부터 거꾸로, 밀리초) */
export const PERIODS = {
  "1h": 3_600_000,
  "24h": 86_400_000,
  "7d": 604_800_000,
  "30d": 2_592_000_000,
};
export type Period = keyof typeof PERIODS | "all";

/** 이력을 다는 종류 가운데 선언(매니페스트) 종류 — 되돌리기·YAML 내보내기가 된다(서버 `ManifestKind`) */
export const MANIFEST_KINDS: readonly Kind[] = [
  "vm",
  "volume",
  "network",
  "router",
  "floating-ip",
  "security-group",
  "load-balancer",
  "storage-pool",
  "folder",
  "placement-rule",
  "ha-group",
  "ha-check",
  "backup-job",
  "backup-store",
  "remote",
  "grant",
  "access-role",
  "group",
  "alert-rule",
  "notification-target",
  "realm",
  "settings",
  "certificate",
  "acme-account",
  "acme-dns",
];

/** 거르기 줄의 종류(이력을 다는 종류 — 서버 `Kind::history_tracked`) */
export const HISTORY_KINDS: readonly Kind[] = [
  ...MANIFEST_KINDS,
  "user",
  "vmware-source",
  "apply-set",
];

export function restorable(kind: string): boolean {
  return (MANIFEST_KINDS as readonly string[]).includes(kind);
}

export interface HistoryFilter {
  kind?: string;
  key?: string;
  user?: string;
  source?: string;
  folder?: string;
  applySet?: string;
  period: Period;
}

/** 쪽 하나의 줄 수 */
export const PAGE = 100;

/** 타임라인(새것부터, `next_before`로 다음 쪽). 거르기의 기간은 읽을 때의 지금부터 잰다. */
export function useTimeline(filter: HistoryFilter) {
  return useInfiniteQuery({
    queryKey: [...HISTORY_KEY, "timeline", filter],
    initialPageParam: undefined as number | undefined,
    queryFn: ({ pageParam }) =>
      call(
        api.GET("/history", {
          params: {
            query: {
              limit: PAGE,
              ...(filter.kind && { kind: filter.kind }),
              ...(filter.kind && filter.key && { key: filter.key }),
              ...(filter.user && { user: filter.user }),
              ...(filter.source && { source: filter.source }),
              ...(filter.folder && { folder: filter.folder }),
              ...(filter.applySet && { apply_set: filter.applySet }),
              ...(filter.period !== "all" && {
                since: String(Date.now() - PERIODS[filter.period]),
              }),
              ...(pageParam !== undefined && { before: pageParam }),
            },
          },
        }),
      ),
    getNextPageParam: (last) => last.next_before,
  });
}

/** 리소스 하나의 이력(⋯ 메뉴의 "설정 기록…" 서랍) */
export function useResourceHistory(kind: string, key: string) {
  return useQuery({
    queryKey: [...HISTORY_KEY, "resource", kind, key],
    queryFn: () => call(api.GET("/history/{kind}/{key}", { params: { path: { kind, key } } })),
    select: (list) => list.items,
  });
}

/** 줄 하나의 전체 spec과 비교(`prev`·`live`·revision) */
export function useHistoryDetail(entry: HistoryEntry, compare: string) {
  return useQuery({
    queryKey: [...HISTORY_KEY, "row", entry.kind, entry.id, entry.revision, compare],
    queryFn: () =>
      call(
        api.GET("/history/{kind}/{key}/{revision}", {
          params: {
            path: { kind: entry.kind, key: entry.id, revision: entry.revision },
            query: { compare },
          },
        }),
      ),
  });
}

export async function refreshHistory(queryClient: QueryClient) {
  await queryClient.invalidateQueries({ queryKey: HISTORY_KEY });
}

/** 하나 되돌리기의 계획(`dry_run` — 쓰지 않음). 막힘이 있어도 200으로 계획을 준다. */
export async function planRestoreOne(
  entry: HistoryEntry,
  body: Omit<HistoryRestoreRequest, "revision">,
) {
  return planOf(
    await call(
      api.POST("/history/{kind}/{key}/restore", {
        params: { path: { kind: entry.kind, key: entry.id } },
        body: { ...body, revision: entry.revision, dry_run: true },
      }),
    ),
  );
}

/** 시점 되돌리기의 계획(`dry_run`) */
export async function planRestorePoint(body: Omit<PointRestoreRequest, "dry_run">) {
  return planOf(await call(api.POST("/history/restore", { body: { ...body, dry_run: true } })));
}

/** 사람이 읽는 주체: 사용자 이름(토큰이면 토큰 이름도), 없으면 system */
export function actorName(actor: Actor): string {
  const user = actor.user ?? "system";
  return actor.auth?.kind === "token" ? `${user} (${actor.auth.name})` : user;
}

/** 날짜(그 언어의 지역 날짜)마다 묶는다 — 새것부터 받은 차례를 그대로 둔다. */
export function groupByDay<T extends { at_ms: number }>(
  rows: readonly T[],
  language: string,
): { day: string; rows: T[] }[] {
  const fmt = new Intl.DateTimeFormat(language, { dateStyle: "full" });
  const groups: { day: string; rows: T[] }[] = [];
  for (const row of rows) {
    const day = fmt.format(new Date(row.at_ms));
    const last = groups.at(-1);
    if (last && last.day === day) {
      last.rows.push(row);
    } else {
      groups.push({ day, rows: [row] });
    }
  }
  return groups;
}

const MEMBER_KINDS: readonly string[] = [
  "vm",
  "network",
  "router",
  "floating-ip",
  "load-balancer",
  "network-migration",
  "storage-pool",
];

/** 그 리소스의 쓰기 권한 대상: 폴더를 따르는 종류는 그 폴더 안 객체, 폴더는 폴더 자체, 나머지는 루트 */
function writeTarget(kind: string, id: string, folder: unknown): Target {
  if (kind === "folder") {
    return { type: "folder", id };
  }
  if (MEMBER_KINDS.includes(kind)) {
    return {
      type: "object",
      kind: kind as MemberKind,
      id,
      folder: typeof folder === "string" ? folder : null,
    };
  }
  return ROOT;
}

/**
 * 되돌리기 단추를 보일지: 선언 종류이고 그 종류의 쓰기 권한이 그 자리에 있다(서버가 단계마다 다시 본다 — 화면은 숨기기만).
 * `folder`는 그 줄 spec의 폴더 id다.
 */
export function canRestore(
  perms: Permissions,
  kind: string,
  id: string,
  folder?: unknown,
): boolean {
  return (
    restorable(kind) &&
    perms.has(kindPrivilege("write", kind as Kind), writeTarget(kind, id, folder))
  );
}

/** 시점 되돌리기 단추: 어딘가에 선언 종류의 쓰기 권한이 하나라도 있다 */
export function canRestoreAnything(perms: Permissions): boolean {
  return MANIFEST_KINDS.some((kind) => perms.anywhere(kindPrivilege("write", kind)));
}
