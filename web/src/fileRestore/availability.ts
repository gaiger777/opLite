import type { Node } from "@/api/types";

/** 파일 단위 복원을 아는 노드가 적는 기능(계획 69, 서버 `node::FILE_RESTORE`) */
export const FILE_RESTORE_FEATURE = "file-restore";

/**
 * 백업 표의 "파일 찾아보기" 동작: `hidden`(기능을 모르는 노드가 있다 — 서버가 409 `cluster_upgrading`), `unavailable`(도우미
 * 이미지가 있는 노드가 없다 — 막고 까닭을 보인다), `ready`. 노드를 읽을 수 없는 사용자(폴더 사용자)는 서버가 판단하게 보인다.
 * 읽는 중에는 숨긴다(보였다 사라지지 않게).
 */
export function fileRestoreAction(
  nodes: readonly Node[] | undefined,
  readable: boolean,
): "hidden" | "unavailable" | "ready" {
  if (!readable) {
    return "ready";
  }
  if (!nodes || nodes.length === 0) {
    return "hidden";
  }
  if (nodes.some((n) => !(n.status.features ?? []).includes(FILE_RESTORE_FEATURE))) {
    return "hidden";
  }
  return nodes.some((n) => n.status.file_restore) ? "ready" : "unavailable";
}

/** 백업의 파일 찾아보기 화면 주소 */
export function filesHref(backupId: string): string {
  return `/backups/${encodeURIComponent(backupId)}/files`;
}
