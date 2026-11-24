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

/**
 * 내려받기·VM으로 복원 한 번에 고를 수 있는 항목 수(서버 `MAX_DOWNLOAD_PATHS`). 디렉터리를 통째로 고르면 그 디렉터리 하나로
 * 보낸다.
 */
export const MAX_PATHS = 1000;

/** zip·tar.gz 맨 위의 보고서 파일(건너뛴 항목·일찍 멈춘 까닭 — 서버 `REPORT_NAME`, 이름이 겹치면 `-2` …가 붙는다) */
export const REPORT_FILE = "ORBIVISOR-RESTORE-REPORT.txt";

/** 파일 표가 한 번에 더 그리는 행 수. 큰 디렉터리를 다 그리지 않고 아래에 닿을 때마다 이만큼 늘린다. */
export const RENDER_STEP = 500;

/**
 * 이름에서 화면에 보이지 않거나 글 방향을 바꾸는 문자(C0·C1 제어 문자, 양방향 서식 문자, 폭 없는 문자, BOM, 줄·문단 구분자,
 * 소프트 하이픈·결합 문자 지우개·한글 채움 문자·몽골어 모음 구분자처럼 빈칸으로 보이는 문자, 행간 주석 문자, 태그 문자). 백업 안
 * 이름은 게스트가 정한 것이라 `invoice` + U+202E + `txt.exe`처럼 다른 이름으로 보이게 하거나 두 이름을 같아 보이게 할 수 있다.
 */
const HIDDEN_CHARS =
  // eslint-disable-next-line no-control-regex, no-misleading-character-class -- U+034F(결합 문자 지우개)도 하나씩 드러낸다
  /[\u0000-\u001f\u007f-\u009f\u00ad\u034f\u061c\u115f\u1160\u180e\u200b-\u200f\u2028-\u202e\u2060-\u2069\u3164\ufeff\ufff9-\ufffb\u{e0000}-\u{e007f}]/gu;

const SHORT_ESCAPES: Record<string, string> = { "\n": "\\n", "\r": "\\r", "\t": "\\t" };

function escapeChar(c: string): string {
  const code = c.codePointAt(0)!;
  const hex = code.toString(16).toUpperCase();
  return SHORT_ESCAPES[c] ?? (code > 0xffff ? `\\u{${hex}}` : `\\u${hex.padStart(4, "0")}`);
}

/** 보이는 이름의 조각: 그대로 보일 글과, 숨은 문자를 바꾼 `\u202E`·`\n` 같은 글(`escaped`) */
export interface NameSegment {
  text: string;
  escaped: boolean;
}

/** 이름을 보이는 조각으로 나눈다(보이기만 — API에는 원래 이름을 보낸다). */
export function nameSegments(name: string): NameSegment[] {
  const out: NameSegment[] = [];
  let last = 0;
  for (const m of name.matchAll(HIDDEN_CHARS)) {
    if (m.index > last) {
      out.push({ text: name.slice(last, m.index), escaped: false });
    }
    out.push({ text: escapeChar(m[0]), escaped: true });
    last = m.index + m[0].length;
  }
  if (last < name.length || out.length === 0) {
    out.push({ text: name.slice(last), escaped: false });
  }
  return out;
}

/** 숨은 문자를 보이는 글로 바꾼 이름(툴팁·접근 이름) */
export function visibleName(name: string): string {
  return name.replace(HIDDEN_CHARS, escapeChar);
}

export function partOf(entry: Part): Part {
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

/** 화면에 보일 경로: `displayPath`에서 숨은 문자를 보이는 글로 바꾼 것 */
export function visiblePath(parts: readonly Part[]): string {
  return `/${parts.map((p) => visibleName(p.name)).join("/")}`;
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
 * 찾아볼 수 없는 까닭(번역 키와 값). 마운트를 해 봤다가 실패했으면 서버의 덧붙임을 쓴다. 찾아볼 수 있으면 null. 덧붙임(마운트
 * 오류 글)과 파일시스템 이름은 게스트 디스크에서 오므로 숨은 문자를 보이는 글로 바꾼다.
 */
export function volumeBlocked(
  v: VolumeView,
): { key: string; values?: Record<string, string> } | null {
  if (v.state === "failed") {
    return { key: "fileRestore.blocked.failed", values: { note: visibleName(v.note ?? "") } };
  }
  if (v.mountable) {
    return null;
  }
  if (v.note) {
    return { key: "fileRestore.blocked.note", values: { note: visibleName(v.note) } };
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
      return { key: "fileRestore.blocked.unsupported", values: { fs: visibleName(v.fs) } };
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
 * VM으로 되돌려 쓸 기본 대상 디렉터리: 고른 항목의 원래 부모(설계 §14). Windows 게스트는 `C:\…`로 바꾼다. UTF-8이 아니거나 숨은
 * 문자(양방향·제어·폭 없는 문자)가 든 조각이 있으면 루트다(게스트 쪽 이름을 짐작하지 않고, 고칠 수 있는 칸에 보이지 않는 글을
 * 넣지 않는다).
 */
export function defaultTargetDir(parent: readonly Part[], os: GuestRestoreCaps["os"] | undefined) {
  const odd = parent.some((p) => p.raw || visibleName(p.name) !== p.name);
  const names = odd ? [] : parent.map((p) => p.name);
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

/**
 * 서버 시계와 이 브라우저 시계의 차이(초, 서버 − 브라우저). 응답의 `Date` 머리(초 단위)로 잰다. 없거나 2초보다 작으면 0이다
 * (초 단위로 잘린 머리 때문에 흔들리지 않게).
 */
export function serverSkew(date: string | null | undefined, nowMs = Date.now()): number {
  const server = date ? Date.parse(date) : NaN;
  if (Number.isNaN(server)) {
    return 0;
  }
  const skew = (server - nowMs) / 1000;
  return Math.abs(skew) < 2 ? 0 : skew;
}

/** 세션 보기의 캐시 키 */
export const sessionKey = (id: string) => ["file-restore", "session", id] as const;
