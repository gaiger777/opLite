import type { TFunction } from "i18next";

import { ApiError } from "@/api/client";
import type { components } from "@/api/schema";
import { codeMessage, errorMessage } from "@/i18n/errors";

export type Session = components["schemas"]["FileRestoreSessionView"];
export type VolumeView = components["schemas"]["VolumeView"];
export type EntryView = components["schemas"]["EntryView"];
export type EntryPath = components["schemas"]["EntryPath"];
export type GuestRestoreCaps = components["schemas"]["GuestRestoreCaps"];
export type GuestConflict = components["schemas"]["GuestConflict"];
export type DownloadFormat = components["schemas"]["DownloadFormat"];

/** 경로 조각: 보이는 이름과, 이름이 UTF-8이 아니면 원래 바이트의 표준 base64(`EntryView.raw`) */
export interface Part {
  name: string;
  raw?: string;
}

/** 나열 한 쪽의 항목 수(서버 기본과 같다) */
export const PAGE_SIZE = 200;

export function partOf(entry: EntryView): Part {
  return entry.raw ? { name: entry.name, raw: entry.raw } : { name: entry.name };
}

/** 같은 디렉터리 안 항목의 열쇠(고르기·React key). UTF-8이 아닌 이름은 바이트로 가른다. */
export function entryKey(entry: Part): string {
  return entry.raw ? `raw:${entry.raw}` : `n:${entry.name}`;
}

/** 보이는 경로(`/etc/nginx`). UTF-8이 아닌 조각은 U+FFFD가 든 이름 그대로다. */
export function displayPath(parts: readonly Part[]): string {
  return `/${parts.map((p) => p.name).join("/")}`;
}

/** API의 `EntryPath`: 조각 목록(UTF-8이 아닌 조각은 `{ b64 }`) */
export function entryPath(parts: readonly Part[]): EntryPath {
  return parts.map((p) => (p.raw ? { b64: p.raw } : p.name));
}

function partBytes(part: Part): Uint8Array {
  if (part.raw) {
    return Uint8Array.from(atob(part.raw), (c) => c.charCodeAt(0));
  }
  return new TextEncoder().encode(part.name);
}

/**
 * 나열·항목 질의의 경로: 모두 UTF-8이면 `path`, 아니면 `path_b64`(조각 바이트를 `/`로 이은 것의 URL-safe base64, 채움 없음 —
 * 서버 `query_path`).
 */
export function pathQuery(parts: readonly Part[]): { path: string } | { path_b64: string } {
  if (!parts.some((p) => p.raw)) {
    return { path: displayPath(parts) };
  }
  const bytes: number[] = [];
  for (const part of parts) {
    bytes.push(0x2f, ...partBytes(part));
  }
  const b64 = btoa(String.fromCharCode(...bytes));
  return { path_b64: b64.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "") };
}

/** `ls -l`의 모드 글(`drwxr-xr-x`, setuid·setgid·sticky 포함) */
export function modeString(kind: EntryView["kind"], mode: number): string {
  const type = kind === "dir" ? "d" : kind === "symlink" ? "l" : kind === "other" ? "?" : "-";
  const triple = (shift: number, special: boolean, specialChar: string) => {
    const r = mode & (4 << shift) ? "r" : "-";
    const w = mode & (2 << shift) ? "w" : "-";
    const x = mode & (1 << shift);
    const last = special ? (x ? specialChar : specialChar.toUpperCase()) : x ? "x" : "-";
    return r + w + last;
  };
  return (
    type +
    triple(6, (mode & 0o4000) !== 0, "s") +
    triple(3, (mode & 0o2000) !== 0, "s") +
    triple(0, (mode & 0o1000) !== 0, "t")
  );
}

/** 볼륨 나무의 디스크 하나: 디스크 볼륨(있으면)과 그 아래 파티션·LV */
export interface DiskGroup {
  disk: number;
  /** 디스크 자체(파티션 표가 없는 파일시스템이거나 내용 없음) */
  self?: VolumeView;
  children: VolumeView[];
}

/** 볼륨을 디스크별로: 파티션은 번호 차례, LV는 이름 차례(LV는 첫 PV의 디스크 아래). */
export function groupVolumes(volumes: readonly VolumeView[]): DiskGroup[] {
  const groups = new Map<number, DiskGroup>();
  for (const v of volumes) {
    const group = groups.get(v.disk) ?? { disk: v.disk, children: [] };
    if (v.kind === "disk") {
      group.self = v;
    } else {
      group.children.push(v);
    }
    groups.set(v.disk, group);
  }
  const rank = (v: VolumeView) => (v.kind === "partition" ? 0 : 1);
  const number = (v: VolumeView) => v.partition?.number ?? 0;
  for (const g of groups.values()) {
    g.children.sort(
      (a, b) => rank(a) - rank(b) || number(a) - number(b) || a.id.localeCompare(b.id),
    );
  }
  return [...groups.values()].sort((a, b) => a.disk - b.disk);
}

/** 볼륨의 보이는 이름: 파티션 번호, LV는 `vg/lv`, 디스크는 디스크 번호(번역 키와 값) */
export function volumeLabel(v: VolumeView): {
  key: string;
  values: Record<string, string | number>;
} {
  if (v.kind === "partition") {
    return { key: "fileRestore.partition", values: { n: v.partition?.number ?? v.id } };
  }
  if (v.kind === "logical_volume") {
    return { key: "fileRestore.logicalVolume", values: { name: v.id.replace(/^lvm\//, "") } };
  }
  return { key: "fileRestore.disk", values: { n: v.disk } };
}

/**
 * 찾아볼 수 없는 까닭(번역 키와 값). 마운트를 해 봤다가 실패했으면 서버의 덧붙임을 쓴다. 찾아볼 수 있으면 null.
 */
export function volumeBlocked(
  v: VolumeView,
): { key: string; values?: Record<string, string> } | null {
  if (v.state === "failed") {
    return { key: "fileRestore.blocked.failed", values: { note: v.note ?? "" } };
  }
  if (v.mountable) {
    return null;
  }
  if (v.note) {
    return { key: "fileRestore.blocked.note", values: { note: v.note } };
  }
  switch (v.fs) {
    case undefined:
    case null:
    case "":
      return { key: "fileRestore.blocked.none" };
    case "LVM2_member":
      return { key: "fileRestore.blocked.lvm" };
    case "crypto_LUKS":
      return { key: "fileRestore.blocked.luks" };
    case "swap":
      return { key: "fileRestore.blocked.swap" };
    default:
      return { key: "fileRestore.blocked.unsupported", values: { fs: v.fs } };
  }
}

/** 마운트된(또는 찾아볼 수 있는) 볼륨의 경고 덧붙임(저널 재생 안 됨 …) */
export function volumeWarning(v: VolumeView): string | null {
  return v.mountable && v.state !== "failed" && v.note ? v.note : null;
}

/** 처음 고를 볼륨: 찾아볼 수 있는 첫 볼륨(나무 차례) */
export function firstBrowsable(volumes: readonly VolumeView[]): VolumeView | undefined {
  for (const g of groupVolumes(volumes)) {
    for (const v of [...(g.self ? [g.self] : []), ...g.children]) {
      if (!volumeBlocked(v)) {
        return v;
      }
    }
  }
  return undefined;
}

/**
 * VM으로 되돌려 쓸 기본 대상 디렉터리: 고른 항목의 원래 부모(설계 §14). Windows 게스트는 `C:\…`로 바꾼다. UTF-8이 아닌 조각이
 * 있으면 루트다(게스트 쪽 이름을 짐작하지 않는다).
 */
export function defaultTargetDir(parent: readonly Part[], os: GuestRestoreCaps["os"] | undefined) {
  const names = parent.some((p) => p.raw) ? [] : parent.map((p) => p.name);
  return os === "windows" ? `C:\\${names.join("\\")}` : `/${names.join("/")}`;
}

/** 대상 디렉터리의 모양 검사(서버 `GuestDir::parse`의 앞부분 — 나머지는 서버가 400 `invalid_target_dir`로 알린다) */
export function targetDirOk(dir: string, os: GuestRestoreCaps["os"]): boolean {
  if (os === "windows") {
    return /^[A-Za-z]:[\\/]/.test(dir);
  }
  return dir.startsWith("/");
}

/** 내려받기 형식: 일반 파일 하나면 그 파일(`auto` → raw), 아니면 zip. tar.gz는 메뉴에서 고른다. */
export function downloadFormat(selected: readonly EntryView[], wanted?: "tar.gz"): DownloadFormat {
  if (wanted) {
    return wanted;
  }
  return selected.length === 1 && selected[0]!.kind === "file" ? "auto" : "zip";
}

/** 남은 시간을 `m:ss`로(부팅 경과) */
export function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** 파일 복원 API 오류를 문장으로(`fileRestore.errors.<code>`가 먼저, 없으면 공통 문구나 서버 설명) */
export function fileRestoreError(t: TFunction, err: unknown): string {
  return err instanceof ApiError
    ? codeMessage(t, err.code, err.message, "fileRestore.errors")
    : errorMessage(t, err);
}

/** 세션 보기의 캐시 키 */
export const sessionKey = (id: string) => ["file-restore", "session", id] as const;
