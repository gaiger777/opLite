// 폼 ↔ 빌더 초안(계획 71 설계 §8.6): 보기를 바꿀 때 지금 값을 탭 안 메모리(모듈 변수)에 두고 넘긴다. 주소의 `state`나
// `sessionStorage`에 두지 않는 까닭은 cloud-init 평문 비밀번호가 기록에 남기 때문이다. 새로 고치면 초안은 없다.
// 마지막 보기(`vmCreate.mode`)는 사용자별 설정이다. 두 만들기 보기가 함께 쓰는 새 VM 폴더 고르기도 여기 있다(첫 화면
// 묶음 밖 — 폼과 빌더 조각만 읽는다).
import type { Permissions } from "@/auth/permissions";
import { folderPath, type FolderTree } from "@/folders/folders";
import { readUserPref, writeUserPref } from "@/lib/prefs";
import type { VmFormValues } from "@/vms/form";
import type { Preset } from "@/vms/form/steps";

export interface Draft {
  values: VmFormValues;
  /** 폼이 열 프리셋(빌더의 템플릿 바탕은 폼의 "템플릿에서") */
  preset?: Preset;
}

let draft: Draft | null = null;

export function putDraft(d: Draft): void {
  draft = d;
}

/** 초안(화면의 처음 값 — 상태 초기화 함수가 두 번 불려도 같은 값). 받은 화면이 붙은 뒤 `clearDraft`로 지운다. */
export function readDraft(): Draft | null {
  return draft;
}

export function clearDraft(): void {
  draft = null;
}

export type CreateMode = "form" | "builder";

export function readMode(user: string): CreateMode {
  return readUserPref(user, "vmCreate.mode") === "builder" ? "builder" : "form";
}

export function writeMode(user: string, mode: CreateMode): void {
  writeUserPref(user, "vmCreate.mode", mode);
}

/**
 * 새 VM의 폴더: 주소의 `?folder=`(폴더 보기의 "여기에 만들기"), 아니면 루트에 만들 수 있으면 루트, 아니면
 * `vm.create`가 있는 첫 폴더(경로순)다.
 */
export function defaultFolder(
  perms: Pick<Permissions, "has">,
  tree: FolderTree,
  asked: string | null,
): string {
  if (asked && tree.byId.has(asked)) {
    return asked;
  }
  if (perms.has("vm.create")) {
    return "";
  }
  const allowed = tree.folders
    .filter((f) => perms.has("vm.create", { type: "object", kind: "vm", folder: f.id }))
    .sort((a, b) => folderPath(tree, a.id).localeCompare(folderPath(tree, b.id)));
  return allowed[0]?.id ?? "";
}
