// 인벤토리 폴더(계획 62 설계 §5·§13 웹): 종류별 트리 셋, 경로, 하위 트리, 옮기기 API.
import { api, call } from "@/api/client";
import type { Folder, FolderKind } from "@/api/types";
import type { MemberKind } from "@/auth/permissions";

/** 폴더 보기의 목록 주소. 상세는 `{주소}/{id}`다. */
export const FOLDER_PATHS: Record<FolderKind, string> = {
  vm: "/vm-folders",
  network: "/network-folders",
  storage: "/storage-folders",
};

/** 트리의 객체 종류(서버 `FolderKind::member_kind`) */
export const MEMBER_KIND: Record<FolderKind, MemberKind> = {
  vm: "vm",
  network: "network",
  storage: "storage-pool",
};

/** 루트(폴더 없음)를 가리키는 트리 노드 id. 주소 `?folder=`에도 쓴다. */
export const ROOT_ID = "root";

/** 폴더가 있는 객체 */
export interface Foldered {
  id: string;
  name: string;
  spec: { folder?: string | null };
}

export interface FolderTree {
  /** 이 종류의 폴더(이름순) */
  folders: Folder[];
  byId: Map<string, Folder>;
  /** 부모 id(루트는 `ROOT_ID`) → 자식. 부모를 볼 수 없는 폴더는 루트 아래에 둔다(CLI와 같다). */
  children: Map<string, Folder[]>;
}

export function folderTree(all: readonly Folder[] | undefined, kind: FolderKind): FolderTree {
  const folders = (all ?? [])
    .filter((f) => f.spec.kind === kind)
    .sort((a, b) => a.spec.name.localeCompare(b.spec.name));
  const byId = new Map(folders.map((f) => [f.id, f]));
  const children = new Map<string, Folder[]>();
  for (const f of folders) {
    const parent = f.spec.parent && byId.has(f.spec.parent) ? f.spec.parent : ROOT_ID;
    children.set(parent, [...(children.get(parent) ?? []), f]);
  }
  return { folders, byId, children };
}

/** 폴더의 경로 `/teamA/prod`(루트는 `/`). 볼 수 없는 조상에서 끊는다. */
export function folderPath(tree: FolderTree, id: string | null | undefined): string {
  const names: string[] = [];
  const seen = new Set<string>();
  let at = id ? tree.byId.get(id) : undefined;
  while (at && !seen.has(at.id)) {
    seen.add(at.id);
    names.unshift(at.spec.name);
    at = at.spec.parent ? tree.byId.get(at.spec.parent) : undefined;
  }
  return `/${names.join("/")}`;
}

/** 이 폴더와 그 아래 모든 폴더의 id */
export function subtree(tree: FolderTree, id: string): Set<string> {
  const out = new Set<string>();
  const walk = (at: string) => {
    if (out.has(at)) {
      return;
    }
    out.add(at);
    for (const child of tree.children.get(at) ?? []) {
      walk(child.id);
    }
  };
  walk(id);
  return out;
}

/**
 * 폴더(`ROOT_ID`면 루트)에 바로 든 객체, `deep`이면 하위 폴더의 것까지. 루트의 하위 포함은 모두다. 볼 수 없는
 * 폴더에 든 객체(객체 grant만 받은 것)는 루트에 있는 것으로 보인다.
 */
export function membersOf<T extends Foldered>(
  tree: FolderTree,
  items: readonly T[],
  folder: string,
  deep: boolean,
): T[] {
  const where = (item: T) =>
    item.spec.folder && tree.byId.has(item.spec.folder) ? item.spec.folder : ROOT_ID;
  if (folder === ROOT_ID) {
    return deep ? [...items] : items.filter((item) => where(item) === ROOT_ID);
  }
  const inside = deep ? subtree(tree, folder) : new Set([folder]);
  return items.filter((item) => inside.has(where(item)));
}

export type MoveResult = Awaited<ReturnType<typeof moveMembers>>[number];

/** 객체들을 폴더(`null`이면 루트)로 옮긴다. 항목마다 결과(`ok`, 실패면 `error`)다. */
export async function moveMembers(
  folder: string | null,
  kind: MemberKind,
  keys: readonly string[],
) {
  const result = await call(
    api.POST("/folders/{id}/members", {
      params: { path: { id: folder ?? ROOT_ID } },
      body: { items: keys.map((key) => ({ kind, key })) },
    }),
  );
  return result.items;
}

/** 드래그로 옮기는 객체(HTML5 DnD의 `dataTransfer` 형식) */
export const DRAG_TYPE = "application/x-orbivisor-members";

export interface DragPayload {
  kind: MemberKind;
  /** 객체 id */
  ids: string[];
  /** 객체 이름(옮기기 API의 key) */
  names: string[];
  /** 지금 폴더(`ROOT_ID` 포함) — 같은 폴더에는 놓지 않는다 */
  from: string[];
}
