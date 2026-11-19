import { useList } from "@/api/live";
import type { Node } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";

/**
 * 풀을 거를 노드 목록(`choosablePools`). 노드를 읽지 못하면(루트 `cluster.read` 없음) null이다. 그때 노드 목록은
 * 영영 오지 않으니 비어 있는 것으로 보지 않는다.
 */
export function usePoolNodes(): Node[] | null {
  const nodes = useList("node");
  const readable = usePermissions().reads("node");
  return readable ? (nodes.data ?? []) : null;
}
