// 새 VM 폴더 고르기(계획 62·71): 만들기 화면이 쓴다(첫 화면 묶음 밖 — 만들기 조각만 읽는다). 폼 ↔ 빌더 초안(계획 71
// §8.6)은 계획 77-5에서 두 화면이 하나가 되어 없앴다.
import type { Permissions } from "@/auth/permissions";
import { folderPath, type FolderTree } from "@/folders/folders";

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
