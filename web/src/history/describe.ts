// 설정 기록·YAML 비교를 사람 말로(계획 77-7, 진단 §2.11): 종류 이름("notification-target" → "알림 대상"), 칸 이름
// ("memory_mib" → "메모리"), 값("8192" → "8 GiB"), 바뀐 줄 요약("메모리 4 GiB → 8 GiB"). 문구는 늦게 받는 설정 기록 문구
// (`history.kinds`·`history.fields`)이고, 모르는 종류·칸은 원문 그대로다.
import type { TFunction } from "i18next";

import type { Change, HistoryEntry } from "@/history/api";
import { secretMark } from "@/history/diff";
import { formatBytes, formatMib } from "@/lib/format";

/** 종류의 사람 이름: 저장 꼴(`storage-pool`)이나 매니페스트 꼴(`StoragePool`) 모두, 모르면 원문 */
export function kindName(kind: string, t: TFunction): string {
  const key = kind.replace(/(?<=[a-z0-9])([A-Z])/g, "-$1").toLowerCase();
  return t(`history.kinds.${key}`, { defaultValue: kind });
}

/** 칸 사전(점 경로가 그대로 열쇠라 객체로 받아 찾는다) */
function fields(t: TFunction): Record<string, string> {
  const dict = t("history.fields", { returnObjects: true });
  return typeof dict === "object" && dict !== null ? (dict as Record<string, string>) : {};
}

/**
 * 칸 경로의 사람 이름: 사전의 경로 그대로 → 끝 조각 → 원문. 레이블은 "레이블 tier"(열쇠는 원문). 아니면 null이 아니라
 * 원문 경로를 돌려주므로 부르는 쪽은 `raw`로 원문인지 안다.
 */
export function fieldName(path: string, t: TFunction): { label: string; raw: boolean } {
  if (path.startsWith("labels.")) {
    return { label: t("history.fieldLabel", { key: path.slice(7) }), raw: false };
  }
  if (path === "") {
    return { label: t("history.wholeSpec"), raw: false };
  }
  // 비밀 칸의 표시 조각(`auth.password.$secret`)은 떼고 그 칸 이름으로
  const base = path.replace(/\.\$secret$/, "");
  const dict = fields(t);
  const parts = base.split(".");
  const last = parts.at(-1) ?? base;
  let label = dict[base];
  if (label === undefined && parts.length > 1 && dict[last] !== undefined) {
    // 끝 조각만 아는 칸: 부모를 알면 "HA 사용 여부", 모르면 맥락 없이도 뜻이 같은 낱말만(알림 대상·종류·상태 같은 낱말은 맥락마다
    // 달라 원문으로 둔다)
    const parent = dict[parts.slice(0, -1).join(".")];
    label = parent
      ? t("history.fieldIn", { parent, field: dict[last] })
      : CONTEXTUAL.has(last)
        ? undefined
        : dict[last];
  }
  return label ? { label, raw: false } : { label: base, raw: true };
}

/** 맥락마다 뜻이 달라 끝 조각만으로 이름을 붙이지 않는 칸 */
const CONTEXTUAL: ReadonlySet<string> = new Set([
  "targets",
  "kind",
  "state",
  "name",
  "enabled",
  "members",
  "nodes",
  "node",
  "rules",
  "role",
  "vms",
  "url",
]);

/** 값을 짧은 글로: 크기 칸(`_mib`·`_gib`·`_bytes`)은 단위, 참거짓은 켬·끔, 비밀 표시는 번역, 없음은 null */
export function valueText(path: string, value: unknown, t: TFunction): string | null {
  if (value === undefined || value === null) {
    return null;
  }
  const mark = secretMark(value);
  if (mark !== null) {
    return t(`history.secret.${mark}`);
  }
  if (typeof value === "number") {
    if (/_mib$/.test(path)) {
      return formatMib(value);
    }
    if (/_gib$/.test(path)) {
      return `${value} GiB`;
    }
    if (/_bytes$/.test(path)) {
      return formatBytes(value);
    }
    return String(value);
  }
  if (typeof value === "boolean") {
    return t(value ? "history.on" : "history.off");
  }
  if (typeof value === "string") {
    return value;
  }
  return JSON.stringify(value);
}

/** 칸 하나의 바뀜: "메모리 4 GiB → 8 GiB", 더함은 "레이블 tier: web", 지움은 "설명 지움" */
export function changeText(c: Change, t: TFunction): string {
  const name = fieldName(c.path, t).label;
  const before = c.op === "add" ? null : valueText(c.path, c.old, t);
  const after = c.op === "remove" ? null : valueText(c.path, c.new, t);
  if (after === null) {
    return t("history.changeRemoved", { field: name });
  }
  if (before === null) {
    return t("history.changeAdded", { field: name, after });
  }
  return t("history.changeValue", { field: name, before, after });
}

/** 보일 칸 수(나머지는 "외 n칸") */
const SHOWN = 3;

/**
 * 바뀐 줄의 요약: 서버가 준 전·후 값(`values`, 계획 77-7)은 "메모리 4 GiB → 8 GiB", 값이 없는 칸(옛 서버, 긴 목록)은 칸
 * 이름만("디스크 바뀜"). 바꾸기가 아니면 빈 목록.
 */
export function changeSummary(entry: HistoryEntry, t: TFunction): string[] {
  const changed = entry.changed ?? [];
  const values = new Map((entry.values ?? []).map((c) => [c.path, c]));
  const out = changed.slice(0, SHOWN).map((path) => {
    const c = values.get(path);
    if (c) {
      return changeText(c, t);
    }
    const field = fieldName(path, t).label;
    // 비밀은 값을 기록하지 않는다("비밀번호 바뀜(값은 기록하지 않음)")
    return t(path.endsWith(".$secret") ? "history.changeSecret" : "history.changeField", { field });
  });
  if (changed.length > SHOWN) {
    out.push(t("history.more", { count: changed.length - SHOWN }));
  }
  return out;
}
