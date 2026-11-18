import { useMemo } from "react";

import { useList } from "@/api/live";
import type { FolderKind } from "@/api/types";
import { folderPath, folderTree } from "@/folders/folders";

const KINDS: FolderKind[] = ["vm", "network", "storage"];

/**
 * 폴더 id → 경로. VM 트리는 `/teamA`, 다른 트리는 `network:/lan`처럼 종류를 앞에 붙인다(CLI와 같다). 볼 수 없는
 * 폴더는 id 끝이다.
 */
export function useFolderPaths(): (id: string) => string {
  const folders = useList("folder");
  return useMemo(() => {
    const trees = KINDS.map((kind) => [kind, folderTree(folders.data, kind)] as const);
    return (id: string) => {
      for (const [kind, tree] of trees) {
        if (tree.byId.has(id)) {
          const path = folderPath(tree, id);
          return kind === "vm" ? path : `${kind}:${path}`;
        }
      }
      return id.slice(-8);
    };
  }, [folders.data]);
}
