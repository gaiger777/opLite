import type { Snapshot } from "@/api/types";
import { validName } from "@/vms/form";

/** 서버의 `MAX_DESCRIPTION_CHARS`(문자 수) */
const MAX_DESCRIPTION_CHARS = 1024;

/** 나무 목록의 한 줄: 스냅샷과 그 깊이(뿌리가 0) */
export interface TreeRow {
  snapshot: Snapshot;
  depth: number;
}

/**
 * 한 VM의 스냅샷을 나무 차례로 편다. 형제는 id(UUIDv7) 차례, 곧 만든 차례다. 목록에 없는 부모를
 * 가리키는 스냅샷은 뿌리로 둔다.
 */
export function snapshotTree(snapshots: readonly Snapshot[]): TreeRow[] {
  const ids = new Set(snapshots.map((s) => s.id));
  const sorted = [...snapshots].sort((a, b) => (a.id < b.id ? -1 : 1));
  const children = new Map<string | null, Snapshot[]>();
  for (const s of sorted) {
    const parent = s.spec.parent && ids.has(s.spec.parent) ? s.spec.parent : null;
    children.set(parent, [...(children.get(parent) ?? []), s]);
  }
  const rows: TreeRow[] = [];
  const seen = new Set<string>();
  const visit = (parent: string | null, depth: number) => {
    for (const s of children.get(parent) ?? []) {
      if (!seen.has(s.id)) {
        seen.add(s.id);
        rows.push({ snapshot: s, depth });
        visit(s.id, depth + 1);
      }
    }
  };
  visit(null, 0);
  // 부모가 돌고 도는(망가진) 스냅샷은 뿌리에서 닿지 않으므로 남은 것을 뿌리로 더한다.
  for (const s of sorted) {
    if (!seen.has(s.id)) {
      seen.add(s.id);
      rows.push({ snapshot: s, depth: 0 });
      visit(s.id, 1);
    }
  }
  return rows;
}

/** 새 스냅샷 이름의 입력 오류(번역 키). `taken`은 이 VM에 이미 있는 스냅샷 이름이다. */
export function snapshotNameError(name: string, taken: readonly string[]): string | undefined {
  if (!validName(name)) {
    return "forms.name";
  }
  if (taken.includes(name)) {
    return "snapshots.errors.exists";
  }
  return undefined;
}

/** 설명의 입력 오류(번역 키). 서버처럼 바이트가 아니라 문자 수로 센다. */
export function descriptionError(description: string): string | undefined {
  return [...description].length > MAX_DESCRIPTION_CHARS
    ? "snapshots.errors.description"
    : undefined;
}
