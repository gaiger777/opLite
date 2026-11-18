// 최근 항목과 즐겨찾기(설계 §5.7). 브라우저의 `prefs`에 사용자마다 따로 둔다(`{user}.recent`·`{user}.pins`).
// 지운 리소스는 읽는 쪽이 지금 목록과 맞춰 뺀다(`alive`).
import { useCallback, useEffect, useState } from "react";

import type { WatchKind } from "@/api/types";
import { readPref, writePref } from "@/lib/prefs";

/** 최근 항목·즐겨찾기 하나. 이름은 주소를 만들고 보일 때 쓰고, 같은지는 종류와 id로 본다. */
export interface ResourceRef {
  kind: WatchKind;
  id: string;
  name: string;
}

/** 즐겨찾기를 둘 수 있는 종류 */
export const PIN_KINDS: readonly WatchKind[] = ["vm", "node", "storage-pool"];

export const MAX_RECENT = 20;

const recentKey = (user: string) => `${user}.recent`;
const pinsKey = (user: string) => `${user}.pins`;
/** 같은 창의 다른 컴포넌트에 바뀐 것을 알린다(`storage` 이벤트는 다른 창에만 간다). */
const CHANGED = "opl:refs";

function valid(value: unknown): value is ResourceRef {
  return (
    typeof value === "object" &&
    value !== null &&
    "kind" in value &&
    typeof value.kind === "string" &&
    "id" in value &&
    typeof value.id === "string" &&
    "name" in value &&
    typeof value.name === "string"
  );
}

function read(key: string): ResourceRef[] {
  try {
    const value: unknown = JSON.parse(readPref(key) ?? "[]");
    return Array.isArray(value) ? value.filter(valid) : [];
  } catch {
    return [];
  }
}

function write(key: string, refs: ResourceRef[]) {
  writePref(key, JSON.stringify(refs));
  window.dispatchEvent(new Event(CHANGED));
}

const same = (a: ResourceRef, b: ResourceRef) => a.kind === b.kind && a.id === b.id;

export function readRecent(user: string): ResourceRef[] {
  return read(recentKey(user));
}

/** 맨 앞에 넣는다(있던 것은 앞으로 옮기고 이름을 새로 한다). 20개까지 둔다. */
export function pushRecent(user: string, ref: ResourceRef) {
  const now = readRecent(user);
  const next = [ref, ...now.filter((r) => !same(r, ref))].slice(0, MAX_RECENT);
  // 그대로이면 쓰지 않는다(다시 그려도 알림이 돌지 않게).
  if (JSON.stringify(now) !== JSON.stringify(next)) {
    write(recentKey(user), next);
  }
}

export function readPins(user: string): ResourceRef[] {
  return read(pinsKey(user));
}

/** 즐겨찾기에 넣거나 뺀다. 넣었으면 참이다. */
export function togglePin(user: string, ref: ResourceRef): boolean {
  const pins = readPins(user);
  const pinned = pins.some((p) => same(p, ref));
  write(pinsKey(user), pinned ? pins.filter((p) => !same(p, ref)) : [...pins, ref]);
  return !pinned;
}

/**
 * 지금 목록에 있는 것만 남기고 이름을 지금 이름으로 한다. 목록을 아직 모르는 종류(`lists`에 없음)는 그대로 둔다.
 */
export function alive(
  refs: readonly ResourceRef[],
  lists: Partial<Record<WatchKind, readonly { id: string; name: string }[] | undefined>>,
): ResourceRef[] {
  return refs.flatMap((ref) => {
    const list = lists[ref.kind];
    if (!list) {
      return [ref];
    }
    const found = list.find((item) => item.id === ref.id);
    return found ? [{ ...ref, name: found.name }] : [];
  });
}

/** 바뀔 때마다 다시 읽는 최근 항목·즐겨찾기 */
export function useRefs(user: string) {
  const [state, setState] = useState(() => ({ recent: readRecent(user), pins: readPins(user) }));
  useEffect(() => {
    const reload = () => setState({ recent: readRecent(user), pins: readPins(user) });
    reload();
    window.addEventListener(CHANGED, reload);
    window.addEventListener("storage", reload);
    return () => {
      window.removeEventListener(CHANGED, reload);
      window.removeEventListener("storage", reload);
    };
  }, [user]);
  const toggle = useCallback((ref: ResourceRef) => togglePin(user, ref), [user]);
  return { ...state, toggle };
}
